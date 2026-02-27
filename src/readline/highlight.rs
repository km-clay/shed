use std::{
  env,
  os::unix::fs::PermissionsExt,
  path::{Path, PathBuf},
};

use crate::{
  libsh::term::{Style, StyleSet, Styled},
  readline::{annotate_input, markers::{self, is_marker}},
  state::{read_logic, read_meta, read_shopts},
};

/// Syntax highlighter for shell input using Unicode marker-based annotation
///
/// The highlighter processes annotated input strings containing invisible
/// Unicode markers (U+FDD0-U+FDEF range) that indicate syntax elements. It
/// generates ANSI escape codes for terminal display while maintaining a style
/// stack for proper color restoration in nested constructs (e.g., variables
/// inside strings inside command substitutions).
pub struct Highlighter {
  input: String,
  output: String,
	linebuf_cursor_pos: usize,
  style_stack: Vec<StyleSet>,
  last_was_reset: bool,
	in_selection: bool
}

impl Highlighter {
  /// Creates a new highlighter with empty buffers and reset state
  pub fn new() -> Self {
    Self {
      input: String::new(),
      output: String::new(),
			linebuf_cursor_pos: 0,
      style_stack: Vec::new(),
      last_was_reset: true, // start as true so we don't emit a leading reset
			in_selection: false
    }
  }

  /// Loads raw input text and annotates it with syntax markers
  ///
  /// The input is passed through the annotator which inserts Unicode markers
  /// indicating token types and sub-token constructs (strings, variables, etc.)
  pub fn load_input(&mut self, input: &str, linebuf_cursor_pos: usize) {
    let input = annotate_input(input);
    self.input = input;
		self.linebuf_cursor_pos = linebuf_cursor_pos;
  }

	pub fn strip_markers(str: &str) -> String {
		let mut out = String::new();
		for ch in str.chars() {
			if !is_marker(ch) {
				out.push(ch);
			}
		}
		out
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
				markers::VISUAL_MODE_START => {
					self.emit_style(Style::BgWhite | Style::Black);
					self.in_selection = true;
				}
				markers::VISUAL_MODE_END => {
					self.reapply_style();
					self.in_selection = false;
				}
        markers::STRING_DQ_END
        | markers::STRING_SQ_END
        | markers::VAR_SUB_END
        | markers::CMD_SUB_END
        | markers::PROC_SUB_END
        | markers::SUBSH_END => self.pop_style(),

        markers::CMD_SEP | markers::RESET => self.clear_styles(),

        markers::STRING_DQ | markers::STRING_SQ | markers::KEYWORD => {
          self.push_style(Style::Yellow)
        }
        markers::BUILTIN => {
          let mut cmd_name = String::new();
          let mut chars_clone = input_chars.clone();
          while let Some(ch) = chars_clone.next() {
            if ch == markers::RESET {
              break;
            }
						if !is_marker(ch) {
							cmd_name.push(ch);
						}
          }

					match cmd_name.as_str() {
						"continue" | "return" | "break" => self.push_style(Style::Magenta),
						_ => self.push_style(Style::Green),
					}
				}
        markers::CASE_PAT => self.push_style(Style::Blue),

        markers::COMMENT => self.push_style(Style::BrightBlack),

        markers::GLOB => self.push_style(Style::Blue),

        markers::REDIRECT | markers::OPERATOR => self.push_style(Style::Magenta | Style::Bold),


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

          self.output.push_str(&Self::strip_markers(&var_name));
          self.push_style(Style::Blue);
          self.output.push('=');
          self.pop_style();
        }

        markers::ARG => {
          let mut arg = String::new();
					let is_last_arg = !input_chars.clone().any(|c| c == markers::ARG || c.is_whitespace());

					if !is_last_arg {
						self.push_style(Style::White);
					} else {
						let mut chars_clone = input_chars.clone();
						while let Some(ch) = chars_clone.next() {
							if ch == markers::RESET {
								break;
							}
							arg.push(ch);
						}

						let style = if Self::is_filename(&Self::strip_markers(&arg)) {
							Style::White | Style::Underline
						} else {
							Style::White.into()
						};

						self.push_style(style);
						self.last_was_reset = false;
					}
        }

