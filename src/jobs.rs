use std::fmt::Write;

use ariadne::Fmt;
use itertools::izip;
use nix::unistd::getpid;
use scopeguard::defer;
use yansi::Color;

use crate::{
  libsh::{error::ShResult, utils::AutoCmdVecUtils},
  prelude::*,
  procio::{IoMode, borrow_fd},
  sherr,
  signal::{disable_reaping, enable_reaping},
  state::{self, AutoCmdKind, CmdTimer, read_logic, set_status, with_term, with_vars, write_jobs, write_meta},
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
  timer: CmdTimer,
}

impl ChildProc {
  pub fn new(
    pid: Pid,
    command: Option<&str>,
    pgid: Option<Pid>,
    report_time: bool,
  ) -> ShResult<Self> {
    let command = command.map(|str| str.to_string());
    let timer = CmdTimer::new(command.clone().unwrap_or_default(), report_time)?;
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
      timer,
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
      ..self
    }
  }
  pub fn with_pgid(self, pgid: Pid) -> Self {
    Self {
      pgid: Some(pgid),
      ..self
    }
  }
  pub fn with_children(self, children: Vec<ChildProc>) -> Self {
    Self { children, ..self }
  }
  pub fn push_child(&mut self, child: ChildProc) {
    self.children.push(child);
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
  pub fn build(self) -> Job {
    Job {
      table_id: self.table_id,
      pgid: self.pgid.unwrap_or(Pid::from_raw(0)),
      children: self.children,
      notify: false,
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
  notify: bool,
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
  pub fn set_notify(&mut self, notify: bool) {
    self.notify = notify;
  }
  pub fn notify(&self) -> bool {
    self.notify
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
    self
      .children
      .iter()
      .map(|c| c.cmd().unwrap_or_default())
      .collect()
  }
  pub fn set_stats(&mut self, stat: WtStat) {
    for child in self.children.iter_mut() {
      child.set_stat(stat);
    }
  }
  pub fn get_stats(&self) -> Vec<WtStat> {
    self.children.iter().map(|chld| chld.stat()).collect()
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
  pub fn wait_pgrp(&mut self) -> ShResult<(Vec<WtStat>, Vec<CmdTimer>)> {
    let mut stats = vec![];
    let mut timers = vec![];
    for child in self.children.iter_mut() {
      if child.pid == Pid::this() {
        // TODO: figure out some way to get the exit code of builtins
        let code = state::get_status();
        stats.push(WtStat::Exited(child.pid, code));
        child.timer.stop()?;
        timers.push(child.timer.clone());
        continue;
      }
      loop {
        let result = child.wait(Some(WtFlag::WSTOPPED));
        child.timer.stop()?;
        match result {
          Ok(stat) => {
            stats.push(stat);
            timers.push(child.timer.clone());
            break;
          }
          Err(Errno::ECHILD) => break,
          Err(Errno::EINTR) => continue, // Retry on signal interruption
          Err(e) => return Err(e.into()),
        }
      }
    }
    Ok((stats, timers))
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
    let prev = (job_order.len() > 2)
      .then(|| job_order.get(job_order.len() - 2))
      .flatten();

    let id = self.table_id.unwrap();
    let symbol = if current == self.table_id.as_ref() {
      "+"
    } else if prev == self.table_id.as_ref() {
      "-"
    } else {
      " "
    };

    let job_pids = self.get_pids();
    let job_stats = self.get_stats();
    let job_cmds = self.get_cmds();
    let zipped = izip!(0.., job_pids.iter(), job_stats.iter(), job_cmds.iter(),);

    let id_box = format!("[{}]{}", id + 1, symbol);
    let id_width = id_box.len();
    let last_cmd = self.get_cmds().len().saturating_sub(1);

    let mut output = format!("{id_box}\t");

    for (i, pid, job_stat, cmd) in zipped {
      let fmt_stat = DisplayWaitStatus(*job_stat).to_string();
      let pipe = if i != last_cmd { " |" } else { "" };

      let stat_line = if pids || init {
        format!("{pid} {fmt_stat}  {cmd}{pipe}")
      } else {
        format!("{fmt_stat}  {cmd}{pipe}")
      };

      let stat_line = match job_stat {
        WtStat::Stopped(..) | WtStat::Signaled(..) => stat_line.fg(Color::Magenta),
        WtStat::Exited(_, 0) => stat_line.fg(Color::Green),
        WtStat::Exited(..) => stat_line.fg(Color::Red),
        _ => stat_line.fg(Color::Cyan),
      }
      .to_string();

      if i == 0 {
        if long {
          writeln!(output, "{pid} {stat_line}").ok();
        } else {
          writeln!(output, "{stat_line}").ok();
        }
      } else if long {
        writeln!(output, "{:>id_width$}\t{pid} {stat_line}", "").ok();
      } else {
        writeln!(output, "{:>id_width$}\t{stat_line}", "").ok();
      }
    }
    output
  }
}

/// Calls attach_tty() on the shell's process group to retake control of the
/// terminal
pub fn take_term() -> ShResult<()> {
  // take the terminal back
  with_term(|t| t.attach(getpgrp()))?;

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
        return Err(sherr!(ExecFail, "wait: No such job with id {:?}", id,));
      };
      let (statuses, timers) = job.wait_pgrp()?;

      for timer in &timers {
        if timer.should_report() {
          report_timer(timer)?;
        }
      }

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

      state::set_pipe_status(&statuses)?;

      if was_stopped {
        write_jobs(|j| j.insert_job(job, false))?;
      }
      set_status(code);
    }
  }
  Ok(())
}

