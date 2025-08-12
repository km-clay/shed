use crate::{
  jobs::{take_term, JobCmdFlags, JobID},
  libsh::{error::ShResult, sys::sh_quit},
  prelude::*,
  state::{read_jobs, write_jobs},
};

pub fn sig_setup() {
  unsafe {
    signal(Signal::SIGCHLD, SigHandler::Handler(handle_sigchld)).unwrap();
    signal(Signal::SIGQUIT, SigHandler::Handler(handle_sigquit)).unwrap();
    signal(Signal::SIGTSTP, SigHandler::Handler(handle_sigtstp)).unwrap();
    signal(Signal::SIGHUP, SigHandler::Handler(handle_sighup)).unwrap();
    signal(Signal::SIGINT, SigHandler::Handler(handle_sigint)).unwrap();
    signal(Signal::SIGTTIN, SigHandler::SigIgn).unwrap();
    signal(Signal::SIGTTOU, SigHandler::SigIgn).unwrap();
  }
}

extern "C" fn handle_sighup(_: libc::c_int) {
  write_jobs(|j| {
    for job in j.jobs_mut().iter_mut().flatten() {
      job.killpg(Signal::SIGTERM).ok();
    }
  });
  std::process::exit(0);
}

extern "C" fn handle_sigtstp(_: libc::c_int) {
  write_jobs(|j| {
    if let Some(job) = j.get_fg_mut() {
      job.killpg(Signal::SIGTSTP).ok();
    }
  });
}

extern "C" fn handle_sigint(_: libc::c_int) {
  write_jobs(|j| {
    if let Some(job) = j.get_fg_mut() {
      job.killpg(Signal::SIGINT).ok();
    }
  });
}

pub extern "C" fn ignore_sigchld(_: libc::c_int) {
  /*
  Do nothing

  This function exists because using SIGIGN to ignore SIGCHLD
  will cause the kernel to automatically reap the child process, which is not what we want.
  This handler will leave the signaling process as a zombie, allowing us
  to handle it somewhere else.

  This handler is used when we want to handle SIGCHLD explicitly,
  like in the case of handling foreground jobs
  */
}

extern "C" fn handle_sigquit(_: libc::c_int) {
  sh_quit(0)
}

pub extern "C" fn handle_sigchld(_: libc::c_int) {
  let flags = WtFlag::WNOHANG | WtFlag::WSTOPPED;
  while let Ok(status) = waitpid(None, Some(flags)) {
    if let Err(e) = match status {
      WtStat::Exited(pid, _) => child_exited(pid, status),
      WtStat::Signaled(pid, signal, _) => child_signaled(pid, signal),
      WtStat::Stopped(pid, signal) => child_stopped(pid, signal),
      WtStat::Continued(pid) => child_continued(pid),
      WtStat::StillAlive => break,
      _ => unimplemented!(),
    } {
      eprintln!("{}", e)
    }
  }
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
  }) {
    if is_finished {
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
  }
  Ok(())
}
