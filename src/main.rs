#![allow(static_mut_refs,unused_unsafe)]

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

bitflags! {
	pub struct FernFlags: u32 {
		const NO_RC = 0b000001;
		const NO_HIST = 0b000010;
		const INTERACTIVE = 0b000100;
	}
}

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
fn parse_args(shenv: &mut ShEnv) {
	let mut args = std::env::args().skip(1);
	let mut script_path: Option<PathBuf> = None;
	let mut command: Option<String> = None;
	let mut flags = FernFlags::empty();

	log!(DEBUG, args);
	while let Some(mut arg) = args.next() {
		log!(DEBUG, arg);
		if arg.starts_with("--") {
			arg = arg.strip_prefix("--").unwrap().to_string();
			match arg.as_str() {
				"no-rc" => flags |= FernFlags::NO_RC,
				"no-hist" => flags |= FernFlags::NO_HIST,
				_ => eprintln!("Warning - Unrecognized option: {arg}")
			}
		} else if arg.starts_with('-') {
			arg = arg.strip_prefix('-').unwrap().to_string();
			match arg.as_str() {
				"c" => command = args.next(),
				_ => eprintln!("Warning - Unrecognized option: {arg}")
			}
		} else {
			let path_check = PathBuf::from(&arg);
			if path_check.is_file() {
				script_path = Some(path_check);
			}
		}
	}

	if !flags.contains(FernFlags::NO_RC) {
		let _ = shenv.source_rc().eprint();
	}

	if let Some(cmd) = command {
		let input = clean_string(cmd);
		let _ = exec_input(input, shenv).eprint();

	} else if let Some(script) = script_path {
		let _ = shenv.source_file(script).eprint();

	} else {
		interactive(shenv);
	}
}

pub fn main() {
	sig_setup();
	save_termios();
	set_termios();
	let mut shenv = ShEnv::new();

	parse_args(&mut shenv);
}

fn interactive(shenv: &mut ShEnv) {
	loop {
		log!(TRACE, "Entered loop");
		match prompt::read_line(shenv) {
			Ok(line) => {
				shenv.meta_mut().start_timer();
				let _ = exec_input(line, shenv).eprint();
			}
			Err(e) => {
				eprintln!("{}",e);
				continue;
			}
		};
	}
}
