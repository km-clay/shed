use rustyline::highlight::Highlighter;
use sys::get_bin_path;

use crate::{parse::lex::KEYWORDS, prelude::*};

use super::readline::SynHelper;

impl<'a> Highlighter for SynHelper<'a> {
	fn highlight<'l>(&self, line: &'l str, pos: usize) -> std::borrow::Cow<'l, str> {
		let mut shenv_clone = self.shenv.clone();
		shenv_clone.new_input(line);

		let mut result = String::new();
		let mut tokens = Lexer::new(line.to_string(),&mut shenv_clone).lex().into_iter();
		let mut is_command = true;
		let mut in_array = false;
		let mut in_case = false;

		while let Some(token) = tokens.next() {
			let raw = token.as_raw(&mut shenv_clone);
			match token.rule() {
				TkRule::Comment => {
					let styled = &raw.styled(Style::BrightBlack);
					result.push_str(&styled);
				}
				TkRule::ErrPipeOp |
				TkRule::OrOp |
				TkRule::AndOp |
				TkRule::PipeOp |
				TkRule::RedirOp |
				TkRule::BgOp => {
					is_command = true;
					let styled = &raw.styled(Style::Cyan);
					result.push_str(&styled);
				}
				TkRule::CasePat => {
					let pat = raw.trim_end_matches(')');
					let len_delta = raw.len().saturating_sub(pat.len());
					let parens = ")".repeat(len_delta);
					let styled = pat.styled(Style::Magenta);
					let rebuilt = format!("{styled}{parens}");
					result.push_str(&rebuilt);
				}
				TkRule::FuncName => {
					let name = raw.strip_suffix("()").unwrap_or(&raw);
					let styled = name.styled(Style::Cyan);
					let rebuilt = format!("{styled}()");
					result.push_str(&rebuilt);
				}
				TkRule::DQuote | TkRule::SQuote => {
					let styled = raw.styled(Style::BrightYellow);
					result.push_str(&styled);
				}
				_ if KEYWORDS.contains(&token.rule()) => {
					if in_array || in_case {
						if &raw == "in" {
							let styled = &raw.styled(Style::Yellow);
							result.push_str(&styled);
							if in_case { in_case = false };
						} else {
							let styled = &raw.styled(Style::Magenta);
							result.push_str(&styled);
						}
					} else {
						if &raw == "for" {
							in_array = true;
						}
						if &raw == "case" {
							in_case = true;
						}
						let styled = &raw.styled(Style::Yellow);
						result.push_str(&styled);
					}
				}
				TkRule::BraceGrp => {
					let body = &raw[1..raw.len() - 1];
					let highlighted = self.highlight(body, 0).to_string();
					let styled_o_brace = "{".styled(Style::BrightBlue);
					let styled_c_brace = "}".styled(Style::BrightBlue);
					let rebuilt = format!("{styled_o_brace}{highlighted}{styled_c_brace}");

					is_command = false;
					result.push_str(&rebuilt);
				}
				TkRule::CmdSub => {
					let body = &raw[2..raw.len() - 1];
					let highlighted = self.highlight(body, 0).to_string();
					let styled_o_paren = "$(".styled(Style::BrightBlue);
					let styled_c_paren = ")".styled(Style::BrightBlue);
					let rebuilt = format!("{styled_o_paren}{highlighted}{styled_c_paren}");

					is_command = false;
					result.push_str(&rebuilt);
				}
				TkRule::Subshell => {
					let body = &raw[1..raw.len() - 1];
					let highlighted = self.highlight(body, 0).to_string();
					let styled_o_paren = "(".styled(Style::BrightBlue);
					let styled_c_paren = ")".styled(Style::BrightBlue);
					let rebuilt = format!("{styled_o_paren}{highlighted}{styled_c_paren}");

					is_command = false;
					result.push_str(&rebuilt);
				}
				TkRule::VarSub => {
					let styled = raw.styled(Style::Magenta);
					result.push_str(&styled);
				}
				TkRule::Ident => {
					if in_array || in_case {
						if &raw == "in" {
							let styled = &raw.styled(Style::Yellow);
							result.push_str(&styled);
							if in_case { in_case = false };
						} else {
							let styled = &raw.styled(Style::Magenta);
							result.push_str(&styled);
						}
					} else if let Some((var,val)) = raw.split_once('=') {
						let var_styled = var.styled(Style::Magenta);
						let val_styled = val.styled(Style::Cyan);
						let rebuilt = vec![var_styled,val_styled].join("=");
						result.push_str(&rebuilt);
					} else if raw.starts_with(['"','\'']) {
						let styled = &raw.styled(Style::BrightYellow);
						result.push_str(&styled);
					} else if &raw == "{" || &raw == "}" {
						result.push_str(&raw);

					} else if is_command {
						if get_bin_path(&token.as_raw(&mut shenv_clone), self.shenv).is_some() ||
						self.shenv.logic().get_alias(&raw).is_some() ||
						self.shenv.logic().get_function(&raw).is_some() ||
						BUILTINS.contains(&raw.as_str()) {
							let styled = &raw.styled(Style::Green);
							result.push_str(&styled);

						} else {
							let styled = &raw.styled(Style::Red | Style::Bold);
							result.push_str(&styled);
						}

						is_command = false;

					} else {
						result.push_str(&raw);
					}
				}
				TkRule::Sep => {
					is_command = true;
					in_array = false;
					result.push_str(&raw);
				}
				_ => {
					result.push_str(&raw);
				}
			}
		}

		std::borrow::Cow::Owned(result)
	}

	fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
		&'s self,
		prompt: &'p str,
		default: bool,
	) -> std::borrow::Cow<'b, str> {
		let _ = default;
		std::borrow::Cow::Borrowed(prompt)
	}

	fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
		std::borrow::Cow::Borrowed(hint)
	}

	fn highlight_candidate<'c>(
		&self,
		candidate: &'c str, // FIXME should be Completer::Candidate
		completion: rustyline::CompletionType,
	) -> std::borrow::Cow<'c, str> {
		let _ = completion;
		std::borrow::Cow::Borrowed(candidate)
	}

	fn highlight_char(&self, line: &str, pos: usize, kind: rustyline::highlight::CmdKind) -> bool {
		let _ = (line, pos, kind);
		true
	}
}
