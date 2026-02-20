use std::{env, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc};

use crate::{
  builtin::BUILTINS,
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::lex::{self, LexFlags, Tk, TkFlags},
  prompt::readline::{
    Marker, annotate_input, annotate_input_recursive, get_insertions,
    markers::{self, is_marker},
  },
  state::{read_logic, read_vars},
};

pub enum CompCtx {
  CmdName,
  FileName,
}

pub enum CompResult {
  NoMatch,
  Single { result: String },
  Many { candidates: Vec<String> },
}

impl CompResult {
  pub fn from_candidates(candidates: Vec<String>) -> Self {
    if candidates.is_empty() {
      Self::NoMatch
    } else if candidates.len() == 1 {
      Self::Single {
        result: candidates[0].clone(),
      }
    } else {
      Self::Many { candidates }
    }
  }
}

pub struct Completer {
  pub candidates: Vec<String>,
  pub selected_idx: usize,
  pub original_input: String,
  pub token_span: (usize, usize),
  pub active: bool,
}

impl Completer {
  pub fn new() -> Self {
    Self {
      candidates: vec![],
      selected_idx: 0,
      original_input: String::new(),
      token_span: (0, 0),
      active: false,
    }
  }

  pub fn slice_line(line: &str, cursor_pos: usize) -> (&str, &str) {
    let (before_cursor, after_cursor) = line.split_at(cursor_pos);
    (before_cursor, after_cursor)
  }

  pub fn get_completion_context(&self, line: &str, cursor_pos: usize) -> (Vec<Marker>, usize) {
    let annotated = annotate_input_recursive(line);
    let mut ctx = vec![markers::NULL];
    let mut last_priority = 0;
    let mut ctx_start = 0;
    let mut pos = 0;

    for ch in annotated.chars() {
      match ch {
        _ if is_marker(ch) => match ch {
          markers::COMMAND | markers::BUILTIN => {
            if last_priority < 2 {
              if last_priority > 0 {
                ctx.pop();
              }
              ctx_start = pos;
              last_priority = 2;
              ctx.push(markers::COMMAND);
            }
          }
          markers::VAR_SUB => {
            if last_priority < 3 {
              if last_priority > 0 {
                ctx.pop();
              }
              ctx_start = pos;
              last_priority = 3;
              ctx.push(markers::VAR_SUB);
            }
          }
          markers::ARG | markers::ASSIGNMENT => {
            if last_priority < 1 {
              ctx_start = pos;
              ctx.push(markers::ARG);
            }
          }
          _ => {}
        },
        _ => {
          last_priority = 0; // reset priority on normal characters
          pos += 1; // we hit a normal character, advance our position
          if pos >= cursor_pos {
            break;
          }
        }
      }
    }

    (ctx, ctx_start)
  }

  pub fn reset(&mut self) {
    self.candidates.clear();
    self.selected_idx = 0;
    self.original_input.clear();
    self.token_span = (0, 0);
    self.active = false;
  }

  pub fn complete(
    &mut self,
    line: String,
    cursor_pos: usize,
    direction: i32,
  ) -> ShResult<Option<String>> {
    if self.active {
      Ok(Some(self.cycle_completion(direction)))
    } else {
      self.start_completion(line, cursor_pos)
    }
  }

  pub fn selected_candidate(&self) -> Option<String> {
    self.candidates.get(self.selected_idx).cloned()
  }

  pub fn cycle_completion(&mut self, direction: i32) -> String {
    if self.candidates.is_empty() {
      return self.original_input.clone();
    }

    let len = self.candidates.len();
    self.selected_idx = (self.selected_idx as i32 + direction).rem_euclid(len as i32) as usize;

    self.get_completed_line()
  }

  pub fn start_completion(&mut self, line: String, cursor_pos: usize) -> ShResult<Option<String>> {
    let result = self.get_candidates(line.clone(), cursor_pos)?;
    match result {
      CompResult::Many { candidates } => {
        self.candidates = candidates.clone();
        self.selected_idx = 0;
        self.original_input = line;
        self.active = true;

        Ok(Some(self.get_completed_line()))
      }
      CompResult::Single { result } => {
        self.candidates = vec![result.clone()];
        self.selected_idx = 0;
        self.original_input = line;
        self.active = false;

        Ok(Some(self.get_completed_line()))
      }
      CompResult::NoMatch => Ok(None),
    }
  }

