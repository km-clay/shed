use std::{env, fmt::{Write,Display}, fs::{self, OpenOptions}, io::Write as IoWrite, path::{Path, PathBuf}, str::FromStr, time::{Duration, SystemTime, UNIX_EPOCH}};

use crate::libsh::error::{ShErr, ShErrKind, ShResult};
use crate::prelude::*;

use super::vicmd::Direction; // surprisingly useful

#[derive(Debug)]
pub struct HistEntry {
	id: u32,
	timestamp: SystemTime,
	command: String,
	new: bool
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
		let mut chars = self.command.chars();
		while let Some(ch) = chars.next() {
			match ch {
				'\\' => {
					escaped.push(ch);
					if let Some(ch) = chars.next() {
						escaped.push(ch)
					}
				}
				'\n' => {
					escaped.push_str("\\\n");
				}
				_ => escaped.push(ch),
			}
		}
		escaped
	}
}

impl FromStr for HistEntry {
	type Err = ShErr;
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let err = Err(
			ShErr::Simple { kind: ShErrKind::HistoryReadErr, msg: format!("Bad formatting on history entry '{s}'"), notes: vec![] }
		);

		//: 248972349;148;echo foo; echo bar
		let Some(cleaned) = s.strip_prefix(": ") else { return err };
		//248972349;148;echo foo; echo bar
		let Some((timestamp,id_and_command)) = cleaned.split_once(';') else { return err };
		//("248972349","148;echo foo; echo bar")
		let Some((id,command)) = id_and_command.split_once(';') else { return err };
		//("148","echo foo; echo bar")
		let Ok(ts_seconds) = timestamp.parse::<u64>() else { return err };
		let Ok(id) = id.parse::<u32>() else { return err };
		let timestamp = UNIX_EPOCH + Duration::from_secs(ts_seconds);
		let command = command.to_string();
		Ok(Self { id, timestamp, command, new: false })
	}
}

impl Display for HistEntry {
	/// Similar to zsh's history format, but not entirely
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let command = self.with_escaped_newlines();
		let HistEntry { id, timestamp, command: _, new: _ } = self;
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

		while let Some((i,line)) = lines.next() {
			if !line.starts_with(": ") {
				return Err(
					ShErr::Simple { kind: ShErrKind::HistoryReadErr, msg: format!("Bad formatting on line {i}"), notes: vec![] }
				)
			}
			let mut chars = line.chars().peekable();
			let mut feeding_lines = true;
			while feeding_lines {
				feeding_lines = false;
				while let Some(ch) = chars.next() {
					match ch {
						'\\' => {
							if let Some(esc_ch) = chars.next() {
								cur_line.push(esc_ch);
							} else {
								cur_line.push('\n');
								feeding_lines = true;
							}
						}
						'\n' => {
							break
						}
						_ => {
							cur_line.push(ch);
						}
					}
				}
				if feeding_lines {
					let Some((_,line)) = lines.next() else {
						return Err(
							ShErr::Simple { kind: ShErrKind::HistoryReadErr, msg: format!("Bad formatting on line {i}"), notes: vec![] }
						)
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

pub struct History {
	path: PathBuf,
	entries: Vec<HistEntry>,
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
		let entries = read_hist_file(&path)?;
		let cursor = entries.len();
		let mut new = Self {
			path,
			entries,
			cursor,
			search_direction: Direction::Backward,
			ignore_dups: true,
			max_size: None,
		};
		new.push_empty_entry(); // Current pending command
		Ok(new)
	}

	pub fn entries(&self) -> &[HistEntry] {
		&self.entries
	}

	pub fn push_empty_entry(&mut self) {
		let id = self.get_new_id();
		let timestamp = SystemTime::now();
		let command = "".into();
		self.entries.push(HistEntry { id, timestamp, command, new: true })
	}

	pub fn update_pending_cmd(&mut self, command: &str) {
		flog!(DEBUG, "updating command");
		let Some(ent) = self.last_mut() else {
			return
		};

		ent.command = command.to_string()
	}

	pub fn last_mut(&mut self) -> Option<&mut HistEntry> {
		self.entries.last_mut()
	}

	pub fn get_new_id(&self) -> u32 {
		let Some(ent) = self.entries.last() else {
			return 0
		};
		ent.id + 1
	}

	pub fn ignore_dups(&mut self, yn: bool) {
		self.ignore_dups = yn
	}

	pub fn max_hist_size(&mut self, size: Option<u32>) {
		self.max_size = size
	}

	pub fn scroll(&mut self, offset: isize) -> Option<&HistEntry> {
		let new_idx = self.cursor
			.saturating_add_signed(offset)
			.clamp(0, self.entries.len());
		let ent = self.entries.get(new_idx)?;

		self.cursor = new_idx;

		Some(ent)
	}

	pub fn push(&mut self, command: String) {
		let timestamp = SystemTime::now();
		let id = self.get_new_id();
		if self.ignore_dups && self.is_dup(&command) {
			return
		}
		self.entries.push(HistEntry { id, timestamp, command, new: true });
	}

	pub fn is_dup(&self, other: &str) -> bool {
		let Some(ent) = self.entries.last() else {
			return false
		};
		let ent_cmd = &ent.command;
		ent_cmd == other
	}

	pub fn save(&mut self) -> ShResult<()> {
		let mut file = OpenOptions::new()
			.create(true)
			.append(true)
			.open(&self.path)?;

		let entries = self.entries.iter_mut().filter(|ent| ent.new);
		let mut data = String::new();
		for ent in entries {
			ent.new = false;
			write!(data, "{ent}").unwrap();
		}

		file.write_all(data.as_bytes())?;

		Ok(())
	}
}