        markers::COMMAND => {
          let mut cmd_name = String::new();
          let mut chars_clone = input_chars.clone();
          while let Some(ch) = chars_clone.next() {
            if ch == markers::RESET {
              break;
            }
            cmd_name.push(ch);
          }
          let style = if matches!(Self::strip_markers(&cmd_name).as_str(), "break" | "continue" | "return") {
						Style::Magenta.into()
					} else if Self::is_valid(&Self::strip_markers(&cmd_name)) {
            Style::Green.into()
          } else {
            Style::Red | Style::Bold
          };
          self.push_style(style);
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

          let inner_clean = Self::strip_markers(&inner);

          // Determine prefix from content (handles both <( and >( for proc subs)
          let prefix = match ch {
            markers::CMD_SUB => "$(",
            markers::SUBSH => "(",
            markers::PROC_SUB => {
              if inner_clean.starts_with("<(") {
                "<("
              } else if inner_clean.starts_with(">(") {
                ">("
              } else {
                "<("
              } // fallback
            }
            _ => unreachable!(),
          };
          let inner_content = if incomplete {
            inner_clean.strip_prefix(prefix).unwrap_or(&inner_clean)
          } else {
            inner_clean
              .strip_prefix(prefix)
              .and_then(|s| s.strip_suffix(")"))
              .unwrap_or(&inner_clean)
          };

          let mut recursive_highlighter = Self::new();
          recursive_highlighter.load_input(inner_content, self.linebuf_cursor_pos);
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
  /// containing ANSI escape codes. The highlighter is ready for reuse after
  /// this.
  pub fn take(&mut self) -> String {
    self.input.clear();
    self.clear_styles();
    std::mem::take(&mut self.output)
  }

  /// Checks if a command name is valid (exists in PATH, is a function, or is an
  /// alias)
  ///
  /// Searches:
  /// 1. Current directory if command is a path
  /// 2. All directories in PATH environment variable
  /// 3. Shell functions and aliases in the current shell state
  fn is_valid(command: &str) -> bool {
    let cmd_path = Path::new(&command);

    if cmd_path.is_absolute() {
      // the user has given us an absolute path
      if cmd_path.is_dir() && read_shopts(|o| o.core.autocd) {
        // this is a directory and autocd is enabled
        true
      } else {
        let Ok(meta) = cmd_path.metadata() else {
          return false;
        };
        // this is a file that is executable by someone
        meta.permissions().mode() & 0o111 != 0
      }
    } else {
			read_meta(|m| m.cached_cmds().get(command).is_some())
		}
  }

  fn is_filename(arg: &str) -> bool {
    let path = Path::new(arg);

    if path.is_absolute() && path.exists() {
      return true;
    }

		if path.is_absolute()
    && let Some(parent_dir) = path.parent()
		&& let Ok(entries) = parent_dir.read_dir() {
      let files = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect::<Vec<_>>();
      let Some(arg_filename) = PathBuf::from(arg)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
      else {
        return false;
      };
      for file in files {
        if file.starts_with(&arg_filename) {
          return true;
        }
      }
		}

		read_meta(|m| {
			let files = m.cwd_cache();
			for file in files {
				if file.starts_with(arg) {
					return true;
				}
			}
			false
		})
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
  fn emit_style(&mut self, style: StyleSet) {
		let mut style = style;
		if !style.styles().contains(&Style::BgWhite) {
			style = style.add_style(Style::BgBlack);
		}
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
		if !self.in_selection {
			self.emit_style(set.clone());
		}
  }

  /// Pops a style from the stack and restores the previous style
  ///
  /// Used when exiting a syntax context. If there's a parent style on the
  /// stack, it's re-emitted to restore the previous color. Otherwise, emits a
  /// reset. This ensures colors are properly restored in nested constructs
  /// like `"string with $VAR"` where the string color resumes after the
  /// variable.
  pub fn pop_style(&mut self) {
    self.style_stack.pop();
    if let Some(style) = self.style_stack.last().cloned() {
      self.emit_style(style);
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
		if !self.in_selection {
			self.emit_reset();
		}
  }

	pub fn reapply_style(&mut self) {
		if let Some(style) = self.style_stack.last().cloned() {
			self.emit_style(style);
		} else {
			self.emit_reset();
		}
	}

  /// Simple marker-to-ANSI replacement (unused in favor of stack-based
  /// highlighting)
  ///
  /// Performs direct string replacement of markers with ANSI codes, without
  /// handling nesting or proper color restoration. Kept for reference but not
  /// used in the current implementation.
  pub fn trivial_replace(&mut self) {
    self.input = self
      .input
      .replace([markers::RESET, markers::ARG], "\x1b[0m")
      .replace(markers::KEYWORD, "\x1b[33m")
      .replace(markers::CASE_PAT, "\x1b[34m")
      .replace(markers::COMMENT, "\x1b[90m")
      .replace(markers::OPERATOR, "\x1b[35m");
  }
}
