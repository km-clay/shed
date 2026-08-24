//! Signal handling for the shell.
//! This module provides functions to install signal handlers, check for pending signals, and handle them appropriately.
//! It also provides functions to manage the job table and child processes in response to signals.

use std::{
  collections::VecDeque,
  sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
};

use nix::{
  libc,
  sys::{
    signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction},
    wait::{WaitPidFlag as WtFlag, WaitStatus as WtStat, waitpid},
  },
  unistd::{Pid, getpgid, setpgid},
};

use super::{
  autocmd,
  eval::execute::exec_nonint,
  sherr,
  state::{
    Shed,
    jobs::{Job, JobData, JobID, SIG_EXIT_OFFSET, take_term},
    logic::TrapTarget,
    meta::MetaTab,
    util::with_vars,
    vars::{Var, VarFlags, VarKind},
  },
  system_msg,
  util::ShResult,
};

use crate::{HashMap, state::vars::VarStr, varstr};

/// A bitset representing all signals that have been received but not yet handled by `check_signals`.
/// "indexed" by bit shifting the signal number (e.g. `1 << SIGINT` for SIGINT).
static SIGNALS: AtomicU64 = AtomicU64::new(0);

/// Signals that don't warrant interrupting a blocking builtin (`read`/`wait`):
/// child/window/urgent/continue notifications the wait loop handles by retrying.
const BENIGN_SIGNALS: u64 = (1 << Signal::SIGCHLD as u64)
  | (1 << Signal::SIGWINCH as u64)
  | (1 << Signal::SIGURG as u64)
  | (1 << Signal::SIGCONT as u64);

/// Whether the SIGCHLD handler is enabled. If disabled, SIGCHLD is ignored and the shell will not reap child processes.
/// This is used in `exec_nonint` to avoid reaping children that are being waited on by the caller.
pub static REAPING_ENABLED: AtomicBool = AtomicBool::new(true);
/// Whether the shell should exit cleanly. Set by `hang_up`, `check_signals`, and other signal handlers.
pub static SHOULD_QUIT: AtomicBool = AtomicBool::new(false);
/// Whether a job has finished and needs to be reaped.
/// Set by `child_exited` and cleared by the main loop after reaping.
pub static JOB_DONE: AtomicBool = AtomicBool::new(false);
/// The exit code to use when quitting cleanly. Set by `hang_up`, `check_signals`, and other signal handlers.
pub static QUIT_CODE: AtomicI32 = AtomicI32::new(0);
/// When exiting by signal, signal number is stored here.
pub static QUIT_SIGNAL: AtomicI32 = AtomicI32::new(-1);

/// Window size change signal
pub static GOT_SIGWINCH: AtomicBool = AtomicBool::new(false);

/// SIGUSR1 tells the prompt that it needs to fully refresh.
/// Useful for dynamic prompt content and asynchronous refreshing
pub static GOT_SIGUSR1: AtomicBool = AtomicBool::new(false);

/// The terminal has notified us that it has regained focus
/// We refresh the prompt now
pub static FOCUS_GAINED: AtomicBool = AtomicBool::new(false);

/// Signals that are not handled specially in `check_signals` and thus are
/// handled generically by checking for a trap and, if none, terminating the shell
/// in a non-interactive shell.
const MISC_SIGNALS: &[Signal] = &[
  Signal::SIGINT,
  Signal::SIGHUP,
  Signal::SIGTERM,
  Signal::SIGQUIT,
  Signal::SIGUSR1,
  Signal::SIGUSR2,
  Signal::SIGPIPE,
  Signal::SIGCHLD,
  Signal::SIGALRM,
  Signal::SIGCONT,
  Signal::SIGURG,
  Signal::SIGXCPU,
  Signal::SIGXFSZ,
  Signal::SIGVTALRM,
  Signal::SIGPROF,
  Signal::SIGWINCH,
  Signal::SIGIO,
  Signal::SIGSYS,
  #[cfg(linux_like)]
  Signal::SIGSTKFLT,
  #[cfg(linux_like)]
  Signal::SIGPWR,
];

