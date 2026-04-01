use std::collections::VecDeque;

use ariadne::Fmt;
use nix::unistd::getpid;
use scopeguard::defer;
use yansi::Color;

use crate::{
  libsh::{
    error::{ShErr, ShErrKind, ShResult},
    sys::TTY_FILENO,
  },
  prelude::*,
  procio::{IoMode, borrow_fd},
  signal::{disable_reaping, enable_reaping},
  state::{self, VarFlags, VarKind, set_status, write_jobs, write_meta, write_vars},
};

pub const SIG_EXIT_OFFSET: i32 = 128;

bitflags! {
  #[derive(Debug, Copy, Clone)]
  pub struct JobCmdFlags: u8 {
    const LONG     = 0b0000_0001; // 0x01
    const PIDS     = 0b0000_0010; // 0x02
    const NEW_ONLY = 0b0000_0100; // 0x04
    const RUNNING  = 0b0000_1000; // 0x08
    const STOPPED  = 0b0001_0000; // 0x10
    const INIT     = 0b0010_0000; // 0x20
  }
}

#[derive(Debug)]
pub struct DisplayWaitStatus(pub WtStat);

impl fmt::Display for DisplayWaitStatus {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &self.0 {
      WtStat::Exited(_, code) => match code {
        0 => write!(f, "done"),
        _ => write!(f, "failed: {}", code),
      },
      WtStat::Signaled(_, signal, _) => {
        write!(f, "signaled: {:?}", signal)
      }
      WtStat::Stopped(_, signal) => {
        write!(f, "stopped: {:?}", signal)
      }
      WtStat::PtraceEvent(_, signal, _) => {
        write!(f, "ptrace event: {:?}", signal)
      }
      WtStat::PtraceSyscall(_) => {
        write!(f, "ptrace syscall")
      }
      WtStat::Continued(_) => {
        write!(f, "continued")
      }
      WtStat::StillAlive => {
        write!(f, "running")
      }
    }
  }
}

pub fn code_from_status(stat: &WtStat) -> Option<i32> {
  match stat {
    WtStat::Exited(_, exit_code) => Some(*exit_code),
    WtStat::Stopped(_, sig) => Some(SIG_EXIT_OFFSET + *sig as i32),
    WtStat::Signaled(_, sig, _) => Some(SIG_EXIT_OFFSET + *sig as i32),
    _ => None,
  }
}

#[derive(Clone, Debug)]
pub enum JobID {
  Pgid(Pid),
  Pid(Pid),
  TableID(usize),
  Command(String),
}

#[derive(Debug, Clone)]
pub struct ChildProc {
  pgid: Pid,
  pid: Pid,
  command: Option<String>,
  stat: WtStat,
}

impl ChildProc {
  pub fn new(pid: Pid, command: Option<&str>, pgid: Option<Pid>) -> ShResult<Self> {
    let command = command.map(|str| str.to_string());
    let stat = if kill(pid, None).is_ok() {
      WtStat::StillAlive
    } else {
      WtStat::Exited(pid, 0)
    };
    let mut child = Self {
      pgid: pid,
      pid,
      command,
      stat,
    };
    if let Some(pgid) = pgid {
      child.set_pgid(pgid).ok();
    }
    Ok(child)
  }
  pub fn pid(&self) -> Pid {
    self.pid
  }
  pub fn pgid(&self) -> Pid {
    self.pgid
  }
  pub fn cmd(&self) -> Option<&str> {
    self.command.as_deref()
  }
  pub fn stat(&self) -> WtStat {
    self.stat
  }
  pub fn wait(&mut self, flags: Option<WtFlag>) -> Result<WtStat, Errno> {
    let result = waitpid(self.pid, flags);
    if let Ok(stat) = result {
      self.stat = stat
    }
    result
  }
  pub fn kill<T: Into<Option<Signal>>>(&self, sig: T) -> ShResult<()> {
    Ok(kill(self.pid, sig)?)
  }
  pub fn set_pgid(&mut self, pgid: Pid) -> ShResult<()> {
    setpgid(self.pid, pgid)?;
    self.pgid = pgid;
    Ok(())
  }
  pub fn set_stat(&mut self, stat: WtStat) {
    self.stat = stat
  }
  pub fn is_alive(&self) -> bool {
    self.stat == WtStat::StillAlive
  }
  pub fn is_stopped(&self) -> bool {
    matches!(self.stat, WtStat::Stopped(..))
  }
  pub fn exited(&self) -> bool {
    matches!(self.stat, WtStat::Exited(..))
  }
}