  pub fn extract_var_name(text: &str) -> Option<(String, usize, usize)> {
    let mut chars = text.chars().peekable();
    let mut name = String::new();
    let mut reading_name = false;
    let mut pos = 0;
    let mut name_start = 0;
    let mut name_end = 0;

    while let Some(ch) = chars.next() {
      match ch {
        '$' => {
          if chars.peek() == Some(&'{') {
            continue;
          }

          reading_name = true;
          name_start = pos + 1; // Start after the '$'
        }
        '{' if !reading_name => {
          reading_name = true;
          name_start = pos + 1;
        }
        ch if ch.is_alphanumeric() || ch == '_' => {
          if reading_name {
            name.push(ch);
          }
        }
        _ => {
          if reading_name {
            name_end = pos; // End before the non-alphanumeric character
            break;
          }
        }
      }
      pos += 1;
    }

    if !reading_name {
      return None;
    }

    if name_end == 0 {
      name_end = pos;
    }

    Some((name, name_start, name_end))
  }

  pub fn get_completed_line(&self) -> String {
    if self.candidates.is_empty() {
      return self.original_input.clone();
    }

    let selected = &self.candidates[self.selected_idx];
    let (start, end) = self.token_span;
    format!(
      "{}{}{}",
      &self.original_input[..start],
      selected,
      &self.original_input[end..]
    )
  }

  pub fn get_candidates(&mut self, line: String, cursor_pos: usize) -> ShResult<CompResult> {
    let source = Arc::new(line.clone());
    let tokens =
      lex::LexStream::new(source, LexFlags::LEX_UNFINISHED).collect::<ShResult<Vec<Tk>>>()?;

    let Some(mut cur_token) = tokens.into_iter().find(|tk| {
      let start = tk.span.start;
      let end = tk.span.end;
      (start..=end).contains(&cursor_pos)
    }) else {
      let candidates = Self::complete_filename("./"); // Default to filename completion if no token is found
      let end_pos = line.len();
      self.token_span = (end_pos, end_pos);
      return Ok(CompResult::from_candidates(candidates));
    };

    self.token_span = (cur_token.span.start, cur_token.span.end);

    // Look for marker at the START of what we're completing, not at cursor
    let (mut ctx, token_start) = self.get_completion_context(&line, cursor_pos);
    self.token_span.0 = token_start; // Update start of token span based on context
    cur_token
      .span
      .set_range(self.token_span.0..self.token_span.1); // Update token span to reflect context

    // If token contains '=', only complete after the '='
    let token_str = cur_token.span.as_str();
    if let Some(eq_pos) = token_str.rfind('=') {
      // Adjust span to only replace the part after '='
      self.token_span.0 = cur_token.span.start + eq_pos + 1;
      cur_token
        .span
        .set_range(self.token_span.0..self.token_span.1);
    }

    if ctx.last().is_some_and(|m| *m == markers::VAR_SUB) {
      let var_sub = &cur_token.as_str();
      if let Some((var_name, start, end)) = Self::extract_var_name(var_sub) {
        if read_vars(|v| v.get_var(&var_name)).is_empty() {
          // if we are here, we have a variable substitution that isn't complete
          // so let's try to complete it
          let ret: ShResult<CompResult> = read_vars(|v| {
            let var_matches = v
              .flatten_vars()
              .keys()
              .filter(|k| k.starts_with(&var_name) && *k != &var_name)
              .map(|k| k.to_string())
              .collect::<Vec<_>>();

            if !var_matches.is_empty() {
              let name_start = cur_token.span.start + start;
              let name_end = cur_token.span.start + end;
              self.token_span = (name_start, name_end);
              cur_token
                .span
                .set_range(self.token_span.0..self.token_span.1);
              Ok(CompResult::from_candidates(var_matches))
            } else {
              Ok(CompResult::NoMatch)
            }
          });

          if !matches!(ret, Ok(CompResult::NoMatch)) {
            return ret;
          } else {
            ctx.pop();
          }
        } else {
          ctx.pop();
        }
      }
    }

    let raw_tk = cur_token.as_str().to_string();
    let expanded_tk = cur_token.expand()?;
    let expanded_words = expanded_tk.get_words().into_iter().collect::<Vec<_>>();
    let expanded = expanded_words.join("\\ ");

    let mut candidates = match ctx.pop() {
      Some(markers::COMMAND) => Self::complete_command(&expanded)?,
      Some(markers::ARG) => Self::complete_filename(&expanded),
      Some(_) => {
        return Ok(CompResult::NoMatch);
      }
      None => {
        return Ok(CompResult::NoMatch);
      }
    };

    // Now we are just going to graft the completed text
    // onto the original token. This prevents something like
    // $SOME_PATH/
    // from being completed into
    // /path/to/some_path/file.txt
    // and instead returns
    // $SOME_PATH/file.txt
    candidates = candidates
      .into_iter()
      .map(|c| match c.strip_prefix(&expanded) {
        Some(suffix) => format!("{raw_tk}{suffix}"),
        None => c,
      })
      .collect();

    let limit = crate::state::read_shopts(|s| s.prompt.comp_limit);
    candidates.truncate(limit);

    Ok(CompResult::from_candidates(candidates))
  }

