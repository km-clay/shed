use std::{env, os::unix::fs::PermissionsExt, path::{Path, PathBuf}, sync::Arc};
use crate::{builtin::BUILTINS, prelude::*};

use rustyline::highlight::Highlighter;
use crate::{libsh::term::{Style, StyleSet, Styled}, parse::lex::{LexFlags, LexStream, Tk, TkFlags, TkRule}, state::read_logic};

use super::readline::FernReadline;

fn is_executable(path: &Path) -> bool {
	path.metadata()
		.map(|m| m.permissions().mode() & 0o111 != 0)
		.unwrap_or(false)
}

#[derive(Default,Debug)]
pub struct FernHighlighter {
	input: String,
}

impl FernHighlighter {
	pub fn new(input: String) -> Self {
		Self {
			input,
		}
	}
	pub fn highlight_subsh(&self, token: Tk) -> String {
		if token.flags.contains(TkFlags::IS_SUBSH) {
			let raw = token.as_str();
			let body = &raw[1..raw.len() - 1];
			let sub_hl = FernHighlighter::new(body.to_string());
			let body_highlighted = sub_hl.hl_input();
			let open_paren = "(".styled(Style::BrightBlue);
			let close_paren = ")".styled(Style::BrightBlue);
			format!("{open_paren}{body_highlighted}{close_paren}")
		} else if token.flags.contains(TkFlags::IS_CMDSUB) {
			let raw = token.as_str();
			let body = &raw[2..raw.len() - 1];
			let sub_hl = FernHighlighter::new(body.to_string());
			let body_highlighted = sub_hl.hl_input();
			let dollar_paren = "$(".styled(Style::BrightBlue);
			let close_paren = ")".styled(Style::BrightBlue);
			format!("{dollar_paren}{body_highlighted}{close_paren}")
		} else {
			unreachable!()
		}
	}
	pub fn hl_command(&self, token: Tk) -> String {
		let raw = token.as_str();
		let paths = env::var("PATH")
			.unwrap_or_default();
		let mut paths = paths.split(':');

		let is_in_path = {
			loop {
				let Some(path) = paths.next() else {
					break false
				};

				let mut path = PathBuf::from(path);
				path.push(PathBuf::from(raw));

				if path.is_file() && is_executable(&path) {
					break true
				};
			}
		};
		// TODO: zsh is capable of highlighting an alias red even if it exists, if the command it refers to is not found
		// Implement some way to find out if the content of the alias is valid as well
		let is_alias_or_function = read_logic(|l| {
			l.get_func(raw).is_some() || l.get_alias(raw).is_some()
		});

		let is_builtin = BUILTINS.contains(&raw);

		if is_alias_or_function || is_in_path || is_builtin {
			raw.styled(Style::Green)
		} else {
			raw.styled(Style::Bold | Style::Red)
		}
	}
	pub fn hl_input(&self) -> String {
		let mut output = self.input.clone();

		// TODO: properly implement highlighting for unfinished input
		let lex_results = LexStream::new(Arc::new(output.clone()), LexFlags::empty());
		let mut tokens = vec![];

		for result in lex_results {
			let Ok(token) = result else {
				return self.input.clone();
			};
			tokens.push(token)
		}

		// Reverse the tokens, because we want to highlight from right to left
		// Doing it this way allows us to trust the spans in the tokens throughout the entire process
		let tokens = tokens.into_iter()
			.rev()
			.collect::<Vec<Tk>>();
		for token in tokens {
			flog!(DEBUG, token.flags);
			match token.class {
				_ if token.flags.intersects(TkFlags::IS_CMDSUB | TkFlags::IS_SUBSH) => {
					let styled = self.highlight_subsh(token.clone());
					output.replace_range(token.span.start..token.span.end, &styled);
				}
				TkRule::Str => {
					if token.flags.contains(TkFlags::IS_CMD) {
						let styled = self.hl_command(token.clone());
						output.replace_range(token.span.start..token.span.end, &styled);
					} else {
						output.replace_range(token.span.start..token.span.end, &token.to_string());
					}
				}
				TkRule::Pipe |
				TkRule::ErrPipe |
				TkRule::And |
				TkRule::Or |
				TkRule::Bg |
				TkRule::Sep |
				TkRule::Redir => self.style_with_token(&token,&mut output,Style::Cyan.into()),
				TkRule::CasePattern => self.style_with_token(&token,&mut output,Style::Blue.into()),
				TkRule::BraceGrpStart |
				TkRule::BraceGrpEnd => self.style_with_token(&token,&mut output,Style::Cyan.into()),
				TkRule::Comment => self.style_with_token(&token,&mut output,Style::BrightBlack.into()),
				_ => { output.replace_range(token.span.start..token.span.end, &token.to_string()); }
			}
		}

		output
	}
	fn style_with_token(&self, token: &Tk, highlighted: &mut String, style: StyleSet) {
		let styled = token.to_string().styled(style);
		highlighted.replace_range(token.span.start..token.span.end, &styled);
	}
}

impl Highlighter for FernReadline {
	fn highlight<'l>(&self, line: &'l str, _pos: usize) -> std::borrow::Cow<'l, str> {
		let highlighter = FernHighlighter::new(line.to_string());
		std::borrow::Cow::Owned(highlighter.hl_input())
	}
	fn highlight_char(&self, _line: &str, _pos: usize, _kind: rustyline::highlight::CmdKind) -> bool {
		true
	}
}
