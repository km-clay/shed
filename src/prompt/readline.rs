use rustyline::{completion::{Completer, FilenameCompleter}, hint::{Hint, Hinter}, history::{History, SearchDirection}, Helper};

use crate::prelude::*;

pub struct SynHelper<'a> {
	file_comp: FilenameCompleter,
	pub shenv: &'a mut ShEnv,
	pub commands: Vec<String>
}

impl<'a> Helper for SynHelper<'a> {}

impl<'a> SynHelper<'a> {
	pub fn new(shenv: &'a mut ShEnv) -> Self {
		Self {
			file_comp: FilenameCompleter::new(),
			shenv,
			commands: vec![]
		}
	}

	pub fn hist_search(&self, term: &str, hist: &dyn History) -> Option<String> {
		let limit = hist.len();
		let mut latest_match = None;
		for i in 0..limit {
			if let Some(hist_entry) = hist.get(i, SearchDirection::Forward).ok()? {
				if hist_entry.entry.starts_with(term) {
					latest_match = Some(hist_entry.entry.into_owned())
				}
			}
		}
		latest_match
	}
}



impl<'a> Completer for SynHelper<'a> {
	type Candidate = String;
	fn complete( &self, line: &str, pos: usize, ctx: &rustyline::Context<'_>,) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
		Ok((0,vec![]))
	}
}

pub struct SynHint {
	text: String,
	styled: String
}

impl SynHint {
	pub fn new(text: String) -> Self {
		let styled = (&text).styled(Style::BrightBlack);
		Self { text, styled }
	}
	pub fn empty() -> Self {
		Self { text: String::new(), styled: String::new() }
	}
}

impl Hint for SynHint {
	fn display(&self) -> &str {
	  &self.styled
	}
	fn completion(&self) -> Option<&str> {
	  if !self.text.is_empty() {
			Some(&self.text)
		} else {
			None
		}
	}
}

impl<'a> Hinter for SynHelper<'a> {
	type Hint = SynHint;
	fn hint(&self, line: &str, pos: usize, ctx: &rustyline::Context<'_>) -> Option<Self::Hint> {
		if line.is_empty() {
			return None
		}
		let history = ctx.history();
		let result = self.hist_search(line, history)?;
		let window = result[line.len()..].trim_end().to_string();
		Some(SynHint::new(window))
	}
}
