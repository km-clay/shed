use std::{
  collections::HashSet, fmt::{Write,Debug}, path::PathBuf, sync::Arc,
};

use nix::sys::signal::Signal;
use unicode_width::UnicodeWidthStr;

use crate::{
  builtin::complete::{CompFlags, CompOptFlags, CompOpts},
  libsh::{
    error::ShResult,
    guards::var_ctx_guard,
    utils::TkVecUtils,
  },
  parse::{
    execute::exec_input,
    lex::{self, LexFlags, Tk, TkRule, ends_with_unescaped},
  },
  readline::{
    Marker, annotate_input_recursive, keys::{KeyCode as C, KeyEvent as K, ModKeys as M}, linebuf::{ClampedUsize, LineBuf}, markers::{self, is_marker}, term::{LineWriter, TermWriter}, vimode::{ViInsert, ViMode}
  },
  state::{VarFlags, VarKind, read_jobs, read_logic, read_meta, read_vars, write_vars},
};

pub fn complete_signals(start: &str) -> Vec<String> {
	Signal::iterator()
		.map(|s| {
			s.to_string()
				.strip_prefix("SIG")
				.unwrap_or(s.as_ref())
				.to_string()
		})
		.filter(|s| s.starts_with(start))
		.collect()
}

pub fn complete_aliases(start: &str) -> Vec<String> {
	read_logic(|l| {
		l.aliases()
			.iter()
			.filter(|a| a.0.starts_with(start))
			.map(|a| a.0.clone())
			.collect()
	})
}

pub fn complete_jobs(start: &str) -> Vec<String> {
  if let Some(prefix) = start.strip_prefix('%') {
    read_jobs(|j| {
      j.jobs()
        .iter()
        .filter_map(|j| j.as_ref())
        .filter_map(|j| j.name())
        .filter(|name| name.starts_with(prefix))
        .map(|name| format!("%{name}"))
        .collect()
    })
  } else {
    read_jobs(|j| {
      j.jobs()
        .iter()
        .filter_map(|j| j.as_ref())
        .map(|j| j.pgid().to_string())
        .filter(|pgid| pgid.starts_with(start))
        .collect()
    })
  }
}

pub fn complete_users(start: &str) -> Vec<String> {
  let Ok(passwd) = std::fs::read_to_string("/etc/passwd") else {
    return vec![];
  };
  passwd
    .lines()
    .filter_map(|line| line.split(':').next())
    .filter(|username| username.starts_with(start))
    .map(|s| s.to_string())
    .collect()
}

