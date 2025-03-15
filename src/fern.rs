pub mod prelude;
pub mod libsh;
pub mod prompt;
pub mod procio;
pub mod parse;
pub mod expand;
pub mod state;
pub mod builtin;
pub mod jobs;
#[cfg(test)]
pub mod tests;

use parse::{execute::Dispatcher, lex::{LexFlags, LexStream}, ParseStream};
use crate::prelude::*;

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
