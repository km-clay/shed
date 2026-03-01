use std::{
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
  readline::linebuf::LineBuf,
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
  id: u32,
  timestamp: SystemTime,
  command: String,
  new: bool,
}

impl HistEntry {
  pub fn id(&self) -> u32 {
    self.id
  }
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
    let Some((timestamp, id_and_command)) = cleaned.split_once(';') else {
      return err;
    };
    //("248972349","148;echo foo; echo bar")
    let Some((id, command)) = id_and_command.split_once(';') else {
      return err;
    };
    //("148","echo foo; echo bar")
    let Ok(ts_seconds) = timestamp.parse::<u64>() else {
      return err;
    };
    let Ok(id) = id.parse::<u32>() else {
      return err;
    };
    let timestamp = UNIX_EPOCH + Duration::from_secs(ts_seconds);
    let command = command.to_string();
    Ok(Self {
      id,
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
      id,
      timestamp,
      command: _,
      new: _,
    } = self;
    let timestamp = timestamp.duration_since(UNIX_EPOCH).unwrap().as_secs();
    writeln!(f, ": {timestamp};{id};{command}")
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

pub struct History {
  path: PathBuf,
  pub pending: Option<LineBuf>, // command, cursor_pos
  entries: Vec<HistEntry>,
  search_mask: Vec<HistEntry>,
  no_matches: bool,
  pub cursor: usize,
  //search_direction: Direction,
  ignore_dups: bool,
  max_size: Option<u32>,
}

impl History {
  pub fn new() -> ShResult<Self> {
    let ignore_dups = crate::state::read_shopts(|s| s.core.hist_ignore_dupes);
    let max_hist = crate::state::read_shopts(|s| s.core.max_hist);
    let path = PathBuf::from(env::var("SHEDHIST").unwrap_or({
      let home = env::var("HOME").unwrap();
      format!("{home}/.shed_history")
    }));
    let mut entries = read_hist_file(&path)?;
    // Enforce max_hist limit on loaded entries
    if entries.len() > max_hist {
      entries = entries.split_off(entries.len() - max_hist);
    }
    let search_mask = dedupe_entries(&entries);
    let cursor = search_mask.len();
    Ok(Self {
      path,
      entries,
      pending: None,
      search_mask,
      no_matches: false,
      cursor,
      //search_direction: Direction::Backward,
      ignore_dups,
      max_size: Some(max_hist as u32),
    })
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

  pub fn get_new_id(&self) -> u32 {
    let Some(ent) = self.entries.last() else {
      return 0;
    };
    ent.id + 1
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
    let id = self.get_new_id();
    if self.ignore_dups && self.is_dup(&command) {
      return;
    }
    self.entries.push(HistEntry {
      id,
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
