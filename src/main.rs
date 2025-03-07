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

use signal::sig_setup;

use crate::prelude::*;

pub fn main() {
	sig_setup();
	let mut shenv = ShEnv::new();
	if let Err(e) = shenv.source_rc() {
		eprintln!("Error sourcing rc file: {}", e.to_string());
	}

	loop {
		log!(TRACE, "Entered loop");
		let line = match prompt::read_line(&mut shenv) {
			Ok(line) => line,
			Err(e) => {
				eprintln!("{}",e);
				continue;
			}
		};
		let _ = exec_input(line, &mut shenv).eprint();
	}
}