pub fn quit_signal() -> Option<Signal> {
  let sig = QUIT_SIGNAL.swap(-1, Ordering::SeqCst);
  if sig > 0 {
    Signal::try_from(sig).ok()
  } else {
    None
  }
}

pub fn parse_signal(s: &str) -> ShResult<Signal> {
  // Try as signal name (e.g. "TERM", "SIGTERM", "term")
  let upper = s.to_uppercase();
  if let Ok(sig) = upper.parse::<Signal>() {
    return Ok(sig);
  }
  if let Ok(sig) = format!("SIG{upper}").parse::<Signal>() {
    return Ok(sig);
  }
  // Try as number (e.g. "9", "137")
  if let Ok(mut n) = s.parse::<usize>() {
    if n > 128 {
      n -= 128;
    }
    if let Ok(sig) = Signal::try_from(n as i32) {
      return Ok(sig);
    }
  }
  Err(sherr!(SyntaxErr, "Invalid signal name or number: {s}"))
}

pub fn signals_pending() -> bool {
  SIGNALS.load(Ordering::SeqCst) != 0 || SHOULD_QUIT.load(Ordering::SeqCst)
}

pub fn sigint_pending() -> bool {
  SIGNALS.load(Ordering::SeqCst) & (1 << Signal::SIGINT as u64) != 0
}

/// Whether a pending signal warrants interrupting a blocking `read`/`wait`.
pub fn has_actionable_pending() -> bool {
  if SHOULD_QUIT.load(Ordering::SeqCst) {
    return true;
  }
  SIGNALS.load(Ordering::SeqCst) & !BENIGN_SIGNALS != 0
}

/// The first available interrupting signal, as an `i32`
pub fn first_actionable_signal() -> Option<i32> {
  let pending = SIGNALS.load(Ordering::SeqCst) & !BENIGN_SIGNALS;
  MISC_SIGNALS
    .iter()
    .copied()
    .find(|s| pending & (1 << *s as u64) != 0)
    .map(|s| s as i32)
}

/// Mark the shell for a clean exit with the wait-style status for `sig`.
fn request_quit(sig: Signal) {
  SHOULD_QUIT.store(true, Ordering::SeqCst);
  QUIT_CODE.store(SIG_EXIT_OFFSET + sig as i32, Ordering::SeqCst);
  QUIT_SIGNAL.store(sig as i32, Ordering::SeqCst);
}

/// Signals that get bespoke handling in `check_signals` before the generic
/// loop. They must be skipped by that loop so their trap doesn't fire twice.
fn has_dedicated_handling(sig: Signal) -> bool {
  matches!(
    sig,
    Signal::SIGINT
      | Signal::SIGHUP
      | Signal::SIGTSTP
      | Signal::SIGCHLD
      | Signal::SIGWINCH
      | Signal::SIGUSR1
      | Signal::SIGTERM
  )
}

/// Whether the OS default disposition for `sig` terminates the process. Used
/// to decide what an untrapped signal does in a non-interactive shell.
fn default_terminates(sig: Signal) -> bool {
  !matches!(
    sig,
    Signal::SIGURG | Signal::SIGCONT | Signal::SIGCHLD | Signal::SIGWINCH
  )
}

