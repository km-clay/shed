use nix::{
  sys::signal::Signal,
  unistd::{Pid, getpgrp},
};

use super::{
  super::state::{jobs::JobTab, terminal::Terminal},
  BuiltinArgs,
  eval::lex::Span,
  opt::OptSpec,
  out, outln, sherr,
  signal::parse_signal,
  state::{
    self, Shed,
    jobs::{JobCmdFlags, JobID, wait_bg, wait_fg},
  },
  util::{ShResult, ShResultExt, with_status},
};

fn parse_job_id(arg: &str, blame: Span) -> ShResult<usize> {
  if arg.starts_with('%') {
    let arg = arg.strip_prefix('%').unwrap();
    if arg.chars().all(|ch| ch.is_ascii_digit()) {
      let num = arg.parse::<usize>().unwrap_or_default();
      if num == 0 {
        Err(sherr!(
          SyntaxErr @ blame,
          "Invalid job id: {arg}",
        ))
      } else {
        Ok(num.saturating_sub(1))
      }
    } else {
      let result = Shed::jobs_mut(|j| {
        let query_result = j.query(JobID::Command(arg.into()));
        query_result.map(|job| job.tabid().unwrap())
      });
      match result {
        Some(id) => Ok(id),
        None => Err(sherr!(
          InternalErr @ blame,
          "Found a job but no table id in parse_job_id()",
        )),
      }
    }
  } else if arg.chars().all(|ch| ch.is_ascii_digit()) {
    let result = Shed::jobs_mut(|j| {
      let pgid_query_result = j.query(JobID::Pgid(Pid::from_raw(arg.parse::<i32>().unwrap())));
      if let Some(job) = pgid_query_result {
        return Some(job.tabid().unwrap());
      }

      if arg.parse::<i32>().unwrap() > 0 {
        let table_id_query_result = j.query(JobID::TableID(arg.parse::<usize>().unwrap()));
        return table_id_query_result.map(|job| job.tabid().unwrap());
      }

      None
    });

    match result {
      Some(id) => Ok(id),
      None => Err(sherr!(
        InternalErr @ blame,
        "Found a job but no table id in parse_job_id()",
      )),
    }
  } else {
    Err(sherr!(
      SyntaxErr @ blame,
      "Invalid arg: {arg}",
    ))
  }
}

pub enum JobBehavior {
  Foregound,
  Background,
}

pub(super) struct Fg;
impl super::Builtin for Fg {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    continue_job(&args, &JobBehavior::Foregound)
  }
}

pub(super) struct Bg;
impl super::Builtin for Bg {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    continue_job(&args, &JobBehavior::Background)
  }
}

pub fn continue_job(args: &BuiltinArgs, behavior: &JobBehavior) -> ShResult<()> {
  let span = args.span();
  let mut arg_vec = args.arguments();

  let Some(curr_job_id) = Shed::jobs(JobTab::curr_job) else {
    return Err(sherr!(ExecFail @ span, "No jobs found"));
  };

  let tabid = match arg_vec.next() {
    Some((arg, blame)) => parse_job_id(&arg.to_str_lossy(), blame.clone())?,
    None => curr_job_id,
  };

  let Some(mut job) = Shed::jobs_mut(|j| j.remove_job(JobID::TableID(tabid))) else {
    return Err(sherr!(
      ExecFail @ span,
      "Job id `{tabid}' not found"
    ));
  };

  job.killpg(Signal::SIGCONT)?;

  match behavior {
    JobBehavior::Foregound => {
      let _cooked = Shed::term_mut(Terminal::prepare_for_exec);
      wait_fg(job, true)?;
    }
    JobBehavior::Background => {
      let job_order = Shed::jobs(|j| j.order().to_vec());
      out!("{}", job.display(&job_order, JobCmdFlags::PIDS));
      Shed::jobs_mut(|j| j.insert_job(job, true));
    }
  }

  with_status(0)
}

