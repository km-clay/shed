#![allow(unused_unsafe)]

pub mod libsh;
pub mod shellenv;
pub mod parse;
pub mod prelude;
pub mod execute;
pub mod signal;
pub mod prompt;
pub mod builtin;
pub mod expand;
pub mod tests;

use std::os::fd::AsRawFd;

use nix::sys::termios::{self, LocalFlags, Termios};
use signal::sig_setup;

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

pub fn main() {
	sig_setup();
	save_termios();
	set_termios();
	let mut shenv = ShEnv::new();
	if let Err(e) = shenv.source_rc() {
		eprintln!("Error sourcing rc file: {}", e.to_string());
	}

	loop {
		log!(TRACE, "Entered loop");
		match prompt::read_line(&mut shenv) {
			Ok(line) => {
				shenv.meta_mut().start_timer();
				let _ = exec_input(line, &mut shenv).eprint();
			}
			Err(e) => {
				eprintln!("{}",e);
				continue;
			}
		};
	}
}
