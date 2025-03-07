use rustyline::validate::{ValidationResult, Validator};

use crate::prelude::*;

use super::readline::SynHelper;

pub fn check_delims(line: &str) -> bool {
	let mut delim_stack = vec![];
	let mut chars = line.chars();
	let mut case_depth: u64 = 0;
	let mut case_check = String::new();
	let mut in_quote = None; // Tracks which quote type is open (`'` or `"`)

	while let Some(ch) = chars.next() {
		case_check.push(ch);
		if case_check.len() > 4 {
			case_check = case_check[1..].to_string();
		}
		if case_check.ends_with("case") {
			case_depth += 1;
		}
		if case_check.ends_with("esac") {
			case_depth = case_depth.saturating_sub(1);
		}
		match ch {
			'{' | '(' | '[' if in_quote.is_none() => delim_stack.push(ch),
			'}' if in_quote.is_none() && delim_stack.pop() != Some('{') => return false,
			')' if in_quote.is_none() && delim_stack.pop() != Some('(') => {
				if case_depth == 0 {
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
	shenv.new_input(line);
	let tokens = Lexer::new(line.to_string(),shenv).lex();
	Parser::new(tokens, shenv).parse().is_ok()
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