/// Reports timing info for a completed command timer.
/// Either fires on-time-report autocmds with context variables,
/// or falls back to formatting with TIMEFORMAT.
fn report_timer(timer: &CmdTimer) -> ShResult<()> {
  let autocmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnTimeReport));

  if autocmds.is_empty() {
    let fmt_str = state::get_time_fmt();
    let report = timer.format_report(&fmt_str)?;
    let stderr = borrow_fd(STDERR_FILENO);
    write(stderr, format!("{report}\n").as_bytes()).ok();
  } else {
    let vars = [
      ("TIME_REAL_MS".into(), timer.total_wall_ms()?.to_string()),
      ("TIME_USER_MS".into(), timer.total_user_ms()?.to_string()),
      ("TIME_SYS_MS".into(), timer.total_sys_ms()?.to_string()),
      (
        "TIME_REAL_FMT".into(),
        timer.total_wall_formatted()?.to_string(),
      ),
      (
        "TIME_USER_FMT".into(),
        timer.total_user_formatted()?.to_string(),
      ),
      (
        "TIME_SYS_FMT".into(),
        timer.total_sys_formatted()?.to_string(),
      ),
      ("TIME_CPU_PCT".into(), timer.cpu_pct()?.to_string()),
      ("TIME_RSS".into(), timer.max_rss()?.to_string()),
      ("TIME_CMD".into(), timer.command().to_string()),
    ];
    with_vars(vars, || autocmds.exec());
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
    with_term(|t| t.attach(job.pgid()))?;
  }
  disable_reaping();
  defer! {
    enable_reaping();
  }
  let (statuses, timers) = write_jobs(|j| j.new_fg(job))?;

  // Report time info after the write_jobs borrow is released
  for timer in &timers {
    if timer.should_report() {
      report_timer(timer)?;
    }
  }

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
  state::set_pipe_status(&statuses)?;

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

pub fn dispatch_job(mut job: Job, is_bg: bool, interactive: bool) -> ShResult<()> {
  if interactive {
    job.set_notify(true);
  }
  if is_bg {
    write_jobs(|j| j.insert_job(job, !interactive))?;
  } else {
    wait_fg(job, interactive)?;
  }
  Ok(())
}