  fn complete_command(start: &str) -> ShResult<Vec<String>> {
    let mut candidates = vec![];

    let path = env::var("PATH").unwrap_or_default();
    let paths = path.split(':').map(PathBuf::from).collect::<Vec<_>>();
    for path in paths {
      // Skip directories that don't exist (common in PATH)
      let Ok(entries) = std::fs::read_dir(path) else {
        continue;
      };
      for entry in entries {
        let Ok(entry) = entry else {
          continue;
        };
        let Ok(meta) = entry.metadata() else {
          continue;
        };

        let file_name = entry.file_name().to_string_lossy().to_string();

        if meta.is_file()
          && (meta.permissions().mode() & 0o111) != 0
          && file_name.starts_with(start)
        {
          candidates.push(file_name);
        }
      }
    }

    let builtin_candidates = BUILTINS
      .iter()
      .filter(|b| b.starts_with(start))
      .map(|s| s.to_string());

    candidates.extend(builtin_candidates);

    read_logic(|l| {
      let func_table = l.funcs();
      let matches = func_table
        .keys()
        .filter(|k| k.starts_with(start))
        .map(|k| k.to_string());

      candidates.extend(matches);

      let aliases = l.aliases();
      let matches = aliases
        .keys()
        .filter(|k| k.starts_with(start))
        .map(|k| k.to_string());

      candidates.extend(matches);
    });

    // Deduplicate (same command may appear in multiple PATH dirs)
    candidates.sort();
    candidates.dedup();

    Ok(candidates)
  }

  fn complete_filename(start: &str) -> Vec<String> {
    let mut candidates = vec![];
    let has_dotslash = start.starts_with("./");

    // Split path into directory and filename parts
    // Use "." if start is empty (e.g., after "foo=")
    let path = PathBuf::from(if start.is_empty() { "." } else { start });
    let (dir, prefix) = if start.ends_with('/') || start.is_empty() {
      // Completing inside a directory: "src/" → dir="src/", prefix=""
      (path, "")
    } else if let Some(parent) = path.parent()
      && !parent.as_os_str().is_empty()
    {
      // Has directory component: "src/ma" → dir="src", prefix="ma"
      (
        parent.to_path_buf(),
        path.file_name().unwrap().to_str().unwrap_or(""),
      )
    } else {
      // No directory: "fil" → dir=".", prefix="fil"
      (PathBuf::from("."), start)
    };

    let Ok(entries) = std::fs::read_dir(&dir) else {
      return candidates;
    };

    for entry in entries.flatten() {
      let file_name = entry.file_name();
      let file_str = file_name.to_string_lossy();

      // Skip hidden files unless explicitly requested
      if !prefix.starts_with('.') && file_str.starts_with('.') {
        continue;
      }

      if file_str.starts_with(prefix) {
        // Reconstruct full path
        let mut full_path = dir.join(&file_name);

        // Add trailing slash for directories
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
          full_path.push(""); // adds trailing /
        }

        let mut path_raw = full_path.to_string_lossy().to_string();
        if path_raw.starts_with("./") && !has_dotslash {
          path_raw = path_raw.trim_start_matches("./").to_string();
        }

        candidates.push(path_raw);
      }
    }

    candidates.sort();
    candidates
  }
}

impl Default for Completer {
  fn default() -> Self {
    Self::new()
  }
}
