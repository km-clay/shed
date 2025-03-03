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

use libc::PIPE_BUF;
use nix::unistd::setpgid;
use signal::sig_setup;

use crate::prelude::*;

pub fn main() {
	sig_setup();
	let mut shenv = ShEnv::new();

	loop {
		log!(TRACE, "Entered loop");
		let mut line = match prompt::read_line(&mut shenv) {
			Ok(line) => line,
			Err(e) => {
				eprintln!("{}",e);
				continue;
			}
		};
		if let Some(line_exp) = expand_aliases(&line, &mut shenv) {
			line = line_exp;
		}
		let input = Rc::new(line);
		log!(INFO, "New input: {:?}", input);
		let token_stream = Lexer::new(input).lex();
		log!(DEBUG, token_stream);
		log!(DEBUG, token_stream);
		log!(TRACE, "Token stream: {:?}", token_stream);
		match Parser::new(token_stream).parse() {
			Err(e) => {
				eprintln!("{}",e);
			}
			Ok(syn_tree) => {
				if let Err(e) = Executor::new(syn_tree, &mut shenv).walk() {
					eprintln!("{}",e);
				}
			}
		}
		log!(TRACE, "Finished iteration");
	}
}
