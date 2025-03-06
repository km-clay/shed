use rustyline::validate::{ValidationResult, Validator};

use crate::prelude::*;

use super::readline::SynHelper;

pub fn check_delims(line: &str) -> bool {
	let mut delim_stack = vec![];
	let mut chars = line.chars();
	let mut in_case = false;
	let mut case_check = String::new();
	let mut in_quote = None; // Tracks which quote type is open (`'` or `"`)

	while let Some(ch) = chars.next() {
		case_check.push(ch);
		if case_check.ends_with("case") {
			in_case = true;
		}
		if case_check.ends_with("esac") {
			in_case = false;
		}
		match ch {
			'{' | '(' | '[' if in_quote.is_none() => delim_stack.push(ch),
			'}' if in_quote.is_none() && delim_stack.pop() != Some('{') => return false,
			')' if in_quote.is_none() && delim_stack.pop() != Some('(') => {
				if !in_case {
					return false
				}
			}
			']' if in_quote.is_none() && delim_stack.pop() != Some('[') => return false,
			'"' | '\'' => {
				if in_quote == Some(ch) {
					in_quote = None;
				} else if in_quote.is_none() {
					in_quote = Some(ch);
				}
			}
			'\\' => { chars.next(); } // Skip next character if escaped
			_ => {}
		}
	}

	delim_stack.is_empty() && in_quote.is_none()
}

pub fn check_keywords(line: &str, shenv: &mut ShEnv) -> bool {
	use TkRule::*;
	let mut expecting: Vec<Vec<TkRule>> = vec![];
	let mut tokens = Lexer::new(line.to_string(),shenv).lex().into_iter();

	while let Some(token) = tokens.next() {
		match token.rule() {
			If => {
				expecting.push(vec![Then]);
			}
			Then => {
				if let Some(frame) = expecting.pop() {
					if frame.contains(&Then) {
						expecting.push(vec![Elif, Else, Fi])
					} else { return false }
				} else { return false }
			}
			Elif => {
				if let Some(frame) = expecting.pop() {
					if frame.contains(&Elif) {
						expecting.push(vec![Then])
					} else { return false }
				} else { return false }
			}
			Else => {
				if let Some(frame) = expecting.pop() {
					if frame.contains(&Else) {
						expecting.push(vec![Fi])
					} else { return false }
				} else { return false }
			}
			Fi => {
				if let Some(frame) = expecting.pop() {
					if frame.contains(&Fi) {
						/* Do nothing */
					} else { return false }
				} else { return false }
			}
			While | Until | For | Select => {
				expecting.push(vec![Do])
			}
			Do => {
				if let Some(frame) = expecting.pop() {
					if frame.contains(&Do) {
						expecting.push(vec![Done])
					} else { return false }
				} else { return false }
			}
			Done => {
				if let Some(frame) = expecting.pop() {
					if frame.contains(&Done) {
						/* Do nothing */
					} else { return false }
				} else { return false }
			}
			Case => {
				expecting.push(vec![Esac])
			}
			Esac => {
				if let Some(frame) = expecting.pop() {
					if frame.contains(&Esac) {
						/* Do nothing */
					} else { return false }
				} else { return false }
			}
			_ => { /* Do nothing */ }
		}
	}

	expecting.is_empty()
}

impl<'a> Validator for SynHelper<'a> {
	fn validate(&self, ctx: &mut rustyline::validate::ValidationContext) -> rustyline::Result<rustyline::validate::ValidationResult> {
		let input = ctx.input();
		let mut shenv_clone = self.shenv.clone();
		match check_delims(input) && check_keywords(input, &mut shenv_clone) {
			true => Ok(ValidationResult::Valid(None)),
			false => Ok(ValidationResult::Incomplete),
		}

	}
}
