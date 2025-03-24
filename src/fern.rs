pub mod prelude;
pub mod libsh;
pub mod prompt;
pub mod procio;
pub mod parse;
pub mod expand;
pub mod state;
pub mod builtin;
pub mod jobs;
pub mod signal;
#[cfg(test)]
pub mod tests;
pub mod getopt;
pub mod shopt;

use std::collections::HashSet;

use expand::expand_aliases;
use libsh::error::ShResult;
use parse::{execute::Dispatcher, ParsedSrc};
use signal::sig_setup;
use state::{source_rc, write_meta};
use termios::{LocalFlags, Termios};
use crate::prelude::*;

/// The previous state of the terminal options.
///
/// This variable stores the terminal settings at the start of the program and restores them when the program exits.
/// It is initialized exactly once at the start of the program and accessed exactly once at the end of the program.
/// It will not be mutated or accessed under any other circumstances.
///
/// This ended up being necessary because wrapping Termios in a thread-safe way was unreasonably tricky.
///
/// The possible states of this variable are:
/// - `None`: The terminal options have not been set yet (before initialization).
/// - `Some(None)`: There were no terminal options to save (i.e., no terminal input detected).
/// - `Some(Some(Termios))`: The terminal options (as `Termios`) have been saved.
///
/// **Important:** This static variable is mutable and accessed via unsafe code. It is only safe to use because:
/// - It is set once during program startup and accessed once during program exit.
/// - It is not mutated or accessed after the initial setup and final read.
///
/// **Caution:** Future changes to this code should respect these constraints to ensure safety. Modifying or accessing this variable outside the defined lifecycle could lead to undefined behavior.
pub(crate) static mut SAVED_TERMIOS: Option<Option<Termios>> = None;

pub fn save_termios() {
	unsafe {
		SAVED_TERMIOS = Some(if isatty(std::io::stdin().as_raw_fd()).unwrap() {
			let mut termios = termios::tcgetattr(std::io::stdin()).unwrap();
			termios.local_flags &= !LocalFlags::ECHOCTL;
			termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, &termios).unwrap();
			Some(termios)
		} else {
			None
		});
	}
}
#[allow(static_mut_refs)]
pub unsafe fn get_saved_termios() -> Option<Termios> {
	// SAVED_TERMIOS should *only ever* be set once and accessed once
	// Set at the start of the program, and accessed during the exit of the program to reset the termios.
	// Do not use this variable anywhere else
	SAVED_TERMIOS.clone().flatten()
}

/// Set termios to not echo control characters, like ^Z for instance
fn set_termios() {
	if isatty(std::io::stdin().as_raw_fd()).unwrap() {
		let mut termios = termios::tcgetattr(std::io::stdin()).unwrap();
		termios.local_flags &= !LocalFlags::ECHOCTL;
		termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, &termios).unwrap();
	}
}

pub fn exec_input(input: String) -> ShResult<()> {
	write_meta(|m| m.start_timer());
	let input = expand_aliases(input, HashSet::new());
	let mut parser = ParsedSrc::new(Arc::new(input));
	parser.parse_src()?;

	let mut dispatcher = Dispatcher::new(parser.extract_nodes());
	dispatcher.begin_dispatch()
}

fn main() {
	save_termios();
	set_termios();
	sig_setup();

	if let Err(e) = source_rc() {
		eprintln!("{e}");
	}

	const MAX_READLINE_ERRORS: u32 = 5;
	let mut readline_err_count: u32 = 0;

	loop { // Main loop
		let input = match prompt::read_line() {
			Ok(line) => {
				readline_err_count = 0;
				line
			}
			Err(e) => {
				eprintln!("{e}");
				readline_err_count += 1;
				if readline_err_count == MAX_READLINE_ERRORS {
					eprintln!("reached maximum readline error count, exiting");
					break
				} else {
					continue
				}
			}
		};

		if let Err(e) = exec_input(input) {
			eprintln!("{e}");
		}
	}
	exit(1);
}
