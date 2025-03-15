use crate::{libsh::{error::ShResult, term::{Style, Styled}}, prelude::*, procio::borrow_fd, state::{set_status, write_jobs}};

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
			WtStat::Exited(_, code) => {
				match code {
					0 => write!(f, "done"),
					_ => write!(f, "failed: {}", code),
				}
			}
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

#[derive(Clone,Debug)]
pub enum JobID {
	Pgid(Pid),
	Pid(Pid),
	TableID(usize),
	Command(String)
}

#[derive(Debug,Clone)]
pub struct ChildProc {
	pgid: Pid,
	pid: Pid,
	command: Option<String>,
	stat: WtStat
}

impl<'a> ChildProc {
	pub fn new(pid: Pid, command: Option<&str>, pgid: Option<Pid>) -> ShResult<Self> {
		let command = command.map(|str| str.to_string());
		let stat = if kill(pid,None).is_ok() {
			WtStat::StillAlive
		} else {
			WtStat::Exited(pid, 0)
		};
		let mut child = Self { pgid: pid, pid, command, stat };
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
		self.command.as_ref().map(|cmd| cmd.as_str())
	}
	pub fn stat(&self) -> WtStat {
		self.stat
	}
	pub fn wait(&mut self, flags: Option<WtFlag>) -> Result<WtStat,Errno> {
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
		matches!(self.stat,WtStat::Stopped(..))
	}
	pub fn exited(&self) -> bool {
		matches!(self.stat,WtStat::Exited(..))
	}
}

pub struct JobBldr {
	table_id: Option<usize>,
	pgid: Option<Pid>,
	children: Vec<ChildProc>
}

impl Default for JobBldr {
	fn default() -> Self {
		Self::new()
	}
}

impl JobBldr {
	pub fn new() -> Self {
		Self { table_id: None, pgid: None, children: vec![] }
	}
	pub fn with_id(self, id: usize) -> Self {
		Self {
			table_id: Some(id),
			pgid: self.pgid,
			children: self.children
		}
	}
	pub fn with_pgid(self, pgid: Pid) -> Self {
		Self {
			table_id: self.table_id,
			pgid: Some(pgid),
			children: self.children
		}
	}
	pub fn with_children(self, children: Vec<ChildProc>) -> Self {
		Self {
			table_id: self.table_id,
			pgid: self.pgid,
			children
		}
	}
	pub fn build(self) -> Job {
		Job {
			table_id: self.table_id,
			pgid: self.pgid.unwrap_or(Pid::from_raw(0)),
			children: self.children
		}
	}
}

#[derive(Debug,Clone)]
pub struct Job {
	table_id: Option<usize>,
	pgid: Pid,
	children: Vec<ChildProc>
}

