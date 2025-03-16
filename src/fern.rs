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

use libsh::error::ShResult;
use parse::{execute::Dispatcher, lex::{LexFlags, LexStream}, ParseStream};
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

pub fn exec_input(input: &str) -> ShResult<()> {
	let mut tokens = vec![];
	for token in LexStream::new(&input, LexFlags::empty()) {
		tokens.push(token?);
	}

	let mut nodes = vec![];
	for result in ParseStream::new(tokens) {
		nodes.push(result?);
	}

	let mut dispatcher = Dispatcher::new(nodes);
	dispatcher.begin_dispatch()?;
	Ok(())
}

fn main() {
	save_termios();
	set_termios();
	loop {
		let input = prompt::read_line().unwrap();
		let start = Instant::now();

		if let Err(e) = exec_input(&input) {
			eprintln!("{e}");
		}
		flog!(INFO, "cmd duration: {:?}", start.elapsed());
	}
}