#[derive(Clone, Debug)]
pub struct RegisteredFd {
  pub fd: IoMode,
  pub owner_pid: Pid,
}

#[derive(Debug)]
pub struct JobBldr {
  table_id: Option<usize>,
  pgid: Option<Pid>,
  children: Vec<ChildProc>,
  send_hup: bool,
}

impl Default for JobBldr {
  fn default() -> Self {
    Self::new()
  }
}

impl JobBldr {
  pub fn new() -> Self {
    Self {
      table_id: None,
      pgid: None,
      children: vec![],
      send_hup: true,
    }
  }
  pub fn with_id(self, id: usize) -> Self {
    Self {
      table_id: Some(id),
      pgid: self.pgid,
      children: self.children,
      send_hup: self.send_hup,
    }
  }
  pub fn with_pgid(self, pgid: Pid) -> Self {
    Self {
      table_id: self.table_id,
      pgid: Some(pgid),
      children: self.children,
      send_hup: self.send_hup,
    }
  }
  pub fn set_pgid(&mut self, pgid: Pid) {
    self.pgid = Some(pgid);
  }
  pub fn pgid(&self) -> Option<Pid> {
    self.pgid
  }
  pub fn no_hup(mut self) -> Self {
    self.send_hup = false;
    self
  }
  pub fn with_children(self, children: Vec<ChildProc>) -> Self {
    Self {
      table_id: self.table_id,
      pgid: self.pgid,
      children,
      send_hup: self.send_hup,
    }
  }
  pub fn push_child(&mut self, child: ChildProc) {
    self.children.push(child);
  }
  pub fn build(self) -> Job {
    Job {
      table_id: self.table_id,
      pgid: self.pgid.unwrap_or(Pid::from_raw(0)),
      children: self.children,
      send_hup: self.send_hup,
    }
  }
}

/// A wrapper around Vec<JobBldr> with some job-specific methods
#[derive(Default, Debug)]
pub struct JobStack(Vec<JobBldr>);

impl JobStack {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn new_job(&mut self) {
    self.0.push(JobBldr::new())
  }
  pub fn curr_job_mut(&mut self) -> Option<&mut JobBldr> {
    self.0.last_mut()
  }
  pub fn finalize_job(&mut self) -> Option<Job> {
    self.0.pop().map(|bldr| bldr.build())
  }
}

#[derive(Debug, Clone)]
pub struct Job {
  table_id: Option<usize>,
  pgid: Pid,
  children: Vec<ChildProc>,
  send_hup: bool,
}

