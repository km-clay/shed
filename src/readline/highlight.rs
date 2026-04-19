use std::{
  os::unix::fs::PermissionsExt,
  path::{Path, PathBuf},
};

use crate::{
  libsh::{error::ShResult, term::color_from_description},
  match_loop,
  readline::{
    annotate_input,
    markers::{self, is_marker},
  },
  state::{read_meta, read_shopts, write_meta},
};

fn resolve_style(raw: &str) -> ShResult<String> {
  if raw.starts_with("\\e") {
    Ok(raw.replace("\\e", "\x1b"))
  } else {
    color_from_description(raw)
  }
}

pub fn string_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.string.clone()))
}

pub fn keyword_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.keyword.clone()))
}

pub fn valid_command_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.valid_command.clone()))
}

pub fn invalid_command_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.invalid_command.clone()))
}

pub fn control_flow_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.control_flow_keyword.clone()))
}

pub fn argument_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.argument.clone()))
}

pub fn argument_file_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.argument_file.clone()))
}

pub fn variable_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.variable.clone()))
}

pub fn operator_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.operator.clone()))
}

pub fn comment_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.comment.clone()))
}

pub fn glob_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.glob.clone()))
}

pub fn selection_style() -> ShResult<String> {
  resolve_style(&read_shopts(|o| o.highlight.selection.clone()))
}

pub struct HighlightTheme {
  pub string: String,
  pub keyword: String,
  pub valid_command: String,
  pub invalid_command: String,
  pub control_flow: String,
  pub argument: String,
  pub argument_file: String,
  pub variable: String,
  pub operator: String,
  pub comment: String,
  pub glob: String,
  pub selection: String,
}

impl Default for HighlightTheme {
  fn default() -> Self {
    Self {
      string: resolve_style("yellow").unwrap(),
      keyword: resolve_style("yellow").unwrap(),
      valid_command: resolve_style("green").unwrap(),
      invalid_command: resolve_style("bold red").unwrap(),
      control_flow: resolve_style("magenta").unwrap(),
      argument: resolve_style("white").unwrap(),
      argument_file: resolve_style("underline white").unwrap(),
      variable: resolve_style("cyan").unwrap(),
      operator: resolve_style("bold").unwrap(),
      comment: resolve_style("italic bright black").unwrap(),
      glob: resolve_style("bright cyan").unwrap(),
      selection: resolve_style("black on white").unwrap(),
    }
  }
}