pub(super) struct Jobs;
impl super::Builtin for Jobs {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("long", 'l'),
      OptSpec::new_short("pids", 'p'),
      OptSpec::new_short("new-only", 'n'),
      OptSpec::new_short("running", 'r'),
      OptSpec::new_short("stopped", 's'),
      OptSpec::new_short("verbose", 'v'),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut flags = JobCmdFlags::empty();
    for opt in args.options() {
      match opt.key() {
        "long" => flags |= JobCmdFlags::LONG,
        "pids" => flags |= JobCmdFlags::PIDS,
        "new-only" => flags |= JobCmdFlags::NEW_ONLY,
        "running" => flags |= JobCmdFlags::RUNNING,
        "stopped" => flags |= JobCmdFlags::STOPPED,
        _ => {
          return Err(sherr!(
            SyntaxErr @ args.span(),
            "Invalid flag in jobs call",
          ));
        }
      }
    }

    Shed::jobs_mut(|j| j.print_jobs(flags))?;
    with_status(0)
  }
}

pub(super) struct Wait;
impl super::Builtin for Wait {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    if Shed::jobs(|j| j.curr_job().is_none()) {
      return with_status(0);
    }
    let arg_vec = args
      .arguments()
      .map(|arg| {
        if arg.0.to_str_lossy().chars().all(|ch| ch.is_ascii_digit()) {
          Ok(JobID::Pid(Pid::from_raw(
            arg.0.to_str_lossy().parse::<i32>().unwrap(),
          )))
        } else {
          Ok(JobID::TableID(parse_job_id(
            &arg.0.to_str_lossy(),
            arg.1.clone(),
          )?))
        }
      })
      .collect::<ShResult<Vec<JobID>>>()
      .promote_err(span.clone())?;

    if arg_vec.is_empty() {
      Shed::jobs_mut(JobTab::wait_all_bg).promote_err(span)?;
    } else {
      for arg in arg_vec {
        wait_bg(&arg).promote_err(span.clone())?;
      }
    }

    // we dont set the status here
    // the status of the waited-on job should be the status of the wait builtin
    Ok(())
  }
}

pub(super) struct Disown;
impl super::Builtin for Disown {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("nohup", 'h'),
      OptSpec::new_short("all", 'a'),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let mut nohup = false;
    let mut disown_all = false;

    for opt in args.options() {
      match opt.key() {
        "nohup" => nohup = true,
        "all" => disown_all = true,
        _ => {
          return Err(sherr!(
            SyntaxErr @ span,
            "Invalid flag in disown call",
          ));
        }
      }
    }

    // -a operates on every job; explicit ids and the current-job
    // fallback are both irrelevant.
    if disown_all {
      Shed::jobs_mut(|j| j.disown_all(nohup));
      return with_status(0);
    }

    let mut ids = vec![];
    for (arg, span) in args.arguments() {
      let id = parse_job_id(&arg.to_str_lossy(), span.clone())?;
      ids.push(id);
    }

    // Fall back to the current job only when no explicit ids were given.
    if ids.is_empty() {
      let Some(id) = Shed::jobs(JobTab::curr_job) else {
        return Err(sherr!(
            ExecFail @ span,
            "disown: No jobs to disown",
        ));
      };
      ids.push(id);
    }

    for id in ids {
      Shed::jobs_mut(|j| j.disown(JobID::TableID(id), nohup));
    }

    with_status(0)
  }
}

enum KillTarget {
  Pid(Pid),
  Pgid(Pid),
  OurPgrp,
  Broadcast,
  Job(JobID),
}

/// A signal value usable by `kill`. `nix::Signal` doesn't represent
/// signal 0 (the POSIX no-op probe), so we wrap it.
#[derive(Debug, Clone, Copy)]
enum KillSig {
  Real(Signal),
  Zero,
}

impl KillSig {
  fn as_i32(self) -> i32 {
    match self {
      KillSig::Real(s) => s as i32,
      KillSig::Zero => 0,
    }
  }
}