impl Job {
  pub fn set_tabid(&mut self, id: usize) {
    self.table_id = Some(id)
  }
  pub fn no_hup(&mut self) {
    self.send_hup = false;
  }
  pub fn send_hup(&self) -> bool {
    self.send_hup
  }
  pub fn running(&self) -> bool {
    !self.children.iter().all(|chld| chld.exited())
  }
  pub fn tabid(&self) -> Option<usize> {
    self.table_id
  }
  pub fn pgid(&self) -> Pid {
    self.pgid
  }
  pub fn get_cmds(&self) -> Vec<&str> {
    let mut cmds = vec![];
    for child in &self.children {
      cmds.push(child.cmd().unwrap_or_default())
    }
    cmds
  }
  pub fn set_stats(&mut self, stat: WtStat) {
    for child in self.children.iter_mut() {
      child.set_stat(stat);
    }
  }
  pub fn get_stats(&self) -> Vec<WtStat> {
    self
      .children
      .iter()
      .map(|chld| chld.stat())
      .collect::<Vec<WtStat>>()
  }
  pub fn pipe_status(stats: &[WtStat]) -> Option<Vec<i32>> {
    if stats.iter().any(|stat| {
      matches!(
        stat,
        WtStat::StillAlive | WtStat::Continued(_) | WtStat::PtraceSyscall(_)
      )
    }) || stats.len() <= 1
    {
      return None;
    }
    Some(
      stats
        .iter()
        .map(|stat| match stat {
          WtStat::Exited(_, code) => *code,
          WtStat::Signaled(_, signal, _) => SIG_EXIT_OFFSET + *signal as i32,
          WtStat::Stopped(_, signal) => SIG_EXIT_OFFSET + *signal as i32,
          WtStat::PtraceEvent(_, signal, _) => SIG_EXIT_OFFSET + *signal as i32,
          WtStat::PtraceSyscall(_) | WtStat::Continued(_) | WtStat::StillAlive => unreachable!(),
        })
        .collect(),
    )
  }
  pub fn get_pids(&self) -> Vec<Pid> {
    self
      .children
      .iter()
      .map(|chld| chld.pid())
      .collect::<Vec<Pid>>()
  }
  pub fn children(&self) -> &[ChildProc] {
    &self.children
  }
  pub fn children_mut(&mut self) -> &mut Vec<ChildProc> {
    &mut self.children
  }
  pub fn is_done(&self) -> bool {
    self.children.iter().all(|chld| {
      chld.exited() || chld.stat() == WtStat::Signaled(chld.pid(), Signal::SIGHUP, true)
    })
  }
  pub fn killpg(&mut self, sig: Signal) -> ShResult<()> {
    let stat = match sig {
      Signal::SIGTSTP => WtStat::Stopped(self.pgid, Signal::SIGTSTP),
      Signal::SIGCONT => WtStat::Continued(self.pgid),
      sig => WtStat::Signaled(self.pgid, sig, false),
    };
    self.set_stats(stat);
    Ok(killpg(self.pgid, sig)?)
  }
  pub fn wait_pgrp(&mut self) -> ShResult<Vec<WtStat>> {
    let mut stats = vec![];
    for child in self.children.iter_mut() {
      if child.pid == Pid::this() {
        // TODO: figure out some way to get the exit code of builtins
        let code = state::get_status();
        stats.push(WtStat::Exited(child.pid, code));
        continue;
      }
      loop {
        let result = child.wait(Some(WtFlag::WSTOPPED));
        match result {
          Ok(stat) => {
            stats.push(stat);
            break;
          }
          Err(Errno::ECHILD) => break,
          Err(Errno::EINTR) => continue, // Retry on signal interruption
          Err(e) => return Err(e.into()),
        }
      }
    }
    Ok(stats)
  }
  pub fn update_by_id(&mut self, id: JobID, stat: WtStat) -> ShResult<()> {
    match id {
      JobID::Pid(pid) => {
        let query_result = self.children.iter_mut().find(|chld| chld.pid == pid);
        if let Some(child) = query_result {
          child.set_stat(stat);
        }
      }
      JobID::Command(cmd) => {
        let query_result = self
          .children
          .iter_mut()
          .find(|chld| chld.cmd().is_some_and(|chld_cmd| chld_cmd.contains(&cmd)));
        if let Some(child) = query_result {
          child.set_stat(stat);
        }
      }
      JobID::TableID(tid) => {
        if self.table_id.is_some_and(|tblid| tblid == tid) {
          for child in self.children.iter_mut() {
            child.set_stat(stat);
          }
        }
      }
      JobID::Pgid(pgid) => {
        if pgid == self.pgid {
          for child in self.children.iter_mut() {
            child.set_stat(stat);
          }
        }
      }
    }
    Ok(())
  }
  pub fn name(&self) -> Option<&str> {
    self.children().first().and_then(|child| child.cmd())
  }
  pub fn display(&self, job_order: &[usize], flags: JobCmdFlags) -> String {
    let long = flags.contains(JobCmdFlags::LONG);
    let init = flags.contains(JobCmdFlags::INIT);
    let pids = flags.contains(JobCmdFlags::PIDS);

    let current = job_order.last();
    let prev = if job_order.len() > 2 {
      job_order.get(job_order.len() - 2)
    } else {
      None
    };

    let id = self.table_id.unwrap();
    let symbol = if current == self.table_id.as_ref() {
      "+"
    } else if prev == self.table_id.as_ref() {
      "-"
    } else {
      " "
    };
    let padding_count = symbol.len() + id.to_string().len() + 3;
    let padding = " ".repeat(padding_count);

    let mut output = String::new();
    let id_box = format!("[{}]{}", id + 1, symbol);
    output.push_str(&format!("{id_box}\t"));
    for (i, cmd) in self.get_cmds().iter().enumerate() {
      let pid = if pids || init {
        let mut pid = self.get_pids().get(i).unwrap().to_string();
        pid.push(' ');
        pid
      } else {
        "".to_string()
      };
      let job_stat = *self.get_stats().get(i).unwrap();
      let fmt_stat = DisplayWaitStatus(job_stat).to_string();

      let mut stat_line = fmt_stat.clone();
      stat_line = format!("{}{} ", pid, stat_line);
      stat_line = format!("{} {}", stat_line, cmd);
      stat_line = match job_stat {
        WtStat::Stopped(..) | WtStat::Signaled(..) => stat_line.fg(Color::Magenta).to_string(),
        WtStat::Exited(_, code) => match code {
          0 => stat_line.fg(Color::Green).to_string(),
          _ => stat_line.fg(Color::Red).to_string(),
        },
        _ => stat_line.fg(Color::Cyan).to_string(),
      };
      if i != 0 {
        let padding = " ".repeat(id_box.len() - 1);
        stat_line = format!("{padding}{}", stat_line);
      }
      if i != self.get_cmds().len() - 1 {
        stat_line.push_str(" |");
      }

      let stat_final = if long {
        format!(
          "{}{} {}",
          if i != 0 { &padding } else { "" },
          self.get_pids().get(i).unwrap(),
          stat_line
        )
      } else {
        format!("{}{}", if i != 0 { &padding } else { "" }, stat_line)
      };
      output.push_str(&stat_final);
      output.push('\n');
    }
    output
  }
}