impl HighlightTheme {
  pub fn resolve() -> Self {
    let fallback = Self::default();
    let mut errors = vec![];

    let try_or = |f: fn() -> ShResult<String>, default: &str, errors: &mut Vec<String>| -> String {
      match f() {
        Ok(s) => s,
        Err(e) => {
          errors.push(e.to_string());
          default.to_string()
        }
      }
    };

    let theme = Self {
      string: try_or(string_style, &fallback.string, &mut errors),
      keyword: try_or(keyword_style, &fallback.keyword, &mut errors),
      valid_command: try_or(valid_command_style, &fallback.valid_command, &mut errors),
      invalid_command: try_or(
        invalid_command_style,
        &fallback.invalid_command,
        &mut errors,
      ),
      control_flow: try_or(control_flow_style, &fallback.control_flow, &mut errors),
      argument: try_or(argument_style, &fallback.argument, &mut errors),
      argument_file: try_or(argument_file_style, &fallback.argument_file, &mut errors),
      variable: try_or(variable_style, &fallback.variable, &mut errors),
      operator: try_or(operator_style, &fallback.operator, &mut errors),
      comment: try_or(comment_style, &fallback.comment, &mut errors),
      glob: try_or(glob_style, &fallback.glob, &mut errors),
      selection: try_or(selection_style, &fallback.selection, &mut errors),
    };

    for err in errors {
      write_meta(|m| m.post_status_message(err));
    }

    theme
  }
}

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
  style_stack: Vec<String>,
  last_was_reset: bool,
  in_selection: bool,
  only_hl_visual: bool,
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
      in_selection: false,
      only_hl_visual: false,
    }
  }
  pub fn only_visual(&mut self, only_visual: bool) {
    self.only_hl_visual = only_visual;
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

  pub fn strip_markers_keep_visual(str: &str) -> String {
    let mut out = String::new();
    for ch in str.chars() {
      if ch == markers::VISUAL_MODE_START || ch == markers::VISUAL_MODE_END {
        out.push(ch); // preserve visual markers
      } else if !is_marker(ch) {
        out.push(ch);
      }
    }
    out
  }

  /// Strip a prefix from a string, skipping over visual markers during matching.
  /// Visual markers that appear after the prefix are preserved in the result.
  fn strip_prefix_skip_visual(text: &str, prefix: &str) -> String {
    let mut chars = text.chars();
    let mut prefix_chars = prefix.chars().peekable();

    // Walk through text, matching prefix chars while skipping visual markers
    while prefix_chars.peek().is_some() {
      match chars.next() {
        Some(c) if c == markers::VISUAL_MODE_START || c == markers::VISUAL_MODE_END => continue,
        Some(c) if Some(&c) == prefix_chars.peek() => {
          prefix_chars.next();
        }
        _ => return text.to_string(), // mismatch, return original
      }
    }
    // Remaining chars (including any visual markers) form the result
    chars.collect()
  }

  /// Strip a suffix from a string, skipping over visual markers during matching.
  fn strip_suffix_skip_visual(text: &str, suffix: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let suffix_chars: Vec<char> = suffix.chars().collect();
    let mut ti = chars.len();
    let mut si = suffix_chars.len();

    while si > 0 {
      if ti == 0 {
        return text.to_string();
      }
      ti -= 1;
      if chars[ti] == markers::VISUAL_MODE_START || chars[ti] == markers::VISUAL_MODE_END {
        continue; // skip visual markers
      }
      si -= 1;
      if chars[ti] != suffix_chars[si] {
        return text.to_string(); // mismatch
      }
    }
    chars[..ti].iter().collect()
  }

  /// Expands control characters in the input to visible representations
  ///
  /// Operates on chars in the range 0x00..0x1F, replacing them with caret notation (e.g., `^A` for 0x01)
  /// Newline (`'\n'`), tab (`'\t'`), and carriage return (`'\r'`) are preserved as-is. This allows control characters to be visible in the highlighted output.
  pub fn expand_control_chars(&mut self) {
    let mut expanded = String::new();
    let mut chars = self.input.chars().peekable();

    match_loop!(chars.next() => ch, {
      '\n' | '\t' | '\r' => expanded.push(ch),
      c if c as u32 <= 0x1F => {
        let display = (c as u8 + b'@') as char;
        expanded.push_str("\x1b[7m^");
        expanded.push(display);
        expanded.push_str("\x1b[0m");
      }
      _ => expanded.push(ch),
    });

    self.input = expanded;
  }

  /// Processes the annotated input and generates ANSI-styled output
  ///
  /// Walks through the input character by character, interpreting markers and
  /// applying appropriate styles. Nested constructs (command substitutions,
  /// subshells, strings) are handled recursively with proper style restoration.
  pub fn highlight(&mut self) {
    let theme = HighlightTheme::resolve();
    let input = self.input.clone();
    let mut input_chars = input.chars().peekable();
    match_loop!(input_chars.next() => ch, {
      markers::VISUAL_MODE_START => {
        self.emit_style(&theme.selection);
        self.in_selection = true;
      }
      markers::VISUAL_MODE_END => {
        self.reapply_style();
        self.in_selection = false;
      }
      _ if self.only_hl_visual => {
        if !is_marker(ch) {
          self.output.push(ch);
        }
      }
      markers::STRING_DQ_END
        | markers::STRING_SQ_END
        | markers::VAR_SUB_END
        | markers::CMD_SUB_END
        | markers::PROC_SUB_END
        | markers::SUBSH_END
        | markers::HIST_EXP_END => self.pop_style(),

        markers::CMD_SEP | markers::RESET => self.clear_styles(),

        markers::STRING_DQ | markers::STRING_SQ => {
          self.push_style(&theme.string);
        }
        markers::KEYWORD => {
          self.push_style(&theme.keyword);
        }
      markers::BUILTIN => {
        let mut cmd_name = String::new();
        let mut chars_clone = input_chars.clone();
        match_loop!(chars_clone.next() => ch, {
          markers::RESET => break,
          _ if !is_marker(ch) => cmd_name.push(ch),
          _ => {}
        });

        match cmd_name.as_str() {
          "continue" | "return" | "break" => self.push_style(&theme.control_flow),
          _ => self.push_style(&theme.valid_command),
        }
      }
      markers::CASE_PAT => self.push_style(&theme.glob),

      markers::COMMENT => self.push_style(&theme.comment),

      markers::GLOB => self.push_style(&theme.glob),

      markers::REDIRECT | markers::OPERATOR => self.push_style(&theme.operator),

      markers::ASSIGNMENT => {
        let mut var_name = String::new();

        match_loop!(input_chars.peek() => &ch => ch, {
          _ if ch == '=' => {
            input_chars.next(); // consume the '='
            break;
          }
          markers::RESET => break,
          markers::VISUAL_MODE_START => {
            self.emit_style(&theme.selection);
            self.in_selection = true;
            input_chars.next();
          }
          markers::VISUAL_MODE_END => {
            self.reapply_style();
            self.in_selection = false;
            input_chars.next();
          }
          _ => {
            var_name.push(ch);
            input_chars.next();
          }
        });

        self.output.push_str(&Self::strip_markers(&var_name));
        self.push_style(&theme.variable);
        self.output.push('=');
        self.pop_style();
      }

      markers::ARG => {
        let mut arg = String::new();
        let is_last_arg = !input_chars
          .clone()
          .any(|c| c == markers::ARG || c.is_whitespace());

        if !is_last_arg {
          self.push_style(&theme.argument);
        } else {
          let mut chars_clone = input_chars.clone();
          match_loop!(chars_clone.next() => ch, {
            markers::RESET => break,
            _ => arg.push(ch)
          });

          let style = if Self::is_filename(&Self::strip_markers(&arg)) {
            &theme.argument_file
          } else {
            &theme.argument
          };

          self.push_style(style);
          self.last_was_reset = false;
        }
      }

      markers::COMMAND => {
        let mut cmd_name = String::new();
        let mut chars_clone = input_chars.clone();
        match_loop!(chars_clone.next() => ch, {
          markers::RESET => break,
          _ => cmd_name.push(ch)
        });
        let style = if matches!(
          Self::strip_markers(&cmd_name).as_str(),
          "break" | "continue" | "return"
        ) {
          &theme.control_flow
        } else if Self::is_valid(&Self::strip_markers(&cmd_name)) {
          &theme.valid_command
        } else {
          &theme.invalid_command
        };
        self.push_style(style);
        self.last_was_reset = false;
      }
      markers::CMD_SUB | markers::SUBSH | markers::PROC_SUB | markers::BACKTICK_SUB => {
        let mut inner = String::new();
        let mut incomplete = true;
        let end_marker = match ch {
          markers::CMD_SUB => markers::CMD_SUB_END,
          markers::SUBSH => markers::SUBSH_END,
          markers::PROC_SUB => markers::PROC_SUB_END,
          markers::BACKTICK_SUB => markers::BACKTICK_SUB_END,
          _ => unreachable!(),
        };
        let selection_at_entry = self.in_selection;
        match_loop!(input_chars.peek() => &ch => ch, {
          _ if ch == end_marker => {
            incomplete = false;
            input_chars.next();
            break;
          }
          m @ (markers::VISUAL_MODE_START | markers::VISUAL_MODE_END) => {
            self.in_selection = m == markers::VISUAL_MODE_START;
            inner.push(m);
            input_chars.next();
          }
          _ => {
            inner.push(ch);
            input_chars.next();
          }
        });

        let inner_clean = Self::strip_markers_keep_visual(&inner);
        let inner_plain = Self::strip_markers(&inner);

        let prefix = match ch {
          markers::BACKTICK_SUB => "`",
          markers::CMD_SUB => "$(",
          markers::SUBSH => "(",
          markers::PROC_SUB => {
            if inner_plain.starts_with("<(") {
              "<("
            } else if inner_plain.starts_with(">(") {
              ">("
            } else {
              "<("
            }
          }
          _ => unreachable!(),
        };

        let inner_content = if incomplete {
          Self::strip_prefix_skip_visual(&inner_clean, prefix)
        } else {
          let stripped = Self::strip_prefix_skip_visual(&inner_clean, prefix);
          Self::strip_suffix_skip_visual(&stripped, ")")
        };

        let mut recursive_highlighter = Self::new();
        recursive_highlighter.in_selection = selection_at_entry;
        if recursive_highlighter.in_selection {
          recursive_highlighter.emit_style(&theme.selection);
        }
        recursive_highlighter.load_input(&inner_content, self.linebuf_cursor_pos);
        recursive_highlighter.highlight();
        self.in_selection = recursive_highlighter.in_selection;
        self
          .style_stack
          .append(&mut recursive_highlighter.style_stack);
        if selection_at_entry {
          self.emit_style(&theme.selection);
          self.output.push_str(prefix);
        } else {
          self.push_style(&theme.operator);
          self.output.push_str(prefix);
          self.pop_style();
        }
        self.output.push_str(&recursive_highlighter.take());
        if !incomplete {
          self.push_style(&theme.operator);
          if ch != markers::BACKTICK_SUB {
            self.output.push(')');
          }
          self.pop_style();
        }
        self.last_was_reset = false;
      }
      markers::HIST_EXP => {
        let mut hist_exp = String::new();
        match_loop!(input_chars.peek() => &ch => ch, {
          markers::HIST_EXP_END => {
            input_chars.next();
            break;
          }
          markers::VISUAL_MODE_START => {
            self.emit_style(&theme.selection);
            self.in_selection = true;
            input_chars.next();
          }
          markers::VISUAL_MODE_END => {
            self.reapply_style();
            self.in_selection = false;
            input_chars.next();
          }
          _ if markers::is_marker(ch) => {
            input_chars.next();
          }
          _ => {
            hist_exp.push(ch);
            input_chars.next();
          }
        });
        self.push_style(&theme.variable);
        self.output.push_str(&hist_exp);
        self.pop_style();
      }
      markers::VAR_SUB => {
        let mut var_sub = String::new();
        match_loop!(input_chars.peek() => &ch => ch, {
          markers::HIST_EXP_END => {
            input_chars.next();
            break;
          }
          markers::VAR_SUB_END => {
            input_chars.next();
            break;
          }
          markers::VISUAL_MODE_START => {
            self.emit_style(&theme.selection);
            self.in_selection = true;
            input_chars.next();
          }
          markers::VISUAL_MODE_END => {
            self.reapply_style();
            self.in_selection = false;
            input_chars.next();
          }
          _ if markers::is_marker(ch) => {
            input_chars.next();
          }
          _ => {
            var_sub.push(ch);
            input_chars.next();
          }
        });
        self.push_style(&theme.variable);
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
    });
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

    if cmd_path.is_dir() && read_shopts(|o| o.core.autocd) {
      // this is a directory and autocd is enabled
      return true;
    }

    if cmd_path.is_absolute() {
      // the user has given us an absolute path
      let Ok(meta) = cmd_path.metadata() else {
        return false;
      };
      // this is a file that is executable by someone
      meta.permissions().mode() & 0o111 != 0
    } else {
      read_meta(|m| m.cache_contains(command))
    }
  }

  fn is_filename(arg: &str) -> bool {
    let path = Path::new(arg);

    if path.is_absolute() && path.exists() {
      return true;
    }

    if path.is_absolute()
      && let Some(parent_dir) = path.parent()
      && let Ok(entries) = parent_dir.read_dir()
    {
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
      let files = m.cached_files();
      for file in files {
        if file.name().starts_with(arg) {
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
      self.output.push_str("\x1b[0m");
      self.last_was_reset = true;
    }
  }

  fn emit_style(&mut self, style: &str) {
    self.output.push_str(style);
    self.last_was_reset = false;
  }

  pub fn push_style(&mut self, style: &str) {
    self.style_stack.push(style.to_string());
    if !self.in_selection {
      self.emit_style(style);
    }
  }

  pub fn pop_style(&mut self) {
    self.style_stack.pop();
    if !self.in_selection {
      if let Some(style) = self.style_stack.last().cloned() {
        self.emit_style(&style);
      } else {
        self.emit_reset();
      }
    }
  }

  pub fn clear_styles(&mut self) {
    self.style_stack.clear();
    if !self.in_selection {
      self.emit_reset();
    }
  }

  pub fn reapply_style(&mut self) {
    if let Some(style) = self.style_stack.last().cloned() {
      self.emit_style(&style);
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

impl Default for Highlighter {
  fn default() -> Self {
    Self::new()
  }
}
