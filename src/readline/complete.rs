use std::{
  collections::HashSet, fmt::{Debug, Display}, fs::DirEntry, os::unix::fs::PermissionsExt, path::PathBuf, rc::Rc
};

use nix::sys::signal::Signal;
use unicode_width::UnicodeWidthStr;

use crate::{
  builtin::complete::{CompFlags, CompOptFlags, CompOpts},
  expand::{escape_str, expand_raw, unescape_str},
  key,
  match_loop,
  parse::{
    execute::exec_nonint,
    lex::Span,
  },
  readline::{
    Marker, annotate_input_recursive, context::{CtxTk, CtxTkRule, get_context_tokens}, editmode::{EditMode, ViInsert}, keys::{KeyCode as C, KeyEvent as K}, linebuf::LineBuf, markers::{self, is_marker, strip_markers}, term::calc_str_width
  },
  state::{
    self, Cols, Rows, TermGuard, Utility, VarFlags, VarKind, read_jobs, read_logic, read_meta, read_shopts, read_vars, with_term, write_vars
  },
  util::{self, error::ShResult, guards::var_ctx_guard, strops::ends_with_unescaped, ui},
  write_term,
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

  pub fn sub(&mut self, n: usize) {
    self.val = self.val.saturating_sub(n);
  }
  pub fn add(&mut self, n: usize) {
    self.val = self.val.saturating_add(n).min(self.max.saturating_sub(1));
  }
}

#[derive(Debug)]
enum CompStrat {
  Var { prefix: String },
  Tilde { prefix: String },
  Command { prefix: String },
  Argument { prefix: String },
  Dirs { prefix: String },
  Files { prefix: String, parent: Option<String> },
  /// Semantic dead end, nothing can meaningfully go here. Suggest a `;` so
  /// the user can move on (e.g. inside or right after a closed subshell).
  Separator,
  /// No completion at all (mid-comment, mid-operator, inside a heredoc body).
  Null,
}

/// Compute the replacement span for a VarSub-shaped sub-token. For
/// `${name...}`-style param expansion, narrow to just `${name` so trailing
/// operators/args (`:-default`, `/pat/rep`, the closing `}`, etc.) are
/// preserved when the candidate (which includes the `${` prefix) replaces.
/// For a bare `$name` VarSub with no ParamName child, the wrapper IS the name
/// region, so we return its full span.
fn narrow_var_span(sub: &CtxTk) -> Span {
  sub
    .sub_tokens()
    .iter()
    .find(|c| matches!(c.class(), CtxTkRule::ParamName))
    .map(|name| Span::new(sub.range().start..name.range().end, sub.span().get_source()))
    .unwrap_or_else(|| sub.span().clone())
}

impl CompStrat {
  pub fn escapes_candidates(&self) -> bool {
    matches!(self,
      Self::Files { .. } | Self::Dirs { .. } | Self::Argument { .. }
    )
  }

  pub fn resolve(tks: &[CtxTk], cursor_pos: usize) -> (Self,Span) {
    // Cursor inside a token's span, complete what's currently being typed.
    if let Some(leaf) = tks.iter().find_map(|t| t.get_leaf(cursor_pos)) {
      log::debug!("Cursor in leaf {:?} with class {:?} and sub_tokens: {:?}", leaf.span().as_str(), leaf.class(), leaf.sub_tokens().iter().map(|s| s.span().as_str()).collect::<Vec<_>>());
      return Self::from_leaf(leaf, cursor_pos);
    }

    let Some(prev) = tks.iter().rfind(|t| t.range().end <= cursor_pos) else {
      log::debug!("Cursor in empty input or leading whitespace");
      return (
        Self::Command { prefix: String::new() },
        Span::new(cursor_pos..cursor_pos, "".into()),
      );
    };
    log::debug!(
      "Cursor after {:?} with class {:?}",
      prev.span().as_str(),
      prev.class(),
    );
    (
      Self::from_predecessor(prev),
      Span::new(cursor_pos..cursor_pos, prev.span().get_source()),
    )
  }

