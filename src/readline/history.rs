use std::{
  cmp::Ordering,
  collections::HashSet,
  env,
  fmt::{Display, Write},
  fs::{self, OpenOptions},
  io::Write as IoWrite,
  path::{Path, PathBuf},
  str::FromStr,
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  readline::{complete::FuzzySelector, linebuf::LineBuf},
  state::read_meta,
};

#[derive(Default, Clone, Copy, Debug)]
pub enum SearchKind {
  Fuzzy,
  #[default]
  Prefix,
}

#[derive(Default, Clone, Debug)]
pub struct SearchConstraint {
  kind: SearchKind,
  term: String,
}

impl SearchConstraint {
  pub fn new(kind: SearchKind, term: String) -> Self {
    Self { kind, term }
  }
}

#[derive(Debug, Clone)]
pub struct HistEntry {
  runtime: Duration,
  timestamp: SystemTime,
  command: String,
  new: bool,
}

impl HistEntry {
  pub fn timestamp(&self) -> &SystemTime {
    &self.timestamp
  }
  pub fn command(&self) -> &str {
    &self.command
  }
  fn with_escaped_newlines(&self) -> String {
    let mut escaped = String::new();
    for ch in self.command.chars() {
      match ch {
        '\\' => escaped.push_str("\\\\"), // escape all backslashes
        '\n' => escaped.push_str("\\\n"), // line continuation
        _ => escaped.push(ch),
      }
    }
    escaped
  }
  pub fn is_new(&self) -> bool {
    self.new
  }
}

impl FromStr for HistEntry {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let err = Err(ShErr::simple(
      ShErrKind::HistoryReadErr,
      format!("Bad formatting on history entry '{s}'"),
    ));

    //: 248972349;148;echo foo; echo bar
    let Some(cleaned) = s.strip_prefix(": ") else {
      return err;
    };
    //248972349;148;echo foo; echo bar
    let Some((timestamp, runtime_and_cmd)) = cleaned.split_once(';') else {
      return err;
    };
    //("248972349","148;echo foo; echo bar")
    let Some((runtime, command)) = runtime_and_cmd.split_once(';') else {
      return err;
    };
    //("148","echo foo; echo bar")
    let Ok(ts_seconds) = timestamp.parse::<u64>() else {
      return err;
    };
    let Ok(runtime) = runtime.parse::<u64>() else {
      return err;
    };
    let runtime = Duration::from_secs(runtime);
    let timestamp = UNIX_EPOCH + Duration::from_secs(ts_seconds);
    let command = command.to_string();
    Ok(Self {
      runtime,
      timestamp,
      command,
      new: false,
    })
  }
}

impl Display for HistEntry {
  /// Similar to zsh's history format, but not entirely
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let command = self.with_escaped_newlines();
    let HistEntry {
      runtime,
      timestamp,
      command: _,
      new: _,
    } = self;
    let timestamp = timestamp.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let runtime = runtime.as_secs();
    writeln!(f, ": {timestamp};{runtime};{command}")
  }
}

pub struct HistEntries(Vec<HistEntry>);

impl FromStr for HistEntries {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let mut entries = vec![];

    let mut lines = s.lines().enumerate().peekable();
    let mut cur_line = String::new();

    while let Some((i, line)) = lines.next() {
      if !line.starts_with(": ") {
        return Err(ShErr::simple(
          ShErrKind::HistoryReadErr,
          format!("Bad formatting on line {i}"),
        ));
      }
      let mut chars = line.chars().peekable();
      let mut feeding_lines = true;
      while feeding_lines {
        feeding_lines = false;
        while let Some(ch) = chars.next() {
          match ch {
            '\\' => {
              if let Some(esc_ch) = chars.next() {
                // Unescape: \\ -> \, \n stays as literal n after backslash was written as \\n
                cur_line.push(esc_ch);
              } else {
                // Trailing backslash = line continuation in history file format
                cur_line.push('\n');
                feeding_lines = true;
              }
            }
            '\n' => break,
            _ => {
              cur_line.push(ch);
            }
          }
        }
        if feeding_lines {
          let Some((_, line)) = lines.next() else {
            return Err(ShErr::simple(
              ShErrKind::HistoryReadErr,
              format!("Bad formatting on line {i}"),
            ));
          };
          chars = line.chars().peekable();
        }
      }
      let entry = cur_line.parse::<HistEntry>()?;
      entries.push(entry);
      cur_line.clear();
    }