/// Check for any pending signals and handle them.
///
/// Returns an error if the shell should exit or if a signal handler requested an interrupt.
///
/// NOTE: the "errors" returned here do not represent failures in the signal handling itself,
/// but rather control flow signals to the shell's main loop.
/// We basically abuse Rust's error propagation to abort execution and travel upward
/// to a place that catches and handles it.
pub fn check_signals() -> ShResult<()> {
  let pending = SIGNALS.swap(0, Ordering::SeqCst);

  let got_signal = |sig: Signal| -> bool { pending & (1 << sig as u64) != 0 };
  // Returns whether a trap was actually registered (and thus ran), so callers
  // can decide what the default action should be when there's no trap.
  let run_trap = |sig: Signal| -> ShResult<bool> {
    if let Some(command) = Shed::logic(|l| l.get_trap(TrapTarget::Signal(sig))) {
      exec_nonint(command, Some("trap".into()))?;
      Ok(true)
    } else {
      Ok(false)
    }
  };

  if got_signal(Signal::SIGINT) {
    interrupt()?;
    // SIGINT with a trap allows execution to continue. SIGINT with no trap interrrupts.
    if !run_trap(Signal::SIGINT)? {
      return Err(sherr!(Interrupt, "Interrupted"));
    }
  }
  if got_signal(Signal::SIGHUP) {
    run_trap(Signal::SIGHUP)?;
    hang_up(0);
  }
  if got_signal(Signal::SIGTSTP) {
    run_trap(Signal::SIGTSTP)?;
    terminal_stop()?;
  }
  if got_signal(Signal::SIGCHLD) && REAPING_ENABLED.load(Ordering::SeqCst) {
    run_trap(Signal::SIGCHLD)?;
    wait_child()?;
  }
  if got_signal(Signal::SIGWINCH) {
    GOT_SIGWINCH.store(true, Ordering::SeqCst);
    run_trap(Signal::SIGWINCH)?;
  }
  if got_signal(Signal::SIGUSR1) {
    GOT_SIGUSR1.store(true, Ordering::SeqCst);
    run_trap(Signal::SIGUSR1)?;
  }
  if got_signal(Signal::SIGTERM) {
    let trapped = run_trap(Signal::SIGTERM)?;
    if !trapped && !Shed::meta(MetaTab::interactive_shell) {
      request_quit(Signal::SIGTERM);
    }
  }

  for &sig in MISC_SIGNALS {
    if has_dedicated_handling(sig) || !got_signal(sig) {
      continue;
    }
    let trapped = run_trap(sig)?;
    if !trapped && !Shed::meta(MetaTab::interactive_shell) && default_terminates(sig) {
      request_quit(sig);
    }
  }

  if SHOULD_QUIT.load(Ordering::SeqCst) {
    let code = QUIT_CODE.load(Ordering::SeqCst);
    return Err(sherr!(CleanExit(code), "exit"));
  }
  Ok(())
}

pub fn disable_reaping() {
  REAPING_ENABLED.store(false, Ordering::SeqCst);
}
pub fn enable_reaping() {
  REAPING_ENABLED.store(true, Ordering::SeqCst);
}

pub fn install_signal_handlers() {
  let flags = SaFlags::empty();
  let action = SigAction::new(SigHandler::Handler(handle_signal), flags, SigSet::empty());

  unsafe {
    for sig in MISC_SIGNALS {
      sigaction(*sig, &action).unwrap();
    }
  }
}

/// Set up signal dispositions for the shell process. Called once at startup.
///
/// SIGTTIN and SIGTTOU are ignored so that the shell can read/write to the terminal,
/// even if it's in the background.
pub fn sig_setup() {
  install_signal_handlers();

  let flags = SaFlags::empty();
  let ignore = SigAction::new(SigHandler::SigIgn, flags, SigSet::empty());

  unsafe {
    sigaction(Signal::SIGTTIN, &ignore).unwrap();
    sigaction(Signal::SIGTTOU, &ignore).unwrap();
  }

  let _ = setpgid(Pid::from_raw(0), Pid::from_raw(0));
  take_term().ok();
}

/// Reset signal dispositions to `SIG_DFL`.
/// Called in child processes before exec so that the shell's custom
/// handlers and `SIG_IGN` dispositions don't leak into child programs.
pub fn reset_signals(is_fg: bool) {
  let default = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
  unsafe {
    sigaction(Signal::SIGPIPE, &default).ok();
    if !is_fg {
      sigaction(Signal::SIGTTIN, &default).ok();
      sigaction(Signal::SIGTTOU, &default).ok();
    }
  }
}

pub fn clear_quit_latch() {
  SHOULD_QUIT.store(false, Ordering::SeqCst);
  SIGNALS.store(0, Ordering::SeqCst);
  QUIT_SIGNAL.store(-1, Ordering::SeqCst);
}

extern "C" fn handle_signal(sig: libc::c_int) {
  SIGNALS.fetch_or(1 << sig, Ordering::SeqCst);
}

pub fn hang_up(_: libc::c_int) {
  SHOULD_QUIT.store(true, Ordering::SeqCst);
  QUIT_CODE.store(1, Ordering::SeqCst);
  Shed::jobs_mut(|j| {
    j.hang_up();
  });
}

