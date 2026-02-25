#![allow(
	clippy::derivable_impls,
	clippy::tabs_in_doc_comments,
	clippy::while_let_on_iterator
)]
pub mod builtin;
pub mod expand;
pub mod getopt;
pub mod jobs;
pub mod libsh;
pub mod parse;
pub mod prelude;
pub mod procio;
pub mod prompt;
pub mod shopt;
pub mod signal;
pub mod state;
#[cfg(test)]
pub mod tests;

use std::os::fd::BorrowedFd;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::read;

use crate::builtin::trap::TrapTarget;
use crate::libsh::error::{ShErr, ShErrKind, ShResult};
use crate::libsh::sys::TTY_FILENO;
use crate::parse::execute::exec_input;
use crate::prelude::*;
use crate::prompt::get_prompt;
use crate::prompt::readline::term::{LineWriter, RawModeGuard, raw_mode};
use crate::prompt::readline::{Prompt, ReadlineEvent, ShedVi};
use crate::signal::{GOT_SIGWINCH, QUIT_CODE, check_signals, sig_setup, signals_pending};
use crate::state::{read_logic, source_rc, write_jobs, write_meta};
use clap::Parser;
use state::{read_vars, write_vars};

#[derive(Parser, Debug)]
struct ShedArgs {
	script: Option<String>,

	#[arg(short)]
	command: Option<String>,

	#[arg(trailing_var_arg = true)]
	script_args: Vec<String>,

	#[arg(long)]
	version: bool,

	#[arg(short)]
	interactive: bool,

	#[arg(long,short)]
	login_shell: bool,
}

/// Force evaluation of lazily-initialized values early in shell startup.
///
/// In particular, this ensures that the variable table is initialized, which
/// populates environment variables from the system. If this initialization is
/// deferred too long, features like prompt expansion may fail due to missing
/// environment variables.
///
/// This function triggers initialization by calling `read_vars` with a no-op
/// closure, which forces access to the variable table and causes its `LazyLock`
/// constructor to run.
fn kickstart_lazy_evals() {
	read_vars(|_| {});
}

/// We need to make sure that even if we panic, our child processes get sighup
fn setup_panic_handler() {
	let default_panic_hook = std::panic::take_hook();
	std::panic::set_hook(Box::new(move |info| {
		let _ = state::FERN.try_with(|shed| {
			if let Ok(mut jobs) = shed.jobs.try_borrow_mut() {
				jobs.hang_up();
			}
		});

		default_panic_hook(info);
	}));
}

fn main() -> ExitCode {
	env_logger::init();
	kickstart_lazy_evals();
	setup_panic_handler();

	let mut args = ShedArgs::parse();
	if env::args().next().is_some_and(|a| a.starts_with('-')) {
		// first arg is '-shed'
		// meaning we are in a login shell
		args.login_shell = true;
	}
	if args.version {
		println!("shed {} ({} {})", env!("CARGO_PKG_VERSION"), std::env::consts::ARCH, std::env::consts::OS);
		return ExitCode::SUCCESS;
	}

	if let Err(e) = if let Some(path) = args.script {
		run_script(path, args.script_args)
	} else if let Some(cmd) = args.command {
		exec_input(cmd, None, false)
	} else {
		shed_interactive()
	} {
		eprintln!("shed: {e}");
	};

	if let Some(trap) = read_logic(|l| l.get_trap(TrapTarget::Exit))
	&& let Err(e) = exec_input(trap, None, false) {
		eprintln!("shed: error running EXIT trap: {e}");
	}

	write_jobs(|j| j.hang_up());
	ExitCode::from(QUIT_CODE.load(Ordering::SeqCst) as u8)
}

fn run_script<P: AsRef<Path>>(path: P, args: Vec<String>) -> ShResult<()> {
	let path = path.as_ref();
	if !path.is_file() {
		eprintln!("shed: Failed to open input file: {}", path.display());
		QUIT_CODE.store(1, Ordering::SeqCst);
		return Err(ShErr::simple(
				ShErrKind::CleanExit(1),
				"input file not found",
		));
	}
	let Ok(input) = fs::read_to_string(path) else {
		eprintln!("shed: Failed to read input file: {}", path.display());
		QUIT_CODE.store(1, Ordering::SeqCst);
		return Err(ShErr::simple(
				ShErrKind::CleanExit(1),
				"failed to read input file",
		));
	};

	write_vars(|v| {
		v.cur_scope_mut()
			.bpush_arg(path.to_string_lossy().to_string())
	});
	for arg in args {
		write_vars(|v| v.cur_scope_mut().bpush_arg(arg))
	}

	exec_input(input, None, false)
}