pub fn complete_vars(start: &str) -> Vec<String> {
  let Some((var_name, name_start, _end)) = extract_var_name(start) else {
    return vec![];
  };
  if !read_vars(|v| v.get_var(&var_name)).is_empty() {
    return vec![];
  }
  // if we are here, we have a variable substitution that isn't complete
  // so let's try to complete it
  let prefix = &start[..name_start]; // e.g. "$" or "${"
  read_vars(|v| {
    v.flatten_vars()
      .keys()
      .filter(|k| k.starts_with(&var_name) && *k != &var_name)
      .map(|k| format!("{prefix}{k}"))
      .collect::<Vec<_>>()
  })
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

fn complete_commands(start: &str) -> Vec<String> {
  let mut candidates: Vec<String> = read_meta(|m| {
    m.cached_cmds()
      .iter()
      .filter(|c| c.starts_with(start))
      .cloned()
      .collect()
  });

  candidates.sort();
  candidates
}

fn complete_dirs(start: &str) -> Vec<String> {
  let filenames = complete_filename(start);
  filenames
    .into_iter()
    .filter(|f| std::fs::metadata(f).map(|m| m.is_dir()).unwrap_or(false))
    .collect()
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

pub enum CompSpecResult {
  NoSpec, // No compspec registered
  NoMatch { flags: CompOptFlags }, /* Compspec found but no candidates matched, returns
           * behavior flags */
  Match { result: CompResult, flags: CompOptFlags }, // Compspec found and candidates returned
}

#[derive(Default, Debug, Clone)]
pub struct BashCompSpec {
  /// -F: The name of a function to generate the possible completions.
  pub function: Option<String>,
  /// -W: The list of words
  pub wordlist: Option<Vec<String>>,
  /// -f: complete file names
  pub files: bool,
  /// -d: complete directory names
  pub dirs: bool,
  /// -c: complete command names
  pub commands: bool,
  /// -u: complete user names
  pub users: bool,
  /// -v: complete variable names
  pub vars: bool,
  /// -A signal: complete signal names
  pub signals: bool,
  /// -j: complete job pids or names
  pub jobs: bool,
	/// -a: complete aliases
	pub aliases: bool,

  pub flags: CompOptFlags,
  /// The original command
  pub source: String,
}

impl BashCompSpec {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn with_func(mut self, func: String) -> Self {
    self.function = Some(func);
    self
  }
  pub fn with_wordlist(mut self, wordlist: Vec<String>) -> Self {
    self.wordlist = Some(wordlist);
    self
  }
  pub fn with_source(mut self, source: String) -> Self {
    self.source = source;
    self
  }
  pub fn files(mut self, enable: bool) -> Self {
    self.files = enable;
    self
  }
  pub fn dirs(mut self, enable: bool) -> Self {
    self.dirs = enable;
    self
  }
  pub fn commands(mut self, enable: bool) -> Self {
    self.commands = enable;
    self
  }
  pub fn users(mut self, enable: bool) -> Self {
    self.users = enable;
    self
  }
  pub fn vars(mut self, enable: bool) -> Self {
    self.vars = enable;
    self
  }
  pub fn signals(mut self, enable: bool) -> Self {
    self.signals = enable;
    self
  }
  pub fn jobs(mut self, enable: bool) -> Self {
    self.jobs = enable;
    self
  }
	pub fn aliases(mut self, enable: bool) -> Self {
		self.aliases = enable;
		self
	}
  pub fn from_comp_opts(opts: CompOpts) -> Self {
    let CompOpts {
      func,
      wordlist,
      action: _,
      flags,
      opt_flags,
    } = opts;
    Self {
      function: func,
      wordlist,
      files: flags.contains(CompFlags::FILES),
      dirs: flags.contains(CompFlags::DIRS),
      commands: flags.contains(CompFlags::CMDS),
      users: flags.contains(CompFlags::USERS),
      vars: flags.contains(CompFlags::VARS),
      jobs: flags.contains(CompFlags::JOBS),
			aliases: flags.contains(CompFlags::ALIAS),
      flags: opt_flags,
      signals: false, // TODO: implement signal completion
      source: String::new(),
    }
  }
  pub fn exec_comp_func(&self, ctx: &CompContext) -> ShResult<Vec<String>> {
    let mut vars_to_unset = HashSet::new();
    for var in [
      "COMP_WORDS",
      "COMP_CWORD",
      "COMP_LINE",
      "COMP_POINT",
      "COMPREPLY",
    ] {
      vars_to_unset.insert(var.to_string());
    }
    let _guard = var_ctx_guard(vars_to_unset);

    let CompContext {
      words,
      cword,
      line,
      cursor_pos,
    } = ctx;

    let raw_words = words.iter().clone().map(|tk| tk.to_string()).collect();
    write_vars(|v| {
      v.set_var(
        "COMP_WORDS",
        VarKind::arr_from_vec(raw_words),
        VarFlags::NONE,
      )
    })?;
    write_vars(|v| {
      v.set_var(
        "COMP_CWORD",
        VarKind::Str(cword.to_string()),
        VarFlags::NONE,
      )
    })?;
    write_vars(|v| v.set_var("COMP_LINE", VarKind::Str(line.to_string()), VarFlags::NONE))?;
    write_vars(|v| {
      v.set_var(
        "COMP_POINT",
        VarKind::Str(cursor_pos.to_string()),
        VarFlags::NONE,
      )
    })?;

    let cmd_name = words.first().map(|s| s.to_string()).unwrap_or_default();

    let cword_str = words.get(*cword).map(|s| s.to_string()).unwrap_or_default();

    let pword_str = if *cword > 0 {
      words
        .get(cword - 1)
        .map(|s| s.to_string())
        .unwrap_or_default()
    } else {
      String::new()
    };

    let input = format!(
      "{} {cmd_name} {cword_str} {pword_str}",
      self.function.as_ref().unwrap()
    );
    exec_input(input, None, false, Some("comp_function".into()))?;

    Ok(read_vars(|v| v.get_arr_elems("COMPREPLY")).unwrap_or_default())
  }
}

impl CompSpec for BashCompSpec {
  fn complete(&self, ctx: &CompContext) -> ShResult<Vec<String>> {
    let mut candidates = vec![];
    let prefix = &ctx.words[ctx.cword];

    let expanded = prefix.clone().expand()?.get_words().join(" ");
    if self.files {
      candidates.extend(complete_filename(&expanded));
    }
    if self.dirs {
      candidates.extend(complete_dirs(&expanded));
    }
    if self.commands {
      candidates.extend(complete_commands(&expanded));
    }
    if self.vars {
      candidates.extend(complete_vars(&expanded));
    }
    if self.users {
      candidates.extend(complete_users(&expanded));
    }
    if self.jobs {
      candidates.extend(complete_jobs(&expanded));
    }
		if self.aliases {
			candidates.extend(complete_aliases(&expanded));
		}
		if self.signals {
			candidates.extend(complete_signals(&expanded));
		}
    if let Some(words) = &self.wordlist {
      candidates.extend(words.iter().filter(|w| w.starts_with(&expanded)).cloned());
    }
    if self.function.is_some() {
      candidates.extend(self.exec_comp_func(ctx)?);
    }
		candidates = candidates.into_iter()
			.map(|c| {
				let stripped = c.strip_prefix(&expanded).unwrap_or_default();
				format!("{prefix}{stripped}")
			})
		.collect();

		candidates.sort_by_key(|c| c.len()); // sort by length to prioritize shorter completions, ties are then sorted alphabetically

    Ok(candidates)
  }

  fn source(&self) -> &str {
    &self.source
  }

  fn get_flags(&self) -> CompOptFlags {
    self.flags
  }
}

pub trait CompSpec: Debug + CloneCompSpec {
  fn complete(&self, ctx: &CompContext) -> ShResult<Vec<String>>;
  fn source(&self) -> &str;
  fn get_flags(&self) -> CompOptFlags {
    CompOptFlags::empty()
  }
}

pub trait CloneCompSpec {
  fn clone_box(&self) -> Box<dyn CompSpec>;
}

impl<T: CompSpec + Clone + 'static> CloneCompSpec for T {
  fn clone_box(&self) -> Box<dyn CompSpec> {
    Box::new(self.clone())
  }
}

impl Clone for Box<dyn CompSpec> {
  fn clone(&self) -> Self {
    self.clone_box()
  }
}

pub struct CompContext {
  pub words: Vec<Tk>,
  pub cword: usize,
  pub line: String,
  pub cursor_pos: usize,
}

impl CompContext {
  pub fn cmd(&self) -> Option<&str> {
    self.words.first().map(|s| s.as_str())
  }
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

pub enum CompResponse {
	Passthrough, // key falls through
	Accept(String), // user accepted completion
	Dismiss, // user canceled completion
	Consumed // key was handled, but completion remains active
}

pub trait Completer {
	fn complete(&mut self, line: String, cursor_pos: usize, direction: i32) -> ShResult<Option<String>>;
	fn reset(&mut self);
	fn is_active(&self) -> bool;
	fn selected_candidate(&self) -> Option<String>;
	fn token_span(&self) -> (usize, usize);
	fn original_input(&self) -> &str;
	fn draw(&mut self, writer: &mut TermWriter) -> ShResult<()>;
	fn clear(&mut self, _writer: &mut TermWriter) -> ShResult<()> { Ok(()) }
	fn handle_key(&mut self, key: K) -> ShResult<CompResponse>;
	fn get_completed_line(&self, candidate: &str) -> String {
		let (start, end) = self.token_span();
		let orig = self.original_input();
		format!("{}{}{}", &orig[..start], candidate, &orig[end..])
	}
}

#[derive(Default, Debug, Clone)]
pub struct ScoredCandidate {
	content: String,
	score: Option<i32>,
}

impl ScoredCandidate {
	const BONUS_BOUNDARY: i32 = 10;
	const BONUS_CONSECUTIVE: i32 = 8;
	const BONUS_FIRST_CHAR: i32 = 5;
	const PENALTY_GAP_START: i32 = 3;
	const PENALTY_GAP_EXTEND: i32 = 1;

	pub fn new(content: String) -> Self {
		Self { content, score: None }
	}
	fn is_word_bound(prev: char, curr: char) -> bool {
		match prev {
			'/' | '_' | '-' | '.' | ' ' => true,
			c if c.is_lowercase() && curr.is_uppercase() => true, // camelCase boundary
			_ => false,
		}
	}
	pub fn fuzzy_score(&mut self, other: &str) -> i32 {
		if other.is_empty() {
			self.score = Some(0);
			return 0;
		}

		let query_chars: Vec<char> = other.chars().collect();
		let content_chars: Vec<char> = self.content.chars().collect();
		let mut indices = vec![];
		let mut qi = 0;
		for (ci, c_ch) in self.content.chars().enumerate() {
			if qi < query_chars.len() && c_ch.eq_ignore_ascii_case(&query_chars[qi]) {
				indices.push(ci);
				qi += 1;
			}
		}

		if indices.len() != query_chars.len() {
			self.score = Some(i32::MIN);
			return i32::MIN;
		}

		let mut score: i32 = 0;

		for (i, &idx) in indices.iter().enumerate() {
			if idx == 0 {
				score += Self::BONUS_FIRST_CHAR;
			}


			if idx == 0 || Self::is_word_bound(content_chars[idx - 1], content_chars[idx])  {
				score += Self::BONUS_BOUNDARY;
			}

			if i > 0 {
				let gap = idx - indices[i - 1] - 1;
				if gap == 0 {
					score += Self::BONUS_CONSECUTIVE;
				} else {
					score -= Self::PENALTY_GAP_START + (gap as i32 - 1) * Self::PENALTY_GAP_EXTEND;
				}
			}
		}

		self.score = Some(score);
		score
	}
}

impl From<String> for ScoredCandidate {
	fn from(content: String) -> Self {
		Self { content, score: None }
	}
}

#[derive(Debug, Clone)]
pub struct FuzzyLayout {
	rows: u16
}

#[derive(Default, Debug, Clone)]
pub struct QueryEditor {
	mode: ViInsert,
	linebuf: LineBuf
}

impl QueryEditor {
	pub fn clear(&mut self) {
		self.linebuf = LineBuf::default();
		self.mode = ViInsert::default();
	}
	pub fn handle_key(&mut self, key: K) -> ShResult<()> {
		let Some(cmd) = self.mode.handle_key(key) else {
			return Ok(())
		};
		self.linebuf.exec_cmd(cmd)
	}
}


#[derive(Clone, Debug)]
pub struct FuzzyCompleter {
	completer: SimpleCompleter,
	query: QueryEditor,
	filtered: Vec<ScoredCandidate>,
	candidates: Vec<String>,
	cursor: ClampedUsize,
	old_layout: Option<FuzzyLayout>,
	max_height: usize,
	scroll_offset: usize,
	active: bool
}

impl FuzzyCompleter {
	fn get_window(&mut self) -> &[ScoredCandidate] {
		let height = self.filtered.len().min(self.max_height);

		self.update_scroll_offset();

		&self.filtered[self.scroll_offset..self.scroll_offset + height]
	}
	pub fn update_scroll_offset(&mut self) {
		let height = self.filtered.len().min(self.max_height);
		if self.cursor.get() < self.scroll_offset + 1 {
			self.scroll_offset = self.cursor.ret_sub(1);
		}
		if self.cursor.get() >= self.scroll_offset + height.saturating_sub(1) {
			self.scroll_offset = self.cursor.ret_sub(height.saturating_sub(2));
		}
		self.scroll_offset = self.scroll_offset.min(self.filtered.len().saturating_sub(height));
	}
	pub fn score_candidates(&mut self) {
		let mut scored: Vec<_> = self.candidates
			.clone()
			.into_iter()
			.filter_map(|c| {
				let mut sc = ScoredCandidate::new(c);
				let score = sc.fuzzy_score(self.query.linebuf.as_str());
				if score > i32::MIN {
					Some(sc)
				} else {
					None
				}
			}).collect();
		scored.sort_by_key(|sc| sc.score.unwrap_or(i32::MIN));
		scored.reverse();
		self.cursor.set_max(scored.len());
		self.filtered = scored;
	}
}

impl Default for FuzzyCompleter {
	fn default() -> Self {
	  Self {
			max_height: 8,
			completer: SimpleCompleter::default(),
			query: QueryEditor::default(),
			filtered: vec![],
			candidates: vec![],
			cursor: ClampedUsize::new(0, 0, true),
			old_layout: None,
			scroll_offset: 0,
			active: false,
		}
	}
}

impl Completer for FuzzyCompleter {
	fn complete(&mut self, line: String, cursor_pos: usize, direction: i32) -> ShResult<Option<String>> {
		self.completer.complete(line, cursor_pos, direction)?;
		let candidates: Vec<_> = self.completer.candidates.clone();
		if candidates.is_empty() {
			self.completer.reset();
			self.active = false;
			return Ok(None);
		}
		self.active = true;
		self.candidates = candidates;
		self.score_candidates();
		self.completer.reset();
		Ok(None) // FuzzyCompleter itself doesn't directly return a completed line, it manages the state of the filtered candidates and selection
	}

	fn handle_key(&mut self, key: K) -> ShResult<CompResponse> {
		match key {
			K(C::Esc, M::NONE) => {
				self.active = false;
				self.filtered.clear();
				Ok(CompResponse::Dismiss)
			}
			K(C::Enter, M::NONE) => {
				if let Some(selected) = self.filtered.get(self.cursor.get()).map(|c| c.content.clone()) {
					self.active = false;
					self.query.clear();
					self.filtered.clear();
					Ok(CompResponse::Accept(selected))
				} else {
					Ok(CompResponse::Passthrough)
				}
			}
			K(C::Tab, M::SHIFT) |
			K(C::Up, M::NONE) => {
				self.cursor.sub(1);
				self.update_scroll_offset();
				Ok(CompResponse::Consumed)
			}
			K(C::Tab, M::NONE) |
			K(C::Down, M::NONE) => {
				self.cursor.add(1);
				self.update_scroll_offset();
				Ok(CompResponse::Consumed)
			}
			_ => {
				self.query.handle_key(key)?;
				self.score_candidates();
				Ok(CompResponse::Consumed)
			}
		}
	}
	fn clear(&mut self, writer: &mut TermWriter) -> ShResult<()> {
		if let Some(layout) = self.old_layout.take() {
			let mut buf = String::new();
			// Cursor is on the query line. Move down to the last candidate.
			if layout.rows > 0 {
				write!(buf, "\x1b[{}B", layout.rows).unwrap();
			}
			// Erase each line and move up, back to the query line
			for _ in 0..layout.rows {
				buf.push_str("\x1b[2K\x1b[A");
			}
			// Erase the query line, then move up to the prompt line
			buf.push_str("\x1b[2K\x1b[A");
			writer.flush_write(&buf)?;
		}
		Ok(())
	}
	fn draw(&mut self, writer: &mut TermWriter) -> ShResult<()> {
		if !self.active {
			return Ok(());
		}

		let mut buf = String::new();
		let cursor_pos = self.cursor.get();
		let offset = self.scroll_offset;
		let query = self.query.linebuf.as_str().to_string();
		let visible = self.get_window();
		buf.push_str("\n\r> ");
		buf.push_str(&query);

		for (i, candidate) in visible.iter().enumerate() {
			buf.push_str("\n\r");
			if i + offset == cursor_pos {
				buf.push_str("\x1b[7m");
				buf.push_str(&candidate.content);
				buf.push_str("\x1b[0m");
			} else {
				buf.push_str(&candidate.content);
			}
		}
		let new_layout = FuzzyLayout {
			rows: visible.len() as u16, // +1 for the query line
		};

		// Move cursor back up to the query line and position after "> " + query text
		write!(buf, "\x1b[{}A\r\x1b[{}C", new_layout.rows, self.query.linebuf.as_str().width() + 2).unwrap();
		writer.flush_write(&buf)?;
		self.old_layout = Some(new_layout);

		Ok(())
	}
	fn reset(&mut self) {
		*self = Self::default();
	}
	fn token_span(&self) -> (usize, usize) {
		self.completer.token_span()
	}
	fn is_active(&self) -> bool {
	  self.active
	}
	fn selected_candidate(&self) -> Option<String> {
		self.filtered.get(self.cursor.get()).map(|c| c.content.clone())
	}
	fn original_input(&self) -> &str {
		&self.completer.original_input
	}
}

#[derive(Default, Debug, Clone)]
pub struct SimpleCompleter {
  pub candidates: Vec<String>,
  pub selected_idx: usize,
  pub original_input: String,
  pub token_span: (usize, usize),
  pub active: bool,
  pub dirs_only: bool,
  pub add_space: bool,
}

impl Completer for SimpleCompleter {
	fn complete(&mut self, line: String, cursor_pos: usize, direction: i32) -> ShResult<Option<String>> {
		if self.active {
			Ok(Some(self.cycle_completion(direction)))
		} else {
			self.start_completion(line, cursor_pos)
		}
	}

	fn reset(&mut self) {
		*self = Self::default();
	}

	fn is_active(&self) -> bool {
		self.active
	}

	fn selected_candidate(&self) -> Option<String> {
		self.candidates.get(self.selected_idx).cloned()
	}

	fn token_span(&self) -> (usize, usize) {
		self.token_span
	}

	fn draw(&mut self, _writer: &mut TermWriter) -> ShResult<()> {
		Ok(())
	}

	fn original_input(&self) -> &str {
		&self.original_input
	}

	fn handle_key(&mut self, _key: K) -> ShResult<CompResponse> {
		Ok(CompResponse::Passthrough)
	}
}

impl SimpleCompleter {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn slice_line(line: &str, cursor_pos: usize) -> (&str, &str) {
    let (before_cursor, after_cursor) = line.split_at(cursor_pos);
    (before_cursor, after_cursor)
  }

  pub fn get_subtoken_completion(&self, line: &str, cursor_pos: usize) -> (Vec<Marker>, usize) {
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
          markers::RESET => {
            if ctx.len() > 1 {
              ctx.pop();
              last_priority = 0;
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

  pub fn cycle_completion(&mut self, direction: i32) -> String {
    if self.candidates.is_empty() {
      return self.original_input.clone();
    }

    let len = self.candidates.len();
    self.selected_idx = (self.selected_idx as i32 + direction).rem_euclid(len as i32) as usize;

    self.get_completed_line()
  }

  pub fn add_spaces(&mut self) {
    if self.add_space {
      self.candidates = std::mem::take(&mut self.candidates)
        .into_iter()
        .map(|c| {
          if !ends_with_unescaped(&c, "/") 		// directory
					&& !ends_with_unescaped(&c, "=") 		// '='-type arg
					&& !ends_with_unescaped(&c, " ")
          {
            // already has a space
            format!("{} ", c)
          } else {
            c
          }
        })
        .collect()
    }
  }

  pub fn start_completion(&mut self, line: String, cursor_pos: usize) -> ShResult<Option<String>> {
    let result = self.get_candidates(line.clone(), cursor_pos)?;
    match result {
      CompResult::Many { candidates } => {
        self.candidates = candidates.clone();
        self.add_spaces();
        self.selected_idx = 0;
        self.original_input = line;
        self.active = true;

        Ok(Some(self.get_completed_line()))
      }
      CompResult::Single { result } => {
        self.candidates = vec![result.clone()];
        self.add_spaces();
        self.selected_idx = 0;
        self.original_input = line;
        self.active = false;

        Ok(Some(self.get_completed_line()))
      }
      CompResult::NoMatch => Ok(None),
    }
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

  pub fn build_comp_ctx(&self, tks: &[Tk], line: &str, cursor_pos: usize) -> ShResult<CompContext> {
    let mut ctx = CompContext {
      words: vec![],
      cword: 0,
      line: line.to_string(),
      cursor_pos,
    };

    let segments = tks
      .iter()
      .filter(|&tk| !matches!(tk.class, TkRule::SOI | TkRule::EOI))
      .cloned()
      .collect::<Vec<_>>()
      .split_at_separators();

    if segments.is_empty() {
      return Ok(ctx);
    }

    let relevant_pos = segments
      .iter()
      .position(|tks| {
        tks
          .iter()
          .next()
          .is_some_and(|tk| tk.span.range().start > cursor_pos)
      })
      .map(|i| i.saturating_sub(1))
      .unwrap_or(segments.len().saturating_sub(1));

    let mut relevant = segments[relevant_pos].to_vec();

    let cword = if let Some(pos) = relevant
      .iter()
      .position(|tk| cursor_pos >= tk.span.range().start && cursor_pos < tk.span.range().end)
    {
      pos
    } else {
      let insert_pos = relevant
        .iter()
        .position(|tk| tk.span.range().start > cursor_pos)
        .unwrap_or(relevant.len());

      let mut new_tk = Tk::default();
      if let Some(tk) = relevant.last() {
        let mut span = tk.span.clone();
        span.set_range(cursor_pos..cursor_pos);
        new_tk.span = span;
      }
      relevant.insert(insert_pos, new_tk);
      insert_pos
    };

    ctx.words = relevant;
    ctx.cword = cword;

    Ok(ctx)
  }

  pub fn try_comp_spec(&self, ctx: &CompContext) -> ShResult<CompSpecResult> {
    let Some(cmd) = ctx.cmd() else {
      return Ok(CompSpecResult::NoSpec);
    };

    let Some(spec) = read_meta(|m| m.get_comp_spec(cmd)) else {
      return Ok(CompSpecResult::NoSpec);
    };

    let candidates = spec.complete(ctx)?;
    if candidates.is_empty() {
      Ok(CompSpecResult::NoMatch {
        flags: spec.get_flags(),
      })
    } else {
      Ok(CompSpecResult::Match {
        result: CompResult::from_candidates(candidates),
        flags: spec.get_flags(),
      })
    }
  }

  pub fn get_candidates(&mut self, line: String, cursor_pos: usize) -> ShResult<CompResult> {
    let source = Arc::new(line.clone());
    let tokens =
      lex::LexStream::new(source, LexFlags::LEX_UNFINISHED).collect::<ShResult<Vec<Tk>>>()?;

    let ctx = self.build_comp_ctx(&tokens, &line, cursor_pos)?;

    // Set token_span from CompContext's current word
    if let Some(cur) = ctx.words.get(ctx.cword) {
      self.token_span = (cur.span.range().start, cur.span.range().end);
    } else {
      self.token_span = (cursor_pos, cursor_pos);
    }

    // Use marker-based context detection for sub-token awareness (e.g. VAR_SUB
    // inside a token). Run this before comp specs so variable completions take
    // priority over programmable completion.
    let (mut marker_ctx, token_start) = self.get_subtoken_completion(&line, cursor_pos);

    if marker_ctx.last() == Some(&markers::VAR_SUB)
		&& let Some(cur) = ctx.words.get(ctx.cword) {
			self.token_span.0 = token_start;
			let mut span = cur.span.clone();
			span.set_range(token_start..self.token_span.1);
			let raw_tk = span.as_str();
			let candidates = complete_vars(raw_tk);
			if !candidates.is_empty() {
				return Ok(CompResult::from_candidates(candidates));
			}
		}

    // Try programmable completion
    match self.try_comp_spec(&ctx)? {
      CompSpecResult::NoMatch { flags } => {
        if flags.contains(CompOptFlags::DIRNAMES) {
          self.dirs_only = true;
        } else if flags.contains(CompOptFlags::DEFAULT) {
          /* fall through */
        } else {
          return Ok(CompResult::NoMatch);
        }

        if flags.contains(CompOptFlags::SPACE) {
          self.add_space = true;
        }
      }
      CompSpecResult::Match { result, flags } => {
        if flags.contains(CompOptFlags::SPACE) {
          self.add_space = true;
        }
        return Ok(result);
      }
      CompSpecResult::NoSpec => { /* carry on */ }
    }

    // Get the current token from CompContext
    let Some(mut cur_token) = ctx.words.get(ctx.cword).cloned() else {
      let candidates = complete_filename("./");
      let end_pos = line.len();
      self.token_span = (end_pos, end_pos);
      return Ok(CompResult::from_candidates(candidates));
    };

    self.token_span = (cur_token.span.range().start, cur_token.span.range().end);

    if token_start >= self.token_span.0 && token_start <= self.token_span.1 {
      self.token_span.0 = token_start;
      cur_token
        .span
        .set_range(self.token_span.0..self.token_span.1);
    }

    // If token contains '=', only complete after the '='
    let token_str = cur_token.span.as_str();
    if let Some(eq_pos) = token_str.rfind('=') {
      self.token_span.0 = cur_token.span.range().start + eq_pos + 1;
      cur_token
        .span
        .set_range(self.token_span.0..self.token_span.1);
    }

    let raw_tk = cur_token.as_str().to_string();
    let expanded_tk = cur_token.expand()?;
    let expanded_words = expanded_tk.get_words().into_iter().collect::<Vec<_>>();
    let expanded = expanded_words.join("\\ ");

    let last_marker = marker_ctx.last().copied();
    let mut candidates = match marker_ctx.pop() {
      _ if self.dirs_only => complete_dirs(&expanded),
      Some(markers::COMMAND) => complete_commands(&expanded),
      Some(markers::VAR_SUB) => {
        // Variable completion already tried above and had no matches,
        // fall through to filename completion
        complete_filename(&expanded)
      }
      Some(markers::ARG) => complete_filename(&expanded),
      _ => complete_filename(&expanded),
    };

    // Graft unexpanded prefix onto candidates to preserve things like
    // $SOME_PATH/file.txt Skip for var completions — complete_vars already
    // returns the full $VAR form
    let is_var_completion = last_marker == Some(markers::VAR_SUB)
      && !candidates.is_empty()
      && candidates.iter().any(|c| c.starts_with('$'));
    if !is_var_completion {
      candidates = candidates
        .into_iter()
        .map(|c| match c.strip_prefix(&expanded) {
          Some(suffix) => format!("{raw_tk}{suffix}"),
          None => c,
        })
        .collect();
    }

    let limit = crate::state::read_shopts(|s| s.prompt.comp_limit);
    candidates.truncate(limit);

    Ok(CompResult::from_candidates(candidates))
  }
}
