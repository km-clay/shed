use std::{fs::{File, OpenOptions}, ops::{Deref, DerefMut}, path::PathBuf};

use bitflags::bitflags;
use rustyline::history::{History, SearchResult};
use serde::{Deserialize, Serialize};

use crate::{libsh::error::{ShErr, ShErrKind, ShResult}, prelude::*};

#[derive(Deserialize,Serialize,Debug)]
pub struct HistEntry {
	body: String,
	id: usize
}

impl HistEntry {
	pub fn new(body: String, id: usize) -> Self {
		Self { body, id }
	}
	pub fn cmd(&self) -> &str {
		&self.body
	}
	pub fn id(&self) -> usize {
		self.id
	}
}

#[derive(Deserialize,Serialize,Default)]
pub struct HistEntries {
	entries: Vec<HistEntry>
}

impl HistEntries {
	pub fn new() -> Self {
		Self { entries: vec![] }
	}
}

impl Deref for HistEntries {
	type Target = Vec<HistEntry>;
	fn deref(&self) -> &Self::Target {
		&self.entries
	}
}

impl DerefMut for HistEntries {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.entries
	}
}

pub struct FernHist {
	file_path: Option<PathBuf>,
	entries: HistEntries,
	max_len: usize,
	pub flags: HistFlags
}

bitflags! {
	pub struct HistFlags: u32 {
		const NO_DUPES     = 0b0000001;
		const IGNORE_SPACE = 0b0000010;
	}
}

impl<'e> FernHist {
	pub fn new() -> Self {
		Self { file_path: None, entries: HistEntries::new(), max_len: 1000, flags: HistFlags::empty() }
	}
	pub fn from_path(file_path: PathBuf) -> ShResult<'e,Self> {
		let mut new_hist = FernHist::new();
		new_hist.file_path = Some(file_path);
		new_hist.load_hist()?;
		Ok(new_hist)
	}
	pub fn create_entry(&self, body: &str) -> HistEntry {
		let id = self.len() + 1;
		HistEntry::new(body.to_string(), id)
	}
	pub fn init_hist_file(&mut self) -> ShResult<'e,()> {
		let Some(path) = self.file_path.clone() else {
			return Ok(());
		};
		self.save(&path)?;
		Ok(())
	}
	pub fn load_hist(&mut self) -> ShResult<'e,()> {
		let Some(file_path) = self.file_path.clone() else {
			return Err(
				ShErr::simple(
					ShErrKind::InternalErr,
					"History file not set"
				)
			)
		};
		if !file_path.is_file() {
			self.init_hist_file()?;
		}
		let hist_file = File::open(&file_path)?;
		self.entries = serde_yaml::from_reader(hist_file).unwrap_or_default();
		Ok(())
	}

}

impl Default for FernHist {
	fn default() -> Self {
		let home = std::env::var("HOME").unwrap();
		let file_path = PathBuf::from(&format!("{home}/.fernhist"));
		Self::from_path(file_path).unwrap()
	}
}

impl History for FernHist {
	fn add(&mut self, line: &str) -> rustyline::Result<bool> {
		let new_entry = self.create_entry(line);
		if self.flags.contains(HistFlags::NO_DUPES) {
			let most_recent = self.get(self.len(), rustyline::history::SearchDirection::Reverse)?.unwrap();
			dbg!(&most_recent);
			if new_entry.body == most_recent.entry.to_string() {
				return Ok(false)
			}
		}
		self.entries.push(new_entry);
		Ok(true)
	}

	fn get(&self, index: usize, dir: rustyline::history::SearchDirection) -> rustyline::Result<Option<rustyline::history::SearchResult>> {
		Ok(self.entries.iter().find(|ent| ent.id() == index).map(|ent| {
			SearchResult { entry: ent.cmd().to_string().into(), idx: index, pos: 0 }
		}))
	}

	fn add_owned(&mut self, line: String) -> rustyline::Result<bool> {
		todo!()
	}

	fn len(&self) -> usize {
		self.entries.len()
	}

	fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	fn set_max_len(&mut self, len: usize) -> rustyline::Result<()> {
		self.max_len = len;
		Ok(())
	}

	fn ignore_dups(&mut self, yes: bool) -> rustyline::Result<()> {
		if yes {
			self.flags |= HistFlags::NO_DUPES;
		} else {
			self.flags &= !HistFlags::NO_DUPES;
		}
		Ok(())
	}

	fn ignore_space(&mut self, yes: bool) {
		if yes {
			self.flags |= HistFlags::IGNORE_SPACE;
		} else {
			self.flags &= !HistFlags::IGNORE_SPACE;
		}
	}

	fn save(&mut self, path: &std::path::Path) -> rustyline::Result<()> {
		let hist_file = File::create(path)?;
		serde_yaml::to_writer(hist_file, &self.entries).unwrap();
		Ok(())
	}

	fn append(&mut self, path: &std::path::Path) -> rustyline::Result<()> {
		todo!()
	}

	fn load(&mut self, path: &std::path::Path) -> rustyline::Result<()> {
		let path = path.to_path_buf();
		self.file_path = Some(path);
		self.load_hist().map_err(|_| rustyline::error::ReadlineError::Io(std::io::Error::last_os_error()))
	}

	fn clear(&mut self) -> rustyline::Result<()> {
		self.entries.clear();
		if self.file_path.is_some() {
			self.save(&self.file_path.clone().unwrap())?;
		}
		Ok(())
	}

	fn search(
		&self,
		term: &str,
		start: usize,
		dir: rustyline::history::SearchDirection,
	) -> rustyline::Result<Option<rustyline::history::SearchResult>> {
		if term.is_empty() {
			return Ok(None)
		}
		let mut matches: Vec<&HistEntry> = self.entries.iter()
			.filter(|ent| is_subsequence(&ent.body, term))
			.collect();

		matches.sort_by(|ent_a, ent_b| {
			let ent_a_rank = fuzzy_rank(term, &ent_a.body);
			let ent_b_rank = fuzzy_rank(term, &ent_b.body);
			ent_a_rank.cmp(&ent_b_rank)
				.then(ent_a.id().cmp(&ent_b.id()))
		});

		Ok(matches.last().map(|ent| {
			SearchResult {
				entry: ent.body.clone().into(),
				idx: ent.id(),
				pos: start
			}
		}))
	}

	fn starts_with(
		&self,
		term: &str,
		start: usize,
		dir: rustyline::history::SearchDirection,
	) -> rustyline::Result<Option<rustyline::history::SearchResult>> {
		let mut matches: Vec<&HistEntry> = self.entries.iter()
			.filter(|ent| ent.body.starts_with(term))
			.collect();

		matches.sort_by(|ent_a, ent_b| ent_a.id().cmp(&ent_b.id()));
		dbg!(&matches);
		Ok(matches.first().map(|ent| {
			SearchResult {
				entry: ent.body.clone().into(),
				idx: ent.id(),
				pos: start
			}
		}))
	}
}

fn fuzzy_rank(search_term: &str, search_result: &str) -> u8 {
	if search_result == search_term {
		4
	} else if search_result.starts_with(search_term) {
		3
	} else if search_result.contains(search_term) {
		2
	} else if is_subsequence(search_result, search_term) {
		1
	} else {
		0
	}
}

// Check if a search term is a subsequence of the body (characters in order but not necessarily adjacent)
fn is_subsequence(search_result: &str, search_term: &str) -> bool {
	let mut result_chars = search_result.chars();
	search_term.chars().all(|ch| result_chars.any(|c| c == ch))
}
