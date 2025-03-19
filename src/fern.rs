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
pub mod tests;

use std::collections::HashSet;

use expand::expand_aliases;
use libsh::error::ShResult;
use parse::{execute::Dispatcher, lex::{LexFlags, LexStream, Tk}, Ast, ParseStream, ParsedSrc};
use procio::IoFrame;
use signal::sig_setup;
use state::{source_rc, write_logic, write_meta};
use termios::{LocalFlags, Termios};
use crate::prelude::*;

pub static mut SAVED_TERMIOS: Option<Option<Termios>> = None;


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
pub fn get_saved_termios() -> Option<Termios> {
	unsafe {
		// This is only used when the shell exits so it's fine
		// SAVED_TERMIOS is only mutated once at the start as well
		SAVED_TERMIOS.clone().flatten()
	}
}
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
	let mut parser = ParsedSrc::new(Rc::new(input));
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

	loop {
		let input = match prompt::read_line() {
			Ok(line) => line,
			Err(e) => {
				eprintln!("{e}");
				continue
			}
		};

		if let Err(e) = exec_input(input) {
			eprintln!("{e}");
		}
	}
}
