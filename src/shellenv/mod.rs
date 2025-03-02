use std::env;

use jobs::Job;

use crate::prelude::*;

pub mod jobs;
pub mod logic;
pub mod exec_ctx;
pub mod meta;
pub mod shenv;
pub mod vars;

/// Calls attach_tty() on the shell's process group to retake control of the terminal
pub fn take_term() -> ShResult<()> {
	attach_tty(getpgrp())?;
	Ok(())
}

pub fn disable_reaping() -> ShResult<()> {
	log!(TRACE, "Disabling reaping");
	unsafe { signal(Signal::SIGCHLD, SigHandler::Handler(crate::signal::ignore_sigchld)) }?;
	Ok(())
}

/// Waits on the current foreground job and updates the shell's last status code
pub fn wait_fg(job: Job, shenv: &mut ShEnv) -> ShResult<()> {
	log!(TRACE, "Waiting on foreground job");
	let mut code = 0;
	attach_tty(job.pgid())?;
	disable_reaping()?;
	let statuses = write_jobs(|j| j.new_fg(job))?;
	for status in statuses {
		match status {
			WtStat::Exited(_, exit_code) => {
				code = exit_code;
			}
			WtStat::Stopped(pid, sig) => {
				write_jobs(|j| j.fg_to_bg(status))?;
				code = sys::SIG_EXIT_OFFSET + sig as i32;
			},
			WtStat::Signaled(pid, sig, _) => {
				if sig == Signal::SIGTSTP {
					write_jobs(|j| j.fg_to_bg(status))?;
				}
				code = sys::SIG_EXIT_OFFSET + sig as i32;
			},
			_ => { /* Do nothing */ }
		}
	}
	take_term()?;
	shenv.set_code(code);
	log!(TRACE, "exit code: {}", code);
	enable_reaping()?;
	Ok(())
}

pub fn log_level() -> crate::libsh::utils::LogLevel {
	let level = env::var("FERN_LOG_LEVEL").unwrap_or_default();
	match level.to_lowercase().as_str() {
		"error" => ERROR,
		"warn" =>  WARN,
		"info" =>  INFO,
		"debug" => DEBUG,
		"trace" => TRACE,
		_ =>       NULL
	}
}

pub fn enable_reaping() -> ShResult<()> {
	log!(TRACE, "Enabling reaping");
	unsafe { signal(Signal::SIGCHLD, SigHandler::Handler(crate::signal::handle_sigchld)) }.unwrap();
	Ok(())
}

pub fn attach_tty(pgid: Pid) -> ShResult<()> {
	if !isatty(0).unwrap_or(false) || pgid == term_ctlr() {
		return Ok(())
	}
	log!(DEBUG, "Attaching tty to pgid: {}",pgid);

	if pgid == getpgrp() && term_ctlr() != getpgrp() {
		kill(term_ctlr(), Signal::SIGTTOU).ok();
	}

	let mut new_mask = SigSet::empty();
	let mut mask_bkup = SigSet::empty();

	new_mask.add(Signal::SIGTSTP);
	new_mask.add(Signal::SIGTTIN);
	new_mask.add(Signal::SIGTTOU);

	pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&mut new_mask), Some(&mut mask_bkup))?;

	let result = unsafe { tcsetpgrp(borrow_fd(0), pgid) };

	pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&mut mask_bkup), Some(&mut new_mask))?;

	match result {
		Ok(_) => return Ok(()),
		Err(e) => {
			log!(ERROR, "error while switching term control: {}",e);
			unsafe { tcsetpgrp(borrow_fd(0), getpgrp())? };
			Ok(())
		}
	}
}

pub fn term_ctlr() -> Pid {
	unsafe { tcgetpgrp(borrow_fd(0)).unwrap_or(getpgrp()) }
}