impl std::fmt::Display for KillSig {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      KillSig::Real(s) => write!(f, "{s}"),
      KillSig::Zero => write!(f, "signal 0"),
    }
  }
}

fn parse_kill_sig(s: &str) -> ShResult<KillSig> {
  // POSIX: signal 0 is a validity-probe (kill -0 pid) — checks whether
  // a signal *could* be sent without sending one. nix::Signal can't
  // represent it; handle it before delegating.
  if let Ok(n) = s.parse::<usize>() {
    let canonical = if n > 128 { n - 128 } else { n };
    if canonical == 0 {
      return Ok(KillSig::Zero);
    }
  }
  parse_signal(s).map(KillSig::Real)
}

fn raw_kill(pid: Pid, sig: i32) -> nix::Result<()> {
  let ret = unsafe { nix::libc::kill(pid.as_raw(), sig) };
  if ret == 0 {
    Ok(())
  } else {
    Err(nix::errno::Errno::last())
  }
}

fn raw_killpg(pgid: Pid, sig: i32) -> nix::Result<()> {
  let ret = unsafe { nix::libc::killpg(pgid.as_raw(), sig) };
  if ret == 0 {
    Ok(())
  } else {
    Err(nix::errno::Errno::last())
  }
}

fn parse_kill_target(arg: &str, blame: Span) -> ShResult<KillTarget> {
  let Ok(n) = arg.parse::<i32>() else {
    let Ok(id) = parse_job_id(arg, blame.clone()) else {
      return Err(sherr!(ParseErr @ blame, "Invalid kill target: {arg}"));
    };
    return Ok(KillTarget::Job(JobID::TableID(id)));
  };

  Ok(match n {
    -1 => KillTarget::Broadcast,
    0 => KillTarget::OurPgrp,
    _ if n < -1 => KillTarget::Pgid(Pid::from_raw(-n)),
    _ => KillTarget::Pid(Pid::from_raw(n)),
  })
}

pub(super) fn list_all_signals() {
  let signals: String = Signal::iterator()
    .map(|sig| {
      let sig = sig.to_string();
      sig.strip_prefix("SIG").unwrap_or(&sig).to_string()
    })
    .collect::<Vec<_>>()
    .join(&state::util::get_separator().to_str_lossy());
  outln!("{signals}");
}

fn send_signal(target: &KillTarget, sig: KillSig, verbose: bool, blame: &Span) -> ShResult<()> {
  let desc = match target {
    KillTarget::Pid(pid) => {
      raw_kill(*pid, sig.as_i32())?;
      format!("killing process {pid} with {sig}")
    }
    KillTarget::Pgid(pid) => {
      raw_killpg(*pid, sig.as_i32())?;
      format!("killing process group {pid} with {sig}")
    }
    KillTarget::OurPgrp => {
      let pgrp = getpgrp();
      raw_killpg(pgrp, sig.as_i32())?;
      format!("killing shell's process group ({pgrp}) with {sig}")
    }
    KillTarget::Broadcast => {
      raw_kill(Pid::from_raw(-1), sig.as_i32())?;
      format!("broadcasting {sig} to all processes")
    }
    KillTarget::Job(job_id) => {
      Shed::jobs_mut(|j| {
        if let Some(job) = j.query_mut(job_id.clone()) {
          match sig {
            // Real signal: delegate to Job::killpg so the job's wait
            // status is updated to match.
            KillSig::Real(s) => job.killpg(s),
            // Signal 0 is a probe — don't touch the job's state.
            KillSig::Zero => Ok(raw_killpg(job.pgid(), 0)?),
          }
        } else {
          Err(sherr!(ExecFail @ blame.clone(), "Job not found"))
        }
      })?;
      format!("killing job {job_id:?} with {sig}")
    }
  };
  if verbose {
    outln!("kill: {desc}");
  }
  Ok(())
}

