use std::{collections::HashMap, sync::{LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard}};

use crate::{jobs::{attach_tty, take_term, wait_fg, Job, JobCmdFlags, JobID}, libsh::error::ShResult, parse::lex::get_char, prelude::*, procio::borrow_fd};

pub static JOB_TABLE: LazyLock<RwLock<JobTab>> = LazyLock::new(|| RwLock::new(JobTab::new()));

pub static VAR_TABLE: LazyLock<RwLock<VarTab>> = LazyLock::new(|| RwLock::new(VarTab::new()));

pub struct JobTab {
	fg: Option<Job>,
	order: Vec<usize>,
	new_updates: Vec<usize>,
	jobs: Vec<Option<Job>>
}

impl JobTab {
	pub fn new() -> Self {
		Self { fg: None, order: vec![], new_updates: vec![], jobs: vec![] }
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
		self.order.last().copied()
	}
	pub fn prev_job(&self) -> Option<usize> {
		self.order.last().copied()
	}
	fn prune_jobs(&mut self) {
		while let Some(job) = self.jobs.last() {
			if job.is_none() {
				self.jobs.pop();
			} else {
				break
			}
		}
	}
	pub fn insert_job(&mut self, mut job: Job, silent: bool) -> ShResult<usize> {
		self.prune_jobs();
		let tab_pos = if let Some(id) = job.tabid() { id } else { self.next_open_pos() };
		job.set_tabid(tab_pos);
		self.order.push(tab_pos);
		if !silent {
			write(borrow_fd(1),format!("{}", job.display(&self.order, JobCmdFlags::INIT)).as_bytes())?;
		}
		if tab_pos == self.jobs.len() {
			self.jobs.push(Some(job))
		} else {
			self.jobs[tab_pos] = Some(job);
		}
		Ok(tab_pos)
	}
	pub fn order(&self) -> &[usize] {
		&self.order
	}
	pub fn query(&self, identifier: JobID) -> Option<&Job> {
		match identifier {
			// Match by process group ID
			JobID::Pgid(pgid) => {
				self.jobs.iter().find_map(|job| {
					job.as_ref().filter(|j| j.pgid() == pgid)
				})
			}
			// Match by process ID
			JobID::Pid(pid) => {
				self.jobs.iter().find_map(|job| {
					job.as_ref().filter(|j| j.children().iter().any(|child| child.pid() == pid))
				})
			}
			// Match by table ID (index in the job table)
			JobID::TableID(id) => {
				self.jobs.get(id).and_then(|job| job.as_ref())
			}
			// Match by command name (partial match)
			JobID::Command(cmd) => {
				self.jobs.iter().find_map(|job| {
					job.as_ref().filter(|j| {
						j.children().iter().any(|child| {
							child.cmd().as_ref().is_some_and(|c| c.contains(&cmd))
						})
					})
				})
			}
		}
	}
	pub fn query_mut(&mut self, identifier: JobID) -> Option<&mut Job> {
		match identifier {
			// Match by process group ID
			JobID::Pgid(pgid) => {
				self.jobs.iter_mut().find_map(|job| {
					job.as_mut().filter(|j| j.pgid() == pgid)
				})
			}
			// Match by process ID
			JobID::Pid(pid) => {
				self.jobs.iter_mut().find_map(|job| {
					job.as_mut().filter(|j| j.children().iter().any(|child| child.pid() == pid))
				})
			}
			// Match by table ID (index in the job table)
			JobID::TableID(id) => {
				self.jobs.get_mut(id).and_then(|job| job.as_mut())
			}
			// Match by command name (partial match)
			JobID::Command(cmd) => {
				self.jobs.iter_mut().find_map(|job| {
					job.as_mut().filter(|j| {
						j.children().iter().any(|child| {
							child.cmd().as_ref().is_some_and(|c| c.contains(&cmd))
						})
					})
				})
			}
		}
	}
	pub fn get_fg(&self) -> Option<&Job> {
		self.fg.as_ref()
	}
	pub fn get_fg_mut(&mut self) -> Option<&mut Job> {
		self.fg.as_mut()
	}
	pub fn new_fg<'a>(&mut self, job: Job) -> ShResult<Vec<WtStat>> {
		let pgid = job.pgid();
		self.fg = Some(job);
		attach_tty(pgid)?;
		let statuses = self.fg.as_mut().unwrap().wait_pgrp()?;
		attach_tty(getpgrp())?;
		Ok(statuses)
	}
	pub fn fg_to_bg(&mut self, stat: WtStat) -> ShResult<()> {
		if self.fg.is_none() {
			return Ok(())
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
			wait_fg(job)?;
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
			&self.jobs
				.iter()
				.filter(|job| job.as_ref().is_some_and(|job| self.new_updates.contains(&job.tabid().unwrap())))
				.map(|job| job.as_ref())
				.collect::<Vec<Option<&Job>>>()
		} else {
			&self.jobs
				.iter()
				.map(|job| job.as_ref())
				.collect::<Vec<Option<&Job>>>()
		};
		let mut jobs_to_remove = vec![];
		for job in jobs.iter().flatten() {
			// Skip foreground job
			let id = job.tabid().unwrap();
			// Filter jobs based on flags
			if flags.contains(JobCmdFlags::RUNNING) && !matches!(job.get_stats().get(id).unwrap(), WtStat::StillAlive | WtStat::Continued(_)) {
				continue;
			}
			if flags.contains(JobCmdFlags::STOPPED) && !matches!(job.get_stats().get(id).unwrap(), WtStat::Stopped(_,_)) {
				continue;
			}
			// Print the job in the selected format
			write(borrow_fd(1), format!("{}\n",job.display(&self.order,flags)).as_bytes())?;
			if job.get_stats().iter().all(|stat| matches!(stat,WtStat::Exited(_, _))) {
				jobs_to_remove.push(JobID::TableID(id));
			}
		}
		for id in jobs_to_remove {
			self.remove_job(id);
		}
		Ok(())
	}
}