    Ok(Self(entries))
  }
}

fn read_hist_file(path: &Path) -> ShResult<Vec<HistEntry>> {
  if !path.exists() {
    fs::File::create(path)?;
  }
  let raw = fs::read_to_string(path)?;
  Ok(raw.parse::<HistEntries>()?.0)
}

/// Deduplicate entries, keeping only the most recent occurrence of each
/// command. Preserves chronological order (oldest to newest).
fn dedupe_entries(entries: &[HistEntry]) -> Vec<HistEntry> {
  let mut seen = HashSet::new();
  // Iterate backwards (newest first), keeping first occurrence of each command
  entries
    .iter()
    .rev()
    .filter(|ent| seen.insert(ent.command.clone()))
    .cloned()
    .collect::<Vec<_>>()
    .into_iter()
    .rev() // Restore chronological order
    .collect()
}

#[derive(Default, Clone, Debug)]
pub struct History {
  path: PathBuf,
  pub pending: Option<LineBuf>, // command, cursor_pos
  entries: Vec<HistEntry>,
  search_mask: Vec<HistEntry>,
  pub fuzzy_finder: FuzzySelector,
  no_matches: bool,
  pub cursor: usize,
  //search_direction: Direction,
  ignore_dups: bool,
  max_size: Option<u32>,
  stateless: bool,
}

impl History {
  pub fn empty() -> Self {
    Self {
      path: PathBuf::new(),
      pending: None,
      entries: Vec::new(),
      search_mask: Vec::new(),
      fuzzy_finder: FuzzySelector::new("History").number_candidates(true),
      no_matches: false,
      cursor: 0,
      //search_direction: Direction::Backward,
      ignore_dups: false,
      max_size: None,
      stateless: true,
    }
  }
  pub fn new() -> ShResult<Self> {
    let ignore_dups = crate::state::read_shopts(|s| s.core.hist_ignore_dupes);
    let max_hist = crate::state::read_shopts(|s| s.core.max_hist);

    let path = PathBuf::from(env::var("SHEDHIST").unwrap_or({
      let home = env::var("HOME").unwrap();
      format!("{home}/.shed_history")
    }));

    let mut entries = read_hist_file(&path)?;

    // Enforce max_hist limit on loaded entries (negative = unlimited)
    if max_hist >= 0 && entries.len() > max_hist as usize {
      entries = entries.split_off(entries.len() - max_hist as usize);
    }

    let search_mask = dedupe_entries(&entries);
    let cursor = search_mask.len();
    let max_size = if max_hist < 0 {
      None
    } else {
      Some(max_hist as u32)
    };

    Ok(Self {
      path,
      entries,
      fuzzy_finder: FuzzySelector::new("History").number_candidates(true),
      pending: None,
      search_mask,
      no_matches: false,
      cursor,
      //search_direction: Direction::Backward,
      ignore_dups,
      max_size,
      stateless: false,
    })
  }

  pub fn start_search(&mut self, initial: &str) -> Option<String> {
    if self.search_mask.is_empty() {
      None
    } else if self.search_mask.len() == 1 {
      Some(self.search_mask[0].command().to_string())
    } else {
      self.fuzzy_finder.set_query(initial.to_string());
      let raw_entries = self
        .search_mask
        .clone()
        .into_iter()
        .map(|ent| super::complete::Candidate::from(ent.command()));
      self.fuzzy_finder.activate(raw_entries.collect());
      None
    }
  }

  pub fn reset(&mut self) {
    self.search_mask = dedupe_entries(&self.entries);
    self.cursor = self.search_mask.len();
  }

  pub fn entries(&self) -> &[HistEntry] {
    &self.entries
  }

  pub fn masked_entries(&self) -> &[HistEntry] {
    &self.search_mask
  }

  pub fn cursor_entry(&self) -> Option<&HistEntry> {
    self.search_mask.get(self.cursor)
  }

  pub fn at_pending(&self) -> bool {
    self.cursor >= self.search_mask.len()
  }

