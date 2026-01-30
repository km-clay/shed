use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use nix::sys::signal::{SaFlags, SigAction, sigaction};

use crate::{
  jobs::{JobCmdFlags, JobID, take_term},
  libsh::error::{ShErr, ShErrKind, ShResult},
  prelude::*,
  state::{read_jobs, write_jobs},
};

static GOT_SIGINT: AtomicBool = AtomicBool::new(false);
static GOT_SIGHUP: AtomicBool = AtomicBool::new(false);
static GOT_SIGTSTP: AtomicBool = AtomicBool::new(false);
static GOT_SIGCHLD: AtomicBool = AtomicBool::new(false);
static REAPING_ENABLED: AtomicBool = AtomicBool::new(true);

pub static SHOULD_QUIT: AtomicBool = AtomicBool::new(false);
pub static QUIT_CODE: AtomicI32 = AtomicI32::new(0);

pub fn signals_pending() -> bool {
	GOT_SIGINT.load(Ordering::SeqCst)
		|| GOT_SIGHUP.load(Ordering::SeqCst)
		|| GOT_SIGTSTP.load(Ordering::SeqCst)
		|| (REAPING_ENABLED.load(Ordering::SeqCst)
			&& GOT_SIGCHLD.load(Ordering::SeqCst))
		|| SHOULD_QUIT.load(Ordering::SeqCst)
}

pub fn check_signals() -> ShResult<()> {
	if GOT_SIGINT.swap(false, Ordering::SeqCst) {
		flog!(DEBUG, "check_signals: processing SIGINT");
		interrupt()?;
		return Err(ShErr::simple(ShErrKind::ClearReadline, ""));
	}
	if GOT_SIGHUP.swap(false, Ordering::SeqCst) {
		flog!(DEBUG, "check_signals: processing SIGHUP");
		hang_up(0);
	}
	if GOT_SIGTSTP.swap(false, Ordering::SeqCst) {
		flog!(DEBUG, "check_signals: processing SIGTSTP");
		terminal_stop()?;
	}
	if REAPING_ENABLED.load(Ordering::SeqCst) && GOT_SIGCHLD.swap(false, Ordering::SeqCst) {
		flog!(DEBUG, "check_signals: processing SIGCHLD (reaping enabled)");
		wait_child()?;
	} else if GOT_SIGCHLD.load(Ordering::SeqCst) {
		flog!(DEBUG, "check_signals: SIGCHLD pending but reaping disabled");
	}
	if SHOULD_QUIT.load(Ordering::SeqCst) {
		let code = QUIT_CODE.load(Ordering::SeqCst);
		flog!(DEBUG, "check_signals: SHOULD_QUIT set, exiting with code {}", code);
		return Err(ShErr::simple(ShErrKind::CleanExit(code), "exit"));
	}
	Ok(())
}

pub fn disable_reaping() {
	flog!(DEBUG, "disable_reaping: turning off SIGCHLD processing");
	REAPING_ENABLED.store(false, Ordering::SeqCst);
}
pub fn enable_reaping() {
	flog!(DEBUG, "enable_reaping: turning on SIGCHLD processing");
	REAPING_ENABLED.store(true, Ordering::SeqCst);
}

pub fn sig_setup() {
	let flags = SaFlags::empty();

	let actions = [
		SigAction::new(
			SigHandler::Handler(handle_sigchld),
			flags,
			SigSet::empty(),
		),
		SigAction::new(
			SigHandler::Handler(handle_sigquit),
			flags,
			SigSet::empty(),
		),
		SigAction::new(
			SigHandler::Handler(handle_sigtstp),
			flags,
			SigSet::empty(),
		),
		SigAction::new(
			SigHandler::Handler(handle_sighup),
			flags,
			SigSet::empty(),
		),
		SigAction::new(
			SigHandler::Handler(handle_sigint),
			flags,
			SigSet::empty(),
		),
		SigAction::new( // SIGTTIN
			SigHandler::SigIgn,
			flags,
			SigSet::empty(),
		),
		SigAction::new( // SIGTTOU
			SigHandler::SigIgn,
			flags,
			SigSet::empty(),
		),
		SigAction::new(
			SigHandler::Handler(handle_sigwinch),
			flags,
			SigSet::empty(),
		),
	];


  unsafe {
    sigaction(Signal::SIGCHLD, &actions[0]).unwrap();
    sigaction(Signal::SIGQUIT, &actions[1]).unwrap();
		sigaction(Signal::SIGTSTP, &actions[2]).unwrap();
		sigaction(Signal::SIGHUP, &actions[3]).unwrap();
		sigaction(Signal::SIGINT, &actions[4]).unwrap();
		sigaction(Signal::SIGTTIN, &actions[5]).unwrap();
		sigaction(Signal::SIGTTOU, &actions[6]).unwrap();
		sigaction(Signal::SIGWINCH, &actions[7]).unwrap();
  }
}

