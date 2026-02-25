use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use nix::sys::signal::{SaFlags, SigAction, sigaction};

use crate::{
  builtin::trap::TrapTarget,
  jobs::{JobCmdFlags, JobID, take_term},
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::execute::exec_input,
  prelude::*,
  state::{read_jobs, read_logic, write_jobs, write_meta},
};

static SIGNALS: AtomicU64 = AtomicU64::new(0);

pub static REAPING_ENABLED: AtomicBool = AtomicBool::new(true);
pub static SHOULD_QUIT: AtomicBool = AtomicBool::new(false);
pub static QUIT_CODE: AtomicI32 = AtomicI32::new(0);

const MISC_SIGNALS: [Signal; 22] = [
  Signal::SIGILL,
  Signal::SIGTRAP,
  Signal::SIGABRT,
  Signal::SIGBUS,
  Signal::SIGFPE,
  Signal::SIGUSR1,
  Signal::SIGSEGV,
  Signal::SIGUSR2,
  Signal::SIGPIPE,
  Signal::SIGALRM,
  Signal::SIGTERM,
  Signal::SIGSTKFLT,
  Signal::SIGCONT,
  Signal::SIGURG,
  Signal::SIGXCPU,
  Signal::SIGXFSZ,
  Signal::SIGVTALRM,
  Signal::SIGPROF,
  Signal::SIGWINCH,
  Signal::SIGIO,
  Signal::SIGPWR,
  Signal::SIGSYS,
];

pub fn signals_pending() -> bool {
  SIGNALS.load(Ordering::SeqCst) != 0 || SHOULD_QUIT.load(Ordering::SeqCst)
}

pub fn sigint_pending() -> bool {
  SIGNALS.load(Ordering::SeqCst) & (1 << Signal::SIGINT as u64) != 0
}

