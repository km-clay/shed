use std::{
  collections::HashSet,
  fmt::{Debug, Display, Write},
  path::PathBuf,
  rc::Rc,
};

use nix::sys::signal::Signal;
use unicode_width::UnicodeWidthStr;

use crate::{
  builtin::complete::{CompFlags, CompOptFlags, CompOpts},
  expand::escape_str,
  libsh::{error::ShResult, guards::var_ctx_guard, sys::TTY_FILENO, utils::TkVecUtils},
  match_loop,
  parse::{
    execute::exec_input,
    lex::{self, LexFlags, Tk, TkRule, ends_with_unescaped},
  },
  readline::{
    Marker, annotate_input_recursive,
    editmode::{EditMode, ViInsert},
    keys::{KeyCode as C, KeyEvent as K, ModKeys as M},
    linebuf::LineBuf,
    markers::{self, is_marker},
    term::{LineWriter, TermWriter, calc_str_width, get_win_size},
  },
  state::{
    self, Utility, VarFlags, VarKind, read_jobs, read_logic, read_meta, read_shopts, read_vars, write_vars
  },
};

/// Compat shim: replaces the old ClampedUsize type that was removed in the linebuf refactor.
/// A simple wrapper around usize with wrapping arithmetic and a max bound.
#[derive(Clone, Default, Debug)]
pub struct ClampedUsize {
  val: usize,
  max: usize,
  wrap: bool,
}

impl ClampedUsize {
  pub fn new(val: usize, max: usize, wrap: bool) -> Self {
    Self { val, max, wrap }
  }
  pub fn get(&self) -> usize {
    self.val
  }
  pub fn set_max(&mut self, max: usize) {
    self.max = max;
    if self.val >= self.max && self.max > 0 {
      self.val = self.max - 1;
    }
  }
  pub fn wrap_add(&mut self, n: usize) {
    if self.max == 0 {
      return;
    }
    if self.wrap {
      self.val = (self.val + n) % self.max;
    } else {
      self.val = (self.val + n).min(self.max.saturating_sub(1));
    }
  }
  pub fn wrap_sub(&mut self, n: usize) {
    if self.max == 0 {
      return;
    }
    if self.wrap {
      self.val = (self.val + self.max - (n % self.max)) % self.max;
    } else {
      self.val = self.val.saturating_sub(n);
    }
  }
}

#[derive(Default, Debug, Clone)]
pub struct Candidate {
  content: String,
  id: Option<usize>, // for stuff like history that cares about the original index
}

impl Eq for Candidate {}

impl PartialEq for Candidate {
  fn eq(&self, other: &Self) -> bool {
    self.content == other.content
  }
}

impl PartialOrd for Candidate {
  fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for Candidate {
  fn cmp(&self, other: &Self) -> std::cmp::Ordering {
    self.content.cmp(&other.content)
  }
}

impl From<String> for Candidate {
  fn from(value: String) -> Self {
    Self {
      content: value,
      id: None,
    }
  }
}

impl From<Rc<Utility>> for Candidate {
	fn from(value: Rc<Utility>) -> Self {
	  From::from(&*value)
	}
}

impl From<&state::meta::Utility> for Candidate {
  fn from(value: &state::meta::Utility) -> Self {
    Self {
      content: value.name().to_string(),
      id: None,
    }
  }
}

impl From<state::meta::Utility> for Candidate {
  fn from(value: state::meta::Utility) -> Self {
    From::from(&value)
  }
}

impl From<&String> for Candidate {
  fn from(value: &String) -> Self {
    Self {
      content: value.clone(),
      id: None,
    }
  }
}

impl From<&str> for Candidate {
  fn from(value: &str) -> Self {
    Self {
      content: value.to_string(),
      id: None,
    }
  }
}

impl From<(usize, String)> for Candidate {
  fn from(value: (usize, String)) -> Self {
    Self {
      content: value.1,
      id: Some(value.0),
    }
  }
}

impl Display for Candidate {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", &self.content)
  }
}

impl AsRef<str> for Candidate {
  fn as_ref(&self) -> &str {
    &self.content
  }
}

impl std::ops::Deref for Candidate {
  type Target = str;
  fn deref(&self) -> &str {
    &self.content
  }
}

impl Candidate {
  pub fn is_match(&self, other: &str) -> bool {
    let ignore_case = read_shopts(|o| o.prompt.completion_ignore_case);
    if ignore_case {
      let other_lower = other.to_lowercase();
      let self_lower = self.content.to_lowercase();
      self_lower.starts_with(&other_lower)
    } else {
      self.content.starts_with(other)
    }
  }
  pub fn content(&self) -> &str {
    &self.content
  }
  pub fn id(&self) -> Option<usize> {
    self.id
  }
  pub fn as_str(&self) -> &str {
    &self.content
  }
  pub fn as_bytes(&self) -> &[u8] {
    self.content.as_bytes()
  }
  pub fn starts_with(&self, pat: char) -> bool {
    self.content.starts_with(pat)
  }
  pub fn strip_prefix(&self, prefix: &str) -> Option<String> {
    let ignore_case = read_shopts(|o| o.prompt.completion_ignore_case);
    if ignore_case {
      let old_len = self.content.len();
      let prefix_lower = prefix.to_lowercase();
      let self_lower = self.content.to_lowercase();
      let stripped = self_lower.strip_prefix(&prefix_lower)?;
      let new_len = stripped.len();
      let delta = old_len - new_len;
      Some(self.content[delta..].to_string())
    } else {
      self.content.strip_prefix(prefix).map(|s| s.to_string())
    }
  }
}

pub fn complete_signals(start: &str) -> Vec<Candidate> {
  Signal::iterator()
    .map(|s| {
      s.to_string()
        .strip_prefix("SIG")
        .unwrap_or(s.as_ref())
        .to_string()
    })
    .map(Candidate::from)
    .filter(|s| s.is_match(start))
    .collect()
}

pub fn complete_aliases(start: &str) -> Vec<Candidate> {
  read_logic(|l| {
    l.aliases()
      .keys()
      .map(Candidate::from)
      .filter(|a| a.is_match(start))
      .collect()
  })
}

pub fn complete_jobs(start: &str) -> Vec<Candidate> {
  if let Some(prefix) = start.strip_prefix('%') {
    read_jobs(|j| {
      j.jobs()
        .iter()
        .filter_map(|j| j.as_ref())
        .filter_map(|j| j.name())
        .map(Candidate::from)
        .filter(|name| name.is_match(prefix))
        .map(|name| format!("%{name}").into())
        .collect()
    })
  } else {
    read_jobs(|j| {
      j.jobs()
        .iter()
        .filter_map(|j| j.as_ref())
        .map(|j| Candidate::from(j.pgid().to_string()))
        .filter(|pgid| pgid.is_match(start))
        .collect()
    })
  }
}