extern "C" fn handle_sigwinch(_: libc::c_int) {
	/* do nothing
	 * this exists for the sole purpose of interrupting readline
	 * readline will be refreshed after the interruption,
	 * which will cause window size calculations to be re-run
	 * and we get window resize handling for free as a result
	 */
}

extern "C" fn handle_sighup(_: libc::c_int) {
	GOT_SIGHUP.store(true, Ordering::SeqCst);
	SHOULD_QUIT.store(true, Ordering::SeqCst);
	QUIT_CODE.store(128 + libc::SIGHUP, Ordering::SeqCst);
}

pub fn hang_up(_: libc::c_int) {
  write_jobs(|j| {
    for job in j.jobs_mut().iter_mut().flatten() {
      job.killpg(Signal::SIGTERM).ok();
    }
  });
}

extern "C" fn handle_sigtstp(_: libc::c_int) {
	GOT_SIGTSTP.store(true, Ordering::SeqCst);
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

extern "C" fn handle_sigint(_: libc::c_int) {
	GOT_SIGINT.store(true, Ordering::SeqCst);
}

pub fn interrupt() -> ShResult<()> {
  flog!(DEBUG, "interrupt: checking for fg job to send SIGINT");
  write_jobs(|j| {
    if let Some(job) = j.get_fg_mut() {
      flog!(DEBUG, "interrupt: sending SIGINT to fg job pgid {}", job.pgid());
      job.killpg(Signal::SIGINT)
    } else {
      flog!(DEBUG, "interrupt: no fg job, clearing readline");
      Ok(())
    }
  })
}

extern "C" fn handle_sigquit(_: libc::c_int) {
	SHOULD_QUIT.store(true, Ordering::SeqCst);
	QUIT_CODE.store(128 + libc::SIGQUIT, Ordering::SeqCst);
}

extern "C" fn handle_sigchld(_: libc::c_int) {
	GOT_SIGCHLD.store(true, Ordering::SeqCst);
}

pub fn wait_child() -> ShResult<()> {
  flog!(DEBUG, "wait_child: starting reap loop");
  let flags = WtFlag::WNOHANG | WtFlag::WSTOPPED;
  while let Ok(status) = waitpid(None, Some(flags)) {
    match status {
      WtStat::Exited(pid, code) => {
        flog!(DEBUG, "wait_child: pid {} exited with code {}", pid, code);
        child_exited(pid, status)?;
      }
      WtStat::Signaled(pid, signal, _) => {
        flog!(DEBUG, "wait_child: pid {} signaled with {:?}", pid, signal);
        child_signaled(pid, signal)?;
      }
      WtStat::Stopped(pid, signal) => {
        flog!(DEBUG, "wait_child: pid {} stopped with {:?}", pid, signal);
        child_stopped(pid, signal)?;
      }
      WtStat::Continued(pid) => {
        flog!(DEBUG, "wait_child: pid {} continued", pid);
        child_continued(pid)?;
      }
      WtStat::StillAlive => {
        flog!(DEBUG, "wait_child: no more children to reap");
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
  })
    && is_finished {
      if is_fg {
        take_term()?;
      } else {
        println!();
        let job_order = read_jobs(|j| j.order().to_vec());
        let result = read_jobs(|j| j.query(JobID::Pgid(pgid)).cloned());
        if let Some(job) = result {
          println!("{}", job.display(&job_order, JobCmdFlags::PIDS))
        }
      }
    }
  Ok(())
}