  /// Cursor is *inside* `leaf`. Returns the dispatch strategy *and* the span
  /// to replace when the candidate is selected.
  ///
  /// For Argument leaves: if the cursor is on a structurally-meaningful
  /// sub-token (VarSub, Tilde, CmdSub) we dispatch on that and the
  /// replacement targets just the sub-token's range,so `foo/$FL/bar`
  /// completing `$FL` to `$FLAKEPATH` produces `foo/$FLAKEPATH/bar`, not a
  /// graft after the last `/`. Otherwise we treat the leaf as path-shaped
  /// and target the whole leaf so `get_completed_line`'s last-`/` graft
  /// preserves the parent text.
  fn from_leaf(leaf: &CtxTk, cursor_pos: usize) -> (Self, Span) {
    if leaf.is_argument() {
      // Cursor on a structurally-meaningful sub-token wins over path slicing.
      if let Some(sub) = leaf
        .sub_tokens()
        .iter()
        .find(|s| s.range_inclusive().contains(&cursor_pos))
      {
        match sub.class() {
          CtxTkRule::VarSub | CtxTkRule::ParamName => {
            let prefix = sub.prefix_from(cursor_pos).unwrap_or_default().to_string();
            return (Self::Var { prefix }, narrow_var_span(sub));
          }
          CtxTkRule::Tilde => {
            let prefix = sub.prefix_from(cursor_pos).unwrap_or_default().to_string();
            return (Self::Tilde { prefix }, sub.span().clone());
          }
          CtxTkRule::CmdSub
          | CtxTkRule::BacktickSub
          | CtxTkRule::ProcSubIn
          | CtxTkRule::ProcSubOut
          | CtxTkRule::DoubleString
          | CtxTkRule::SingleString
          | CtxTkRule::DollarString => {
            if let Some(inner) = sub.get_leaf(cursor_pos) {
              return Self::from_leaf(inner, cursor_pos);
            }
            // Fall through to path-slicing if recursion produced nothing.
          }
          // Glob, Escape, etc., fall through to path-slicing default.
          _ => {}
        }
      }

      // Path-shaped argument: slice on the most recent unescaped `/` before
      // the cursor. `can_split_at` rejects positions inside protective
      // sub-tokens (Escape, VarSub, etc.), so escaped slashes and slashes
      // inside `$(...)` etc. are skipped automatically.
      let leaf_text = leaf.span().as_str();
      let leaf_start = leaf.range().start;
      let cursor_local = cursor_pos - leaf_start;

      let parent_end = leaf_text[..cursor_local]
        .char_indices()
        .rev()
        .find(|(byte, ch)| *ch == '/' && leaf.can_split_at(leaf_start + byte))
        .map(|(byte, _)| byte + 1)
        .unwrap_or(0);

      let prefix = leaf_text[parent_end..cursor_local].to_string();
      let parent = (parent_end > 0).then(|| leaf_text[..parent_end].to_string());

      return (Self::Files { prefix, parent }, leaf.span().clone());
    }

    let prefix = leaf.prefix_from(cursor_pos).unwrap_or_default().to_string();
    let strat = match leaf.class() {
      CtxTkRule::ValidCommand
      | CtxTkRule::InvalidCommand
      | CtxTkRule::Keyword                 => Self::Command  { prefix },
      CtxTkRule::AssignmentRight
      | CtxTkRule::CmdSub
      | CtxTkRule::BacktickSub
      | CtxTkRule::ProcSubIn
      | CtxTkRule::ProcSubOut
      | CtxTkRule::DoubleString
      | CtxTkRule::SingleString
      | CtxTkRule::DollarString            => Self::Argument { prefix },
      CtxTkRule::Glob
      | CtxTkRule::Redirect                => Self::Files    { prefix, parent: None },
      CtxTkRule::Tilde                     => Self::Tilde    { prefix },
      CtxTkRule::VarSub
      | CtxTkRule::ParamName
      | CtxTkRule::ArithVar                => Self::Var      { prefix },

      CtxTkRule::Subshell
      | CtxTkRule::BraceGroup
      | CtxTkRule::Arithmetic              => Self::Separator,

      // Argument is unreachable here (the is_argument branch handles it),
      // but listed for exhaustiveness.
      CtxTkRule::Argument
      | CtxTkRule::ArgumentFile            => Self::Files { prefix, parent: None },

      // Everything else inside a leaf means "no useful completion here".
      CtxTkRule::Comment
      | CtxTkRule::CasePattern
      | CtxTkRule::HistExp
      | CtxTkRule::Escape
      | CtxTkRule::Separator
      | CtxTkRule::ArithOp
      | CtxTkRule::ArithNumber
      | CtxTkRule::ParamPrefix
      | CtxTkRule::ParamIndex
      | CtxTkRule::ParamOp
      | CtxTkRule::ParamArg
      | CtxTkRule::AssignmentLeft
      | CtxTkRule::AssignmentOp
      | CtxTkRule::Operator
      | CtxTkRule::HereDoc
      | CtxTkRule::HereDocStart
      | CtxTkRule::HereDocBody
      | CtxTkRule::HereDocEnd
      | CtxTkRule::Null                    => Self::Null,
    };
    // VarSub/ParamName get a narrowed span (`${name`) so trailing param
    // expansion bits (`:-default`, `}`, etc.) are preserved on replace.
    let span = match leaf.class() {
      CtxTkRule::VarSub | CtxTkRule::ParamName => narrow_var_span(leaf),
      _ => leaf.span().clone(),
    };
    (strat, span)
  }

  /// Cursor is *past* `prev` (in whitespace or at end of input). The prefix
  /// is empty; what comes next depends on what we just finished.
  fn from_predecessor(prev: &CtxTk) -> Self {
    let prefix = String::new();

    match prev.class() {
      // After a finished command/argument-position token, we're typing args.
      CtxTkRule::ValidCommand
      | CtxTkRule::InvalidCommand
      | CtxTkRule::Argument
      | CtxTkRule::ArgumentFile
      | CtxTkRule::CmdSub
      | CtxTkRule::BacktickSub
      | CtxTkRule::ProcSubIn
      | CtxTkRule::ProcSubOut
      | CtxTkRule::VarSub
      | CtxTkRule::Tilde
      | CtxTkRule::Glob
      | CtxTkRule::DoubleString
      | CtxTkRule::SingleString
      | CtxTkRule::DollarString
      | CtxTkRule::AssignmentRight         => Self::Argument { prefix },

      // After a separator or operator, we're at the start of a new segment.
      CtxTkRule::Separator
      | CtxTkRule::Operator                => Self::Command  { prefix },

      // After a keyword (for/while/if/etc.) the next token is a command head.
      // TODO: split per-keyword once the cases matter (e.g. `for <var>`).
      CtxTkRule::Keyword                   => Self::Command  { prefix },

      // After a redirect we expect a file path.
      CtxTkRule::Redirect                  => Self::Files    { prefix, parent: None },

      // After a closed structural construct, semantically nothing follows
      // until a separator, suggest one.
      CtxTkRule::Subshell
      | CtxTkRule::BraceGroup
      | CtxTkRule::Arithmetic              => Self::Separator,

      // Past a comment / heredoc / odd internal-only class, no completion.
      CtxTkRule::Comment
      | CtxTkRule::HereDoc
      | CtxTkRule::HereDocStart
      | CtxTkRule::HereDocBody
      | CtxTkRule::HereDocEnd
      | CtxTkRule::CasePattern
      | CtxTkRule::HistExp
      | CtxTkRule::Escape
      | CtxTkRule::ArithOp
      | CtxTkRule::ArithNumber
      | CtxTkRule::ArithVar
      | CtxTkRule::ParamPrefix
      | CtxTkRule::ParamName
      | CtxTkRule::ParamIndex
      | CtxTkRule::ParamOp
      | CtxTkRule::ParamArg
      | CtxTkRule::AssignmentLeft
      | CtxTkRule::AssignmentOp
      | CtxTkRule::Null                    => Self::Null,
    }
  }
}

