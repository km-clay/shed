use std::borrow::Cow;

use rustyline::{completion::Completer, highlight::Highlighter, hint::{Hint, Hinter}, validate::{ValidationResult, Validator}, Helper};

use crate::{libsh::term::{Style, Styled}, parse::{lex::{LexFlags, LexStream}, ParseStream}};
use crate::prelude::*;

pub struct FernReadline {
}

impl FernReadline {
	pub fn new() -> Self {
		Self { }
	}
}

impl Helper for FernReadline {}

impl Completer for FernReadline {
	type Candidate = String;
}

pub struct FernHint {
	raw: String,
	styled: String
}

impl FernHint {
	pub fn new(raw: String) -> Self {
		let styled = (&raw).styled(Style::Dim | Style::BrightBlack);
		Self { raw, styled }
	}
}

impl Hint for FernHint {
	fn display(&self) -> &str {
		&self.styled
	}
	fn completion(&self) -> Option<&str> {
		if !self.raw.is_empty() {
			Some(&self.raw)
		} else {
			None
		}
	}
}

impl Hinter for FernReadline {
	type Hint = FernHint;
	fn hint(&self, line: &str, pos: usize, ctx: &rustyline::Context<'_>) -> Option<Self::Hint> {
		let ent = ctx.history().search(
			line,
			ctx.history().len() - 1,
			rustyline::history::SearchDirection::Reverse
		).ok()??;
		let entry_raw = ent.entry.get(pos..)?.to_string();
		Some(FernHint::new(entry_raw))
	}
}

impl Highlighter for FernReadline {
	fn highlight<'l>(&self, line: &'l str, pos: usize) -> std::borrow::Cow<'l, str> {
		Cow::Owned(line.to_string())
	}
}

impl Validator for FernReadline {
	fn validate(&self, ctx: &mut rustyline::validate::ValidationContext) -> rustyline::Result<rustyline::validate::ValidationResult> {
		return Ok(ValidationResult::Valid(None));
		let mut tokens = vec![];
		let tk_stream = LexStream::new(Rc::new(ctx.input().to_string()), LexFlags::empty());
		for tk in tk_stream {
			if tk.is_err() {
				return Ok(ValidationResult::Incomplete)
			}
			tokens.push(tk.unwrap());
		}
		let nd_stream = ParseStream::new(tokens);
		for nd in nd_stream {
			if nd.is_err() {
				return Ok(ValidationResult::Incomplete)
			}
		}
		Ok(ValidationResult::Valid(None))
	}
}
