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
pub mod getopt;
pub mod shopt;
#[cfg(test)]
pub mod tests;

use crate::libsh::sys::{save_termios, set_termios};
use crate::parse::execute::exec_input;
use crate::signal::sig_setup;
use crate::state::source_rc;
use crate::prelude::*;



fn main() {
	save_termios();
	set_termios();
	sig_setup();

	if let Err(e) = source_rc() {
		eprintln!("{e}");
	}

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
				if readline_err_count == 5 {
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