pub fn complete_users(start: &str) -> Vec<Candidate> {
  let Ok(passwd) = std::fs::read_to_string("/etc/passwd") else {
    return vec![];
  };
  passwd
    .lines()
    .filter_map(|line| line.split(':').next())
    .map(Candidate::from)
    .filter(|username| username.is_match(start))
    .collect()
}

pub fn complete_vars(start: &str) -> Vec<Candidate> {
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
      .map(Candidate::from)
      .collect::<Vec<_>>()
  })
}

pub fn complete_vars_raw(raw: &str) -> Vec<Candidate> {
  if !read_vars(|v| v.get_var(raw)).is_empty() {
    return vec![];
  }
  // if we are here, we have a variable substitution that isn't complete
  // so let's try to complete it
  read_vars(|v| {
    v.flatten_vars()
      .keys()
      .filter(|k| k.starts_with(raw) && *k != raw)
      .map(Candidate::from)
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

  match_loop!(chars.next() => ch, {
    '$' => {
      if chars.peek() == Some(&'{') {
        continue;
      }

      reading_name = true;
      name_start = pos + 1; // Start after the '$'
      pos += 1;
      }
    '{' if !reading_name => {
      reading_name = true;
      name_start = pos + 1;
      pos += 1;
    }
    ch if ch.is_alphanumeric() || ch == '_' => {
      if reading_name {
        name.push(ch);
      }
      pos += 1;
    }
    _ => {
      if reading_name {
        name_end = pos; // End before the non-alphanumeric character
        break;
      }
      pos += 1;
    }
  });

  if !reading_name {
    return None;
  }

  if name_end == 0 {
    name_end = pos;
  }

  Some((name, name_start, name_end))
}

fn complete_commands(start: &str) -> Vec<Candidate> {
  let mut candidates: Vec<Candidate> = read_meta(|m| {
    m.cached_utils()
      .map(Candidate::from)
      .filter(|c| c.is_match(start))
      .collect()
  });

  if read_shopts(|o| o.core.autocd) {
    let dirs = complete_dirs(start);
    candidates.extend(dirs);
  }

  candidates.sort();
  candidates
}

fn complete_dirs(start: &str) -> Vec<Candidate> {
  let filenames = complete_filename(start);

  filenames
    .into_iter()
    .filter(|f| {
      std::fs::metadata(&f.content)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    })
    .collect()
}

fn complete_filename(start: &str) -> Vec<Candidate> {
  let mut candidates = vec![];
  let has_dotslash = start.starts_with("./");

  // Split path into directory and filename parts
  // Use "." if start is empty (e.g., after "foo=")
  let path = PathBuf::from(if start.is_empty() { "." } else { start });
  let (dir, prefix) = if start.ends_with('/') || start.is_empty() {
    // Completing inside a directory: "src/" -> dir="src/", prefix=""
    (path, "")
  } else if let Some(parent) = path.parent()
    && !parent.as_os_str().is_empty()
  {
    // Has directory component: "src/ma" -> dir="src", prefix="ma"
    (
      parent.to_path_buf(),
      path.file_name().unwrap().to_str().unwrap_or(""),
    )
  } else {
    // No directory: "fil" -> dir=".", prefix="fil"
    (PathBuf::from("."), start)
  };

  let Ok(entries) = std::fs::read_dir(&dir) else {
    return candidates;
  };

  for entry in entries.flatten() {
    let file_name = entry.file_name();
    let file_str: Candidate = file_name.to_string_lossy().to_string().into();

    // Skip hidden files unless explicitly requested
    if !prefix.starts_with('.') && file_str.content.starts_with('.') {
      continue;
    }

    if file_str.is_match(prefix) {
      // Reconstruct full path
      let mut full_path = dir.join(&file_name);

      // Add trailing slash for directories
      if entry.metadata().map(|m| m.is_dir()).unwrap_or(false) {
        full_path.push(""); // adds trailing /
      }

      let mut path_raw = full_path.to_string_lossy().to_string();
      if path_raw.starts_with("./") && !has_dotslash {
        path_raw = path_raw.trim_start_matches("./").to_string();
      }

      candidates.push(path_raw.into());
    }
  }

  candidates.sort();
  candidates
}

pub enum CompSpecResult {
  NoSpec, // No compspec registered
  NoMatch {
    flags: CompOptFlags,
  }, /* Compspec found but no candidates matched, returns
           * behavior flags */
  Match {
    result: CompResult,
    flags: CompOptFlags,
  }, // Compspec found and candidates returned
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
  pub fn exec_comp_func(&self, ctx: &CompContext) -> ShResult<Vec<Candidate>> {
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

    let comp_reply = read_vars(|v| v.get_arr_elems("COMPREPLY"))
      .unwrap_or_default()
      .into_iter()
      .map(Candidate::from)
      .collect();

    Ok(comp_reply)
  }
}

impl CompSpec for BashCompSpec {
  fn complete(&self, ctx: &CompContext) -> ShResult<Vec<Candidate>> {
    let mut candidates: Vec<Candidate> = vec![];
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
      candidates.extend(complete_vars_raw(&expanded));
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
      candidates.extend(
        words
          .iter()
          .map(Candidate::from)
          .filter(|w| w.is_match(&expanded)),
      );
    }
    if self.function.is_some() {
      candidates.extend(self.exec_comp_func(ctx)?);
    }
    candidates = candidates
      .into_iter()
      .map(|c| {
        let stripped = c.content.strip_prefix(&expanded).unwrap_or_default();
        format!("{prefix}{stripped}").into()
      })
      .collect();

    candidates.sort_by_key(|c| c.content.len()); // sort by length to prioritize shorter completions, ties are then sorted alphabetically

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
  fn complete(&self, ctx: &CompContext) -> ShResult<Vec<Candidate>>;
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
  Single { result: Candidate },
  Many { candidates: Vec<Candidate> },
}

impl CompResult {
  pub fn from_candidates(mut candidates: Vec<Candidate>) -> Self {
    if candidates.is_empty() {
      Self::NoMatch
    } else if candidates.len() == 1 {
      Self::Single {
        result: candidates.remove(0),
      }
    } else {
      Self::Many { candidates }
    }
  }
}

pub enum CompResponse {
  Passthrough,       // key falls through
  Accept(Candidate), // user accepted completion
  Dismiss,           // user canceled completion
  Consumed,          // key was handled, but completion remains active
}

pub enum SelectorResponse {
  Accept(Candidate),
  Dismiss,
  Consumed,
}

pub trait Completer {
  fn complete(
    &mut self,
    line: String,
    cursor_pos: usize,
    direction: i32,
  ) -> ShResult<Option<String>>;
  fn reset(&mut self);
  fn reset_stay_active(&mut self);
  fn is_active(&self) -> bool;
  fn all_candidates(&self) -> Vec<Candidate> {
    vec![]
  }
  fn selected_candidate(&self) -> Option<Candidate>;
  fn token_span(&self) -> (usize, usize);
  fn original_input(&self) -> &str;
  fn token(&self) -> &str {
    let orig = self.original_input();
    let (s, e) = self.token_span();
    orig.get(s..e).unwrap_or(orig)
  }
  fn draw(&mut self, writer: &mut TermWriter) -> ShResult<usize>;
  fn clear(&mut self, _writer: &mut TermWriter) -> ShResult<()> {
    Ok(())
  }
  fn set_prompt_line_context(&mut self, _line_width: u16, _cursor_col: u16) {}
  fn handle_key(&mut self, key: K) -> ShResult<CompResponse>;
  fn get_completed_line(&self, candidate: &str) -> String;
}

#[derive(Default, Debug, Clone)]
pub struct ScoredCandidate {
  pub candidate: Candidate,
  pub score: Option<i32>,
  pub penalize_len_diff: bool,
}

impl ScoredCandidate {
  const BONUS_BOUNDARY: i32 = 10;
  const BONUS_CONSECUTIVE: i32 = 8;
  const BONUS_FIRST_CHAR: i32 = 5;
  const PENALTY_GAP_START: i32 = 3;
  const PENALTY_GAP_EXTEND: i32 = 1;

  pub fn new(candidate: Candidate) -> Self {
    Self {
      candidate,
      score: None,
      penalize_len_diff: false,
    }
  }
  pub fn with_len_penalty(mut self, enable: bool) -> Self {
    self.penalize_len_diff = enable;
    self
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
    let candidate_chars: Vec<char> = self.candidate.chars().collect();
    let mut indices = vec![];
    let mut qi = 0;
    for (ci, c_ch) in self.candidate.chars().enumerate() {
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

      if idx == 0 || Self::is_word_bound(candidate_chars[idx - 1], candidate_chars[idx]) {
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

    if self.penalize_len_diff {
      let len_diff = (candidate_chars.len() as isize - query_chars.len() as isize).unsigned_abs();
      let len_penalty = (len_diff as i32) * 2;
      score -= len_penalty;
    }

    self.score = Some(score);
    score
  }
}

impl From<String> for ScoredCandidate {
  fn from(content: String) -> Self {
    Self {
      candidate: content.into(),
      score: None,
      penalize_len_diff: false,
    }
  }
}

impl From<Candidate> for ScoredCandidate {
  fn from(candidate: Candidate) -> Self {
    Self {
      candidate,
      score: None,
      penalize_len_diff: false,
    }
  }
}

#[derive(Debug, Clone)]
pub struct FuzzyLayout {
  rows: u16,
  cols: u16,
  cursor_col: u16,
  /// Width of the prompt line above the `\n` that starts the fuzzy window.
  /// If PSR was drawn, this is `t_cols`; otherwise the content width.
  preceding_line_width: u16,
  /// Cursor column on the prompt line before the fuzzy window was drawn.
  preceding_cursor_col: u16,
}

#[derive(Default, Debug, Clone)]
pub struct QueryEditor {
  mode: ViInsert,
  scroll_offset: usize,
  available_width: usize,
  linebuf: LineBuf,
}

impl QueryEditor {
  pub fn clear(&mut self) {
    self.linebuf = LineBuf::new();
    self.mode = ViInsert::default();
    self.scroll_offset = 0;
  }
  pub fn set_available_width(&mut self, width: usize) {
    self.available_width = width;
  }
  pub fn update_scroll_offset(&mut self) {
    let cursor_pos = self.linebuf.cursor_to_flat();
    if cursor_pos < self.scroll_offset + 1 {
      self.scroll_offset = self.linebuf.cursor_to_flat().saturating_sub(1)
    }
    if cursor_pos >= self.scroll_offset + self.available_width.saturating_sub(1) {
      self.scroll_offset = self
        .linebuf
        .cursor_to_flat()
        .saturating_sub(self.available_width.saturating_sub(1));
    }
    let max_offset = self
      .linebuf
      .count_graphemes()
      .saturating_sub(self.available_width);
    self.scroll_offset = self.scroll_offset.min(max_offset);
  }
  pub fn get_window(&mut self) -> String {
    let buf_len = self.linebuf.count_graphemes();
    if buf_len <= self.available_width {
      return self.linebuf.joined();
    }
    let start = self
      .scroll_offset
      .min(buf_len.saturating_sub(self.available_width));
    let end = (start + self.available_width).min(buf_len);
    self.linebuf.slice(start..end).unwrap_or_default()
  }
  pub fn handle_key(&mut self, key: K) -> ShResult<()> {
    let Some(cmd) = self.mode.handle_key(key) else {
      return Ok(());
    };
    self.linebuf.exec_cmd(cmd)
  }
}

#[derive(Clone, Default, Debug)]
pub struct FuzzySelector {
  query: QueryEditor,
  filtered: Vec<ScoredCandidate>,
  candidates: Vec<Candidate>,
  cursor: ClampedUsize,
  number_candidates: bool,
  old_layout: Option<FuzzyLayout>,
  max_height: usize,
  scroll_offset: usize,
  active: bool,
  prompt_line_width: u16,
  prompt_cursor_col: u16,
  title: String,
}

#[derive(Clone, Debug)]
pub struct FuzzyCompleter {
  completer: SimpleCompleter,
  pub selector: FuzzySelector,
}

impl FuzzySelector {
  const BOT_LEFT: &str = "\x1b[90m╰\x1b[0m";
  const BOT_RIGHT: &str = "\x1b[90m╯\x1b[0m";
  const TOP_LEFT: &str = "\x1b[90m╭\x1b[0m";
  const TOP_RIGHT: &str = "\x1b[90m╮\x1b[0m";
  const HOR_LINE: &str = "\x1b[90m─\x1b[0m";
  const VERT_LINE: &str = "\x1b[90m│\x1b[0m";
  const SELECTOR_GRAY: &str = "\x1b[90m▌\x1b[0m";
  const SELECTOR_HL: &str = "\x1b[38;2;200;0;120m▌\x1b[1;39;48;5;237m";
  const PROMPT_ARROW: &str = "\x1b[1;36m>\x1b[0m";
  const TREE_LEFT: &str = "\x1b[90m├\x1b[0m";
  const TREE_RIGHT: &str = "\x1b[90m┤\x1b[0m";

  pub fn new(title: impl Into<String>) -> Self {
    Self {
      max_height: 8,
      query: QueryEditor::default(),
      filtered: vec![],
      candidates: vec![],
      cursor: ClampedUsize::new(0, 0, true),
      number_candidates: false,
      old_layout: None,
      scroll_offset: 0,
      active: false,
      prompt_line_width: 0,
      prompt_cursor_col: 0,
      title: title.into(),
    }
  }

  pub fn number_candidates(self, enable: bool) -> Self {
    Self {
      number_candidates: enable,
      ..self
    }
  }

  pub fn candidates(&self) -> &[Candidate] {
    &self.candidates
  }

  pub fn filtered(&self) -> &[ScoredCandidate] {
    &self.filtered
  }

  pub fn filtered_len(&self) -> usize {
    self.filtered.len()
  }

  pub fn candidates_len(&self) -> usize {
    self.candidates.len()
  }

  pub fn activate(&mut self, candidates: Vec<Candidate>) {
    self.active = true;
    self.candidates = candidates;
    self.score_candidates();
  }

  pub fn set_query(&mut self, query: String) {
    self.query.linebuf = LineBuf::new().with_initial(&query, query.len());
    self.query.update_scroll_offset();
    self.score_candidates();
  }

  pub fn reset(&mut self) {
    self.query.clear();
    self.filtered.clear();
    self.candidates.clear();
    self.cursor = ClampedUsize::new(0, 0, true);
    self.old_layout = None;
    self.scroll_offset = 0;
    self.active = false;
  }

  pub fn reset_stay_active(&mut self) {
    if self.active {
      self.query.clear();
      self.score_candidates();
    }
  }

  pub fn is_active(&self) -> bool {
    self.active
  }

  pub fn selected_candidate(&self) -> Option<Candidate> {
    self
      .filtered
      .get(self.cursor.get())
      .map(|c| c.candidate.clone())
  }

  pub fn set_prompt_line_context(&mut self, line_width: u16, cursor_col: u16) {
    self.prompt_line_width = line_width;
    self.prompt_cursor_col = cursor_col;
  }

  fn candidate_height(&self, idx: usize) -> usize {
    self
      .filtered
      .get(idx)
      .map(|c| c.candidate.content().trim_end().lines().count().max(1))
      .unwrap_or(1)
  }

  fn get_window(&mut self) -> &[ScoredCandidate] {
    self.update_scroll_offset();

    let mut lines = 0;
    let mut end = self.scroll_offset;
    while end < self.filtered.len() {
      if lines >= self.max_height {
        break;
      }
      lines += self.candidate_height(end);
      end += 1;
    }

    &self.filtered[self.scroll_offset..end]
  }

  pub fn update_scroll_offset(&mut self) {
    let cursor = self.cursor.get();

    // Scroll up: cursor above window
    if cursor < self.scroll_offset {
      self.scroll_offset = cursor;
      return;
    }

    // Scroll down: ensure all candidates from scroll_offset through cursor
    // fit within max_height rendered lines
    loop {
      let mut lines = 0;
      let last = cursor.min(self.filtered.len().saturating_sub(1));
      for idx in self.scroll_offset..=last {
        lines += self.candidate_height(idx);
      }
      if lines <= self.max_height || self.scroll_offset >= cursor {
        break;
      }
      self.scroll_offset += 1;
    }
  }

  pub fn score_candidates(&mut self) {
    let mut scored: Vec<_> = self
      .candidates
      .clone()
      .into_iter()
      .filter_map(|c| {
        let mut sc = ScoredCandidate::new(c);
        let score = sc.fuzzy_score(&self.query.linebuf.joined());
        if score > i32::MIN { Some(sc) } else { None }
      })
      .collect();
    scored.sort_by_key(|sc| sc.score.unwrap_or(i32::MIN));
    scored.reverse();
    self.cursor.set_max(scored.len());
    self.filtered = scored;
  }

  pub fn handle_key(&mut self, key: K) -> ShResult<SelectorResponse> {
    match key {
      K(C::Char('D'), M::CTRL) | K(C::Esc, M::NONE) => {
        self.active = false;
        self.filtered.clear();
        Ok(SelectorResponse::Dismiss)
      }
      K(C::Enter, M::NONE) => {
        self.active = false;
        if let Some(selected) = self.filtered.get(self.cursor.get()) {
          Ok(SelectorResponse::Accept(selected.candidate.clone()))
        } else {
          Ok(SelectorResponse::Dismiss)
        }
      }
      K(C::Tab, M::SHIFT) | K(C::Up, M::NONE) => {
        self.cursor.wrap_sub(1);
        self.update_scroll_offset();
        Ok(SelectorResponse::Consumed)
      }
      K(C::Tab, M::NONE) | K(C::Down, M::NONE) => {
        self.cursor.wrap_add(1);
        self.update_scroll_offset();
        Ok(SelectorResponse::Consumed)
      }
      _ => {
        self.query.handle_key(key)?;
        self.score_candidates();
        Ok(SelectorResponse::Consumed)
      }
    }
  }

  pub fn draw(&mut self, writer: &mut TermWriter) -> ShResult<usize> {
    if !self.active {
      return Ok(0);
    }
    let (cols, _) = get_win_size(*TTY_FILENO);

    let mut buf = String::new();
    let cursor_pos = self.cursor.get();
    let offset = self.scroll_offset;
    self
      .query
      .set_available_width(cols.saturating_sub(6) as usize);
    self.query.update_scroll_offset();
    let query = self.query.get_window();
    let num_filtered = format!("\x1b[33m{}\x1b[0m", self.filtered.len());
    let num_candidates = format!("\x1b[33m{}\x1b[0m", self.candidates.len());
    let title = self.title.clone();
    let title_width = title.len() as u16;
    let number_candidates = self.number_candidates;
    let min_pad = self
      .candidates
      .len()
      .to_string()
      .len()
      .saturating_add(1)
      .max(6);
    let max_height = self.max_height;
    let visible = self.get_window();
    let mut rows: u16 = 0;
    let top_bar = format!(
      "\n{}{} \x1b[1m{}\x1b[0m {}{}",
      Self::TOP_LEFT,
      Self::HOR_LINE,
      title,
      Self::HOR_LINE.repeat(cols.saturating_sub(title_width + 5) as usize),
      Self::TOP_RIGHT
    );
    buf.push_str(&top_bar);
    rows += 1;
    for _ in 0..rows {}

    let prompt = format!("{} {} {}", Self::VERT_LINE, Self::PROMPT_ARROW, &query);
    let cols_used = calc_str_width(&prompt);
    let right_pad = " ".repeat(cols.saturating_sub(cols_used + 1) as usize);
    let prompt_line_final = format!("{}{}{}", prompt, right_pad, Self::VERT_LINE);
    buf.push_str(&prompt_line_final);
    rows += 1;

    let sep_line_left = format!(
      "{}{}{}/{}",
      Self::TREE_LEFT,
      Self::HOR_LINE.repeat(2),
      &num_filtered,
      &num_candidates
    );
    let cols_used = calc_str_width(&sep_line_left);
    let right_pad = Self::HOR_LINE.repeat(cols.saturating_sub(cols_used + 1) as usize);
    let sep_line_final = format!("{}{}{}", sep_line_left, right_pad, Self::TREE_RIGHT);
    buf.push_str(&sep_line_final);
    rows += 1;

    let mut lines_drawn = 0;
    for (i, s_cand) in visible.iter().enumerate() {
      if lines_drawn >= max_height {
        break;
      }
      let selector = if i + offset == cursor_pos {
        Self::SELECTOR_HL
      } else {
        Self::SELECTOR_GRAY
      };
      let mut drew_number = false;
      for line in s_cand.candidate.content().trim_end().lines() {
        if lines_drawn >= max_height {
          break;
        }
        let mut line = line.trim_end().replace('\t', "    ");
        let col_lim = if number_candidates {
          cols.saturating_sub(3 + min_pad as u16)
        } else {
          cols.saturating_sub(3)
        };
        if calc_str_width(&line) >= col_lim {
          line.truncate(col_lim.saturating_sub(6) as usize);
          line.push_str("...");
        }
        let left = if number_candidates {
          if !drew_number {
            let this_num = i + offset + 1;
            let right_pad = " ".repeat(min_pad.saturating_sub(this_num.to_string().len()));
            format!(
              "{} {}\x1b[33m{}\x1b[39m{right_pad}{}\x1b[0m",
              Self::VERT_LINE,
              &selector,
              i + offset + 1,
              &line
            )
          } else {
            let right_pad = " ".repeat(min_pad);
            format!(
              "{} {}{}{}\x1b[0m",
              Self::VERT_LINE,
              &selector,
              right_pad,
              &line
            )
          }
        } else {
          format!("{} {}{}\x1b[0m", Self::VERT_LINE, &selector, &line)
        };
        let cols_used = calc_str_width(&left);
        let right_pad = " ".repeat(cols.saturating_sub(cols_used + 1) as usize);
        let hl_cand_line = format!("{}{}{}", left, right_pad, Self::VERT_LINE);
        buf.push_str(&hl_cand_line);
        rows += 1;
        drew_number = true;
        lines_drawn += 1;
      }
    }

    let bot_bar = format!(
      "{}{}{}",
      Self::BOT_LEFT,
      Self::HOR_LINE
        .to_string()
        .repeat(cols.saturating_sub(2) as usize),
      Self::BOT_RIGHT
    );
    buf.push_str(&bot_bar);
    rows += 1;

    let lines_below_prompt = rows.saturating_sub(2);
    let cursor_in_window = self
      .query
      .linebuf
      .cursor_to_flat()
      .saturating_sub(self.query.scroll_offset);
    let cursor_col = (cursor_in_window + 4) as u16;
    write!(buf, "\x1b[{}A\r\x1b[{}C", lines_below_prompt, cursor_col).unwrap();

    let new_layout = FuzzyLayout {
      rows,
      cols,
      cursor_col,
      preceding_line_width: self.prompt_line_width,
      preceding_cursor_col: self.prompt_cursor_col,
    };
    writer.flush_write(&buf)?;
    self.old_layout = Some(new_layout);

    Ok(rows as usize)
  }

  pub fn clear(&mut self, writer: &mut TermWriter) -> ShResult<()> {
    if let Some(layout) = self.old_layout.take() {
      let (new_cols, _) = get_win_size(*TTY_FILENO);
      let total_cells = layout.rows as u32 * layout.cols as u32;
      let physical_rows = if new_cols > 0 {
        total_cells.div_ceil(new_cols as u32) as u16
      } else {
        layout.rows
      };
      let cursor_offset = layout.cols as u32 + layout.cursor_col as u32;
      let cursor_phys_row = if new_cols > 0 {
        (cursor_offset / new_cols as u32) as u16
      } else {
        1
      };
      let lines_below = physical_rows.saturating_sub(cursor_phys_row + 1);

      let gap_extra = if new_cols > 0 && layout.preceding_line_width > new_cols {
        let wrap_rows = (layout.preceding_line_width as u32).div_ceil(new_cols as u32) as u16;
        let cursor_wrap_row = layout.preceding_cursor_col / new_cols;
        wrap_rows.saturating_sub(cursor_wrap_row + 1)
      } else {
        0
      };

      let mut buf = String::new();
      if lines_below > 0 {
        write!(buf, "\x1b[{}B", lines_below).unwrap();
      }
      for _ in 0..physical_rows {
        buf.push_str("\x1b[2K\x1b[A");
      }
      buf.push_str("\x1b[2K");
      for _ in 0..gap_extra {
        buf.push_str("\x1b[A\x1b[2K");
      }
      writer.flush_write(&buf)?;
    }
    Ok(())
  }
}

impl Default for FuzzyCompleter {
  fn default() -> Self {
    Self {
      completer: SimpleCompleter::default(),
      selector: FuzzySelector::new("Complete"),
    }
  }
}

impl Completer for FuzzyCompleter {
  fn all_candidates(&self) -> Vec<Candidate> {
    self.selector.candidates.clone()
  }
  fn set_prompt_line_context(&mut self, line_width: u16, cursor_col: u16) {
    self
      .selector
      .set_prompt_line_context(line_width, cursor_col);
  }
  fn reset_stay_active(&mut self) {
    self.selector.reset_stay_active();
  }
  fn get_completed_line(&self, _candidate: &str) -> String {
    log::debug!("Getting completed line for candidate: {}", _candidate);

    let selected = self.selector.selected_candidate().unwrap_or_default();
    let (mut start, end) = self.completer.token_span;
    let slice = self
      .completer
      .original_input
      .get(start..end)
      .unwrap_or_default();
    let ignore_case = read_shopts(|o| o.prompt.completion_ignore_case);
    let (prefix, completion) = if ignore_case {
      // Replace the filename part (after last /) with the candidate's casing
      // but preserve any unexpanded prefix like $VAR/
      if let Some(last_sep) = slice.rfind('/') {
        let prefix_end = start + last_sep + 1;
        let trailing_slash = selected.ends_with('/');
        let trimmed = selected.trim_end_matches('/');
        let mut basename = trimmed.rsplit('/').next().unwrap_or(&selected).to_string();
        if trailing_slash {
          basename.push('/');
        }
        (
          self.completer.original_input[..prefix_end].to_string(),
          basename.into(),
        )
      } else {
        (
          self.completer.original_input[..start].to_string(),
          selected.clone(),
        )
      }
    } else {
      start += slice.width();
      let completion = selected
        .strip_prefix(slice)
        .unwrap_or(selected.content().to_string());
      (
        self.completer.original_input[..start].to_string(),
        completion.into(),
      )
    };
    let escaped = escape_str(&completion, false);
    log::debug!(
      "Prefix: '{}', Completion: '{}', Escaped: '{}'",
      prefix,
      completion,
      escaped
    );
    let ret = format!(
      "{}{}{}",
      prefix,
      escaped,
      &self.completer.original_input[end..]
    );
    log::debug!("Completed line: {}", ret);
    ret
  }
  fn complete(
    &mut self,
    line: String,
    cursor_pos: usize,
    direction: i32,
  ) -> ShResult<Option<String>> {
    self.completer.complete(line, cursor_pos, direction)?;
    let candidates: Vec<_> = self.completer.candidates.clone();
    if candidates.is_empty() {
      self.completer.reset();
      self.selector.active = false;
      return Ok(None);
    } else if candidates.len() == 1 {
      self.selector.filtered = candidates.into_iter().map(ScoredCandidate::from).collect();
      let selected = self.selector.filtered[0].candidate.content().to_string();
      let completed = self.get_completed_line(&selected);
      self.selector.active = false;
      return Ok(Some(completed));
    }
    self.selector.activate(candidates);
    Ok(None)
  }

  fn handle_key(&mut self, key: K) -> ShResult<CompResponse> {
    match self.selector.handle_key(key)? {
      SelectorResponse::Accept(s) => Ok(CompResponse::Accept(s)),
      SelectorResponse::Dismiss => Ok(CompResponse::Dismiss),
      SelectorResponse::Consumed => Ok(CompResponse::Consumed),
    }
  }
  fn clear(&mut self, writer: &mut TermWriter) -> ShResult<()> {
    self.selector.clear(writer)
  }
  fn draw(&mut self, writer: &mut TermWriter) -> ShResult<usize> {
    self.selector.draw(writer)
  }
  fn reset(&mut self) {
    self.completer.reset();
    self.selector.reset();
  }
  fn token_span(&self) -> (usize, usize) {
    self.completer.token_span()
  }
  fn is_active(&self) -> bool {
    self.selector.is_active()
  }
  fn selected_candidate(&self) -> Option<Candidate> {
    self.selector.selected_candidate()
  }
  fn original_input(&self) -> &str {
    &self.completer.original_input
  }
}

#[derive(Default, Debug, Clone)]
pub struct SimpleCompleter {
  pub candidates: Vec<Candidate>,
  pub selected_idx: usize,
  pub original_input: String,
  pub token_span: (usize, usize),
  pub active: bool,
  pub dirs_only: bool,
  pub add_space: bool,
}

impl Completer for SimpleCompleter {
  fn all_candidates(&self) -> Vec<Candidate> {
    self.candidates.clone()
  }
  fn reset_stay_active(&mut self) {
    let active = self.is_active();
    self.reset();
    self.active = active;
  }
  fn get_completed_line(&self, _candidate: &str) -> String {
    self.get_completed_line()
  }
  fn complete(
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

  fn reset(&mut self) {
    *self = Self::default();
  }

  fn is_active(&self) -> bool {
    self.active
  }

  fn selected_candidate(&self) -> Option<Candidate> {
    self.candidates.get(self.selected_idx).cloned()
  }

  fn token_span(&self) -> (usize, usize) {
    self.token_span
  }

  fn draw(&mut self, _writer: &mut TermWriter) -> ShResult<usize> {
    Ok(0)
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
            Candidate::from(format!("{} ", c))
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
    let (mut start, end) = self.token_span;
    let slice = self.original_input.get(start..end).unwrap_or("");
    let ignore_case = read_shopts(|o| o.prompt.completion_ignore_case);
    let (prefix, completion) = if ignore_case {
      if let Some(last_sep) = slice.rfind('/') {
        let prefix_end = start + last_sep + 1;
        let trailing_slash = selected.ends_with('/');
        let trimmed = selected.trim_end_matches('/');
        let mut basename = trimmed
          .rsplit('/')
          .next()
          .unwrap_or(selected.as_str())
          .to_string();
        if trailing_slash {
          basename.push('/');
        }
        (self.original_input[..prefix_end].to_string(), basename)
      } else {
        (
          self.original_input[..start].to_string(),
          selected.to_string(),
        )
      }
    } else {
      start += slice.width();
      let completion = selected.strip_prefix(slice).unwrap_or(selected.to_string());
      (self.original_input[..start].to_string(), completion)
    };
    let escaped = escape_str(&completion, false);
    format!("{}{}{}", prefix, escaped, &self.original_input[end..])
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
      .position(|tk| cursor_pos >= tk.span.range().start && cursor_pos <= tk.span.range().end)
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
    let source: Rc<str> = line.into();
    let tokens = lex::LexStream::new(source.clone(), LexFlags::LEX_UNFINISHED)
      .collect::<ShResult<Vec<Tk>>>()?;

    let ctx = self.build_comp_ctx(&tokens, &source, cursor_pos)?;

    // Set token_span from CompContext's current word
    if let Some(cur) = ctx.words.get(ctx.cword) {
      self.token_span = (cur.span.range().start, cur.span.range().end);
    } else {
      self.token_span = (cursor_pos, cursor_pos);
    }

    // Use marker-based context detection for sub-token awareness (e.g. VAR_SUB
    // inside a token). Run this before comp specs so variable completions take
    // priority over programmable completion.
    let (mut marker_ctx, token_start) = self.get_subtoken_completion(&source, cursor_pos);

    if marker_ctx.last() == Some(&markers::VAR_SUB)
      && let Some(cur) = ctx.words.get(ctx.cword)
    {
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
      let end_pos = source.len();
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

    // If token contains any COMP_WORDBREAKS, break the word
    let token_str = cur_token.span.as_str();

    let word_breaks = read_vars(|v| v.try_get_var("COMP_WORDBREAKS")).unwrap_or("=".into());
    if let Some(break_pos) = token_str.rfind(|c: char| word_breaks.contains(c)) {
      self.token_span.0 = cur_token.span.range().start + break_pos + 1;
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
    // $SOME_PATH/file.txt Skip for var completions - complete_vars already
    // returns the full $VAR form
    let is_var_completion = last_marker == Some(markers::VAR_SUB)
      && !candidates.is_empty()
      && candidates.iter().any(|c| c.starts_with('$'));
    let ignore_case = read_shopts(|o| o.prompt.completion_ignore_case);
    if !is_var_completion && !ignore_case {
      candidates = candidates
        .into_iter()
        .map(|c| match c.strip_prefix(&expanded) {
          Some(suffix) => Candidate::from(format!("{raw_tk}{suffix}")),
          None => c,
        })
        .collect();
    }

    let limit = crate::state::read_shopts(|s| s.prompt.comp_limit);
    candidates.truncate(limit);

    Ok(CompResult::from_candidates(candidates))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{
    readline::{Prompt, ShedLine},
    state::{VarFlags, VarKind, write_vars},
    testutil::TestGuard,
  };
  use std::os::fd::AsRawFd;

  fn test_vi(initial: &str) -> (ShedLine, TestGuard) {
    let g = TestGuard::new();
    let prompt = Prompt::default();
    let vi = ShedLine::new_no_hist(prompt, g.pty_slave().as_raw_fd())
      .unwrap()
      .with_initial(initial);
    (vi, g)
  }

  // ===================== extract_var_name =====================

  #[test]
  fn extract_var_simple() {
    let (name, start, end) = extract_var_name("$HOME").unwrap();
    assert_eq!(name, "HOME");
    assert_eq!(start, 1);
    assert_eq!(end, 5);
  }

  #[test]
  fn extract_var_braced() {
    let (name, start, end) = extract_var_name("${PATH}").unwrap();
    assert_eq!(name, "PATH");
    // '$' hits continue (no pos++), '{' is at pos=0, so name_start = 1
    assert_eq!(start, 1);
    assert_eq!(end, 5);
  }

  #[test]
  fn extract_var_partial() {
    let (name, start, _end) = extract_var_name("$HO").unwrap();
    assert_eq!(name, "HO");
    assert_eq!(start, 1);
  }

  #[test]
  fn extract_var_none() {
    assert!(extract_var_name("hello").is_none());
  }

  // ===================== ScoredCandidate::fuzzy_score =====================

  #[test]
  fn fuzzy_exact_match() {
    let mut c = ScoredCandidate::new("hello".into());
    let score = c.fuzzy_score("hello");
    assert!(score > 0);
  }

  #[test]
  fn fuzzy_prefix_match() {
    let mut c = ScoredCandidate::new("hello_world".into());
    let score = c.fuzzy_score("hello");
    assert!(score > 0);
  }

  #[test]
  fn fuzzy_no_match() {
    let mut c = ScoredCandidate::new("abc".into());
    let score = c.fuzzy_score("xyz");
    assert_eq!(score, i32::MIN);
  }

  #[test]
  fn fuzzy_empty_query() {
    let mut c = ScoredCandidate::new("anything".into());
    let score = c.fuzzy_score("");
    assert_eq!(score, 0);
  }

  #[test]
  fn fuzzy_boundary_bonus() {
    let mut a = ScoredCandidate::new("foo_bar".into());
    let mut b = ScoredCandidate::new("fxxxbxr".into());
    let score_a = a.fuzzy_score("fbr");
    let score_b = b.fuzzy_score("fbr");
    // word-boundary match should score higher
    assert!(score_a > score_b);
  }

  // ===================== CompResult::from_candidates =====================

  #[test]
  fn comp_result_no_match() {
    let result = CompResult::from_candidates(vec![]);
    assert!(matches!(result, CompResult::NoMatch));
  }

  #[test]
  fn comp_result_single() {
    let result = CompResult::from_candidates(vec!["foo".into()]);
    assert!(matches!(result, CompResult::Single { .. }));
  }

  #[test]
  fn comp_result_many() {
    let result = CompResult::from_candidates(vec!["foo".into(), "bar".into()]);
    assert!(matches!(result, CompResult::Many { .. }));
  }

  // ===================== complete_signals =====================

  #[test]
  fn complete_signals_int() {
    let results = complete_signals("INT");
    assert!(results.contains(&Candidate::from("INT")));
  }

  #[test]
  fn complete_signals_empty() {
    let results = complete_signals("");
    assert!(!results.is_empty());
  }

  #[test]
  fn complete_signals_no_match() {
    let results = complete_signals("ZZZZZZZ");
    assert!(results.is_empty());
  }

  // ===================== COMP_WORDBREAKS =====================

  #[test]
  fn wordbreak_equals_default() {
    let _g = TestGuard::new();
    let mut comp = SimpleCompleter::new();

    let line = "cmd --foo=bar".to_string();
    let cursor = line.len();
    let _ = comp.get_candidates(line.clone(), cursor);

    let eq_idx = line.find('=').unwrap();
    assert_eq!(
      comp.token_span.0,
      eq_idx + 1,
      "token_span.0 ({}) should be right after '=' ({})",
      comp.token_span.0,
      eq_idx
    );
  }

  #[test]
  fn wordbreak_colon_when_set() {
    let _g = TestGuard::new();
    write_vars(|v| v.set_var("COMP_WORDBREAKS", VarKind::Str("=:".into()), VarFlags::NONE))
      .unwrap();

    let mut comp = SimpleCompleter::new();
    let line = "scp host:foo".to_string();
    let cursor = line.len();
    let _ = comp.get_candidates(line.clone(), cursor);

    let colon_idx = line.find(':').unwrap();
    assert_eq!(
      comp.token_span.0,
      colon_idx + 1,
      "token_span.0 ({}) should be right after ':' ({})",
      comp.token_span.0,
      colon_idx
    );
  }

  #[test]
  fn wordbreak_rightmost_wins() {
    let _g = TestGuard::new();
    write_vars(|v| v.set_var("COMP_WORDBREAKS", VarKind::Str("=:".into()), VarFlags::NONE))
      .unwrap();

    let mut comp = SimpleCompleter::new();
    let line = "cmd --opt=host:val".to_string();
    let cursor = line.len();
    let _ = comp.get_candidates(line.clone(), cursor);

    let colon_idx = line.rfind(':').unwrap();
    assert_eq!(
      comp.token_span.0,
      colon_idx + 1,
      "should break at rightmost wordbreak char"
    );
  }

  // ===================== SimpleCompleter cycling =====================

  #[test]
  fn cycle_wraps_forward() {
    let _g = TestGuard::new();
    let mut comp = SimpleCompleter {
      candidates: vec!["aaa".into(), "bbb".into(), "ccc".into()],
      selected_idx: 2,
      original_input: "".into(),
      token_span: (0, 0),
      active: true,
      dirs_only: false,
      add_space: false,
    };
    comp.cycle_completion(1);
    assert_eq!(comp.selected_idx, 0);
  }

  #[test]
  fn cycle_wraps_backward() {
    let _g = TestGuard::new();
    let mut comp = SimpleCompleter {
      candidates: vec!["aaa".into(), "bbb".into(), "ccc".into()],
      selected_idx: 0,
      original_input: "".into(),
      token_span: (0, 0),
      active: true,
      dirs_only: false,
      add_space: false,
    };
    comp.cycle_completion(-1);
    assert_eq!(comp.selected_idx, 2);
  }

  // ===================== Completion escaping =====================

  #[test]
  fn escape_str_special_chars() {
    use crate::expand::escape_str;
    let escaped = escape_str("hello world", false);
    assert_eq!(escaped, "hello\\ world");
  }

  #[test]
  fn escape_str_multiple_specials() {
    use crate::expand::escape_str;
    let escaped = escape_str("a&b|c", false);
    assert_eq!(escaped, "a\\&b\\|c");
  }

  #[test]
  fn escape_str_no_specials() {
    use crate::expand::escape_str;
    let escaped = escape_str("hello", false);
    assert_eq!(escaped, "hello");
  }

  #[test]
  fn escape_str_all_shell_metacharacters() {
    use crate::expand::escape_str;
    for ch in [
      '\'', '"', '\\', '|', '&', ';', '(', ')', '<', '>', '$', '*', '!', '`', '{', '?', '[', '#',
      ' ', '\t', '\n',
    ] {
      let input = format!("a{ch}b");
      let escaped = escape_str(&input, false);
      let expected = format!("a\\{ch}b");
      assert_eq!(escaped, expected, "failed to escape {:?}", ch);
    }
  }

  #[test]
  fn escape_str_kitchen_sink() {
    use crate::expand::escape_str;
    let input = "f$le (with) 'spaces' & {braces} | pipes; #hash ~tilde `backtick` !bang";
    let escaped = escape_str(input, false);
    assert_eq!(
      escaped,
      "f\\$le\\ \\(with\\)\\ \\'spaces\\'\\ \\&\\ \\{braces}\\ \\|\\ pipes\\;\\ \\#hash\\ ~tilde\\ \\`backtick\\`\\ \\!bang"
    );
  }

  #[test]
  fn completed_line_only_escapes_new_text() {
    let _g = TestGuard::new();
    // Simulate: user typed "echo hel", completion candidate is "hello world"
    let comp = SimpleCompleter {
      candidates: vec!["hello world".into()],
      selected_idx: 0,
      original_input: "echo hel".into(),
      token_span: (5, 8), // "hel" spans bytes 5..8
      active: true,
      dirs_only: false,
      add_space: false,
    };
    let result = comp.get_completed_line();
    // "hel" is the user's text (not escaped), "lo world" is new (escaped)
    assert_eq!(result, "echo hello\\ world");
  }

  #[test]
  fn completed_line_no_new_text() {
    let _g = TestGuard::new();
    // User typed the full token, nothing new to escape
    let comp = SimpleCompleter {
      candidates: vec!["hello".into()],
      selected_idx: 0,
      original_input: "echo hello".into(),
      token_span: (5, 10),
      active: true,
      dirs_only: false,
      add_space: false,
    };
    let result = comp.get_completed_line();
    assert_eq!(result, "echo hello");
  }

  #[test]
  fn completed_line_appends_suffix_with_escape() {
    let _g = TestGuard::new();
    // User typed "echo hel", candidate is "hello world" (from filesystem)
    // strip_prefix("hel") => "lo world", which gets escaped
    let comp = SimpleCompleter {
      candidates: vec!["hello world".into()],
      selected_idx: 0,
      original_input: "echo hel".into(),
      token_span: (5, 8),
      active: true,
      dirs_only: false,
      add_space: false,
    };
    let result = comp.get_completed_line();
    assert_eq!(result, "echo hello\\ world");
  }

  #[test]
  fn completed_line_suffix_only_escapes_new_part() {
    let _g = TestGuard::new();
    // User typed "echo hello", candidate is "hello world&done"
    // strip_prefix("hello") => " world&done", only that gets escaped
    let comp = SimpleCompleter {
      candidates: vec!["hello world&done".into()],
      selected_idx: 0,
      original_input: "echo hello".into(),
      token_span: (5, 10),
      active: true,
      dirs_only: false,
      add_space: false,
    };
    let result = comp.get_completed_line();
    // "hello" is preserved as-is, " world&done" gets escaped
    assert_eq!(result, "echo hello\\ world\\&done");
  }

  #[test]
  fn tab_escapes_special_in_filename() {
    let tmp = std::env::temp_dir().join("shed_test_tab_esc");
    let _ = std::fs::create_dir_all(&tmp);
    std::fs::write(tmp.join("hello world.txt"), "").unwrap();

    let (mut vi, _g) = test_vi("");
    std::env::set_current_dir(&tmp).unwrap();

    vi.feed_bytes(b"echo hello\t");
    let _ = vi.process_input();

    let line = vi.editor.joined();
    assert!(
      line.contains("hello\\ world.txt"),
      "expected escaped space in completion: {line:?}"
    );

    std::fs::remove_dir_all(&tmp).ok();
  }

  #[test]
  fn tab_does_not_escape_user_text() {
    let tmp = std::env::temp_dir().join("shed_test_tab_noesc");
    let _ = std::fs::create_dir_all(&tmp);
    std::fs::write(tmp.join("my file.txt"), "").unwrap();

    let (mut vi, _g) = test_vi("");
    std::env::set_current_dir(&tmp).unwrap();

    // User types "echo my\ " with the space already escaped
    vi.feed_bytes(b"echo my\\ \t");
    let _ = vi.process_input();

    let line = vi.editor.joined();
    // The user's "my\ " should be preserved, not double-escaped to "my\\\ "
    assert!(
      !line.contains("my\\\\ "),
      "user text should not be double-escaped: {line:?}"
    );
    assert!(
      line.contains("my\\ file.txt"),
      "expected completion with preserved user escape: {line:?}"
    );

    std::fs::remove_dir_all(&tmp).ok();
  }

  // ===================== Integration tests (pty) =====================

  #[test]
  fn tab_completes_filename() {
    let tmp = std::env::temp_dir().join("shed_test_tab_fn");
    let _ = std::fs::create_dir_all(&tmp);
    std::fs::write(tmp.join("unique_shed_test_file.txt"), "").unwrap();

    let (mut vi, _g) = test_vi("");
    std::env::set_current_dir(&tmp).unwrap();

    // Type "echo unique_shed_test" then press Tab
    vi.feed_bytes(b"echo unique_shed_test\t");
    let _ = vi.process_input();

    let line = vi.editor.joined();
    assert!(
      line.contains("unique_shed_test_file.txt"),
      "expected completion in line: {line:?}"
    );

    std::fs::remove_dir_all(&tmp).ok();
  }

  #[test]
  fn tab_completes_directory_with_slash() {
    let tmp = std::env::temp_dir().join("shed_test_tab_dir");
    let _ = std::fs::create_dir_all(tmp.join("mysubdir"));

    let (mut vi, _g) = test_vi("");
    std::env::set_current_dir(&tmp).unwrap();

    vi.feed_bytes(b"cd mysub\t");
    let _ = vi.process_input();

    let line = vi.editor.joined();
    assert!(
      line.contains("mysubdir/"),
      "expected dir completion with trailing slash: {line:?}"
    );

    std::fs::remove_dir_all(&tmp).ok();
  }

  #[test]
  fn tab_after_equals() {
    let tmp = std::env::temp_dir().join("shed_test_tab_eq");
    let _ = std::fs::create_dir_all(&tmp);
    std::fs::write(tmp.join("eqfile.txt"), "").unwrap();

    let (mut vi, _g) = test_vi("");
    std::env::set_current_dir(&tmp).unwrap();

    vi.feed_bytes(b"cmd --opt=eqf\t");
    let _ = vi.process_input();

    let line = vi.editor.joined();
    assert!(
      line.contains("--opt=eqfile.txt"),
      "expected completion after '=': {line:?}"
    );

    std::fs::remove_dir_all(&tmp).ok();
  }
}
