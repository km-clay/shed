use std::{env, path::{Path, PathBuf}};

use crate::{libsh::term::{Style, StyleSet, Styled}, prompt::readline::{annotate_input, markers}, state::read_logic};

/// Syntax highlighter for shell input using Unicode marker-based annotation
///
/// The highlighter processes annotated input strings containing invisible Unicode markers
/// (U+FDD0-U+FDEF range) that indicate syntax elements. It generates ANSI escape codes
/// for terminal display while maintaining a style stack for proper color restoration
/// in nested constructs (e.g., variables inside strings inside command substitutions).
pub struct Highlighter {
	input: String,
	output: String,
	style_stack: Vec<StyleSet>,
	last_was_reset: bool,
}

impl Highlighter {
	/// Creates a new highlighter with empty buffers and reset state
	pub fn new() -> Self {
		Self {
			input: String::new(),
			output: String::new(),
			style_stack: Vec::new(),
			last_was_reset: true, // start as true so we don't emit a leading reset
		}
	}

	/// Loads raw input text and annotates it with syntax markers
	///
	/// The input is passed through the annotator which inserts Unicode markers
	/// indicating token types and sub-token constructs (strings, variables, etc.)
	pub fn load_input(&mut self, input: &str) {
		let input = annotate_input(input);
		self.input = input;
	}

	/// Processes the annotated input and generates ANSI-styled output
	///
	/// Walks through the input character by character, interpreting markers and
	/// applying appropriate styles. Nested constructs (command substitutions,
	/// subshells, strings) are handled recursively with proper style restoration.
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
						} else if markers::is_marker(*ch) {
							input_chars.next(); // skip the marker
							continue;
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
					if markers::is_marker(ch) {
					} else {
						self.output.push(ch);
						self.last_was_reset = false;
					}
				}
			}
		}
	}

	/// Extracts the highlighted output and resets the highlighter state
	///
	/// Clears the input buffer, style stack, and returns the generated output
	/// containing ANSI escape codes. The highlighter is ready for reuse after this.
	pub fn take(&mut self) -> String {
		self.input.clear();
		self.clear_styles();
		std::mem::take(&mut self.output)
	}

	/// Checks if a command name is valid (exists in PATH, is a function, or is an alias)
	///
	/// Searches:
	/// 1. Current directory if command is a path
	/// 2. All directories in PATH environment variable
	/// 3. Shell functions and aliases in the current shell state
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

	/// Emits a reset ANSI code to the output, with deduplication
	///
	/// Only emits the reset if the last emitted code was not already a reset,
	/// preventing redundant `\x1b[0m` sequences in the output.
	fn emit_reset(&mut self) {
		if !self.last_was_reset {
			self.output.push_str(&Style::Reset.to_string());
			self.last_was_reset = true;
		}
	}

	/// Emits a style ANSI code to the output
	///
	/// Unconditionally appends the ANSI escape sequence for the given style
	/// and marks that we're no longer in a reset state.
	fn emit_style(&mut self, style: &StyleSet) {
		self.output.push_str(&style.to_string());
		self.last_was_reset = false;
	}

	/// Pushes a new style onto the stack and emits its ANSI code
	///
	/// Used when entering a new syntax context (string, variable, command, etc.).
	/// The style stack allows proper restoration when exiting nested constructs.
	pub fn push_style(&mut self, style: impl Into<StyleSet>) {
		let set: StyleSet = style.into();
		self.style_stack.push(set.clone());
		self.emit_style(&set);
	}

	/// Pops a style from the stack and restores the previous style
	///
	/// Used when exiting a syntax context. If there's a parent style on the stack,
	/// it's re-emitted to restore the previous color. Otherwise, emits a reset.
	/// This ensures colors are properly restored in nested constructs like
	/// `"string with $VAR"` where the string color resumes after the variable.
	pub fn pop_style(&mut self) {
		self.style_stack.pop();
		if let Some(style) = self.style_stack.last().cloned() {
			self.emit_style(&style);
		} else {
			self.emit_reset();
		}
	}

	/// Clears all styles from the stack and emits a reset
	///
	/// Used at command separators and explicit reset markers to return to
	/// the default terminal color between independent commands.
	pub fn clear_styles(&mut self) {
		self.style_stack.clear();
		self.emit_reset();
	}

	/// Simple marker-to-ANSI replacement (unused in favor of stack-based highlighting)
	///
	/// Performs direct string replacement of markers with ANSI codes, without
	/// handling nesting or proper color restoration. Kept for reference but not
	/// used in the current implementation.
	pub fn trivial_replace(&mut self) {
		self.input = self.input
			.replace([markers::RESET, markers::ARG], "\x1b[0m")
			.replace(markers::KEYWORD, "\x1b[33m")
			.replace(markers::CASE_PAT, "\x1b[34m")
			.replace(markers::COMMENT, "\x1b[90m")
			.replace(markers::OPERATOR, "\x1b[35m");
	}
}
