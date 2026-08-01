use std::fmt::{self, Write};

use ariadne::Fmt;
use bitflags::bitflags;
use itertools::izip;
use nix::{
  errno::Errno,
  sys::{
    signal::{Signal, kill, killpg},
    wait::{WaitPidFlag as WtFlag, WaitStatus as WtStat, waitpid},
  },
  unistd::{Pid, getpgrp, getpid, setpgid, write},
};
use scopeguard::defer;
use yansi::Color;

use crate::{
  shopt,
  state::vars::{VarStr, VarStrSliceExt},
  varstr,
};

use super::{
  ShResult, Shed,
  meta::CmdTimer,
  procio::stdout_fileno,
  sherr,
  signal::{disable_reaping, enable_reaping},
  system_msg,
  vars::ShellParam,
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
        _ => write!(f, "failed: {code}"),
      },
      WtStat::Signaled(_, signal, _) => {
        write!(f, "signaled: {signal:?}")
      }
      WtStat::Stopped(_, signal) => {
        write!(f, "stopped: {signal:?}")
      }
      #[cfg(linux_like)]
      WtStat::PtraceEvent(_, signal, _) => {
        write!(f, "ptrace event: {signal:?}")
      }
      #[cfg(linux_like)]
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
    WtStat::Stopped(_, sig) | WtStat::Signaled(_, sig, _) => Some(SIG_EXIT_OFFSET + *sig as i32),
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

#[derive(Debug)]
pub struct ChildProc {
  pgid: Pid,
  pid: Pid,
  command: Option<VarStr>,
  stat: WtStat,
  timer: Option<CmdTimer>,
}

#[expect(clippy::similar_names)]
impl ChildProc {
  pub fn new(pid: Pid, command: Option<&str>, pgid: Option<Pid>, timer: Option<CmdTimer>) -> Self {
    let command = command.map(VarStr::from);
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
    child
  }
  pub fn pid(&self) -> Pid {
    self.pid
  }
  pub fn take_timer(&mut self) -> Option<CmdTimer> {
    self.timer.take()
  }
  pub fn cmd(&self) -> Option<VarStr> {
    self.command.clone()
  }
  pub fn stat(&self) -> WtStat {
    self.stat
  }
  pub fn wait(&mut self, flags: Option<WtFlag>) -> Result<WtStat, Errno> {
    let result = waitpid(self.pid, flags);
    if let Ok(stat) = result {
      self.stat = stat;
    }
    result
  }
  pub fn set_pgid(&mut self, pgid: Pid) -> ShResult<()> {
    setpgid(self.pid, pgid)?;
    self.pgid = pgid;
    Ok(())
  }
  pub fn set_stat(&mut self, stat: WtStat) {
    self.stat = stat;
  }
  pub fn exited(&self) -> bool {
    matches!(self.stat, WtStat::Exited(..))
  }
  pub fn terminated(&self) -> bool {
    matches!(self.stat, WtStat::Exited(..) | WtStat::Signaled(..))
  }
}

impl Clone for ChildProc {
  fn clone(&self) -> Self {
    Self {
      pgid: self.pgid,
      pid: self.pid,
      command: self.command.clone(),
      stat: self.stat,
      timer: None, // Timers are not cloned
    }
  }
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
  pub fn push_child(&mut self, child: ChildProc) {
    self.children.push(child);
  }
  pub fn set_pgid(&mut self, pgid: Pid) {
    self.pgid = Some(pgid);
  }
  pub fn pgid(&self) -> Option<Pid> {
    self.pgid
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
    self.0.push(JobBldr::new());
  }
  pub fn curr_job_mut(&mut self) -> Option<&mut JobBldr> {
    self.0.last_mut()
  }
  pub fn finalize_job(&mut self) -> Option<Job> {
    self.0.pop().map(JobBldr::build)
  }
}

#[derive(Debug)]
pub struct JobData {
  pub table_id: VarStr,
  pub notify: bool,
  pub stats: Vec<WtStat>,
  pub cmds: Vec<VarStr>,
  pub display: String,
  pub timer: Option<CmdTimer>,
}

#[derive(Debug)]
pub struct Job {
  table_id: Option<usize>,
  pgid: Pid,
  children: Vec<ChildProc>,
  notify: bool,
  send_hup: bool,
}

