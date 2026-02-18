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

use crate::libsh::error::{ShErr, ShErrKind, ShResult};
use crate::prelude::*;

use super::vicmd::Direction; // surprisingly useful

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
    let err = Err(ShErr::Simple {
      kind: ShErrKind::HistoryReadErr,
      msg: format!("Bad formatting on history entry '{s}'"),
      notes: vec![],
    });

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
        return Err(ShErr::Simple {
          kind: ShErrKind::HistoryReadErr,
          msg: format!("Bad formatting on line {i}"),
          notes: vec![],
        });
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
            return Err(ShErr::Simple {
              kind: ShErrKind::HistoryReadErr,
              msg: format!("Bad formatting on line {i}"),
              notes: vec![],
            });
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

/// Deduplicate entries, keeping only the most recent occurrence of each command.
/// Preserves chronological order (oldest to newest).
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
  entries: Vec<HistEntry>,
  search_mask: Vec<HistEntry>,
  cursor: usize,
  search_direction: Direction,
  ignore_dups: bool,
  max_size: Option<u32>,
}

impl History {
  pub fn new() -> ShResult<Self> {
    let path = PathBuf::from(env::var("FERNHIST").unwrap_or({
      let home = env::var("HOME").unwrap();
      format!("{home}/.fern_history")
    }));
    let mut entries = read_hist_file(&path)?;
    // Create pending entry for current input
    let id = entries.last().map(|ent| ent.id + 1).unwrap_or(0);
    entries.push(HistEntry {
      id,
      timestamp: SystemTime::now(),
      command: String::new(),
      new: true,
    });
    let search_mask = dedupe_entries(&entries);
    let cursor = search_mask.len().saturating_sub(1);
    Ok(Self {
      path,
      entries,
      search_mask,
      cursor,
      search_direction: Direction::Backward,
      ignore_dups: true,
      max_size: None,
    })
  }

	pub fn reset(&mut self) {
		self.search_mask = dedupe_entries(&self.entries);
		self.cursor = self.search_mask.len().saturating_sub(1);
	}

  pub fn entries(&self) -> &[HistEntry] {
    &self.entries
  }

  pub fn masked_entries(&self) -> &[HistEntry] {
    &self.search_mask
  }

  pub fn push_empty_entry(&mut self) {
		let timestamp = SystemTime::now();
		let id = self.get_new_id();
		self.entries.push(HistEntry {
			id,
			timestamp,
			command: String::new(),
			new: true,
		});
	}

  pub fn cursor_entry(&self) -> Option<&HistEntry> {
    self.search_mask.get(self.cursor)
  }

  pub fn update_pending_cmd(&mut self, command: &str) {
    let Some(ent) = self.last_mut() else { return };
    let cmd = command.to_string();
    let constraint = SearchConstraint {
      kind: SearchKind::Prefix,
      term: cmd.clone(),
    };

    ent.command = cmd;
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
    log::debug!("{constraint:?}");
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
					log::debug!("search mask len: {}", self.search_mask.len());
        }
        self.cursor = self.search_mask.len().saturating_sub(1);
      }
      SearchKind::Fuzzy => todo!(),
    }
  }

  pub fn hint_entry(&self) -> Option<&HistEntry> {
    let second_to_last = self.search_mask.len().checked_sub(2)?;
		log::info!("search mask: {:?}", self.search_mask.iter().map(|e| e.command()).collect::<Vec<_>>());
    self.search_mask.get(second_to_last)
  }

  pub fn get_hint(&self) -> Option<String> {
		log::info!("checking cursor entry: {:?}", self.cursor_entry());
    if self
      .cursor_entry()
      .is_some_and(|ent| ent.is_new() && !ent.command().is_empty())
    {
      let entry = self.hint_entry()?;
      let prefix = self.cursor_entry()?.command();
      Some(entry.command().to_string())
    } else {
      None
    }
  }

  pub fn scroll(&mut self, offset: isize) -> Option<&HistEntry> {
    let new_idx = self
      .cursor
      .saturating_add_signed(offset)
      .clamp(0, self.search_mask.len().saturating_sub(1));
    let ent = self.search_mask.get(new_idx)?;

    self.cursor = new_idx;

    Some(ent)
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
      .iter().rfind(|ent| !ent.new)
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
		self.push_empty_entry(); // Prepare for next command
		self.reset(); // Reset search mask to include new pending entry

    Ok(())
  }
}
