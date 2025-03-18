use std::collections::VecDeque;

use crate::parse::lex::{Span, Tk};
use crate::parse::{Redir, RedirType};
use crate::prelude::*;

pub trait VecDequeExt<T> {
	fn to_vec(self) -> Vec<T>;
}

pub trait TkVecUtils<Tk> {
	fn get_span(&self) -> Option<Span>;
	fn debug_tokens(&self);
}

pub trait RedirVecUtils<Redir> {
	/// Splits the vector of redirections into two vectors
	///
	/// One vector contains input redirs, the other contains output redirs
	fn split_by_channel(self) -> (Vec<Redir>,Vec<Redir>);
}

impl<T> VecDequeExt<T> for VecDeque<T> {
	fn to_vec(self) -> Vec<T> {
		self.into_iter().collect::<Vec<T>>()
	}
}

impl TkVecUtils<Tk> for Vec<Tk> {
	fn get_span(&self) -> Option<Span> {
		if let Some(first_tk) = self.first() {
			if let Some(last_tk) = self.last() {
				Some(
					Span::new(
						first_tk.span.start..last_tk.span.end,
						first_tk.source()
					)
				)
			} else {
				None
			}
		} else {
			None
		}
	}
	fn debug_tokens(&self) {
		for token in self {
			flog!(DEBUG, "token: {}",token)
		}
	}
}

impl RedirVecUtils<Redir> for Vec<Redir> {
	fn split_by_channel(self) -> (Vec<Redir>,Vec<Redir>) {
		let mut input = vec![];
		let mut output = vec![];
		for redir in self {
			match redir.class {
				RedirType::Input => input.push(redir),
				RedirType::Pipe => {
					match redir.io_mode.tgt_fd() {
						STDIN_FILENO => input.push(redir),
						STDOUT_FILENO |
							STDERR_FILENO => output.push(redir),
						_ => unreachable!()
					}
				}
				_ => output.push(redir)
			}
		}
		(input,output)
	}
}