pub(super) struct Kill;
impl super::Builtin for Kill {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("list", 'l'),
      OptSpec::new_short("verbose", 'v'),
      OptSpec::new_short("signal", 's').argc(1),
    ]
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut signal: Option<KillSig> = None;
    let mut list_sig = false;
    let mut verbose = false;

    for opt in args.options() {
      match opt.key() {
        "verbose" => verbose = true,
        "list" => list_sig = true,
        "signal" => {
          let sig_name = opt.value()?;
          signal = Some(parse_kill_sig(sig_name).promote_err(args.span.clone())?);
        }
        _ => {}
      }
    }

    if list_sig {
      let Some((arg, span)) = args.arguments().next() else {
        // kill -l - list all signals
        list_all_signals();
        return Ok(());
      };

      // kill -l <arg> converts between names and numbers. A numeric argument
      // prints the signal name; a name prints its number. Signal 0 isn't named,
      // so we only accept real signals here.
      let sig = parse_signal(&arg.to_str_lossy()).promote_err(span.clone())?;
      if arg.to_str_lossy().trim().parse::<i32>().is_ok() {
        let name = sig.to_string();
        outln!("{}", name.strip_prefix("SIG").unwrap_or(&name));
      } else {
        outln!("{}", sig as i32);
      }

      return with_status(0);
    }

    if args.no_arguments() {
      return Err(sherr!(SyntaxErr @ args.span, "usage: kill [-signal] pid ..."));
    }

    let sig = signal.unwrap_or(KillSig::Real(Signal::SIGTERM));

    for (arg, span) in args.arguments() {
      // Check if the arg looks like a signal (e.g. kill -TERM pid, kill -0 pid)
      if arg.to_str_lossy().starts_with('-') && !arg.to_str_lossy().starts_with("--") {
        let stripped = arg.to_str_lossy();
        let stripped = stripped.trim_start_matches('-');
        if let Ok(sig_override) = parse_kill_sig(stripped).promote_err(span.clone()) {
          signal.replace(sig_override);
          continue;
        }
      }

      let target = parse_kill_target(&arg.to_str_lossy(), span.clone())?;
      send_signal(&target, signal.unwrap_or(sig), verbose, span)?;
    }

    with_status(0)
  }
}

