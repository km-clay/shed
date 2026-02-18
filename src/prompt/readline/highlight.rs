use std::{env, path::{Path, PathBuf}};

use crate::{libsh::term::{Style, StyleSet, Styled}, prompt::readline::{annotate_input, markers}, state::read_logic};

pub struct Highlighter {
	input: String,
	output: String,
	style_stack: Vec<StyleSet>,
	last_was_reset: bool,
}

impl Highlighter {
	pub fn new() -> Self {
		Self {
			input: String::new(),
			output: String::new(),
			style_stack: Vec::new(),
			last_was_reset: true, // start as true so we don't emit a leading reset
		}
	}

	pub fn load_input(&mut self, input: &str) {
		let input = annotate_input(input);
		self.input = input;
	}

	pub fn highlight(&mut self) {
		let input = self.input.clone();
		let mut input_chars = input.chars().peekable();
		while let Some(ch) = input_chars.next() {
			match ch {
				markers::STRING_DQ_END |
				markers::STRING_SQ_END |
				markers::VAR_SUB_END |
				markers::CMD_SUB_END |
				markers::PROC_SUB_END |
				markers::SUBSH_END => self.pop_style(),

				markers::CMD_SEP |
				markers::RESET => self.clear_styles(),


				markers::STRING_DQ |
				markers::STRING_SQ |
				markers::KEYWORD => self.push_style(Style::Yellow),
				markers::BUILTIN => self.push_style(Style::Green),
				markers::CASE_PAT => self.push_style(Style::Blue),
				markers::ARG => self.push_style(Style::White),
				markers::COMMENT => self.push_style(Style::BrightBlack),

				markers::GLOB => self.push_style(Style::Blue),

				markers::REDIRECT |
				markers::OPERATOR => self.push_style(Style::Magenta | Style::Bold),

				markers::ASSIGNMENT => {
					let mut var_name = String::new();

					while let Some(ch) = input_chars.peek() {
						if ch == &'=' {
							input_chars.next(); // consume the '='
							break;
						}
						match *ch {
							markers::RESET => break,
							_ => {
								var_name.push(*ch);
								input_chars.next();
							}
						}
					}

					self.output.push_str(&var_name);
					self.push_style(Style::Blue);
					self.output.push('=');
					self.pop_style();
				}

				markers::COMMAND => {
					let mut cmd_name = String::new();
					while let Some(ch) = input_chars.peek() {
						if *ch == markers::RESET {
							break;
						}
						cmd_name.push(*ch);
						input_chars.next();
					}
					let style = if Self::is_valid(&cmd_name) {
						Style::Green.into()
					} else {
						Style::Red | Style::Bold
					};
					self.push_style(style);
					self.output.push_str(&cmd_name);
					self.last_was_reset = false;
				}
				markers::CMD_SUB | markers::SUBSH | markers::PROC_SUB => {
					let mut inner = String::new();
					let mut incomplete = true;
					let end_marker = match ch {
						markers::CMD_SUB => markers::CMD_SUB_END,
						markers::SUBSH => markers::SUBSH_END,
						markers::PROC_SUB => markers::PROC_SUB_END,
						_ => unreachable!(),
					};
					while let Some(ch) = input_chars.peek() {
						if *ch == end_marker {
							incomplete = false;
							input_chars.next(); // consume the end marker
							break;
						}
						inner.push(*ch);
						input_chars.next();
					}

					// Determine prefix from content (handles both <( and >( for proc subs)
					let prefix = match ch {
						markers::CMD_SUB => "$(",
						markers::SUBSH => "(",
						markers::PROC_SUB => {
							if inner.starts_with("<(") { "<(" }
							else if inner.starts_with(">(") { ">(" }
							else { "<(" } // fallback
						}
						_ => unreachable!(),
					};

					let inner_content = if incomplete {
						inner
							.strip_prefix(prefix)
							.unwrap_or(&inner)
					} else {
						inner
							.strip_prefix(prefix)
							.and_then(|s| s.strip_suffix(")"))
							.unwrap_or(&inner)
					};

					let mut recursive_highlighter = Self::new();
					recursive_highlighter.load_input(inner_content);
					recursive_highlighter.highlight();
					self.push_style(Style::Blue);
					self.output.push_str(prefix);
					self.pop_style();
					self.output.push_str(&recursive_highlighter.take());
					if !incomplete {
						self.push_style(Style::Blue);
						self.output.push(')');
						self.pop_style();
					}
					self.last_was_reset = false;
				}
				markers::VAR_SUB => {
					let mut var_sub = String::new();
					while let Some(ch) = input_chars.peek() {
						if *ch == markers::VAR_SUB_END {
							input_chars.next(); // consume the end marker
							break;
						}
						var_sub.push(*ch);
						input_chars.next();
					}
					let style = Style::Cyan;
					self.push_style(style);
					self.output.push_str(&var_sub);
					self.pop_style();
				}
				_ => {
					self.output.push(ch);
					self.last_was_reset = false;
				}
			}
		}
	}

	pub fn take(&mut self) -> String {
		log::info!("Highlighting result: {:?}", self.output);
		self.input.clear();
		self.clear_styles();
		std::mem::take(&mut self.output)
	}

	fn is_valid(command: &str) -> bool {
		let path = env::var("PATH").unwrap_or_default();
		let paths = path.split(':');
		if PathBuf::from(&command).exists() {
			return true;
		} else {
			for path in paths {
				let path = PathBuf::from(path).join(command);
				if path.exists() {
					return true;
				}
			}

			let found = read_logic(|l| l.get_func(command).is_some() || l.get_alias(command).is_some());
			if found {
				return true;
			}
		}

		false
	}

	fn emit_reset(&mut self) {
		if !self.last_was_reset {
			self.output.push_str(&Style::Reset.to_string());
			self.last_was_reset = true;
		}
	}

	fn emit_style(&mut self, style: &StyleSet) {
		self.output.push_str(&style.to_string());
		self.last_was_reset = false;
	}

	pub fn push_style(&mut self, style: impl Into<StyleSet>) {
		let set: StyleSet = style.into();
		self.style_stack.push(set.clone());
		self.emit_style(&set);
	}

	pub fn pop_style(&mut self) {
		self.style_stack.pop();
		if let Some(style) = self.style_stack.last().cloned() {
			self.emit_style(&style);
		} else {
			self.emit_reset();
		}
	}

	pub fn clear_styles(&mut self) {
		self.style_stack.clear();
		self.emit_reset();
	}

	pub fn trivial_replace(&mut self) {
		self.input = self.input
			.replace([markers::RESET, markers::ARG], "\x1b[0m")
			.replace(markers::KEYWORD, "\x1b[33m")
			.replace(markers::CASE_PAT, "\x1b[34m")
			.replace(markers::COMMENT, "\x1b[90m")
			.replace(markers::OPERATOR, "\x1b[35m");
	}
}