pub fn check_signals() -> ShResult<()> {
  let pending = SIGNALS.swap(0, Ordering::SeqCst);
  let got_signal = |sig: Signal| -> bool { pending & (1 << sig as u64) != 0 };
  let run_trap = |sig: Signal| -> ShResult<()> {
    if let Some(command) = read_logic(|l| l.get_trap(TrapTarget::Signal(sig))) {
      exec_input(command, None, false)?;
    }
    Ok(())
  };

  if got_signal(Signal::SIGINT) {
    interrupt()?;
    run_trap(Signal::SIGINT)?;
    return Err(ShErr::simple(ShErrKind::ClearReadline, ""));
  }
  if got_signal(Signal::SIGHUP) {
    run_trap(Signal::SIGHUP)?;
    hang_up(0);
  }
  if got_signal(Signal::SIGQUIT) {
    run_trap(Signal::SIGQUIT)?;
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

  for sig in MISC_SIGNALS {
    if got_signal(sig) {
      run_trap(sig)?;
    }
  }

  if SHOULD_QUIT.load(Ordering::SeqCst) {
    let code = QUIT_CODE.load(Ordering::SeqCst);
    return Err(ShErr::simple(ShErrKind::CleanExit(code), "exit"));
  }
  Ok(())
}

pub fn disable_reaping() {
  REAPING_ENABLED.store(false, Ordering::SeqCst);
}
pub fn enable_reaping() {
  REAPING_ENABLED.store(true, Ordering::SeqCst);
}

pub fn sig_setup() {
  let flags = SaFlags::empty();

  let action = SigAction::new(SigHandler::Handler(handle_signal), flags, SigSet::empty());

  let ignore = SigAction::new(SigHandler::SigIgn, flags, SigSet::empty());

  unsafe {
    sigaction(Signal::SIGTTIN, &ignore).unwrap();
    sigaction(Signal::SIGTTOU, &ignore).unwrap();

    sigaction(Signal::SIGCHLD, &action).unwrap();
    sigaction(Signal::SIGHUP, &action).unwrap();
    sigaction(Signal::SIGINT, &action).unwrap();
    sigaction(Signal::SIGQUIT, &action).unwrap();
    sigaction(Signal::SIGILL, &action).unwrap();
    sigaction(Signal::SIGTRAP, &action).unwrap();
    sigaction(Signal::SIGABRT, &action).unwrap();
    sigaction(Signal::SIGBUS, &action).unwrap();
    sigaction(Signal::SIGFPE, &action).unwrap();
    sigaction(Signal::SIGUSR1, &action).unwrap();
    sigaction(Signal::SIGSEGV, &action).unwrap();
    sigaction(Signal::SIGUSR2, &action).unwrap();
    sigaction(Signal::SIGPIPE, &action).unwrap();
    sigaction(Signal::SIGALRM, &action).unwrap();
    sigaction(Signal::SIGTERM, &action).unwrap();
    sigaction(Signal::SIGSTKFLT, &action).unwrap();
    sigaction(Signal::SIGCONT, &action).unwrap();
    sigaction(Signal::SIGTSTP, &action).unwrap();
    sigaction(Signal::SIGURG, &action).unwrap();
    sigaction(Signal::SIGXCPU, &action).unwrap();
    sigaction(Signal::SIGXFSZ, &action).unwrap();
    sigaction(Signal::SIGVTALRM, &action).unwrap();
    sigaction(Signal::SIGPROF, &action).unwrap();
    sigaction(Signal::SIGWINCH, &action).unwrap();
    sigaction(Signal::SIGIO, &action).unwrap();
    sigaction(Signal::SIGPWR, &action).unwrap();
    sigaction(Signal::SIGSYS, &action).unwrap();
  }
}

extern "C" fn handle_signal(sig: libc::c_int) {
  SIGNALS.fetch_or(1 << sig, Ordering::SeqCst);
}

pub fn hang_up(_: libc::c_int) {
  SHOULD_QUIT.store(true, Ordering::SeqCst);
  QUIT_CODE.store(1, Ordering::SeqCst);
  write_jobs(|j| {
		j.hang_up();
  });
}

pub fn terminal_stop() -> ShResult<()> {
  write_jobs(|j| {
    if let Some(job) = j.get_fg_mut() {
      job.killpg(Signal::SIGTSTP)
    } else {
      Ok(())
    }
  })
  // TODO: It seems like there is supposed to be a take_term() call here
}

pub fn interrupt() -> ShResult<()> {
  write_jobs(|j| {
    if let Some(job) = j.get_fg_mut() {
      job.killpg(Signal::SIGINT)
    } else {
      Ok(())
    }
  })
}

pub fn wait_child() -> ShResult<()> {
  let flags = WtFlag::WNOHANG | WtFlag::WSTOPPED;
  while let Ok(status) = waitpid(None, Some(flags)) {
    match status {
      WtStat::Exited(pid, _) => {
        child_exited(pid, status)?;
      }
      WtStat::Signaled(pid, signal, _) => {
        child_signaled(pid, signal)?;
      }
      WtStat::Stopped(pid, signal) => {
        child_stopped(pid, signal)?;
      }
      WtStat::Continued(pid) => {
        child_continued(pid)?;
      }
      WtStat::StillAlive => {
        break;
      }
      _ => unimplemented!(),
    }
  }
  Ok(())
}

pub fn child_signaled(pid: Pid, sig: Signal) -> ShResult<()> {
  let pgid = getpgid(Some(pid)).unwrap_or(pid);
  write_jobs(|j| {
    if let Some(job) = j.query_mut(JobID::Pgid(pgid)) {
      let child = job
        .children_mut()
        .iter_mut()
        .find(|chld| pid == chld.pid())
        .unwrap();
      let stat = WtStat::Signaled(pid, sig, false);
      child.set_stat(stat);
    }
  });
  if sig == Signal::SIGINT {
    take_term().unwrap()
  }
  Ok(())
}

pub fn child_stopped(pid: Pid, sig: Signal) -> ShResult<()> {
  let pgid = getpgid(Some(pid)).unwrap_or(pid);
  write_jobs(|j| {
    if let Some(job) = j.query_mut(JobID::Pgid(pgid)) {
      let child = job
        .children_mut()
        .iter_mut()
        .find(|chld| pid == chld.pid())
        .unwrap();
      let status = WtStat::Stopped(pid, sig);
      child.set_stat(status);
    } else if j.get_fg_mut().is_some_and(|fg| fg.pgid() == pgid) {
      j.fg_to_bg(WtStat::Stopped(pid, sig)).unwrap();
    }
  });
  take_term()?;
  Ok(())
}

pub fn child_continued(pid: Pid) -> ShResult<()> {
  let pgid = getpgid(Some(pid)).unwrap_or(pid);
  write_jobs(|j| {
    if let Some(job) = j.query_mut(JobID::Pgid(pgid)) {
      job.killpg(Signal::SIGCONT).ok();
    }
  });
  Ok(())
}

pub fn child_exited(pid: Pid, status: WtStat) -> ShResult<()> {
  /*
   * Here we are going to get metadata on the exited process by querying the
   * job table with the pid. Then if the discovered job is the fg task,
   * return terminal control to rsh If it is not the fg task, print the
   * display info for the job in the job table We can reasonably assume that
   * if it is not a foreground job, then it exists in the job table
   * If this assumption is incorrect, the code has gone wrong somewhere.
   */
  write_jobs(|j| j.close_job_fds(pid));
  if let Some((pgid, is_fg, is_finished)) = write_jobs(|j| {
    let fg_pgid = j.get_fg().map(|job| job.pgid());
    if let Some(job) = j.query_mut(JobID::Pid(pid)) {
      let pgid = job.pgid();
      let is_fg = fg_pgid.is_some_and(|fg| fg == pgid);
      job.update_by_id(JobID::Pid(pid), status).unwrap();
      let is_finished = !job.running();

      if let Some(child) = job.children_mut().iter_mut().find(|chld| pid == chld.pid()) {
        child.set_stat(status);
      }

      Some((pgid, is_fg, is_finished))
    } else {
      None
    }
  }) && is_finished
  {
    if is_fg {
      take_term()?;
    } else {
      let job_order = read_jobs(|j| j.order().to_vec());
      let result = read_jobs(|j| j.query(JobID::Pgid(pgid)).cloned());
      if let Some(job) = result {
        let job_complete_msg = job.display(&job_order, JobCmdFlags::PIDS).to_string();
        write_meta(|m| m.post_system_message(job_complete_msg))
      }
    }
  }
  Ok(())
}
