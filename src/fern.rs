pub mod prelude;
pub mod libsh;
pub mod prompt;
pub mod procio;
pub mod parse;
pub mod expand;
pub mod state;
#[cfg(test)]
pub mod tests;

use std::process::exit;

use parse::{execute::{get_pipe_stack, Dispatcher}, lex::{LexFlags, LexStream}, ParseResult, ParseStream};
use state::write_vars;

fn main() {
	loop {
		let input = prompt::read_line().unwrap();
		if input == "quit" { break };
		write_vars(|v| v.new_var("foo", "bar"));

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
				ParseResult::Error(e) => panic!("{}",e),
				ParseResult::Match(node) => nodes.push(node),
				_ => unreachable!()
			}
		}

		let mut dispatcher = Dispatcher::new(nodes);
		dispatcher.begin_dispatch().unwrap();
	}
}