impl Job {
	pub fn set_tabid(&mut self, id: usize) {
		self.table_id = Some(id)
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
		self.children
			.iter()
			.map(|chld| chld.stat())
			.collect::<Vec<WtStat>>()
	}
	pub fn get_pids(&self) -> Vec<Pid> {
		self.children
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
	pub fn killpg(&mut self, sig: Signal) -> ShResult<()> {
		let stat = match sig {
			Signal::SIGTSTP => WtStat::Stopped(self.pgid, Signal::SIGTSTP),
			Signal::SIGCONT => WtStat::Continued(self.pgid),
			Signal::SIGTERM => WtStat::Signaled(self.pgid, Signal::SIGTERM, false),
			_ => unimplemented!("{}",sig)
		};
		self.set_stats(stat);
		Ok(killpg(self.pgid, sig)?)
	}
	pub fn wait_pgrp<'a>(&mut self) -> ShResult<Vec<WtStat>> {
		let mut stats = vec![];
		for child in self.children.iter_mut() {
			let result = child.wait(Some(WtFlag::WUNTRACED));
			match result {
				Ok(stat) => {
					stats.push(stat);
				}
				Err(Errno::ECHILD) => break,
				Err(e) => return Err(e.into())
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
				let query_result = self.children
					.iter_mut()
					.find(|chld| chld
						.cmd()
						.is_some_and(|chld_cmd| chld_cmd.contains(&cmd))
					);
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

		let mut output = format!("[{}]{}\t", id + 1, symbol);
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

			let mut stat_line = if init {
				"".to_string()
			} else {
				fmt_stat.clone()
			};
			stat_line = format!("{}{} ",pid,stat_line);
			stat_line = format!("{} {}", stat_line, cmd);
			stat_line = match job_stat {
				WtStat::Stopped(..) | WtStat::Signaled(..) => stat_line.styled(Style::Magenta),
				WtStat::Exited(_, code) => {
					match code {
						0 => stat_line.styled(Style::Green),
						_ => stat_line.styled(Style::Red),
					}
				}
				_ => stat_line.styled(Style::Cyan)
			};
			if i != self.get_cmds().len() - 1 {
				stat_line = format!("{} |",stat_line);
			}

			let stat_final = if long {
				format!(
					"{}{} {}",
					if i != 0 { &padding } else { "" },
					self.get_pids().get(i).unwrap(),
					stat_line
				)
			} else {
				format!(
					"{}{}",
					if i != 0 { &padding } else { "" },
					stat_line
				)
			};
			output.push_str(&stat_final);
			output.push('\n');
		}
		output
	}
}

pub fn term_ctlr() -> Pid {
	tcgetpgrp(borrow_fd(0)).unwrap_or(getpgrp())
}

/// Calls attach_tty() on the shell's process group to retake control of the terminal
pub fn take_term() -> ShResult<()> {
	attach_tty(getpgrp())?;
	Ok(())
}

pub fn disable_reaping() -> ShResult<()> {
	flog!(TRACE, "Disabling reaping");
	unsafe { signal(Signal::SIGCHLD, SigHandler::Handler(crate::signal::ignore_sigchld)) }?;
	Ok(())
}

pub fn enable_reaping() -> ShResult<()> {
	flog!(TRACE, "Enabling reaping");
	unsafe { signal(Signal::SIGCHLD, SigHandler::Handler(crate::signal::handle_sigchld)) }.unwrap();
	Ok(())
}

/// Waits on the current foreground job and updates the shell's last status code
pub fn wait_fg(job: Job) -> ShResult<()> {
	flog!(TRACE, "Waiting on foreground job");
	let mut code = 0;
	attach_tty(job.pgid())?;
	disable_reaping()?;
	let statuses = write_jobs(|j| j.new_fg(job))?;
	for status in statuses {
		match status {
			WtStat::Exited(_, exit_code) => {
				code = exit_code;
			}
			WtStat::Stopped(_, sig) => {
				write_jobs(|j| j.fg_to_bg(status))?;
				code = SIG_EXIT_OFFSET + sig as i32;
			},
			WtStat::Signaled(_, sig, _) => {
				if sig == Signal::SIGTSTP {
					write_jobs(|j| j.fg_to_bg(status))?;
				}
				code = SIG_EXIT_OFFSET + sig as i32;
			},
			_ => { /* Do nothing */ }
		}
	}
	take_term()?;
	set_status(code);
	flog!(TRACE, "exit code: {}", code);
	enable_reaping()?;
	Ok(())
}

pub fn dispatch_job(job: Job, is_bg: bool) -> ShResult<()> {
	if is_bg {
		write_jobs(|j| {
			j.insert_job(job, false)
		})?;
	} else {
		wait_fg(job)?;
	}
	Ok(())
}

pub fn attach_tty(pgid: Pid) -> ShResult<()> {
	// If we aren't attached to a terminal, the pgid already controls it, or the process group does not exist
	// Then return ok
	if !isatty(0).unwrap_or(false) || pgid == term_ctlr() || killpg(pgid, None).is_err() {
		return Ok(())
	}
	flog!(TRACE, "Attaching tty to pgid: {}",pgid);

	if pgid == getpgrp() && term_ctlr() != getpgrp() {
		kill(term_ctlr(), Signal::SIGTTOU).ok();
	}

	let mut new_mask = SigSet::empty();
	let mut mask_bkup = SigSet::empty();

	new_mask.add(Signal::SIGTSTP);
	new_mask.add(Signal::SIGTTIN);
	new_mask.add(Signal::SIGTTOU);

	pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&mut new_mask), Some(&mut mask_bkup))?;

	let result = tcsetpgrp(borrow_fd(0), pgid);

	pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&mut mask_bkup), Some(&mut new_mask))?;

	match result {
		Ok(_) => return Ok(()),
		Err(e) => {
			flog!(ERROR, "error while switching term control: {}",e);
			tcsetpgrp(borrow_fd(0), getpgrp())?;
			Ok(())
		}
	}
}