  pub fn reset_to_pending(&mut self) {
    self.cursor = self.search_mask.len();
  }

  pub fn update_pending_cmd(&mut self, buf: (&str, usize)) {
    let cursor_pos = if let Some(pending) = &self.pending {
      pending.cursor.get()
    } else {
      buf.1
    };
    let cmd = buf.0.to_string();
    let constraint = SearchConstraint {
      kind: SearchKind::Prefix,
      term: cmd.clone(),
    };

    if let Some(pending) = &mut self.pending {
      pending.set_buffer(cmd);
      pending.cursor.set(cursor_pos);
    } else {
      self.pending = Some(LineBuf::new().with_initial(&cmd, cursor_pos));
    }
    self.constrain_entries(constraint);
  }

  pub fn last_mut(&mut self) -> Option<&mut HistEntry> {
    self.entries.last_mut()
  }
  pub fn last(&self) -> Option<&HistEntry> {
    self.entries.last()
  }

  pub fn resolve_hist_token(&self, token: &str) -> Option<String> {
    let token = token.strip_prefix('!').unwrap_or(token).to_string();
    if let Ok(num) = token.parse::<i32>()
      && num != 0
    {
      match num.cmp(&0) {
        Ordering::Less => {
          if num.unsigned_abs() > self.entries.len() as u32 {
            return None;
          }

          let rev_idx = self.entries.len() - num.unsigned_abs() as usize;
          self.entries.get(rev_idx).map(|e| e.command().to_string())
        }
        Ordering::Greater => self
          .entries
          .get(num as usize)
          .map(|e| e.command().to_string()),
        _ => unreachable!(),
      }
    } else {
      let mut rev_search = self.entries.iter();
      rev_search
        .rfind(|e| e.command().starts_with(&token))
        .map(|e| e.command().to_string())
    }
  }

  pub fn ignore_dups(&mut self, yn: bool) {
    self.ignore_dups = yn
  }

  pub fn max_hist_size(&mut self, size: Option<u32>) {
    self.max_size = size
  }

  pub fn constrain_entries(&mut self, constraint: SearchConstraint) {
    let SearchConstraint { kind, term } = constraint;
    match kind {
      SearchKind::Prefix => {
        if term.is_empty() {
          self.search_mask = dedupe_entries(&self.entries);
        } else {
          let filtered: Vec<_> = self
            .entries
            .iter()
            .filter(|ent| ent.command().starts_with(&term))
            .cloned()
            .collect();

          self.search_mask = dedupe_entries(&filtered);
          self.no_matches = self.search_mask.is_empty();
          if self.no_matches {
            // If no matches, reset to full history so user can still scroll through it
            self.search_mask = dedupe_entries(&self.entries);
          }
        }
        self.cursor = self.search_mask.len();
      }
      SearchKind::Fuzzy => todo!(),
    }
  }

  pub fn hint_entry(&self) -> Option<&HistEntry> {
    if self.no_matches {
      return None;
    };
    self.search_mask.last()
  }

  pub fn get_hint(&self) -> Option<String> {
    if self.at_pending() && self.pending.as_ref().is_some_and(|p| !p.buffer.is_empty()) {
      let entry = self.hint_entry()?;
      Some(entry.command().to_string())
    } else {
      None
    }
  }

  pub fn scroll(&mut self, offset: isize) -> Option<&HistEntry> {
    self.cursor = self
      .cursor
      .saturating_add_signed(offset)
      .clamp(0, self.search_mask.len());

    self.search_mask.get(self.cursor)
  }

  pub fn push(&mut self, command: String) {
    let timestamp = SystemTime::now();
    let runtime = read_meta(|m| m.get_time()).unwrap_or_default();
    if self.ignore_dups && self.is_dup(&command) {
      return;
    }
    self.entries.push(HistEntry {
      runtime,
      timestamp,
      command,
      new: true,
    });
  }

  pub fn is_dup(&self, other: &str) -> bool {
    let Some(ent) = self.entries.last() else {
      return false;
    };
    let ent_cmd = &ent.command;
    ent_cmd == other
  }

