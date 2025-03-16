use std::collections::VecDeque;

use crate::parse::lex::{Span, Tk};
use crate::prelude::*;

pub trait VecDequeExt<T> {
	fn to_vec(self) -> Vec<T>;
}

pub trait TkVecUtils<Tk> {
	fn get_span(&self) -> Option<Span>;
	fn debug_tokens(&self);
}

impl<T> VecDequeExt<T> for VecDeque<T> {
	fn to_vec(self) -> Vec<T> {
		self.into_iter().collect::<Vec<T>>()
	}
}

impl<'t> TkVecUtils<Tk<'t>> for Vec<Tk<'t>> {
	fn get_span(&self) -> Option<Span<'t>> {
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