#[derive(Default, Debug, Clone)]
pub struct Candidate {
  content: String,
  desc: Option<String>,
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
      desc: None,
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
      desc: None,
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
      desc: None,
      id: None,
    }
  }
}

impl From<&str> for Candidate {
  fn from(value: &str) -> Self {
    Self {
      content: value.to_string(),
      desc: None,
      id: None,
    }
  }
}

impl From<(usize, String)> for Candidate {
  fn from(value: (usize, String)) -> Self {
    Self {
      content: value.1,
      desc: None,
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
  pub fn with_desc(mut self, desc: String) -> Self {
    self.desc = Some(desc);
    self
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
      .iter()
      .map(|(a,v)| Candidate::from(a.to_string()).with_desc(v.to_string()))
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
        .filter_map(|j| {
          let name = j.name()?;
          Some(Candidate::from(name.to_string()).with_desc(format!("{} ({})", j.pgid(), j.get_cmd_line())))
        })
        .filter(|name| name.is_match(prefix))
        .map(|name| format!("%{name}").into())
        .collect()
    })
  } else {
    read_jobs(|j| {
      j.jobs()
        .iter()
        .filter_map(|j| j.as_ref())
        .map(|j| Candidate::from(j.pgid().to_string()).with_desc(j.get_cmd_line()))
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
      .map(|s| {
        if let Some(val) = read_vars(|v| v.try_get_var(&s)) {
          Candidate::from(s).with_desc(val)
        } else {
          Candidate::from(s)
        }
      })
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
      .map(|k| {
        if let Some(val) = read_vars(|v| v.try_get_var(k)) {
          Candidate::from(k.to_string()).with_desc(val)
        } else {
          Candidate::from(k.to_string())
        }
      })
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
        // Skip past the `$`, the `{` arm below will set up reading_name.
        pos += 1;
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

fn unescape_for_completion(raw: &str) -> String {
  let unescaped = unescape_str(raw);
  expand_raw(&mut unescaped.chars().peekable())
    .map(|s| strip_markers(&s))
    .unwrap_or_else(|_| raw.to_string())
}

fn complete_files_in(prefix: &str, parent: Option<&str>) -> Vec<Candidate> {
  let prefix = unescape_for_completion(prefix);
  match parent {
    Some(parent) => {
      let expanded_parent = unescape_for_completion(parent);
      complete_filename(&format!("{expanded_parent}{prefix}"))
    }
    None => complete_filename(&prefix),
  }
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
      let cand = Candidate::from(path_raw.clone()).with_desc(file_description(&entry));

      candidates.push(cand);
    }
  }

  candidates.sort();
  candidates
}

fn file_description(file: &DirEntry) -> String {
  let Ok(meta) = file.metadata() else { return String::new(); };
  let kind = if meta.is_dir() { "dir" }
    else if file.file_type().is_ok_and(|f| f.is_symlink()) { "link" }
    else if meta.permissions().mode() & 0o111 != 0 { "exec" }
    else if meta.is_file() { "file" }
    else { "?" };

  let size = if kind != "dir" {
    util::format_size(meta.len())
  } else {
    String::from("-")
  };
  let mode = util::format_mode(meta.permissions().mode());

  format!("{kind:<4} {size:>6} {mode}")
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
    exec_nonint(input, None, Some("comp_function".into()))?;

    let comp_reply = read_vars(|v| v.get_arr_elems("COMPREPLY"))
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

    let unescaped = unescape_str(prefix.as_str());
    let expanded = expand_raw(&mut unescaped.chars().peekable())?;
    let stripped  = strip_markers(&expanded);
    if self.files {
      candidates.extend(complete_filename(&stripped));
    }
    if self.dirs {
      candidates.extend(complete_dirs(&stripped));
    }
    if self.commands {
      candidates.extend(complete_commands(&stripped));
    }
    if self.vars {
      candidates.extend(complete_vars_raw(&stripped));
    }
    if self.users {
      candidates.extend(complete_users(&stripped));
    }
    if self.jobs {
      candidates.extend(complete_jobs(&stripped));
    }
    if self.aliases {
      candidates.extend(complete_aliases(&stripped));
    }
    if self.signals {
      candidates.extend(complete_signals(&stripped));
    }
    if let Some(words) = &self.wordlist {
      candidates.extend(
        words
          .iter()
          .map(Candidate::from)
          .filter(|w| w.is_match(&stripped)),
      );
    }
    if self.function.is_some() {
      candidates.extend(self.exec_comp_func(ctx)?);
    }
    candidates = candidates
      .into_iter()
      .map(|c| {
        let stripped = c.content.strip_prefix(&stripped).unwrap_or_default();
        format!("{prefix}{stripped}").into()
      })
      .collect();

    candidates.sort_by_key(|c| c.content.len()); // sort by length to prioritize shorter completions, ties are then sorted alphabetically
    candidates.reverse();

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
  pub words: Vec<String>,
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
  fn draw(&mut self) -> ShResult<usize>;
  fn clear(&mut self) -> ShResult<()> {
    Ok(())
  }
  fn set_prompt_line_context(&mut self, _line_width: usize, _cursor_col: usize) {}
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
  top_left: usize,
  rows: usize,
  cols: usize,
  cursor_col: usize,
  /// Width of the prompt line above the `\n` that starts the fuzzy window.
  /// If PSR was drawn, this is `t_cols`; otherwise the content width.
  preceding_line_width: usize,
  /// Cursor column on the prompt line before the fuzzy window was drawn.
  preceding_cursor_col: usize,
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

#[derive(Default, Debug)]
pub struct FuzzySelector {
  query: QueryEditor,
  filtered: Vec<ScoredCandidate>,
  candidates: Vec<Candidate>,
  cursor: ClampedUsize,
  number_candidates: bool,
  old_layout: Option<FuzzyLayout>,
  max_height: usize,
  scroll_offset: usize,
  prompt_line_width: usize,
  prompt_cursor_col: usize,
  row_map: Vec<Option<usize>>,
  hovered: Option<usize>, // index of the currently hovered candidate, if any
  title: String,
  _mouse_guard: Option<TermGuard>,
}

#[derive(Debug)]
pub struct FuzzyCompleter {
  completer: SimpleCompleter,
  pub selector: FuzzySelector,
}

impl FuzzySelector {
  const SELECTOR_GRAY: &str = "\x1b[90m▌\x1b[0m";
  const SELECTOR_HL: &str = "\x1b[38;2;200;0;120m▌\x1b[1;39;48;5;237m";
  const SELECTOR_HOVER: &str = "\x1b[90m▌\x1b[1;39;48;5;237m";
  const PROMPT_ARROW: &str = "\x1b[1;36m>\x1b[0m";

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
      prompt_line_width: 0,
      row_map: vec![],
      prompt_cursor_col: 0,
      hovered: None,
      title: title.into(),
      _mouse_guard: with_term(|t| t.mouse_support_guard(true)).ok()
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
    self.candidates = candidates;
    self.score_candidates();
  }

  pub fn set_query(&mut self, query: String) {
    self.query.linebuf = LineBuf::new().with_initial(&query, query.len());
    self.query.update_scroll_offset();
    self.score_candidates();
  }

  pub fn reset_query(&mut self) {
    self.query.clear();
    self.score_candidates();
  }

  pub fn selected_candidate(&self) -> Option<Candidate> {
    self
      .filtered
      .get(self.cursor.get())
      .map(|c| c.candidate.clone())
  }

  pub fn set_prompt_line_context(&mut self, line_width: usize, cursor_col: usize) {
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

    // Scroll down: work backwards from cursor to find the
    // earliest offset that fits within max_height lines.
    let mut lines = 0;
    let mut new_offset = cursor;
    loop {
      let h = self.candidate_height(new_offset);
      if lines + h > self.max_height && new_offset < cursor {
        new_offset += 1;
        break;
      }
      lines += h;
      if new_offset == 0 {
        break;
      }
      new_offset -= 1;
    }

    if new_offset > self.scroll_offset {
      self.scroll_offset = new_offset;
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

  pub fn handle_click(&mut self, row: usize, _col: usize) -> ShResult<SelectorResponse> {
    let top_left = self.old_layout.as_ref().map(|l| l.top_left).unwrap_or(0);
    let relative_row = row.saturating_sub(top_left);
    if let Some(idx) = self.row_map.get(relative_row).copied().flatten() {
      if self.cursor.val == idx {
        Ok(SelectorResponse::Accept(self.filtered[idx].candidate.clone()))
      } else {
        self.cursor = ClampedUsize::new(idx, self.filtered.len(), true);
        Ok(SelectorResponse::Consumed)
      }
    } else {
      Ok(SelectorResponse::Consumed)
    }
  }

  pub fn handle_hover(&mut self, row: usize) -> ShResult<SelectorResponse> {
    let top_left = self.old_layout.as_ref().map(|l| l.top_left).unwrap_or(0);
    let relative_row = row.saturating_sub(top_left);
    let idx = self.row_map.get(relative_row).copied().flatten();

    if self.hovered != idx {
      self.hovered = idx;
    }

    Ok(SelectorResponse::Consumed)
  }

  pub fn handle_key(&mut self, key: K) -> ShResult<SelectorResponse> {
    match key {
      K(C::MousePos(row,_), _) => self.handle_hover(row),
      K(C::LeftClick(row,col), _) => self.handle_click(row, col),
      key!(Ctrl + 'd') | key!(Esc) => {
        self.filtered.clear();
        Ok(SelectorResponse::Dismiss)
      }
      key!(Enter) => {
        if let Some(selected) = self.filtered.get(self.cursor.get()) {
          Ok(SelectorResponse::Accept(selected.candidate.clone()))
        } else {
          Ok(SelectorResponse::Dismiss)
        }
      }
      key @(key!(ScrollUp) | key!(Shift + Tab) | key!(Up)) => {
        match key {
          key!(ScrollUp) => self.cursor.sub(1), // no wrap
          key!(Up) |
          key!(Shift + Tab) => self.cursor.wrap_sub(1), // wrap
          _ => unreachable!()
        }
        Ok(SelectorResponse::Consumed)
      }
      key @(key!(ScrollDown) | key!(Tab) | key!(Down)) => {
        match key {
          key!(ScrollDown) => self.cursor.add(1), // no wrap
          key!(Down) |
          key!(Tab) => self.cursor.wrap_add(1), // wrap
          _ => unreachable!()
        }
        self.update_scroll_offset();
        Ok(SelectorResponse::Consumed)
      }
      key!(Ctrl + 'c') => {
        self.query.clear();
        self.score_candidates();
        Ok(SelectorResponse::Consumed)
      }
      _ => {
        self.query.handle_key(key)?;
        self.score_candidates();
        Ok(SelectorResponse::Consumed)
      }
    }
  }

  pub fn draw(&mut self) -> ShResult<usize> {
    self.row_map.clear();
    let (cols,top_left) = with_term(|t| {
      (t.t_cols(), t.get_cursor_pos().ok().flatten().unwrap_or((Rows(0),Cols(0))).0.0 + 1)
    });

    let pad = |content: &str, fill: &str, right_border: &str| {
      ui::pad_line(content, fill, right_border, cols);
    };

    let mut row_map = vec![];
    let cursor_pos = self.cursor.get();
    let offset = self.scroll_offset;
    let number_candidates = self.number_candidates;
    let max_height = self.max_height;
    let num_filtered = self.filtered.len();
    let num_candidates = self.candidates.len();
    let min_pad = num_candidates.to_string().len().saturating_add(1).max(6);
    let hovered = self.hovered;

    self.query.set_available_width(cols.saturating_sub(6));
    self.query.update_scroll_offset();
    let query = self.query.get_window();
    let title = self.title.clone();
    let visible = self.get_window();
    let mut rows: usize = 0;

    // ╭─ Title ──────────────────╮
    let title_content = format!(
      "\n{}{} \x1b[1m{}\x1b[0m ",
      ui::TOP_LEFT,
      ui::HOR_LINE,
      title
    );
    pad(&title_content, ui::HOR_LINE, ui::TOP_RIGHT);
    rows += 1;
    row_map.push(None);

    // │ > query                  │
    let prompt_content = format!("{} {} {}", ui::VERT_LINE, Self::PROMPT_ARROW, query);
    pad(&prompt_content, " ", ui::VERT_LINE);
    rows += 1;

    // ├──filtered/total──────────┤
    let sep_content = format!(
      "{}{}\x1b[33m{}\x1b[0m/\x1b[33m{}\x1b[0m",
      ui::TREE_LEFT,
      ui::HOR_LINE.repeat(2),
      num_filtered,
      num_candidates
    );
    pad(&sep_content, ui::HOR_LINE, ui::TREE_RIGHT);
    rows += 1;

    // Candidate lines
    let mut lines_drawn = 0;
    let col_lim = if number_candidates {
      cols.saturating_sub(3 + min_pad)
    } else {
      cols.saturating_sub(3)
    };

    const MAX_DESC_COL: usize = 32;
    let desc_col_width = visible
      .iter()
      .filter(|sc| sc.candidate.desc.is_some())
      .filter_map(|sc| sc.candidate.content().trim_end().lines().next())
      .map(calc_str_width)
      .max()
      .unwrap_or(0)
      .min(MAX_DESC_COL);

    for (i, s_cand) in visible.iter().enumerate() {
      if lines_drawn >= max_height {
        break;
      }

      let selected = i + offset == cursor_pos;
      let hovered = hovered == Some(i + offset);
      let selector = if selected {
        Self::SELECTOR_HL
      } else if hovered {
        Self::SELECTOR_HOVER
      } else {
        Self::SELECTOR_GRAY
      };
      let mut drew_number = false;

      let mut first = true;
      for line in s_cand.candidate.content().trim_end().lines() {
        if lines_drawn >= max_height {
          break;
        }

        let mut line = line.trim_end().replace('\t', "    ");
        if first {
          first = false;
          if let Some(desc) = &s_cand.candidate.desc {
            let cand_width = calc_str_width(&line);
            let pad = desc_col_width.saturating_sub(cand_width);
            line = format!(
              "{line}{}\x1b[90m  {desc}\x1b[0m",
              " ".repeat(pad)
            );
          }
        }
        if calc_str_width(&line) >= col_lim {
          line.truncate(col_lim.saturating_sub(6));
          line.push_str("...");
        }

        let left = if number_candidates && !drew_number {
          let num = i + offset + 1;
          format!(
            "{} {}\x1b[33m{num:<min_pad$}\x1b[39m{line}\x1b[0m",
            ui::VERT_LINE,
            selector
          )
        } else if number_candidates {
          format!(
            "{} {}{:>min_pad$}{line}\x1b[0m",
            ui::VERT_LINE,
            selector,
            ""
          )
        } else {
          format!("{} {}{line}\x1b[0m", ui::VERT_LINE, selector)
        };

        pad(&left, " ", ui::VERT_LINE);
        rows += 1;
        row_map.push(Some(i + offset));
        drew_number = true;
        lines_drawn += 1;
      }
    }

    // ╰──────────────────────────╯
    write_term!(
      "{}{}{}",
      ui::BOT_LEFT,
      ui::HOR_LINE.repeat(cols.saturating_sub(2)),
      ui::BOT_RIGHT
    )
    .unwrap();
    rows += 1;
    row_map.push(None);

    // Move cursor back up to the query input line
    let lines_below_prompt = rows.saturating_sub(2);
    let cursor_in_window = self
      .query
      .linebuf
      .cursor_to_flat()
      .saturating_sub(self.query.scroll_offset);
    let cursor_col = cursor_in_window + 4;
    write_term!("\x1b[{lines_below_prompt}A\r\x1b[{cursor_col}C").unwrap();

    let new_layout = FuzzyLayout {
      top_left,
      rows,
      cols,
      cursor_col,
      preceding_line_width: self.prompt_line_width,
      preceding_cursor_col: self.prompt_cursor_col,
    };
    self.old_layout = Some(new_layout);
    self.row_map = row_map;

    Ok(rows)
  }

  pub fn clear(&mut self) -> ShResult<()> {
    if let Some(layout) = self.old_layout.take() {
      let new_cols = with_term(|t| t.t_cols());
      let total_cells = layout.rows * layout.cols;
      let physical_rows = if new_cols > 0 {
        total_cells.div_ceil(new_cols)
      } else {
        layout.rows
      };
      let cursor_offset = layout.cols + layout.cursor_col;
      let cursor_phys_row = if new_cols > 0 {
        cursor_offset / new_cols
      } else {
        1
      };
      let lines_below = physical_rows.saturating_sub(cursor_phys_row + 1);

      let gap_extra = if new_cols > 0 && layout.preceding_line_width > new_cols {
        let wrap_rows = (layout.preceding_line_width).div_ceil(new_cols);
        let cursor_wrap_row = layout.preceding_cursor_col / new_cols;
        wrap_rows.saturating_sub(cursor_wrap_row + 1)
      } else {
        0
      };

      if lines_below > 0 {
        write_term!("\x1b[{lines_below}B").unwrap();
      }
      for _ in 0..physical_rows {
        write_term!("\x1b[2K\x1b[A").unwrap();
      }
      write_term!("\x1b[2K").unwrap();
      for _ in 0..gap_extra {
        write_term!("\x1b[2K\x1b[A").unwrap();
      }
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
  fn set_prompt_line_context(&mut self, line_width: usize, cursor_col: usize) {
    self
      .selector
      .set_prompt_line_context(line_width, cursor_col);
  }
  fn reset_stay_active(&mut self) {
    self.selector.reset_query();
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
      if let Some(graft) = self.completer.graft_pos {
        let trailing_slash = selected.ends_with('/');
        let trimmed = selected.trim_end_matches('/');
        let mut basename = trimmed.rsplit('/').next().unwrap_or(&selected).to_string();
        if trailing_slash {
          basename.push('/');
        }
        (
          self.completer.original_input[..graft].to_string(),
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
    let final_completion = if self.completer.escape_candidates {
      escape_str(&completion, false)
    } else {
      completion.to_string()
    };
    log::debug!(
      "Prefix: '{}', Completion: '{}', Final: '{}'",
      prefix,
      completion,
      final_completion
    );
    let ret = format!(
      "{}{}{}",
      prefix,
      final_completion,
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
      return Ok(None);
    } else if candidates.len() == 1 {
      self.selector.filtered = candidates.into_iter().map(ScoredCandidate::from).collect();
      let selected = self.selector.filtered[0].candidate.content().to_string();
      let completed = self.get_completed_line(&selected);
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
  fn clear(&mut self) -> ShResult<()> {
    self.selector.clear()
  }
  fn draw(&mut self) -> ShResult<usize> {
    self.selector.draw()
  }
  fn reset(&mut self) {
    self.completer.reset();
    self.selector.reset_query();
  }
  fn token_span(&self) -> (usize, usize) {
    self.completer.token_span()
  }
  fn is_active(&self) -> bool {
    !self.selector.candidates.is_empty()
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
  pub cursor_pos: usize,
  pub active: bool,
  pub dirs_only: bool,
  pub add_space: bool,
  pub escape_candidates: bool,
  pub graft_pos: Option<usize>,
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

  fn draw(&mut self) -> ShResult<usize> {
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
    self.cursor_pos = cursor_pos;
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
      if let Some(graft) = self.graft_pos {
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
        (self.original_input[..graft].to_string(), basename)
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
    let final_completion = if self.escape_candidates {
      escape_str(&completion, false)
    } else {
      completion
    };
    format!("{}{}{}", prefix, final_completion, &self.original_input[end..])
  }

  pub fn build_comp_ctx(&self, tks: &[CtxTk], line: &str, cursor_pos: usize) -> ShResult<CompContext> {
    let mut ctx = CompContext {
      words: vec![],
      cword: 0,
      line: line.to_string(),
      cursor_pos,
    };

    let segments = tks
      .split(|t| matches!(t.class(), CtxTkRule::Operator | CtxTkRule::Separator))
      .filter(|&s| !s.is_empty())
      .map(|s| s.to_vec())
      .collect::<Vec<_>>();

    if segments.is_empty() {
      return Ok(ctx);
    }

    let relevant_pos = segments
      .iter()
      .position(|tks| {
        tks
          .iter()
          .next()
          .is_some_and(|tk| tk.range().start > cursor_pos)
      })
      .map(|i| i.saturating_sub(1))
      .unwrap_or(segments.len().saturating_sub(1));

    let relevant = segments[relevant_pos].to_vec();
    let mut words = relevant.iter()
      .map(|s| s.span().as_str().to_string())
      .collect::<Vec<_>>();

    let cword = if let Some(pos) = relevant
      .iter()
      .position(|tk| tk.range_inclusive().contains(&cursor_pos))
    {
      pos
    } else {
      let insert_pos = relevant
        .iter()
        .position(|tk| tk.range().start > cursor_pos)
        .unwrap_or(relevant.len());
      words.insert(insert_pos, String::new());
      insert_pos
    };

    ctx.words = words;
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
    let tks = get_context_tokens(&line);
    let (strat, replace_span) = CompStrat::resolve(&tks, cursor_pos);

    self.token_span = (replace_span.range().start, replace_span.range().end);
    self.escape_candidates = strat.escapes_candidates();
    self.graft_pos = match &strat {
      CompStrat::Files { parent: Some(p), .. } => Some(self.token_span.0 + p.len()),
      _ => None,
    };
    match strat {
      CompStrat::Var { prefix } => Ok(CompResult::from_candidates(complete_vars(&prefix))),
      CompStrat::Tilde { prefix } => Ok(CompResult::from_candidates(complete_users(&prefix))),
      CompStrat::Command { prefix } => Ok(CompResult::from_candidates(complete_commands(&prefix))),
      CompStrat::Dirs { prefix } => Ok(CompResult::from_candidates(complete_dirs(&prefix))),
      CompStrat::Files { prefix, parent } => Ok(CompResult::from_candidates(complete_files_in(&prefix, parent.as_deref()))),
      CompStrat::Separator => Ok(CompResult::Single { result: Candidate::from(";"), }),
      CompStrat::Null => Ok(CompResult::NoMatch),
      CompStrat::Argument { prefix } => {
        let ctx = self.build_comp_ctx(&tks, &line, cursor_pos)?;
        // Expanded form: backslashes/quotes/var-subs in the user's literal
        // input resolved for filesystem matching. The original `prefix` is
        // still used for `complete -F` (bash compat: user functions get
        // COMP_WORDS as the user typed them) but file-completion fallbacks
        // need the expanded form to match real filenames.
        let expanded = unescape_for_completion(&prefix);
        match self.try_comp_spec(&ctx)? {
          CompSpecResult::Match { result, flags } => {
            if flags.contains(CompOptFlags::SPACE) {
              self.add_space = true;
            }
            Ok(result)
          }
          CompSpecResult::NoSpec => {
            Ok(CompResult::from_candidates(complete_filename(&expanded)))
          }
          CompSpecResult::NoMatch { flags } => {
            if flags.contains(CompOptFlags::SPACE) {
              self.add_space = true;
            }
            if flags.contains(CompOptFlags::DIRNAMES) {
              Ok(CompResult::from_candidates(complete_dirs(&expanded)))
            } else if flags.contains(CompOptFlags::DEFAULT) {
              Ok(CompResult::from_candidates(complete_filename(&expanded)))
            } else {
              Ok(CompResult::NoMatch)
            }
          }
        }
      }
    }
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
  fn test_vi(initial: &str) -> (ShedLine, TestGuard) {
    let g = TestGuard::new();
    let prompt = Prompt::default();
    let vi = ShedLine::new_no_hist(prompt).unwrap().with_initial(initial);
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
    // `${` covers bytes 0..2; the name starts at byte 2 and ends at byte 6.
    assert_eq!(start, 2);
    assert_eq!(end, 6);
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
      cursor_pos: 0,
      escape_candidates: true,
      graft_pos: None,
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
      cursor_pos: 0,
      escape_candidates: true,
      graft_pos: None,
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
      cursor_pos: 0,
      escape_candidates: true,
      graft_pos: None,
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
      cursor_pos: 0,
      escape_candidates: true,
      graft_pos: None,
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
      cursor_pos: 0,
      escape_candidates: true,
      graft_pos: None,
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
      cursor_pos: 0,
      escape_candidates: true,
      graft_pos: None,
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

    crate::state::with_term(|t| t.feed_bytes(b"echo hello\t"));
    let keys = crate::state::with_term(|t| t.drain_keys()).unwrap();
    let _ = vi.process_input(keys);

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
    crate::state::with_term(|t| t.feed_bytes(b"echo my\\ \t"));
    let keys = crate::state::with_term(|t| t.drain_keys()).unwrap();
    let _ = vi.process_input(keys);

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

  // ===================== CompStrat::resolve =====================

  /// Run the dispatcher against a literal source string and cursor position.
  /// Returns (strategy, replacement-span as a (start, end) tuple).
  fn dispatch(input: &str, cursor: usize) -> (CompStrat, (usize, usize)) {
    let tks = get_context_tokens(input);
    let (strat, span) = CompStrat::resolve(&tks, cursor);
    (strat, (span.range().start, span.range().end))
  }

  /// Helper: extract the prefix from a Var/Tilde/Command/Argument/Files/Dirs strat.
  fn prefix_of(strat: &CompStrat) -> &str {
    match strat {
      CompStrat::Var { prefix }
      | CompStrat::Tilde { prefix }
      | CompStrat::Command { prefix }
      | CompStrat::Argument { prefix }
      | CompStrat::Dirs { prefix }
      | CompStrat::Files { prefix, .. } => prefix,
      CompStrat::Separator | CompStrat::Null => "",
    }
  }

  #[test]
  fn dispatch_bare_var_sub() {
    // `$FL` cursor at end → Var{prefix:"$FL"}, span covers `$FL`.
    let input = "echo $FL";
    let (strat, span) = dispatch(input, input.len());
    assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
    assert_eq!(prefix_of(&strat), "$FL");
    assert_eq!(&input[span.0..span.1], "$FL");
  }

  #[test]
  fn dispatch_braced_var_sub_unclosed() {
    // `${FL` cursor at end → Var, span narrowed to `${FL`.
    // The closing `}` doesn't exist yet; replacement targets exactly what
    // the user has typed.
    let input = "echo ${FL";
    let (strat, span) = dispatch(input, input.len());
    assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
    assert_eq!(prefix_of(&strat), "${FL");
    assert_eq!(&input[span.0..span.1], "${FL");
  }

  #[test]
  fn dispatch_braced_var_sub_closed() {
    // `${FL}` cursor at end → Var, span narrowed to `${FL`. The `}` lives
    // outside the span so it's preserved when the candidate replaces.
    let input = "echo ${FL}";
    let (strat, span) = dispatch(input, input.len());
    assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
    assert_eq!(prefix_of(&strat), "${FL}");
    assert_eq!(&input[span.0..span.1], "${FL");
  }

  #[test]
  fn dispatch_braced_var_with_substitution_op() {
    // `${FL/bar` parses as a (greedy) substitution param expansion.
    // Cursor on FL → Var, span narrowed to `${FL` so the `/bar` is preserved.
    let input = "echo ${FL/bar";
    let cursor = input.find("FL").unwrap() + 2; // end of FL
    let (strat, span) = dispatch(input, cursor);
    assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
    assert_eq!(&input[span.0..span.1], "${FL");
  }

  #[test]
  fn dispatch_var_sub_inside_path() {
    // `/foo/$FL/bar` cursor on FL → Var, span covers exactly `$FL`.
    // Sibling path bytes (`/foo/` and `/bar`) stay outside the span.
    let input = "echo /foo/$FL/bar";
    let cursor = input.find("$FL").unwrap() + 3; // end of $FL
    let (strat, span) = dispatch(input, cursor);
    assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
    assert_eq!(prefix_of(&strat), "$FL");
    assert_eq!(&input[span.0..span.1], "$FL");
  }

  #[test]
  fn dispatch_var_sub_inside_double_quoted_string() {
    // `"foo $FL` (unclosed double-quoted string) cursor at end.
    // Recursion descends into DoubleString → VarSub. Span covers `$FL`.
    let input = "echo \"foo $FL";
    let (strat, span) = dispatch(input, input.len());
    assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
    assert_eq!(prefix_of(&strat), "$FL");
    assert_eq!(&input[span.0..span.1], "$FL");
  }

  #[test]
  fn dispatch_braced_var_inside_double_quoted_string() {
    let input = "echo \"foo ${FL}";
    let cursor = input.find("FL").unwrap() + 2; // end of FL
    let (strat, span) = dispatch(input, cursor);
    assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
    assert_eq!(&input[span.0..span.1], "${FL");
  }

  #[test]
  fn dispatch_path_with_var_prefix() {
    // `$HOME/proj` cursor at end → Files with parent="$HOME/", prefix="proj".
    // The cursor is past the var sub, so we fall through to path slicing.
    let input = "echo $HOME/proj";
    let (strat, _) = dispatch(input, input.len());
    match strat {
      CompStrat::Files { prefix, parent } => {
        assert_eq!(prefix, "proj");
        assert_eq!(parent.as_deref(), Some("$HOME/"));
      }
      other => panic!("expected Files, got {other:?}"),
    }
  }

  #[test]
  fn dispatch_simple_path_no_parent() {
    // `foo` cursor at end → Files{prefix:"foo", parent:None}, span = leaf.
    let input = "echo foo";
    let (strat, span) = dispatch(input, input.len());
    match strat {
      CompStrat::Files { prefix, parent } => {
        assert_eq!(prefix, "foo");
        assert_eq!(parent, None);
      }
      other => panic!("expected Files, got {other:?}"),
    }
    assert_eq!(&input[span.0..span.1], "foo");
  }

  #[test]
  fn dispatch_path_with_literal_parent() {
    // `bar/foo` cursor at end → Files{prefix:"foo", parent:Some("bar/")}.
    let input = "echo bar/foo";
    let (strat, _) = dispatch(input, input.len());
    match strat {
      CompStrat::Files { prefix, parent } => {
        assert_eq!(prefix, "foo");
        assert_eq!(parent.as_deref(), Some("bar/"));
      }
      other => panic!("expected Files, got {other:?}"),
    }
  }

  #[test]
  fn dispatch_empty_input_is_command() {
    // Empty input → Command{prefix:""}.
    let (strat, _) = dispatch("", 0);
    assert!(matches!(strat, CompStrat::Command { .. }), "got {strat:?}");
    assert_eq!(prefix_of(&strat), "");
  }

  #[test]
  fn dispatch_after_separator_is_command() {
    // `ls foo | ` cursor at end → Command{prefix:""} (new pipeline segment).
    let input = "ls foo | ";
    let (strat, _) = dispatch(input, input.len());
    assert!(matches!(strat, CompStrat::Command { .. }), "got {strat:?}");
    assert_eq!(prefix_of(&strat), "");
  }

  #[test]
  fn dispatch_in_gap_after_command_uses_zero_width_span() {
    // `echo ` cursor at end (after trailing space) → Argument synthesized
    // from the predecessor. Replace span must be zero-width at cursor so
    // candidates are INSERTED, not replacing "echo".
    let input = "echo ";
    let (strat, span) = dispatch(input, input.len());
    assert!(matches!(strat, CompStrat::Argument { .. }), "got {strat:?}");
    assert_eq!(span, (input.len(), input.len()),
      "expected zero-width span at cursor, got {span:?}");
  }

  #[test]
  fn dispatch_partial_command_name() {
    // `ls<cursor>` (cursor inside the command word) → Command{prefix:"ls"}.
    let input = "ls";
    let (strat, span) = dispatch(input, input.len());
    assert!(matches!(strat, CompStrat::Command { .. }), "got {strat:?}");
    assert_eq!(prefix_of(&strat), "ls");
    assert_eq!(&input[span.0..span.1], "ls");
  }

  #[test]
  fn dispatch_preserves_braces_under_string_recursion() {
    // Combined: param-expansion-style var sub inside a string. After our
    // get_leaf VarSub-stop-point fix, this dispatches correctly via the
    // non-Argument branch with narrow_var_span finding the ParamName.
    let input = "echo \"foo ${FL}/bar\"";
    let cursor = input.find("FL").unwrap() + 2;
    let (strat, span) = dispatch(input, cursor);
    assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
    assert_eq!(&input[span.0..span.1], "${FL");
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
    crate::state::with_term(|t| t.feed_bytes(b"echo unique_shed_test\t"));
    let keys = crate::state::with_term(|t| t.drain_keys()).unwrap();
    let _ = vi.process_input(keys);

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

    crate::state::with_term(|t| t.feed_bytes(b"cd mysub\t"));
    let keys = crate::state::with_term(|t| t.drain_keys()).unwrap();
    let _ = vi.process_input(keys);

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

    crate::state::with_term(|t| t.feed_bytes(b"cmd --opt=eqf\t"));
    let keys = crate::state::with_term(|t| t.drain_keys()).unwrap();
    let _ = vi.process_input(keys);

    let line = vi.editor.joined();
    assert!(
      line.contains("--opt=eqfile.txt"),
      "expected completion after '=': {line:?}"
    );

    std::fs::remove_dir_all(&tmp).ok();
  }
}
