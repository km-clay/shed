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

fn main() {
	'main: loop {
		let input = prompt::read_line().unwrap();
		if input == "quit" { break };
		let start = Instant::now();

		let mut tokens = vec![];
		for token in LexStream::new(&input, LexFlags::empty()) {
			if token.is_err() {
				let error = format!("{:?}: {}",token.err,token.err_span.unwrap().as_str());
				panic!("{error}");
			}
			tokens.push(token);
		}

		let mut nodes = vec![];
		for result in ParseStream::new(tokens) {
			match result {
				Ok(node) => nodes.push(node),
				Err(e) => {
					eprintln!("{:?}",e);
					continue 'main // Isn't rust cool
				}
			}
		}

		let mut dispatcher = Dispatcher::new(nodes);
		dispatcher.begin_dispatch().unwrap();
		flog!(INFO, "elapsed: {:?}", start.elapsed());
	}
}