fn shed_interactive() -> ShResult<()> {
	let _raw_mode = raw_mode(); // sets raw mode, restores termios on drop
	sig_setup();

	if let Err(e) = source_rc() {
		eprintln!("{e}");
	}

	// Create readline instance with initial prompt
	let mut readline = match ShedVi::new(Prompt::new(), *TTY_FILENO) {
		Ok(rl) => rl,
		Err(e) => {
			eprintln!("Failed to initialize readline: {e}");
			QUIT_CODE.store(1, Ordering::SeqCst);
			return Err(ShErr::simple(
					ShErrKind::CleanExit(1),
					"readline initialization failed",
			));
		}
	};

	// Main poll loop
	loop {
		write_meta(|m| {
			m.try_rehash_commands();
			m.try_rehash_cwd_listing();
		});

		// Handle any pending signals
		while signals_pending() {
			if let Err(e) = check_signals() {
				match e.kind() {
					ShErrKind::ClearReadline => {
						// Ctrl+C - clear current input and show new prompt
						readline.reset(Prompt::new());
					}
					ShErrKind::CleanExit(code) => {
						QUIT_CODE.store(*code, Ordering::SeqCst);
						return Ok(());
					}
					_ => eprintln!("{e}"),
				}
			}
		}

		if GOT_SIGWINCH.swap(false, Ordering::SeqCst) {
			log::info!("Window size change detected, updating readline dimensions");
			readline.writer.update_t_cols();
		}

		readline.prompt_mut().refresh();
		readline.print_line(false)?;

		// Poll for stdin input
		let mut fds = [PollFd::new(
			unsafe { BorrowedFd::borrow_raw(*TTY_FILENO) },
			PollFlags::POLLIN,
		)];

		match poll(&mut fds, PollTimeout::MAX) {
			Ok(_) => {}
			Err(Errno::EINTR) => {
				// Interrupted by signal, loop back to handle it
				continue;
			}
			Err(e) => {
				eprintln!("poll error: {e}");
				break;
			}
		}

		// Check if stdin has data
		if fds[0].revents().is_some_and(|r| r.contains(PollFlags::POLLIN)) {
			let mut buffer = [0u8; 1024];
			match read(*TTY_FILENO, &mut buffer) {
				Ok(0) => {
					// EOF
					break;
				}
				Ok(n) => {
					readline.feed_bytes(&buffer[..n]);
				}
				Err(Errno::EINTR) => {
					// Interrupted, continue to handle signals
					continue;
				}
				Err(e) => {
					eprintln!("read error: {e}");
					break;
				}
			}
		}

		// Process any available input
		match readline.process_input() {
			Ok(ReadlineEvent::Line(input)) => {
				let start = Instant::now();
				write_meta(|m| m.start_timer());
				if let Err(e) = RawModeGuard::with_cooked_mode(|| exec_input(input, None, true)) {
					match e.kind() {
						ShErrKind::CleanExit(code) => {
							QUIT_CODE.store(*code, Ordering::SeqCst);
							return Ok(());
						}
						_ => eprintln!("{e}"),
					}
				}
				let command_run_time = start.elapsed();
				log::info!("Command executed in {:.2?}", command_run_time);
				write_meta(|m| m.stop_timer());
				readline.writer.flush_write("\n")?;

				// Reset for next command with fresh prompt
				readline.reset(Prompt::new());
				let real_end = start.elapsed();
				log::info!("Total round trip time: {:.2?}", real_end);
			}
			Ok(ReadlineEvent::Eof) => {
				// Ctrl+D on empty line
				QUIT_CODE.store(0, Ordering::SeqCst);
				return Ok(());
			}
			Ok(ReadlineEvent::Pending) => {
				// No complete input yet, keep polling
			}
			Err(e) => match e.kind() {
				ShErrKind::CleanExit(code) => {
					QUIT_CODE.store(*code, Ordering::SeqCst);
					return Ok(());
				}
				_ => eprintln!("{e}"),
			}
		}
	}

	Ok(())
}