impl Job {
  pub fn set_tabid(&mut self, id: usize) {
    self.table_id = Some(id);
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
    !self.children.iter().all(ChildProc::terminated)
  }
  pub fn tabid(&self) -> Option<usize> {
    self.table_id
  }
  pub fn pgid(&self) -> Pid {
    self.pgid
  }
  pub fn get_cmds(&self) -> Vec<VarStr> {
    self
      .children
      .iter()
      .map(|c| c.cmd().unwrap_or_default())
      .collect()
  }
  pub fn take_job_data(&mut self, job_order: &[usize], pid: Option<Pid>) -> JobData {
    JobData {
      table_id: varstr!("{}", self.tabid().unwrap_or_default()),
      notify: self.notify(),
      stats: self.get_stats(),
      cmds: self.get_cmds().into_iter().collect::<Vec<_>>(),
      display: self.display(job_order, JobCmdFlags::PIDS).clone(),
      timer: pid.and_then(|pid| {
        self
          .children_mut()
          .iter_mut()
          .find(|chld| chld.pid() == pid)
          .and_then(ChildProc::take_timer)
      }),
    }
  }
  pub fn get_cmd_line(&self) -> VarStr {
    self.get_cmds().join_with(" | ")
  }
  pub fn set_stats(&mut self, stat: WtStat) {
    for child in &mut self.children {
      child.set_stat(stat);
    }
  }
  pub fn get_stats(&self) -> Vec<WtStat> {
    self.children.iter().map(ChildProc::stat).collect()
  }
  pub fn pipe_status(stats: &[WtStat]) -> Option<Vec<i32>> {
    if stats.iter().any(|stat| {
      if let WtStat::StillAlive | WtStat::Continued(_) = stat {
        return true;
      }

      #[cfg(linux_like)]
      if let WtStat::PtraceSyscall(_) = stat {
        return true;
      }

      false
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
          #[cfg(linux_like)]
          WtStat::PtraceEvent(_, signal, _) => SIG_EXIT_OFFSET + *signal as i32,
          #[cfg(linux_like)]
          WtStat::PtraceSyscall(_) => unreachable!(),
          WtStat::Continued(_) | WtStat::StillAlive => unreachable!(),
        })
        .collect(),
    )
  }
  pub fn get_pids(&self) -> Vec<Pid> {
    self
      .children
      .iter()
      .map(ChildProc::pid)
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
    for child in &mut self.children {
      if child.pid == Pid::this() {
        let code = Shed::get_status();
        stats.push(WtStat::Exited(child.pid, code));
        child.take_timer();
        continue;
      }
      loop {
        let result = child.wait(Some(WtFlag::WUNTRACED));
        child.take_timer();
        match result {
          Ok(stat) => {
            stats.push(stat);
            break;
          }
          Err(Errno::ECHILD) => {
            // we already reaped this somewhere else
            if matches!(child.stat(), WtStat::Exited(..) | WtStat::Signaled(..)) {
              stats.push(child.stat());
            }
            break;
          }
          Err(Errno::EINTR) => (),
          Err(e) => return Err(e.into()),
        }
      }
    }
    Ok(stats)
  }
  pub fn update_by_id(&mut self, id: JobID, stat: WtStat) {
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
          for child in &mut self.children {
            child.set_stat(stat);
          }
        }
      }
      JobID::Pgid(pgid) => {
        if pgid == self.pgid {
          for child in &mut self.children {
            child.set_stat(stat);
          }
        }
      }
    }
  }
  pub fn name(&self) -> Option<VarStr> {
    self.children().first().and_then(ChildProc::cmd)
  }
  pub fn display(&self, job_order: &[usize], flags: JobCmdFlags) -> String {
    let long = flags.contains(JobCmdFlags::LONG);
    let init = flags.contains(JobCmdFlags::INIT);
    let pids = flags.contains(JobCmdFlags::PIDS);

    let current = job_order.last();
    let prev = (job_order.len() >= 2)
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
      let pipe = if i == last_cmd { "" } else { " |" };

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

/// Calls `attach_tty()` on the shell's process group to retake control of the
/// terminal
pub fn take_term() -> ShResult<()> {
  Shed::term_mut(|t| t.attach(getpgrp()))?;
  killpg(getpgrp(), Signal::SIGWINCH)?;
  Ok(())
}

pub fn wait_bg(id: &JobID) -> ShResult<()> {
  disable_reaping();
  defer! {
    enable_reaping();
  };
  if let JobID::Pid(pid) = id {
    let stat = loop {
      match waitpid(*pid, None) {
        Ok(stat) => break stat,
        Err(Errno::EINTR) => (),
        Err(Errno::ECHILD) => return Ok(()),
        Err(e) => return Err(e.into()),
      }
    };
    Shed::jobs_mut(|j| j.update_by_id(id, stat));
    Shed::set_status(code_from_status(&stat).unwrap_or(0));
  } else {
    let Some(mut job) = Shed::jobs_mut(|j| j.remove_job(id.clone())) else {
      return Err(sherr!(ExecFail, "wait: No such job with id {:?}", id,));
    };
    let statuses = job.wait_pgrp()?;

    let mut was_stopped = false;
    let mut code = 0;
    let pipefail = shopt!(set.pipefail);

    for status in &statuses {
      let stage_code = code_from_status(status).unwrap_or(0);
      if !pipefail || stage_code != 0 {
        code = stage_code;
      }
      match status {
        WtStat::Stopped(_, _) => {
          was_stopped = true;
        }
        WtStat::Signaled(_, sig, _) if *sig == Signal::SIGTSTP => {
          was_stopped = true;
        }
        _ => {}
      }
    }

    Shed::set_pipe_status(&statuses)?;

    if was_stopped {
      Shed::jobs_mut(|j| j.insert_job(job, false));
    }
    Shed::set_status(code);
  }
  Ok(())
}

pub fn wait_fg(job: Job, interactive: bool) -> ShResult<()> {
  if job.children().is_empty() {
    return Ok(());
  }
  let mut was_stopped = false;
  if interactive {
    Shed::term_mut(|t| t.attach(job.pgid()))?;
  }
  disable_reaping();
  defer! {
    enable_reaping();
  }
  let statuses = Shed::jobs_mut(|j| j.new_fg(job))?;
  let mut code = 0;
  let pipefail = shopt!(set.pipefail);

  for status in &statuses {
    let stage_code = code_from_status(status).unwrap_or(0);
    if !pipefail || stage_code != 0 {
      code = stage_code;
    }
    match status {
      WtStat::Stopped(_, _) => {
        was_stopped = true;
        Shed::jobs_mut(|j| j.fg_to_bg(*status))?;
      }
      WtStat::Signaled(_, sig, _) => {
        if *sig == Signal::SIGINT {
          kill(getpid(), Signal::SIGINT)?;
        } else if *sig == Signal::SIGTSTP {
          was_stopped = true;
          Shed::jobs_mut(|j| j.fg_to_bg(*status))?;
        }
      }
      _ => {}
    }
  }
  Shed::set_pipe_status(&statuses)?;

  if !was_stopped {
    let job = Shed::jobs_mut(JobTab::take_fg);
    if interactive {
      Shed::meta_mut(|m| m.set_last_job(job));
    }
  }
  if interactive {
    take_term()?;
  }
  Shed::set_status(code);
  Ok(())
}

pub fn dispatch_job(mut job: Job, is_bg: bool, interactive: bool) -> ShResult<()> {
  if interactive {
    job.set_notify(true);
  }
  if is_bg {
    Shed::jobs_mut(|j| j.insert_job(job, !interactive));
  } else {
    wait_fg(job, interactive)?;
  }
  Ok(())
}

#[derive(Default, Debug)]
pub struct JobTab {
  fg: Option<Job>,
  order: Vec<usize>,
  new_updates: Vec<usize>,
  jobs: Vec<Option<Job>>,
}

impl JobTab {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn take_fg(&mut self) -> Option<Job> {
    self.fg.take()
  }
  fn next_open_pos(&self) -> usize {
    if let Some(position) = self.jobs.iter().position(Option::is_none) {
      position
    } else {
      self.jobs.len()
    }
  }
  pub fn jobs(&self) -> &Vec<Option<Job>> {
    &self.jobs
  }
  pub fn jobs_mut(&mut self) -> &mut Vec<Option<Job>> {
    &mut self.jobs
  }
  pub fn curr_job(&self) -> Option<usize> {
    self
      .order
      .iter()
      .rev()
      .find(|&&id| self.jobs.get(id).is_some_and(Option::is_some))
      .copied()
  }
  pub fn marker_order(&self) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for &id in self.order.iter().rev() {
      let live = self.jobs.get(id).is_some_and(Option::is_some);
      if live && !out.contains(&id) {
        out.push(id);
      }
    }
    out.reverse();
    out
  }
  fn prune_jobs(&mut self) {
    while let Some(job) = self.jobs.last() {
      if job.is_none() || job.as_ref().unwrap().is_done() {
        self.jobs.pop();
      } else {
        break;
      }
    }
  }
  pub fn insert_job(&mut self, mut job: Job, silent: bool) -> usize {
    self.prune_jobs();
    let tab_pos = if let Some(id) = job.tabid() {
      id
    } else {
      self.next_open_pos()
    };
    job.set_tabid(tab_pos);
    let last_pid = job.children().last().map(ChildProc::pid);
    self.order.push(tab_pos);
    if tab_pos >= self.jobs.len() {
      self.jobs.resize_with(tab_pos + 1, || None);
    }
    self.jobs[tab_pos] = Some(job);
    if !silent {
      // Display after inserting so `marker_order` sees the new job as current.
      let order = self.marker_order();
      let msg = self.jobs[tab_pos]
        .as_ref()
        .unwrap()
        .display(&order, JobCmdFlags::INIT);
      system_msg!("{msg}");
    }

    if let Some(pid) = last_pid {
      Shed::vars_mut(|v| v.set_param(ShellParam::LastJob, &pid.to_string()));
    }

    tab_pos
  }
  pub fn order(&self) -> &[usize] {
    &self.order
  }
  pub fn query(&self, identifier: JobID) -> Option<&Job> {
    match identifier {
      JobID::Pgid(pgid) => self
        .jobs
        .iter()
        .find_map(|job| job.as_ref().filter(|j| j.pgid() == pgid)),
      JobID::Pid(pid) => self.jobs.iter().find_map(|job| {
        job
          .as_ref()
          .filter(|j| j.children().iter().any(|child| child.pid() == pid))
      }),
      JobID::TableID(id) => self.jobs.get(id).and_then(|job| job.as_ref()),
      JobID::Command(cmd) => self.jobs.iter().find_map(|job| {
        job.as_ref().filter(|j| {
          j.children()
            .iter()
            .any(|child| child.cmd().as_ref().is_some_and(|c| c.contains(&cmd)))
        })
      }),
    }
  }
  pub fn update_by_id(&mut self, id: &JobID, stat: WtStat) {
    let Some(job) = self.query_mut(id.clone()) else {
      return;
    };
    match id {
      JobID::Pid(pid) => {
        let Some(child) = job.children_mut().iter_mut().find(|c| c.pid() == *pid) else {
          return;
        };
        child.set_stat(stat);
      }
      JobID::Pgid(_) | JobID::TableID(_) | JobID::Command(_) => {
        job.set_stats(stat);
      }
    }
  }
  pub fn query_mut(&mut self, identifier: JobID) -> Option<&mut Job> {
    match identifier {
      JobID::Pgid(pgid) => self
        .jobs
        .iter_mut()
        .find_map(|job| job.as_mut().filter(|j| j.pgid() == pgid)),
      JobID::Pid(pid) => self.jobs.iter_mut().find_map(|job| {
        job
          .as_mut()
          .filter(|j| j.children().iter().any(|child| child.pid() == pid))
      }),
      JobID::TableID(id) => self.jobs.get_mut(id).and_then(|job| job.as_mut()),
      JobID::Command(cmd) => self.jobs.iter_mut().find_map(|job| {
        job.as_mut().filter(|j| {
          j.children()
            .iter()
            .any(|child| child.cmd().as_ref().is_some_and(|c| c.contains(&cmd)))
        })
      }),
    }
  }
  pub fn get_fg(&self) -> Option<&Job> {
    self.fg.as_ref()
  }
  pub fn get_fg_mut(&mut self) -> Option<&mut Job> {
    self.fg.as_mut()
  }
  pub fn new_fg(&mut self, job: Job) -> ShResult<Vec<WtStat>> {
    self.fg = Some(job);
    let statuses = self.fg.as_mut().unwrap().wait_pgrp()?;
    Ok(statuses)
  }
  pub fn fg_to_bg(&mut self, stat: WtStat) -> ShResult<()> {
    if self.fg.is_none() {
      return Ok(());
    }
    take_term()?;
    let fg = std::mem::take(&mut self.fg);
    if let Some(mut job) = fg {
      job.set_stats(stat);
      self.insert_job(job, false);
    }
    Ok(())
  }
  pub fn wait_all_bg(&mut self) -> ShResult<()> {
    disable_reaping();
    defer! {
      enable_reaping();
    }
    let mut code = 0;
    for job in &mut self.jobs {
      let Some(job) = job else { continue };
      let statuses = job.wait_pgrp()?;
      code = statuses.last().and_then(code_from_status).unwrap_or(0);
    }
    Shed::set_status(code);
    Ok(())
  }
  pub fn remove_job(&mut self, id: JobID) -> Option<Job> {
    let tabid = self.query(id).map(|job| job.tabid().unwrap());
    if let Some(tabid) = tabid {
      self.jobs.get_mut(tabid).and_then(Option::take)
    } else {
      None
    }
  }
  pub fn print_jobs(&mut self, flags: JobCmdFlags) -> ShResult<()> {
    let marker_order = self.marker_order();
    let jobs = if flags.contains(JobCmdFlags::NEW_ONLY) {
      &self
        .jobs
        .iter()
        .filter(|job| {
          job
            .as_ref()
            .is_some_and(|job| self.new_updates.contains(&job.tabid().unwrap()))
        })
        .map(|job| job.as_ref())
        .collect::<Vec<Option<&Job>>>()
    } else {
      &self
        .jobs
        .iter()
        .map(|job| job.as_ref())
        .collect::<Vec<Option<&Job>>>()
    };
    let mut jobs_to_remove = vec![];
    for job in jobs.iter().flatten() {
      let id = job.tabid().unwrap();
      let stats = job.get_stats();
      if flags.contains(JobCmdFlags::RUNNING)
        && !stats
          .iter()
          .any(|s| matches!(s, WtStat::StillAlive | WtStat::Continued(_)))
      {
        continue;
      }
      if flags.contains(JobCmdFlags::STOPPED)
        && !stats.iter().any(|s| matches!(s, WtStat::Stopped(_, _)))
      {
        continue;
      }
      write(
        stdout_fileno(),
        format!("{}\n", job.display(&marker_order, flags)).as_bytes(),
      )?;
      if job
        .get_stats()
        .iter()
        .all(|stat| matches!(stat, WtStat::Exited(_, _) | WtStat::Signaled(_, _, _)))
      {
        jobs_to_remove.push(JobID::TableID(id));
      }
    }
    for id in jobs_to_remove {
      self.remove_job(id);
    }
    Ok(())
  }

  pub fn hang_up(&mut self) {
    for job in self.jobs_mut().iter_mut().flatten() {
      if job.send_hup() {
        job.killpg(Signal::SIGHUP).ok();
      }
    }
  }

  pub fn disown(&mut self, id: JobID, nohup: bool) {
    if let Some(job) = self.query_mut(id.clone()) {
      if nohup {
        job.no_hup();
      } else {
        self.remove_job(id);
      }
    }
  }

  pub fn disown_all(&mut self, nohup: bool) {
    let mut ids_to_remove = vec![];
    for job in self.jobs_mut().iter_mut().flatten() {
      if nohup {
        job.no_hup();
      } else {
        ids_to_remove.push(JobID::TableID(job.tabid().unwrap()));
      }
    }
    for id in ids_to_remove {
      self.remove_job(id);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::lifecycle;
  use crate::tests::testutil::TestGuard;
  use nix::unistd::{ForkResult, fork};

  // ─── No-fork paths ───────────────────────────────────────────────────

  #[test]
  fn wait_bg_pid_no_such_child_returns_ok() {
    let _g = TestGuard::new();
    // Use a pid that almost certainly doesn't exist as our child.
    // waitpid returns ECHILD; wait_bg swallows that as Ok.
    let result = wait_bg(&JobID::Pid(Pid::from_raw(1)));
    // root pid 1 IS alive but isn't our child → ECHILD.
    assert!(result.is_ok());
  }

  #[test]
  fn wait_bg_unknown_table_id_errors() {
    let _g = TestGuard::new();
    // No job with TableID 99999 — remove_job returns None, wait_bg errors.
    let result = wait_bg(&JobID::TableID(99999));
    assert!(result.is_err());
  }

  #[test]
  fn wait_bg_unknown_pgid_errors() {
    let _g = TestGuard::new();
    let result = wait_bg(&JobID::Pgid(Pid::from_raw(99999)));
    assert!(result.is_err());
  }

  // ─── Fork-based happy paths ──────────────────────────────────────────
  //
  // These tests fork a real child, have it exit quickly, then wait
  // for it via wait_bg. The child must _exit() (not std::process::exit)
  // so the test framework's cleanup doesn't run twice.

  #[test]
  fn wait_bg_pid_reaps_child_with_exit_zero() {
    let _g = TestGuard::new();
    let pid = match unsafe { fork() }.unwrap() {
      ForkResult::Child => {
        lifecycle::setup_child();

        // Don't let test machinery run in the child.
        unsafe { nix::libc::_exit(0) };
      }
      ForkResult::Parent { child } => child,
    };
    wait_bg(&JobID::Pid(pid)).unwrap();
    assert_eq!(Shed::get_status(), 0);
  }

  #[test]
  fn wait_bg_pid_reaps_child_with_nonzero_exit() {
    let _g = TestGuard::new();
    let pid = match unsafe { fork() }.unwrap() {
      ForkResult::Child => {
        lifecycle::setup_child();
        unsafe { nix::libc::_exit(42) };
      }
      ForkResult::Parent { child } => child,
    };
    wait_bg(&JobID::Pid(pid)).unwrap();
    assert_eq!(Shed::get_status(), 42);
  }

  #[test]
  fn wait_bg_pid_handles_signal_killed_child() {
    let _g = TestGuard::new();
    let pid = match unsafe { fork() }.unwrap() {
      ForkResult::Child => {
        lifecycle::setup_child();
        // Kill self with SIGTERM. The parent should see Signaled.
        unsafe { nix::libc::raise(nix::libc::SIGTERM) };
        // Should never reach here, but just in case:
        unsafe { nix::libc::_exit(0) };
      }
      ForkResult::Parent { child } => child,
    };
    wait_bg(&JobID::Pid(pid)).unwrap();
    // code_from_status maps Signaled(sig) → SIG_EXIT_OFFSET + sig.
    assert_eq!(Shed::get_status(), SIG_EXIT_OFFSET + Signal::SIGTERM as i32);
  }

  // ===================== Job::display =====================

  use crate::state::meta::CmdTimer;

  /// Strip ANSI CSI sequences so we can assert on the visible text.
  fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
      if c == '\x1b' && chars.peek() == Some(&'[') {
        chars.next(); // [
        while let Some(&next) = chars.peek() {
          chars.next();
          // CSI ends at a byte in 0x40..=0x7E (letter / @ / etc.)
          if next.is_ascii_alphabetic() || matches!(next, '@'..='`') {
            break;
          }
        }
      } else {
        out.push(c);
      }
    }
    out
  }

  fn mk_child(pid: i32, cmd: &str, stat: WtStat) -> ChildProc {
    ChildProc {
      pgid: Pid::from_raw(pid),
      pid: Pid::from_raw(pid),
      command: Some(cmd.into()),
      stat,
      timer: None,
    }
  }

  fn mk_job(table_id: usize, children: Vec<ChildProc>) -> Job {
    let pgid = children.first().map_or(Pid::from_raw(0), |c| c.pid);
    Job {
      table_id: Some(table_id),
      pgid,
      children,
      notify: false,
      send_hup: true,
    }
  }

  // ─── id-box symbol per job_order position ──────────────────────────

  #[test]
  fn display_current_job_gets_plus_symbol() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(100, "ls", WtStat::Exited(Pid::from_raw(100), 0))],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    assert!(out.contains("[1]+"), "got: {out:?}");
  }

  #[test]
  fn display_prev_job_gets_minus_symbol() {
    let _g = TestGuard::new();
    let job = mk_job(
      1,
      vec![mk_child(100, "ls", WtStat::Exited(Pid::from_raw(100), 0))],
    );
    // order = [0, 1, 2]: current=2, prev=1 → job with id=1 gets "-".
    let out = strip_ansi(&job.display(&[0, 1, 2], JobCmdFlags::empty()));
    assert!(out.contains("[2]-"), "got: {out:?}");
  }

  #[test]
  fn display_neither_current_nor_prev_gets_space_symbol() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(100, "ls", WtStat::Exited(Pid::from_raw(100), 0))],
    );
    // order = [3, 1, 2]: current=2, prev=1 → job with id=0 is neither.
    let out = strip_ansi(&job.display(&[3, 1, 2], JobCmdFlags::empty()));
    assert!(out.contains("[1] "), "got: {out:?}");
  }

  #[test]
  fn display_prev_gets_minus_with_exactly_two_jobs() {
    let _g = TestGuard::new();
    // order = [0, 1]: two jobs is enough for a `-` (bash/zsh show `[1]- [2]+`).
    // Job with id=0 is the previous job → "-".
    let job = mk_job(
      0,
      vec![mk_child(100, "ls", WtStat::Exited(Pid::from_raw(100), 0))],
    );
    let out = strip_ansi(&job.display(&[0, 1], JobCmdFlags::empty()));
    assert!(out.contains("[1]-"), "expected prev marker, got: {out:?}");
  }

  #[test]
  fn display_single_job_has_no_prev_marker() {
    let _g = TestGuard::new();
    // order = [0]: one job → only `+`, never `-`.
    let job = mk_job(
      0,
      vec![mk_child(100, "ls", WtStat::Exited(Pid::from_raw(100), 0))],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    assert!(out.contains("[1]+"), "got: {out:?}");
    assert!(!out.contains("[1]-"), "got: {out:?}");
  }

  #[test]
  fn marker_order_excludes_removed_jobs() {
    // Regression: `order` is append-only, so after a job is removed the raw
    // history still references its (dead) id — `marker_order` must drop it so
    // the `+`/`-` markers don't point at a removed job.
    let _g = TestGuard::new();
    let mut tab = JobTab::default();
    tab.insert_job(
      mk_job(0, vec![mk_child(100, "a", WtStat::StillAlive)]),
      true,
    );
    tab.insert_job(
      mk_job(1, vec![mk_child(200, "b", WtStat::StillAlive)]),
      true,
    );
    assert_eq!(tab.marker_order(), vec![0, 1]);
    tab.remove_job(JobID::TableID(1));
    assert_eq!(
      tab.marker_order(),
      vec![0],
      "removed job must not remain in marker order"
    );
  }

  // ─── status-text per WaitStatus variant ────────────────────────────

  #[test]
  fn display_exited_zero_shows_done() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(100, "ls", WtStat::Exited(Pid::from_raw(100), 0))],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    assert!(out.contains("done"), "got: {out:?}");
  }

  #[test]
  fn display_exited_nonzero_shows_failed_with_code() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(
        100,
        "false",
        WtStat::Exited(Pid::from_raw(100), 42),
      )],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    assert!(out.contains("failed: 42"), "got: {out:?}");
  }

  #[test]
  fn job_with_signaled_member_is_not_running() {
    // Regression: a pipeline member killed by a signal used to keep the whole
    // job perpetually "running" (running() only counted Exited), so the job
    // never completed once a stage was signal-killed.
    let job = mk_job(
      1,
      vec![
        mk_child(100, "a", WtStat::Exited(Pid::from_raw(100), 0)),
        mk_child(
          200,
          "b",
          WtStat::Signaled(Pid::from_raw(200), Signal::SIGKILL, false),
        ),
      ],
    );
    assert!(
      !job.running(),
      "a job whose members have all terminated is not running"
    );
  }

  #[test]
  fn job_with_alive_member_is_running() {
    // Control: a still-alive leader keeps the job running even if another stage
    // was signal-killed (the reported scenario before the leader exits).
    let job = mk_job(
      1,
      vec![
        mk_child(100, "a", WtStat::StillAlive),
        mk_child(
          200,
          "b",
          WtStat::Signaled(Pid::from_raw(200), Signal::SIGKILL, false),
        ),
      ],
    );
    assert!(job.running());
  }

  #[test]
  fn display_still_alive_shows_running() {
    let _g = TestGuard::new();
    let job = mk_job(0, vec![mk_child(100, "sleep 99", WtStat::StillAlive)]);
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    assert!(out.contains("running"), "got: {out:?}");
  }

  #[test]
  fn display_stopped_shows_stopped_with_signal() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(
        100,
        "cat",
        WtStat::Stopped(Pid::from_raw(100), Signal::SIGTSTP),
      )],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    assert!(out.contains("stopped"), "got: {out:?}");
    assert!(out.contains("SIGTSTP"), "got: {out:?}");
  }

  #[test]
  fn display_signaled_shows_signaled_with_signal() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(
        100,
        "loop",
        WtStat::Signaled(Pid::from_raw(100), Signal::SIGKILL, false),
      )],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    assert!(out.contains("signaled"), "got: {out:?}");
    assert!(out.contains("SIGKILL"), "got: {out:?}");
  }

  // ─── flags: LONG / PIDS / INIT ────────────────────────────────────

  #[test]
  fn display_long_flag_includes_first_child_pid_at_head() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(
        12345,
        "ls",
        WtStat::Exited(Pid::from_raw(12345), 0),
      )],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::LONG));
    assert!(out.contains("12345"), "got: {out:?}");
  }

  #[test]
  fn display_pids_flag_includes_pid_in_status_line() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(
        54321,
        "ls",
        WtStat::Exited(Pid::from_raw(54321), 0),
      )],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::PIDS));
    assert!(out.contains("54321"), "got: {out:?}");
  }

  #[test]
  fn display_init_flag_includes_pid_in_status_line() {
    // INIT goes through the same `pids || init` branch as PIDS.
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(
        67890,
        "ls",
        WtStat::Exited(Pid::from_raw(67890), 0),
      )],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::INIT));
    assert!(out.contains("67890"), "got: {out:?}");
  }

  #[test]
  fn display_default_flags_omits_pid_text() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![mk_child(
        11111,
        "ls",
        WtStat::Exited(Pid::from_raw(11111), 0),
      )],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    assert!(!out.contains("11111"), "got: {out:?}");
  }

  // ─── multi-child pipeline ─────────────────────────────────────────

  #[test]
  fn display_multi_child_uses_pipe_marker_except_on_last() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![
        mk_child(101, "echo hi", WtStat::Exited(Pid::from_raw(101), 0)),
        mk_child(102, "cat", WtStat::Exited(Pid::from_raw(102), 0)),
        mk_child(103, "wc", WtStat::Exited(Pid::from_raw(103), 0)),
      ],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    // First two cmds carry a trailing " |"; the last does not.
    assert!(out.contains("echo hi |"), "got: {out:?}");
    assert!(out.contains("cat |"), "got: {out:?}");
    // "wc" should NOT be followed by " |" — the final cmd has no pipe.
    assert!(!out.contains("wc |"), "got: {out:?}");
    assert!(out.contains("wc"), "got: {out:?}");
  }

  #[test]
  fn display_multi_child_emits_one_line_per_child() {
    let _g = TestGuard::new();
    let job = mk_job(
      0,
      vec![
        mk_child(101, "a", WtStat::Exited(Pid::from_raw(101), 0)),
        mk_child(102, "b", WtStat::Exited(Pid::from_raw(102), 0)),
        mk_child(103, "c", WtStat::Exited(Pid::from_raw(103), 0)),
      ],
    );
    let out = strip_ansi(&job.display(&[0], JobCmdFlags::empty()));
    // Three children → three trailing-newline lines in the body.
    assert_eq!(out.matches('\n').count(), 3, "got: {out:?}");
  }

  // ===================== wait_pgrp ECHILD fallback =====================

  #[test]
  fn wait_pgrp_echild_falls_back_to_recorded_exit_status() {
    // A child already reaped elsewhere (pid isn't ours → waitpid ECHILD) must
    // still contribute its recorded terminal status, not vanish (which would
    // make `wait %n` report 0 for a job that actually failed).
    let mut job = mk_job(
      7,
      vec![mk_child(
        31_001,
        "boom",
        WtStat::Exited(Pid::from_raw(31_001), 3),
      )],
    );
    let stats = job.wait_pgrp().unwrap();
    assert_eq!(stats, vec![WtStat::Exited(Pid::from_raw(31_001), 3)]);
  }

  #[test]
  fn wait_pgrp_echild_recovers_signaled_status() {
    let mut job = mk_job(
      7,
      vec![mk_child(
        31_002,
        "killed",
        WtStat::Signaled(Pid::from_raw(31_002), Signal::SIGTERM, false),
      )],
    );
    let stats = job.wait_pgrp().unwrap();
    assert_eq!(
      stats,
      vec![WtStat::Signaled(
        Pid::from_raw(31_002),
        Signal::SIGTERM,
        false
      )]
    );
  }

  #[test]
  fn wait_pgrp_echild_does_not_fabricate_status_for_non_terminal() {
    // If the recorded status isn't terminal (e.g. StillAlive), ECHILD must not
    // invent one — the stats stay empty rather than reporting a bogus exit.
    let mut job = mk_job(7, vec![mk_child(31_003, "alive", WtStat::StillAlive)]);
    let stats = job.wait_pgrp().unwrap();
    assert!(stats.is_empty(), "got: {stats:?}");
  }

  // ===================== Job::update_by_id =====================

  /// Build a 3-child job for the update tests. `table_id=5`, pgid taken
  /// from first child.
  fn three_child_job() -> Job {
    mk_job(
      5,
      vec![
        mk_child(100, "alpha cmd", WtStat::StillAlive),
        mk_child(200, "beta cmd", WtStat::StillAlive),
        mk_child(300, "gamma cmd", WtStat::StillAlive),
      ],
    )
  }

  // ─── JobID::Pid ───────────────────────────────────────────────────

  #[test]
  fn update_by_id_pid_matches_single_child() {
    let mut job = three_child_job();
    let new_stat = WtStat::Exited(Pid::from_raw(200), 0);
    job.update_by_id(JobID::Pid(Pid::from_raw(200)), new_stat);
    let stats = job.get_stats();
    // Only the child with pid=200 should be updated.
    assert_eq!(stats[0], WtStat::StillAlive);
    assert_eq!(stats[1], new_stat);
    assert_eq!(stats[2], WtStat::StillAlive);
  }

  #[test]
  fn update_by_id_pid_no_match_is_noop() {
    let mut job = three_child_job();
    job.update_by_id(
      JobID::Pid(Pid::from_raw(9999)),
      WtStat::Exited(Pid::from_raw(9999), 1),
    );
    for stat in job.get_stats() {
      assert_eq!(stat, WtStat::StillAlive);
    }
  }

  // ─── JobID::Command ──────────────────────────────────────────────

  #[test]
  fn update_by_id_command_substring_match_updates_first_match() {
    let mut job = three_child_job();
    let new_stat = WtStat::Exited(Pid::from_raw(100), 42);
    // "alpha" matches "alpha cmd" (substring).
    job.update_by_id(JobID::Command("alpha".into()), new_stat);
    let stats = job.get_stats();
    assert_eq!(stats[0], new_stat);
    assert_eq!(stats[1], WtStat::StillAlive);
    assert_eq!(stats[2], WtStat::StillAlive);
  }

  #[test]
  fn update_by_id_command_finds_first_match_only() {
    // Both children contain "cmd"; only the first matching child gets
    // updated.
    let mut job = three_child_job();
    let new_stat = WtStat::Exited(Pid::from_raw(100), 1);
    job.update_by_id(JobID::Command("cmd".into()), new_stat);
    let stats = job.get_stats();
    assert_eq!(stats[0], new_stat);
    assert_eq!(stats[1], WtStat::StillAlive);
    assert_eq!(stats[2], WtStat::StillAlive);
  }

  #[test]
  fn update_by_id_command_no_match_is_noop() {
    let mut job = three_child_job();
    job.update_by_id(
      JobID::Command("zzz".into()),
      WtStat::Exited(Pid::from_raw(100), 1),
    );
    for stat in job.get_stats() {
      assert_eq!(stat, WtStat::StillAlive);
    }
  }

  // ─── JobID::TableID ──────────────────────────────────────────────

  #[test]
  fn update_by_id_table_id_matches_updates_all_children() {
    let mut job = three_child_job();
    let new_stat = WtStat::Exited(Pid::from_raw(0), 7);
    job.update_by_id(JobID::TableID(5), new_stat);
    // table_id matched → every child gets the new stat.
    for stat in job.get_stats() {
      assert_eq!(stat, new_stat);
    }
  }

  #[test]
  fn update_by_id_table_id_no_match_is_noop() {
    let mut job = three_child_job();
    job.update_by_id(JobID::TableID(99), WtStat::Exited(Pid::from_raw(0), 1));
    for stat in job.get_stats() {
      assert_eq!(stat, WtStat::StillAlive);
    }
  }

  #[test]
  fn update_by_id_table_id_with_unset_tableid_is_noop() {
    // Build a job whose table_id is None (e.g., a fresh job not yet
    // inserted into the table). update_by_id with TableID should be a
    // no-op.
    let mut job = mk_job(0, vec![mk_child(100, "x", WtStat::StillAlive)]);
    job.set_tabid(usize::MAX); // ensure it's Some but doesn't collide
    let () = job.update_by_id(JobID::TableID(0), WtStat::Exited(Pid::from_raw(0), 99));
    assert_eq!(job.get_stats()[0], WtStat::StillAlive);
  }

  // ─── JobID::Pgid ─────────────────────────────────────────────────

  #[test]
  fn update_by_id_pgid_matches_updates_all_children() {
    let mut job = three_child_job();
    // mk_job took pgid from first child = pid 100.
    let new_stat = WtStat::Exited(Pid::from_raw(100), 0);
    job.update_by_id(JobID::Pgid(Pid::from_raw(100)), new_stat);
    for stat in job.get_stats() {
      assert_eq!(stat, new_stat);
    }
  }

  #[test]
  fn update_by_id_pgid_no_match_is_noop() {
    let mut job = three_child_job();
    job.update_by_id(
      JobID::Pgid(Pid::from_raw(9999)),
      WtStat::Exited(Pid::from_raw(9999), 1),
    );
    for stat in job.get_stats() {
      assert_eq!(stat, WtStat::StillAlive);
    }
  }

  // ===================== report_timer =====================

  use crate::state::logic::{AutoCmd, AutoCmdKind};

  /// Build a `CmdTimer` that's already stopped, ready for reporting.
  fn complete_timer() {
    // drops instantly
    CmdTimer::new().ok();
  }

  #[test]
  fn report_timer_no_autocmds_writes_default_format_to_stderr() {
    let g = TestGuard::new();
    // Make sure no OnTimeReport autocmds are registered.
    Shed::logic_mut(|l| l.clear_autocmds(AutoCmdKind::OnTimeReport));
    complete_timer();
    let out = g.read_output();
    // Default fmt is "\nreal\t%*E\nuser\t%*U\nsys\t%*S" — three labels.
    assert!(out.contains("real"), "got: {out:?}");
    assert!(out.contains("user"), "got: {out:?}");
    assert!(out.contains("sys"), "got: {out:?}");
  }

  #[test]
  #[expect(non_snake_case)] // name preserves the TIMEFMT env-var spelling
  fn report_timer_respects_TIMEFMT_var() {
    let g = TestGuard::new();
    Shed::logic_mut(|l| l.clear_autocmds(AutoCmdKind::OnTimeReport));
    // Custom format with our own marker text.
    Shed::vars_mut(|v| {
      v.set_var(
        "TIMEFMT",
        crate::state::vars::VarKind::Str("CUSTOM_TIMEFMT_MARKER".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
    });
    complete_timer();
    let out = g.read_output();
    assert!(out.contains("CUSTOM_TIMEFMT_MARKER"), "got: {out:?}");
  }

  #[test]
  fn report_timer_with_autocmd_fires_with_time_vars_set() {
    // Register an OnTimeReport autocmd that echoes the wall-time var.
    // Verify it surfaces in the captured output.
    let g = TestGuard::new();
    Shed::logic_mut(|l| {
      l.clear_autocmds(AutoCmdKind::OnTimeReport);
      l.insert_autocmd(AutoCmd::new(
        AutoCmdKind::OnTimeReport,
        "echo RFM=$TIME_REAL_FMT".into(),
      ));
    });
    complete_timer();
    let out = g.read_output();
    assert!(
      out.contains("RFM="),
      "expected TIME_REAL_FMT to be set; got: {out:?}"
    );
    // The default-format path should NOT have run.
    assert!(!out.contains("\nreal\t"), "default format leaked: {out:?}");
  }

  // ===================== JobTab::print_jobs =====================

  /// Insert a fake job into the live job table. Mark its child
  /// `StillAlive` by default so `prune_jobs` doesn't drop it. Returns the
  /// assigned tabid.
  fn insert_real_job(pid: i32, cmd: &str, stat: WtStat) -> usize {
    let child = mk_child(pid, cmd, stat);
    let mut bldr = JobBldr::new();
    bldr.push_child(child);
    bldr.set_pgid(Pid::from_raw(pid));
    let job = bldr.build();
    Shed::jobs_mut(|j| j.insert_job(job, true))
  }

  fn drain_jobs() {
    Shed::jobs_mut(|j| {
      // Mark all jobs done so prune sweeps them.
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

  #[test]
  fn print_jobs_no_jobs_no_output() {
    let g = TestGuard::new();
    drain_jobs();
    Shed::jobs_mut(|j| j.print_jobs(JobCmdFlags::empty())).unwrap();
    assert_eq!(g.read_output(), "");
  }

  #[test]
  fn print_jobs_running_job_appears_in_output() {
    let g = TestGuard::new();
    drain_jobs();
    insert_real_job(50001, "running_cmd", WtStat::StillAlive);
    Shed::jobs_mut(|j| j.print_jobs(JobCmdFlags::empty())).unwrap();
    let out = strip_ansi(&g.read_output());
    assert!(out.contains("running_cmd"), "got: {out:?}");
  }

  #[test]
  fn print_jobs_running_filter_includes_alive_jobs() {
    let g = TestGuard::new();
    drain_jobs();
    insert_real_job(50010, "alive_cmd", WtStat::StillAlive);
    Shed::jobs_mut(|j| j.print_jobs(JobCmdFlags::RUNNING)).unwrap();
    let out = strip_ansi(&g.read_output());
    assert!(out.contains("alive_cmd"), "got: {out:?}");
  }

  #[test]
  fn print_jobs_running_filter_excludes_stopped_jobs() {
    let g = TestGuard::new();
    drain_jobs();
    insert_real_job(
      50020,
      "stopped_cmd",
      WtStat::Stopped(Pid::from_raw(50020), Signal::SIGTSTP),
    );
    Shed::jobs_mut(|j| j.print_jobs(JobCmdFlags::RUNNING)).unwrap();
    let out = strip_ansi(&g.read_output());
    assert!(!out.contains("stopped_cmd"), "got: {out:?}");
  }

  #[test]
  fn print_jobs_stopped_filter_includes_stopped_jobs() {
    let g = TestGuard::new();
    drain_jobs();
    insert_real_job(
      50030,
      "stop_me",
      WtStat::Stopped(Pid::from_raw(50030), Signal::SIGTSTP),
    );
    Shed::jobs_mut(|j| j.print_jobs(JobCmdFlags::STOPPED)).unwrap();
    let out = strip_ansi(&g.read_output());
    assert!(out.contains("stop_me"), "got: {out:?}");
  }

  #[test]
  fn print_jobs_stopped_filter_excludes_running_jobs() {
    let g = TestGuard::new();
    drain_jobs();
    insert_real_job(50040, "alive_one", WtStat::StillAlive);
    Shed::jobs_mut(|j| j.print_jobs(JobCmdFlags::STOPPED)).unwrap();
    let out = strip_ansi(&g.read_output());
    assert!(!out.contains("alive_one"), "got: {out:?}");
  }

  #[test]
  fn print_jobs_exited_jobs_get_removed_after_print() {
    let g = TestGuard::new();
    drain_jobs();
    let id = insert_real_job(50050, "done_cmd", WtStat::Exited(Pid::from_raw(50050), 0));
    assert!(Shed::jobs(|j| j.query(JobID::TableID(id)).is_some()));
    Shed::jobs_mut(|j| j.print_jobs(JobCmdFlags::empty())).unwrap();
    let out = strip_ansi(&g.read_output());
    assert!(out.contains("done_cmd"), "got: {out:?}");
    // After print, exited jobs are swept.
    assert!(
      !Shed::jobs(|j| j.query(JobID::TableID(id)).is_some()),
      "exited job should have been removed"
    );
  }

  #[test]
  fn print_jobs_alive_jobs_not_removed() {
    let g = TestGuard::new();
    drain_jobs();
    let id = insert_real_job(50060, "still_running", WtStat::StillAlive);
    Shed::jobs_mut(|j| j.print_jobs(JobCmdFlags::empty())).unwrap();
    g.read_output();
    assert!(
      Shed::jobs(|j| j.query(JobID::TableID(id)).is_some()),
      "alive job should not be removed"
    );
  }

  #[test]
  fn print_jobs_multiple_jobs_all_printed() {
    let g = TestGuard::new();
    drain_jobs();
    insert_real_job(50070, "first_cmd", WtStat::StillAlive);
    insert_real_job(50071, "second_cmd", WtStat::StillAlive);
    Shed::jobs_mut(|j| j.print_jobs(JobCmdFlags::empty())).unwrap();
    let out = strip_ansi(&g.read_output());
    assert!(out.contains("first_cmd"), "got: {out:?}");
    assert!(out.contains("second_cmd"), "got: {out:?}");
  }
}