pub fn term_ctlr() -> Pid {
  tcgetpgrp(borrow_fd(*TTY_FILENO)).unwrap_or(getpgrp())
}

/// Calls attach_tty() on the shell's process group to retake control of the
/// terminal
pub fn take_term() -> ShResult<()> {
  // take the terminal back
  attach_tty(getpgrp())?;

  // send SIGWINCH to tell readline to update its window size in case it changed while we were in the background
  killpg(getpgrp(), Signal::SIGWINCH)?;
  Ok(())
}

pub fn wait_bg(id: JobID) -> ShResult<()> {
  disable_reaping();
  defer! {
    enable_reaping();
  };
  match id {
    JobID::Pid(pid) => {
      let stat = loop {
        match waitpid(pid, None) {
          Ok(stat) => break stat,
          Err(Errno::EINTR) => continue, // Retry on signal interruption
          Err(Errno::ECHILD) => return Ok(()), // No such child, treat as already reaped
          Err(e) => return Err(e.into()),
        }
      };
      write_jobs(|j| j.update_by_id(id, stat))?;
      set_status(code_from_status(&stat).unwrap_or(0));
    }
    _ => {
      let Some(mut job) = write_jobs(|j| j.remove_job(id.clone())) else {
        return Err(ShErr::simple(
          ShErrKind::ExecFail,
          format!("wait: No such job with id {:?}", id),
        ));
      };
      let statuses = job.wait_pgrp()?;
      let mut was_stopped = false;
      let mut code = 0;
      for status in &statuses {
        code = code_from_status(status).unwrap_or(0);
        match status {
          WtStat::Stopped(_, _) => {
            was_stopped = true;
          }
          WtStat::Signaled(_, sig, _) => {
            if *sig == Signal::SIGTSTP {
              was_stopped = true;
            }
          }
          _ => { /* Do nothing */ }
        }
      }

      if let Some(pipe_status) = Job::pipe_status(&statuses) {
        let pipe_status = pipe_status
          .into_iter()
          .map(|s| s.to_string())
          .collect::<VecDeque<String>>();

        write_vars(|v| v.set_var("PIPESTATUS", VarKind::Arr(pipe_status), VarFlags::NONE))?;
      }

      if was_stopped {
        write_jobs(|j| j.insert_job(job, false))?;
      }
      set_status(code);
    }
  }
  Ok(())
}