pub struct VarTab {
	vars: HashMap<String,String>,
	params: HashMap<char,String>,
}

impl VarTab {
	pub fn new() -> Self {
		let vars = HashMap::new();
		let params = Self::init_params();
		Self { vars, params }
	}
	fn init_params() -> HashMap<char, String> {
		let mut params = HashMap::new();
		params.insert('?', "0".into());  // Last command exit status
		params.insert('#', "0".into());  // Number of positional parameters
		params.insert('0', std::env::current_exe().unwrap().to_str().unwrap().to_string()); // Name of the shell
		params.insert('$', Pid::this().to_string()); // PID of the shell
		params.insert('!', "".into()); // PID of the last background job (if any)
		params
	}
	pub fn vars(&self) -> &HashMap<String,String> {
		&self.vars
	}
	pub fn vars_mut(&mut self) -> &mut HashMap<String,String> {
		&mut self.vars
	}
	pub fn params(&self) -> &HashMap<char,String> {
		&self.params
	}
	pub fn params_mut(&mut self) -> &mut HashMap<char,String> {
		&mut self.params
	}
	pub fn get_var(&self, var: &str) -> String {
		if var.chars().count() == 1 {
			let param = self.get_param(get_char(var, 0).unwrap());
			if !param.is_empty() {
				return param
			}
		}
		if let Some(var) = self.vars.get(var).map(|s| s.to_string()) {
			var
		} else {
			std::env::var(var).unwrap_or_default()
		}
	}
	pub fn new_var(&mut self, var: &str, val: &str) {
		self.vars.insert(var.to_string(), val.to_string());
	}
	pub fn set_param(&mut self, param: char, val: &str) {
		self.params.insert(param,val.to_string());
	}
	pub fn get_param(&self, param: char) -> String {
		self.params.get(&param).map(|s| s.to_string()).unwrap_or("0".to_string())
	}
}

/// Read from the job table
pub fn read_jobs<T, F: FnOnce(RwLockReadGuard<JobTab>) -> T>(f: F) -> T {
	let lock = JOB_TABLE.read().unwrap();
	f(lock)
}

/// Write to the job table
pub fn write_jobs<T, F: FnOnce(&mut RwLockWriteGuard<JobTab>) -> T>(f: F) -> T {
	let lock = &mut JOB_TABLE.write().unwrap();
	f(lock)
}

/// Read from the variable table
pub fn read_vars<T, F: FnOnce(RwLockReadGuard<VarTab>) -> T>(f: F) -> T {
	let lock = VAR_TABLE.read().unwrap();
	f(lock)
}

/// Write to the variable table
pub fn write_vars<T, F: FnOnce(&mut RwLockWriteGuard<VarTab>) -> T>(f: F) -> T {
	let lock = &mut VAR_TABLE.write().unwrap();
	f(lock)
}

pub fn get_status() -> i32 {
	read_vars(|v| v.get_param('?')).parse::<i32>().unwrap()
}
pub fn set_status(code: i32) {
	write_vars(|v| v.set_param('?', &code.to_string()))
}
