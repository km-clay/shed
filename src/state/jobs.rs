use super::*;

use scopeguard::defer;

use crate::{
  jobs::{Job, JobCmdFlags, JobID, RegisteredFd, take_term, wait_fg},
  libsh::error::ShResult,
  prelude::*,
  procio::{IoMode, borrow_fd},
  signal::{disable_reaping, enable_reaping},
};

#[derive(Clone, Default, Debug)]
pub struct JobTab {
  fg: Option<Job>,
  order: Vec<usize>,
  new_updates: Vec<usize>,
  jobs: Vec<Option<Job>>,
  fd_registry: Vec<RegisteredFd>,
}

impl JobTab {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn take_fg(&mut self) -> Option<Job> {
    self.fg.take()
  }
  fn next_open_pos(&self) -> usize {
    if let Some(position) = self.jobs.iter().position(|slot| slot.is_none()) {
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
    // Find the most recent valid job (order can have stale entries)
    self
      .order
      .iter()
      .rev()
      .find(|&&id| self.jobs.get(id).is_some_and(|slot| slot.is_some()))
      .copied()
  }
  pub fn prev_job(&self) -> Option<usize> {
    // Find the second most recent valid job
    let mut found_curr = false;
    for &id in self.order.iter().rev() {
      if self.jobs.get(id).is_some_and(|slot| slot.is_some()) {
        if found_curr {
          return Some(id);
        }
        found_curr = true;
      }
    }
    None
  }
  pub fn close_job_fds(&mut self, pid: Pid) {
    self.fd_registry.retain(|fd| fd.owner_pid != pid)
  }
  pub fn registered_fds(&self) -> &[RegisteredFd] {
    &self.fd_registry
  }
  pub fn register_fd(&mut self, owner_pid: Pid, fd: IoMode) {
    let registered_fd = RegisteredFd { fd, owner_pid };
    self.fd_registry.push(registered_fd)
  }
  /// Close all registered proc sub fds. Called after fork in exec_cmd
  /// so the parent doesn't hold pipe ends that prevent EOF.
  pub fn drain_registered_fds(&mut self) {
    self.fd_registry.clear();
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
  pub fn insert_job(&mut self, mut job: Job, silent: bool) -> ShResult<usize> {
    self.prune_jobs();
    let tab_pos = if let Some(id) = job.tabid() {
      id
    } else {
      self.next_open_pos()
    };
    job.set_tabid(tab_pos);
    let last_pid = job.children().last().map(|c| c.pid());
    self.order.push(tab_pos);
    if !silent {
      let msg = job.display(&self.order, JobCmdFlags::INIT);
      write_meta(|m| m.post_system_message(msg));
    }
    if tab_pos == self.jobs.len() {
      self.jobs.push(Some(job))
    } else {
      self.jobs[tab_pos] = Some(job);
    }

    if let Some(pid) = last_pid {
      write_vars(|v| v.set_param(ShellParam::LastJob, &pid.to_string()))
    }

    Ok(tab_pos)
  }
  pub fn order(&self) -> &[usize] {
    &self.order
  }
  pub fn query(&self, identifier: JobID) -> Option<&Job> {
    match identifier {
      // Match by process group ID
      JobID::Pgid(pgid) => self
        .jobs
        .iter()
        .find_map(|job| job.as_ref().filter(|j| j.pgid() == pgid)),
      // Match by process ID
      JobID::Pid(pid) => self.jobs.iter().find_map(|job| {
        job
          .as_ref()
          .filter(|j| j.children().iter().any(|child| child.pid() == pid))
      }),
      // Match by table ID (index in the job table)
      JobID::TableID(id) => self.jobs.get(id).and_then(|job| job.as_ref()),
      // Match by command name (partial match)
      JobID::Command(cmd) => self.jobs.iter().find_map(|job| {
        job.as_ref().filter(|j| {
          j.children()
            .iter()
            .any(|child| child.cmd().as_ref().is_some_and(|c| c.contains(&cmd)))
        })
      }),
    }
  }
  pub fn update_by_id(&mut self, id: JobID, stat: WtStat) -> ShResult<()> {
    let Some(job) = self.query_mut(id.clone()) else {
      return Ok(());
    };
    match id {
      JobID::Pid(pid) => {
        let Some(child) = job.children_mut().iter_mut().find(|c| c.pid() == pid) else {
          return Ok(());
        };
        child.set_stat(stat);
      }
      JobID::Pgid(_) | JobID::TableID(_) | JobID::Command(_) => {
        job.set_stats(stat);
      }
    }
    Ok(())
  }
  pub fn query_mut(&mut self, identifier: JobID) -> Option<&mut Job> {
    match identifier {
      // Match by process group ID
      JobID::Pgid(pgid) => self
        .jobs
        .iter_mut()
        .find_map(|job| job.as_mut().filter(|j| j.pgid() == pgid)),
      // Match by process ID
      JobID::Pid(pid) => self.jobs.iter_mut().find_map(|job| {
        job
          .as_mut()
          .filter(|j| j.children().iter().any(|child| child.pid() == pid))
      }),
      // Match by table ID (index in the job table)
      JobID::TableID(id) => self.jobs.get_mut(id).and_then(|job| job.as_mut()),
      // Match by command name (partial match)
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
  pub fn new_fg(&mut self, job: Job) -> ShResult<(Vec<WtStat>, Vec<CmdTimer>)> {
    self.fg = Some(job);
    let (statuses, timers) = self.fg.as_mut().unwrap().wait_pgrp()?;
    Ok((statuses, timers))
  }
  pub fn fg_to_bg(&mut self, stat: WtStat) -> ShResult<()> {
    if self.fg.is_none() {
      return Ok(());
    }
    take_term()?;
    let fg = std::mem::take(&mut self.fg);
    if let Some(mut job) = fg {
      job.set_stats(stat);
      self.insert_job(job, false)?;
    }
    Ok(())
  }
  pub fn bg_to_fg(&mut self, id: JobID) -> ShResult<()> {
    let job = self.remove_job(id);
    if let Some(job) = job {
      wait_fg(job, true)?;
    }
    Ok(())
  }
  pub fn wait_all_bg(&mut self) -> ShResult<()> {
    disable_reaping();
    defer! {
      enable_reaping();
    }
    for job in self.jobs.iter_mut() {
      let Some(job) = job else { continue };
      job.wait_pgrp()?;
    }
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
      // Skip foreground job
      let id = job.tabid().unwrap();
      // Filter jobs based on flags
      if flags.contains(JobCmdFlags::RUNNING)
        && !matches!(
          job.get_stats().get(id).unwrap(),
          WtStat::StillAlive | WtStat::Continued(_)
        )
      {
        continue;
      }
      if flags.contains(JobCmdFlags::STOPPED)
        && !matches!(job.get_stats().get(id).unwrap(), WtStat::Stopped(_, _))
      {
        continue;
      }
      // Print the job in the selected format
      write(
        borrow_fd(1),
        format!("{}\n", job.display(&self.order, flags)).as_bytes(),
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

  pub fn disown(&mut self, id: JobID, nohup: bool) -> ShResult<()> {
    if let Some(job) = self.query_mut(id.clone()) {
      if nohup {
        job.no_hup();
      } else {
        self.remove_job(id);
      }
    }
    Ok(())
  }

  pub fn disown_all(&mut self, nohup: bool) -> ShResult<()> {
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
    Ok(())
  }
}