  pub fn save(&mut self) -> ShResult<()> {
    if self.stateless {
      return Ok(());
    }
    let mut file = OpenOptions::new()
      .create(true)
      .append(true)
      .open(&self.path)?;

    let last_file_entry = self
      .entries
      .iter()
      .rfind(|ent| !ent.new)
      .map(|ent| ent.command.clone())
      .unwrap_or_default();

    let entries = self.entries.iter_mut().filter(|ent| {
      ent.new
        && !ent.command.is_empty()
        && if self.ignore_dups {
          ent.command() != last_file_entry
        } else {
          true
        }
    });

    let mut data = String::new();
    for ent in entries {
      ent.new = false;
      write!(data, "{ent}").unwrap();
    }

    file.write_all(data.as_bytes())?;
    self.pending = None;
    self.reset();

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{state, testutil::TestGuard};
  use scopeguard::guard;
  use std::{env, fs, path::Path};
  use tempfile::tempdir;

  fn with_env_var(key: &str, val: &str) -> impl Drop {
    let prev = env::var(key).ok();
    unsafe {
      env::set_var(key, val);
    }
    guard(prev, move |p| match p {
      Some(v) => unsafe { env::set_var(key, v) },
      None => unsafe { env::remove_var(key) },
    })
  }

  /// Temporarily mutate shell options for a test and restore the
  /// previous values when the returned guard is dropped.
  fn with_shopts(modifier: impl FnOnce(&mut crate::shopt::ShOpts)) -> impl Drop {
    let original = state::read_shopts(|s| s.clone());
    state::write_shopts(|s| modifier(s));
    guard(original, |orig| {
      state::write_shopts(|s| *s = orig);
    })
  }

  fn write_history_file(path: &Path) {
    fs::write(
      path,
      [": 1;1;first\n", ": 2;1;second\n", ": 3;1;third\n"].concat(),
    )
    .unwrap();
  }

  #[test]
  fn history_new_respects_max_hist_limit() {
    let _lock = TestGuard::new();
    let tmp = tempdir().unwrap();
    let hist_path = tmp.path().join("history");
    write_history_file(&hist_path);

    let _env_guard = with_env_var("SHEDHIST", hist_path.to_str().unwrap());
    let _opts_guard = with_shopts(|s| {
      s.core.max_hist = 2;
      s.core.hist_ignore_dupes = true;
    });

    let history = History::new().unwrap();

    assert_eq!(history.entries.len(), 2);
    assert_eq!(history.search_mask.len(), 2);
    assert_eq!(history.cursor, 2);
    assert_eq!(history.max_size, Some(2));
    assert!(history.ignore_dups);
    assert!(history.pending.is_none());
    assert_eq!(history.entries[0].command(), "second");
    assert_eq!(history.entries[1].command(), "third");
  }

  #[test]
  fn history_new_keeps_all_when_unlimited() {
    let _lock = TestGuard::new();
    let tmp = tempdir().unwrap();
    let hist_path = tmp.path().join("history");
    write_history_file(&hist_path);

    let _env_guard = with_env_var("SHEDHIST", hist_path.to_str().unwrap());
    let _opts_guard = with_shopts(|s| {
      s.core.max_hist = -1;
      s.core.hist_ignore_dupes = false;
    });

    let history = History::new().unwrap();

    assert_eq!(history.entries.len(), 3);
    assert_eq!(history.search_mask.len(), 3);
    assert_eq!(history.cursor, 3);
    assert_eq!(history.max_size, None);
    assert!(!history.ignore_dups);
  }

  #[test]
  fn history_new_dedupes_search_mask_to_latest_occurrence() {
    let _lock = TestGuard::new();
    let tmp = tempdir().unwrap();
    let hist_path = tmp.path().join("history");
    fs::write(
      &hist_path,
      [": 1;1;repeat\n", ": 2;1;unique\n", ": 3;1;repeat\n"].concat(),
    )
    .unwrap();

    let _env_guard = with_env_var("SHEDHIST", hist_path.to_str().unwrap());
    let _opts_guard = with_shopts(|s| {
      s.core.max_hist = 10;
    });

    let history = History::new().unwrap();

    let masked: Vec<_> = history.search_mask.iter().map(|e| e.command()).collect();
    assert_eq!(masked, vec!["unique", "repeat"]);
    assert_eq!(history.cursor, history.search_mask.len());
  }
}