#[cfg(test)]
mod kill_tests {
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};
  use nix::sys::signal::Signal;
  use nix::sys::wait::{WaitStatus, waitpid};
  use nix::unistd::{ForkResult, fork};

  // ─── kill -l (list signals) ─────────────────────────────────────────

  #[test]
  fn kill_dash_l_lists_signal_names() {
    let g = TestGuard::new();
    test_input("kill -l").unwrap();
    let out = g.read_output();
    // Status 0 and output mentions a handful of well-known signals.
    assert_eq!(state::Shed::get_status(), 0);
    assert!(out.contains("TERM"), "missing TERM: {out:?}");
    assert!(out.contains("INT"), "missing INT: {out:?}");
    assert!(out.contains("KILL"), "missing KILL: {out:?}");
  }

  #[test]
  fn kill_dash_l_with_name_prints_number() {
    let g = TestGuard::new();
    test_input("kill -l TERM").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(g.read_output().trim(), "15");
  }

  #[test]
  fn kill_dash_l_with_sigprefix_name_prints_number() {
    let g = TestGuard::new();
    test_input("kill -l SIGTERM").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(g.read_output().trim(), "15");
  }

  #[test]
  fn kill_dash_l_with_kill_name_prints_nine() {
    let g = TestGuard::new();
    test_input("kill -l KILL").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(g.read_output().trim(), "9");
  }

  #[test]
  fn kill_dash_l_numeric_over_128_wraps_to_name() {
    // 137 = 128 + 9 -> KILL
    let g = TestGuard::new();
    test_input("kill -l 137").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(g.read_output().trim(), "KILL");
  }

  #[test]
  fn kill_dash_l_with_numeric_resolves_to_name() {
    let g = TestGuard::new();
    test_input("kill -l 15").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(g.read_output().trim(), "TERM");
  }

  #[test]
  fn kill_dash_l_with_invalid_signal_is_error() {
    let _g = TestGuard::new();
    test_input("kill -l NOTASIGNAL").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── kill argument validation ───────────────────────────────────────

  #[test]
  fn kill_with_no_args_is_syntax_error() {
    let _g = TestGuard::new();
    test_input("kill").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn kill_with_unparseable_target_errors() {
    let _g = TestGuard::new();
    test_input("kill not_a_pid_or_job").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn kill_with_dash_s_invalid_signal_errors() {
    let _g = TestGuard::new();
    test_input("kill -s NOTASIG 1").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn kill_nonexistent_job_errors() {
    let _g = TestGuard::new();
    // %99 has no matching entry in the job table.
    test_input("kill %99").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── kill sending real signals to forked children ───────────────────
  // Child calls pause() and waits to be signaled; parent sends a signal
  // via the builtin, then waitpid()'s to verify the child received it.

  fn fork_pausing_child() -> nix::unistd::Pid {
    match unsafe { fork() }.unwrap() {
      ForkResult::Child => {
        crate::lifecycle::setup_child();

        unsafe { nix::libc::pause() };
        // Should never get here — pause only returns on signal, and
        // default disposition for SIGTERM/SIGKILL is termination.
        unsafe { nix::libc::_exit(0) };
      }
      ForkResult::Parent { child } => child,
    }
  }

  #[test]
  fn kill_signal_name_sends_to_pid() {
    let _g = TestGuard::new();
    let pid = fork_pausing_child();
    test_input(format!("kill -TERM {}", pid.as_raw())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    match waitpid(pid, None).unwrap() {
      WaitStatus::Signaled(_, sig, _) => assert_eq!(sig, Signal::SIGTERM),
      other => panic!("expected Signaled(SIGTERM), got {other:?}"),
    }
  }

  #[test]
  fn kill_default_signal_is_term() {
    let _g = TestGuard::new();
    let pid = fork_pausing_child();
    test_input(format!("kill {}", pid.as_raw())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    match waitpid(pid, None).unwrap() {
      WaitStatus::Signaled(_, sig, _) => assert_eq!(sig, Signal::SIGTERM),
      other => panic!("expected Signaled(SIGTERM), got {other:?}"),
    }
  }

  #[test]
  fn kill_dash_s_sets_signal() {
    let _g = TestGuard::new();
    let pid = fork_pausing_child();
    test_input(format!("kill -s KILL {}", pid.as_raw())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    match waitpid(pid, None).unwrap() {
      WaitStatus::Signaled(_, sig, _) => assert_eq!(sig, Signal::SIGKILL),
      other => panic!("expected Signaled(SIGKILL), got {other:?}"),
    }
  }

  #[test]
  fn kill_numeric_signal_arg_works() {
    // `kill -9 <pid>` — `-9` arrives as a positional arg starting with
    // `-`, the builtin strips the `-` and parses "9" as a signal.
    let _g = TestGuard::new();
    let pid = fork_pausing_child();
    test_input(format!("kill -9 {}", pid.as_raw())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    match waitpid(pid, None).unwrap() {
      WaitStatus::Signaled(_, sig, _) => assert_eq!(sig, Signal::SIGKILL),
      other => panic!("expected Signaled(SIGKILL), got {other:?}"),
    }
  }

  #[test]
  fn kill_dash_signame_overrides_signal_per_arg() {
    // First positional `-INT` should set the signal for subsequent pids.
    let _g = TestGuard::new();
    let pid = fork_pausing_child();
    test_input(format!("kill -INT {}", pid.as_raw())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    match waitpid(pid, None).unwrap() {
      WaitStatus::Signaled(_, sig, _) => assert_eq!(sig, Signal::SIGINT),
      other => panic!("expected Signaled(SIGINT), got {other:?}"),
    }
  }

  #[test]
  fn kill_verbose_prints_description() {
    let g = TestGuard::new();
    let pid = fork_pausing_child();
    test_input(format!("kill -v -TERM {}", pid.as_raw())).unwrap();
    waitpid(pid, None).unwrap();
    let out = g.read_output();
    assert!(out.contains("killing process"), "got: {out:?}");
  }

  // ─── kill -0: POSIX no-op probe ─────────────────────────────────────

  #[test]
  fn kill_dash_zero_probes_live_pid_without_signaling() {
    use nix::sys::wait::WaitPidFlag;
    // `kill -0 <pid>` against a running process succeeds without
    // actually delivering anything; child stays alive.
    let _g = TestGuard::new();
    let pid = fork_pausing_child();
    // The probe didn't deliver — child is still paused. Confirm via
    // WNOHANG: nothing has changed.
    test_input(format!("kill -0 {}", pid.as_raw())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);

    let stat = waitpid(pid, Some(WaitPidFlag::WNOHANG)).unwrap();
    assert!(
      matches!(stat, WaitStatus::StillAlive),
      "expected child to still be alive after -0 probe, got {stat:?}"
    );

    // Clean up: signal for real.
    test_input(format!("kill -KILL {}", pid.as_raw())).unwrap();
    waitpid(pid, None).unwrap();
  }

  #[test]
  fn kill_dash_zero_against_dead_pid_errors() {
    // A reaped/never-existed pid: kill(2) returns ESRCH and the builtin
    // propagates it as an error (status != 0).
    let _g = TestGuard::new();
    // Spawn and immediately reap so the pid is gone.
    let pid = fork_pausing_child();
    let raw = pid.as_raw();
    test_input(format!("kill -KILL {raw}")).unwrap();
    waitpid(pid, None).unwrap();

    // Now probe — the pid should be gone.
    test_input(format!("kill -0 {raw}")).ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn kill_dash_s_zero_probes_live_pid() {
    use nix::sys::wait::WaitPidFlag;
    // Same probe semantics, but via -s 0 instead of -0.
    let _g = TestGuard::new();
    let pid = fork_pausing_child();
    test_input(format!("kill -s 0 {}", pid.as_raw())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);

    let stat = waitpid(pid, Some(WaitPidFlag::WNOHANG)).unwrap();
    assert!(matches!(stat, WaitStatus::StillAlive));

    test_input(format!("kill -KILL {}", pid.as_raw())).unwrap();
    waitpid(pid, None).unwrap();
  }
}

#[cfg(test)]
mod disown_tests {
  use crate::state::jobs::{ChildProc, Job, JobBldr, JobID};
  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, test_input};
  use nix::unistd::Pid;

  /// Build a synthetic Job and insert it into the job table. Returns
  /// the assigned table id.
  ///
  /// We explicitly force the child's stat to `StillAlive` because
  /// `ChildProc::new` with a fake pid defaults to `Exited(pid, 0)`
  /// (the liveness probe fails), and `is_done()` jobs get pruned on
  /// the next `insert_job` call — which would silently drop earlier
  /// jobs as we set up multi-job tests.
  fn insert_fake_job(pid: i32, cmd: &str) -> usize {
    use nix::sys::wait::WaitStatus;
    let pid = Pid::from_raw(pid);
    let mut child = ChildProc::new(pid, Some(cmd), Some(pid), None);
    child.set_stat(WaitStatus::StillAlive);
    let mut bldr = JobBldr::new();
    bldr.push_child(child);
    bldr.set_pgid(pid);
    let job = bldr.build();
    Shed::jobs_mut(|j| j.insert_job(job, true))
  }

  fn job_exists(tabid: usize) -> bool {
    Shed::jobs(|j| j.query(JobID::TableID(tabid)).is_some())
  }

  fn job_send_hup(tabid: usize) -> Option<bool> {
    Shed::jobs(|j| j.query(JobID::TableID(tabid)).map(Job::send_hup))
  }

  // ─── no jobs → error ────────────────────────────────────────────

  #[test]
  fn disown_with_no_jobs_errors() {
    let _g = TestGuard::new();
    test_input("disown").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── current-job default removal ───────────────────────────────

  #[test]
  fn disown_removes_current_job_from_table() {
    let _g = TestGuard::new();
    let tabid = insert_fake_job(99001, "fake_cmd");
    assert!(job_exists(tabid));
    test_input("disown").unwrap();
    assert!(!job_exists(tabid), "job should have been removed");
  }

  // ─── -h: mark nohup, keep job in table ─────────────────────────

  #[test]
  fn disown_dash_h_marks_nohup_and_keeps_job() {
    let _g = TestGuard::new();
    let tabid = insert_fake_job(99002, "fake_cmd");
    assert_eq!(job_send_hup(tabid), Some(true));
    test_input("disown -h").unwrap();
    assert!(job_exists(tabid), "job should remain in table");
    assert_eq!(
      job_send_hup(tabid),
      Some(false),
      "send_hup should be cleared"
    );
  }

  // ─── -a: remove all jobs ────────────────────────────────────────

  #[test]
  fn disown_dash_a_removes_all_jobs() {
    let _g = TestGuard::new();
    let id1 = insert_fake_job(99010, "cmd_a");
    let id2 = insert_fake_job(99011, "cmd_b");
    let id3 = insert_fake_job(99012, "cmd_c");
    test_input("disown -a").unwrap();
    assert!(!job_exists(id1));
    assert!(!job_exists(id2));
    assert!(!job_exists(id3));
  }

  #[test]
  fn disown_dash_a_dash_h_keeps_all_jobs_marks_nohup() {
    let _g = TestGuard::new();
    let id1 = insert_fake_job(99020, "cmd_a");
    let id2 = insert_fake_job(99021, "cmd_b");
    test_input("disown -a -h").unwrap();
    assert!(job_exists(id1));
    assert!(job_exists(id2));
    assert_eq!(job_send_hup(id1), Some(false));
    assert_eq!(job_send_hup(id2), Some(false));
  }

  // ─── explicit %N removes only the named job ────────────────────

  #[test]
  fn disown_with_explicit_jobid_removes_only_named() {
    // Regression: previously execute() unconditionally seeded `ids`
    // with the current job and appended argv on top, so `disown %N`
    // would also remove the current job. Now the current-job fallback
    // only applies when argv is empty.
    let _g = TestGuard::new();
    let id1 = insert_fake_job(99030, "cmd_a");
    let id2 = insert_fake_job(99031, "cmd_b");
    test_input(format!("disown %{}", id1 + 1)).unwrap();
    assert!(!job_exists(id1), "named job should be removed");
    assert!(job_exists(id2), "unnamed current job should remain");
  }

  #[test]
  fn disown_with_multiple_explicit_ids_removes_only_named() {
    let _g = TestGuard::new();
    let id1 = insert_fake_job(99050, "cmd_a");
    let id2 = insert_fake_job(99051, "cmd_b");
    let id3 = insert_fake_job(99052, "cmd_c");
    test_input(format!("disown %{} %{}", id1 + 1, id2 + 1)).unwrap();
    assert!(!job_exists(id1));
    assert!(!job_exists(id2));
    assert!(job_exists(id3), "unnamed current job should remain");
  }

  // ─── invalid job id ────────────────────────────────────────────

  #[test]
  fn disown_invalid_jobid_errors() {
    let _g = TestGuard::new();
    let _id = insert_fake_job(99040, "fake_cmd");
    test_input("disown %not_a_number").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}

#[cfg(test)]
mod jobs_builtin_tests {
  use crate::state::jobs::{ChildProc, JobBldr, JobID};
  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, test_input};
  use nix::sys::wait::WaitStatus;
  use nix::unistd::Pid;

  fn insert_job(pid: i32, cmd: &str) -> usize {
    let pid = Pid::from_raw(pid);
    let mut child = ChildProc::new(pid, Some(cmd), Some(pid), None);
    child.set_stat(WaitStatus::StillAlive);
    let mut bldr = JobBldr::new();
    bldr.push_child(child);
    bldr.set_pgid(pid);
    let job = bldr.build();
    Shed::jobs_mut(|j| j.insert_job(job, true))
  }

  fn drain_jobs() {
    Shed::jobs_mut(|j| {
      let ids: Vec<JobID> = j
        .jobs()
        .iter()
        .flatten()
        .filter_map(|job| job.tabid().map(JobID::TableID))
        .collect();
      for id in ids {
        j.remove_job(id);
      }
    });
  }

  // ─── plain `jobs` ───────────────────────────────────────────────

  #[test]
  fn jobs_with_no_jobs_no_output() {
    let g = TestGuard::new();
    drain_jobs();
    test_input("jobs").unwrap();
    assert_eq!(g.read_output(), "");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn jobs_lists_inserted_jobs() {
    let g = TestGuard::new();
    drain_jobs();
    insert_job(60001, "uniq_jobs_cmd");
    test_input("jobs").unwrap();
    let out = g.read_output();
    assert!(out.contains("uniq_jobs_cmd"), "got: {out:?}");
  }

  // ─── -l (LONG): includes pid ────────────────────────────────────

  #[test]
  fn jobs_dash_l_includes_pid() {
    let g = TestGuard::new();
    drain_jobs();
    insert_job(60010, "long_cmd");
    test_input("jobs -l").unwrap();
    let out = g.read_output();
    assert!(out.contains("60010"), "got: {out:?}");
  }

  // ─── -p (PIDS): also includes pid ───────────────────────────────

  #[test]
  fn jobs_dash_p_includes_pid() {
    let g = TestGuard::new();
    drain_jobs();
    insert_job(60020, "pid_cmd");
    test_input("jobs -p").unwrap();
    let out = g.read_output();
    assert!(out.contains("60020"), "got: {out:?}");
  }

  // ─── -r (RUNNING) ─────────────────────────────────────────────

  #[test]
  fn jobs_dash_r_shows_only_running() {
    let g = TestGuard::new();
    drain_jobs();
    insert_job(60030, "running_one");
    test_input("jobs -r").unwrap();
    let out = g.read_output();
    assert!(out.contains("running_one"), "got: {out:?}");
  }

  // ─── -s (STOPPED) — running job should be excluded ──────────────

  #[test]
  fn jobs_dash_s_excludes_running_jobs() {
    let g = TestGuard::new();
    drain_jobs();
    insert_job(60040, "running_two");
    test_input("jobs -s").unwrap();
    let out = g.read_output();
    assert!(!out.contains("running_two"), "got: {out:?}");
  }

  // ─── -n (NEW_ONLY) ─────────────────────────────────────────────

  #[test]
  fn jobs_dash_n_doesnt_crash() {
    let _g = TestGuard::new();
    drain_jobs();
    insert_job(60050, "any_cmd");
    test_input("jobs -n").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // The catchall `_` arm in Jobs::execute returns SyntaxErr, but in
  // practice getopt rejects unknown short flags or treats them as
  // positional args (non-strict mode) before they reach the match, so
  // the catchall is unreachable through normal shell invocation. We
  // skip testing it.

  // ===================== Wait::execute =====================

  #[test]
  fn wait_with_no_jobs_succeeds() {
    let _g = TestGuard::new();
    drain_jobs();
    test_input("wait").unwrap();
    // ExecFail from the builtin is converted to a nonzero status.
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn wait_with_numeric_arg_uses_pid_path() {
    let _g = TestGuard::new();
    drain_jobs();
    // Need a job in the table so the up-front "No jobs" check passes.
    insert_job(60100, "stub_for_wait");
    // pid 1 is alive but not our child → waitpid → ECHILD → wait_bg Ok.
    test_input("wait 1").unwrap();
  }

  #[test]
  fn wait_with_unknown_job_spec_errors() {
    let _g = TestGuard::new();
    drain_jobs();
    insert_job(60101, "stub_for_wait_spec");
    // %99 — no such job → parse_job_id errors → nonzero status.
    test_input("wait %99").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