/// Send SIGTSTP to the foreground job, if any, to stop it and return control of the terminal to the shell.
///
/// This is called when the user presses Ctrl-Z, or when a SIGTSTP signal is received.
pub fn terminal_stop() -> ShResult<()> {
  Shed::jobs_mut(|j| {
    if let Some(job) = j.get_fg_mut() {
      job.killpg(Signal::SIGTSTP)
    } else {
      Ok(())
    }
  })
  // TODO: It seems like there is supposed to be a take_term() call here, needs testing
}

pub fn interrupt() -> ShResult<()> {
  Shed::jobs_mut(|j| {
    if let Some(job) = j.get_fg_mut() {
      job.killpg(Signal::SIGINT)
    } else {
      Ok(())
    }
  })
}

/// Wait for any child processes to change state (exit, stop, continue) and update the job table accordingly.
pub fn wait_child() -> ShResult<()> {
  let flags = WtFlag::WNOHANG | WtFlag::WUNTRACED;
  while let Ok(status) = waitpid(None, Some(flags)) {
    match status {
      WtStat::Exited(pid, _) => {
        child_exited(pid, status)?;
      }
      WtStat::Signaled(pid, signal, _) => {
        child_signaled(pid, signal);
      }
      WtStat::Stopped(pid, signal) => {
        child_stopped(pid, signal)?;
      }
      WtStat::Continued(pid) => {
        child_continued(pid);
      }
      WtStat::StillAlive => {
        break;
      }
      #[cfg(linux_like)]
      _ => unimplemented!(),
    }
  }
  Ok(())
}

/// Child process received a signal (e.g. SIGINT, SIGTERM, etc).
pub fn child_signaled(pid: Pid, sig: Signal) {
  Shed::jobs_mut(|j| {
    if let Some(job) = j.query_mut(JobID::Pid(pid))
      && let Some(child) = job.children_mut().iter_mut().find(|chld| pid == chld.pid())
    {
      child.set_stat(WtStat::Signaled(pid, sig, false));
    }
  });
  if sig == Signal::SIGINT {
    take_term().unwrap();
  }
}

/// Child process stopped (received SIGTSTP).
pub fn child_stopped(pid: Pid, sig: Signal) -> ShResult<()> {
  let child_pgid = getpgid(Some(pid)).unwrap_or(pid);
  Shed::jobs_mut(|j| {
    if let Some(job) = j.query_mut(JobID::Pgid(child_pgid)) {
      if let Some(child) = job.children_mut().iter_mut().find(|chld| pid == chld.pid()) {
        child.set_stat(WtStat::Stopped(pid, sig));
      }
    } else if j.get_fg_mut().is_some_and(|fg| fg.pgid() == child_pgid) {
      j.fg_to_bg(WtStat::Stopped(pid, sig)).unwrap();
    }
  });
  take_term()?;
  Ok(())
}

/// Child process continued (received SIGCONT).
/// Resume the job in the job table if it exists.
pub fn child_continued(pid: Pid) {
  let child_pgid = getpgid(Some(pid)).unwrap_or(pid);
  Shed::jobs_mut(|j| {
    if let Some(job) = j.query_mut(JobID::Pgid(child_pgid)) {
      job.killpg(Signal::SIGCONT).ok();
    }
  });
}