/// Waits on the current foreground job and updates the shell's last status code
pub fn wait_fg(job: Job, interactive: bool) -> ShResult<()> {
  if job.children().is_empty() {
    return Ok(()); // Nothing to do
  }
  let mut code = 0;
  let mut was_stopped = false;
  if interactive {
    attach_tty(job.pgid())?;
  }
  disable_reaping();
  defer! {
    enable_reaping();
  }
  let statuses = write_jobs(|j| j.new_fg(job))?;
  for status in &statuses {
    code = code_from_status(status).unwrap_or(0);
    match status {
      WtStat::Stopped(_, _) => {
        was_stopped = true;
        write_jobs(|j| j.fg_to_bg(*status))?;
      }
      WtStat::Signaled(_, sig, _) => {
        if *sig == Signal::SIGINT {
          // interrupt propagates to the shell
          // necessary for interrupting stuff like
          // while/for loops
          kill(getpid(), Signal::SIGINT)?;
        } else if *sig == Signal::SIGTSTP {
          was_stopped = true;
          write_jobs(|j| j.fg_to_bg(*status))?;
        }
      }
      _ => { /* Do nothing */ }
    }
  }
  if let Some(pipe_status) = Job::pipe_status(&statuses) {
    let pipe_status = pipe_status
      .into_iter()
      .map(|s| s.to_string())
      .collect::<VecDeque<String>>();

    write_vars(|v| v.set_var("PIPESTATUS", VarKind::Arr(pipe_status), VarFlags::NONE))?;
  }
  // If job wasn't stopped (moved to bg), clear the fg slot
  if !was_stopped {
    let job = write_jobs(|j| j.take_fg());

    if interactive {
      write_meta(|m| m.set_last_job(job));
    }
  }
  if interactive {
    take_term()?;
  }
  set_status(code);
  Ok(())
}

pub fn dispatch_job(job: Job, is_bg: bool, interactive: bool) -> ShResult<()> {
  if is_bg {
    write_jobs(|j| j.insert_job(job, false))?;
  } else {
    wait_fg(job, interactive)?;
  }
  Ok(())
}

pub fn attach_tty(pgid: Pid) -> ShResult<()> {
  // If we aren't attached to a terminal, the pgid already controls it, or the
  // process group does not exist Then return ok
  if !isatty(*TTY_FILENO).unwrap_or(false) || pgid == term_ctlr() || killpg(pgid, None).is_err() {
    return Ok(());
  }

  if pgid == getpgrp() && term_ctlr() != getpgrp() {
    kill(term_ctlr(), Signal::SIGTTOU).ok();
  }

  let mut new_mask = SigSet::empty();
  let mut mask_bkup = SigSet::empty();

  new_mask.add(Signal::SIGTSTP);
  new_mask.add(Signal::SIGTTIN);
  new_mask.add(Signal::SIGTTOU);

  pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&new_mask), Some(&mut mask_bkup))?;

  let result = tcsetpgrp(borrow_fd(*TTY_FILENO), pgid);

  pthread_sigmask(
    SigmaskHow::SIG_SETMASK,
    Some(&mask_bkup),
    Some(&mut new_mask),
  )?;

  match result {
    Ok(_) => Ok(()),
    Err(_e) => {
      tcsetpgrp(borrow_fd(*TTY_FILENO), getpgrp())?;
      Ok(())
    }
  }
}
