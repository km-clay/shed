use rustyline::highlight::Highlighter;
use sys::get_bin_path;

use crate::prelude::*;

use super::readline::SynHelper;

impl<'a> Highlighter for SynHelper<'a> {
	fn highlight<'l>(&self, line: &'l str, pos: usize) -> std::borrow::Cow<'l, str> {
		let mut result = String::new();
		let mut tokens = Lexer::new(Rc::new(line.to_string())).lex().into_iter();
		let mut is_command = true;
		let mut in_array = false;

		while let Some(token) = tokens.next() {
			let raw = token.to_string();
			match token.rule() {
				TkRule::Comment => {
					let styled = style_text(&raw, Style::BrightBlack);
					result.push_str(&styled);
				}
				TkRule::ErrPipeOp |
				TkRule::OrOp |
				TkRule::AndOp |
				TkRule::PipeOp |
				TkRule::RedirOp |
				TkRule::BgOp => {
					is_command = true;
					let styled = style_text(&raw, Style::Cyan);
					result.push_str(&styled);
				}
				TkRule::Keyword => {
					if &raw == "for" {
						in_array = true;
					}
					let styled = style_text(&raw, Style::Yellow);
					result.push_str(&styled);
				}
				TkRule::Subshell => {
					let body = &raw[1..raw.len() - 1];
					let highlighted = self.highlight(body, 0).to_string();
					let styled_o_paren = style_text("(", Style::BrightBlue);
					let styled_c_paren = style_text(")", Style::BrightBlue);
					let rebuilt = format!("{styled_o_paren}{highlighted}{styled_c_paren}");
					is_command = false;
					result.push_str(&rebuilt);
				}
				TkRule::Ident => {
					if in_array {
						if &raw == "in" {
							let styled = style_text(&raw, Style::Yellow);
							result.push_str(&styled);
						} else {
							let styled = style_text(&raw, Style::Magenta);
							result.push_str(&styled);
						}
					} else if is_command {
						if get_bin_path(&token.to_string(), self.shenv).is_some() ||
						self.shenv.logic().get_alias(&raw).is_some() ||
						self.shenv.logic().get_function(&raw).is_some() ||
						BUILTINS.contains(&raw.as_str()) {
							let styled = style_text(&raw, Style::Green);
							result.push_str(&styled);
						} else {
							let styled = style_text(&raw, Style::Red | Style::Bold);
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