/// Child process exited normally.
pub fn child_exited(pid: Pid, status: WtStat) -> ShResult<()> {
  /*
   * Here we are going to get metadata on the exited process by querying the
   * job table with the pid. Then if the discovered job is the fg task,
   * return terminal control to shed. If it is not the fg task, print the
   * display info for the job in the job table. We can reasonably assume that
   * if it is not a foreground job, then it exists in the job table.
   * If this assumption is incorrect, the code has gone wrong somewhere.
   */

  let child_data = Shed::jobs_mut(|j| {
    let fg_pgid = j.get_fg().map(Job::pgid);

    // update the job table with the new status for the child process
    j.query_mut(JobID::Pid(pid)).map(|job| {
      let child_pgid = job.pgid();
      let is_fg = fg_pgid.is_some_and(|fg| fg == child_pgid);
      job.update_by_id(JobID::Pid(pid), status);
      let is_finished = !job.running();

      if let Some(child) = job.children_mut().iter_mut().find(|chld| pid == chld.pid()) {
        child.set_stat(status);
      }

      (child_pgid, is_fg, is_finished)
    })
  });

  let Some((child_pgid, is_fg, is_finished)) = child_data else {
    return Ok(());
  };
  if !is_finished {
    return Ok(());
  }

  if is_fg {
    return take_term();
  }

  // If it was a background job, we need to notify the main loop
  JOB_DONE.store(true, Ordering::SeqCst);
  let job_data = Shed::jobs_mut(|j| {
    let order = j.marker_order();
    j.query_mut(JobID::Pgid(child_pgid))
      .map(|job| job.take_job_data(&order, Some(pid)))
  });

  let Some(JobData {
    timer: _timer,
    table_id,
    notify,
    stats,
    cmds,
    display,
  }) = job_data
  else {
    return Ok(());
  };

  for status in &stats {
    if let WtStat::Signaled(_, Signal::SIGINT, _) = status {
      // Inherit SIGINT
      handle_signal(Signal::SIGINT as i32);
    }
  }

  // Set PIPESTATUS
  if let Some(pipe_status) = Job::pipe_status(&stats) {
    let pipe_status = pipe_status
      .into_iter()
      .map(|s| s.to_string())
      .collect::<VecDeque<String>>();

    Shed::vars_mut(|v| {
      v.set_var(
        "PIPESTATUS",
        VarKind::arr(pipe_status.into_iter().map(Into::into)),
        VarFlags::empty(),
      )
    })?;
  }

  let status_strs = stats.iter().map(|s| match s {
    WtStat::Exited(_, code) => varstr!("{code}"),
    WtStat::Signaled(_, sig, _) => varstr!("{}", 128 + *sig as i32),
    _ => "1".into(),
  });

  let children: Vec<(VarStr, VarStr)> = cmds.into_iter().zip(status_strs).collect();
  let last_status = children.last().map(|c| c.1.clone()).unwrap_or_default();
  let cmd_count = children.len();

  // now run our post job autocmds
  // with these variables set
  let post_job_vars: HashMap<VarStr, Var> = [
    (
      "CHILDREN".into(),
      Var::new(VarKind::assoc_arr(children), VarFlags::empty()),
    ),
    (
      "CHILD_COUNT".into(),
      Var::new(
        VarKind::string(cmd_count.to_string().into()),
        VarFlags::empty(),
      ),
    ),
    (
      "JOB_ID".into(),
      Var::new(VarKind::string(table_id), VarFlags::empty()),
    ),
    (
      "JOB_STATUS".into(),
      Var::new(VarKind::string(last_status), VarFlags::empty()),
    ),
  ]
  .into_iter()
  .collect();

  with_vars(post_job_vars, || autocmd!(OnJobFinish));

  // post the job status notification
  if notify {
    system_msg!("{display}");
  }

  Shed::jobs(|j| {
    if let Some(job) = j.query(JobID::Pgid(child_pgid)) {
      Shed::notify_job_complete(job);
    }
  });

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::ShErrKind;
  use crate::state::logic::TrapTarget;
  use crate::tests::testutil::TestGuard;

  /// Reset all signal-related global state so tests don't pollute each
  /// other. Call at the top of every `check_signals` test.
  fn reset_signal_state() {
    SIGNALS.store(0, Ordering::SeqCst);
    SHOULD_QUIT.store(false, Ordering::SeqCst);
    QUIT_CODE.store(0, Ordering::SeqCst);
    GOT_SIGWINCH.store(false, Ordering::SeqCst);
    GOT_SIGUSR1.store(false, Ordering::SeqCst);
    JOB_DONE.store(false, Ordering::SeqCst);
  }

  fn set_signal(sig: Signal) {
    SIGNALS.fetch_or(1 << sig as u64, Ordering::SeqCst);
  }

  // ─── No pending signals ──────────────────────────────────────────────

  #[test]
  fn check_signals_no_pending_is_ok() {
    let _g = TestGuard::new();
    reset_signal_state();
    assert!(check_signals().is_ok());
  }

  #[test]
  fn check_signals_clears_pending_bitmask() {
    let _g = TestGuard::new();
    reset_signal_state();
    Shed::meta_mut(|m| m.set_interactive_shell(true));
    set_signal(Signal::SIGUSR2);
    assert!(check_signals().is_ok());
    assert_eq!(SIGNALS.load(Ordering::SeqCst), 0);
  }

  #[test]
  fn check_signals_untrapped_misc_terminates_non_interactive() {
    let _g = TestGuard::new();
    reset_signal_state();
    Shed::meta_mut(|m| m.set_interactive_shell(false));
    // A non-interactive shell takes the default (terminate) action for an
    // untrapped fatal signal instead of silently swallowing it.
    set_signal(Signal::SIGUSR2);
    let result = check_signals();
    let quit = SHOULD_QUIT.load(Ordering::SeqCst);
    // Clear the exit request we just raised before asserting, so it can't
    // leak into other tests sharing this process (these globals are static).
    reset_signal_state();
    let err = result.expect_err("untrapped SIGUSR2 should quit");
    assert!(matches!(err.kind(), ShErrKind::CleanExit(_)));
    assert!(quit);
  }

  // ─── SIGINT → interrupt + Err(Interrupt) ─────────────────────────────

  #[test]
  fn check_signals_sigint_returns_interrupt_err() {
    let _g = TestGuard::new();
    reset_signal_state();
    set_signal(Signal::SIGINT);
    let err = check_signals().expect_err("SIGINT should return Err");
    assert!(matches!(err.kind(), ShErrKind::Interrupt));
  }

  // ─── SIGHUP → SHOULD_QUIT + QUIT_CODE=1 ──────────────────────────────

  #[test]
  fn check_signals_sighup_sets_should_quit() {
    let _g = TestGuard::new();
    reset_signal_state();
    set_signal(Signal::SIGHUP);
    // hang_up() sets SHOULD_QUIT and QUIT_CODE; the post-loop check
    // converts that into Err(CleanExit).
    let err = check_signals().expect_err("SIGHUP should trigger CleanExit");
    assert!(matches!(err.kind(), ShErrKind::CleanExit(1)));
    assert!(SHOULD_QUIT.load(Ordering::SeqCst));
  }

  // ─── SIGCHLD: gated by REAPING_ENABLED ──────────────────────────────

  #[test]
  fn check_signals_sigchld_when_reaping_disabled_is_noop() {
    let _g = TestGuard::new();
    reset_signal_state();
    disable_reaping();
    // The defer here ensures we re-enable for other tests in the same
    // run-thread.
    crate::defer! { enable_reaping(); }
    set_signal(Signal::SIGCHLD);
    assert!(
      check_signals().is_ok(),
      "SIGCHLD with reaping disabled should not error"
    );
  }

  #[test]
  fn check_signals_sigchld_with_no_children_is_ok() {
    let _g = TestGuard::new();
    reset_signal_state();
    enable_reaping();
    set_signal(Signal::SIGCHLD);
    // wait_child does WNOHANG and breaks on StillAlive — with no
    // children it returns immediately.
    assert!(check_signals().is_ok());
  }

  // ─── SIGWINCH → sets GOT_SIGWINCH ───────────────────────────────────

  #[test]
  fn check_signals_sigwinch_sets_flag() {
    let _g = TestGuard::new();
    reset_signal_state();
    set_signal(Signal::SIGWINCH);
    check_signals().unwrap();
    assert!(GOT_SIGWINCH.load(Ordering::SeqCst));
  }

  // ─── SIGUSR1 → sets GOT_SIGUSR1 ─────────────────────────────────────

  #[test]
  fn check_signals_sigusr1_sets_flag() {
    let _g = TestGuard::new();
    reset_signal_state();
    set_signal(Signal::SIGUSR1);
    check_signals().unwrap();
    assert!(GOT_SIGUSR1.load(Ordering::SeqCst));
  }

  // ─── SIGTERM: branches on interactive_shell flag ────────────────────

  #[test]
  fn check_signals_sigterm_in_non_interactive_shell_quits() {
    let _g = TestGuard::new();
    reset_signal_state();
    Shed::meta_mut(|m| m.set_interactive_shell(false));
    set_signal(Signal::SIGTERM);
    let err = check_signals().expect_err("SIGTERM in non-interactive quits");
    assert!(matches!(err.kind(), ShErrKind::CleanExit(_)));
    assert!(SHOULD_QUIT.load(Ordering::SeqCst));
    assert_eq!(
      QUIT_CODE.load(Ordering::SeqCst),
      SIG_EXIT_OFFSET + Signal::SIGTERM as i32
    );
  }

  #[test]
  fn check_signals_sigterm_in_interactive_shell_is_ignored() {
    let _g = TestGuard::new();
    reset_signal_state();
    Shed::meta_mut(|m| m.set_interactive_shell(true));
    set_signal(Signal::SIGTERM);
    // POSIX: interactive shell ignores SIGTERM except for trap firing.
    assert!(check_signals().is_ok());
    assert!(!SHOULD_QUIT.load(Ordering::SeqCst));
  }

  // ─── Combined: pending SHOULD_QUIT triggers CleanExit at end ────────

  #[test]
  fn check_signals_should_quit_already_set_returns_clean_exit() {
    let _g = TestGuard::new();
    reset_signal_state();
    SHOULD_QUIT.store(true, Ordering::SeqCst);
    QUIT_CODE.store(42, Ordering::SeqCst);
    let err = check_signals().expect_err("SHOULD_QUIT set → CleanExit");
    assert!(matches!(err.kind(), ShErrKind::CleanExit(42)));
  }

  // ─── Misc signal traps fire ─────────────────────────────────────────

  #[test]
  fn check_signals_misc_signal_runs_trap() {
    let _g = TestGuard::new();
    reset_signal_state();
    // Install a trap on SIGUSR2 that sets a variable.
    Shed::logic_mut(|l| {
      l.insert_trap(
        TrapTarget::Signal(Signal::SIGUSR2),
        "export TRAP_FIRED=1".into(),
      );
    });
    set_signal(Signal::SIGUSR2);
    check_signals().unwrap();
    assert_eq!(crate::var!("TRAP_FIRED"), "1");
  }

  #[test]
  fn check_signals_sigwinch_trap_fires_alongside_flag() {
    let _g = TestGuard::new();
    reset_signal_state();
    Shed::logic_mut(|l| {
      l.insert_trap(
        TrapTarget::Signal(Signal::SIGWINCH),
        "export WINCH_TRAP=yes".into(),
      );
    });
    set_signal(Signal::SIGWINCH);
    check_signals().unwrap();
    // Both the flag AND the trap should have effect.
    assert!(GOT_SIGWINCH.load(Ordering::SeqCst));
    assert_eq!(crate::var!("WINCH_TRAP"), "yes");
  }

  // ─── Multiple signals in one swap ───────────────────────────────────

  #[test]
  fn check_signals_processes_winch_and_usr1_in_one_call() {
    let _g = TestGuard::new();
    reset_signal_state();
    set_signal(Signal::SIGWINCH);
    set_signal(Signal::SIGUSR1);
    check_signals().unwrap();
    assert!(GOT_SIGWINCH.load(Ordering::SeqCst));
    assert!(GOT_SIGUSR1.load(Ordering::SeqCst));
  }

  // ─── SIGINT short-circuits later signals ────────────────────────────

  #[test]
  fn check_signals_sigint_short_circuits_other_signals() {
    let _g = TestGuard::new();
    reset_signal_state();
    set_signal(Signal::SIGINT);
    set_signal(Signal::SIGWINCH); // would normally set GOT_SIGWINCH
    let err = check_signals().expect_err("SIGINT returns early");
    assert!(matches!(err.kind(), ShErrKind::Interrupt));
    // SIGWINCH never got processed because SIGINT returned early.
    assert!(
      !GOT_SIGWINCH.load(Ordering::SeqCst),
      "SIGINT should have returned before reaching SIGWINCH"
    );
  }
}
