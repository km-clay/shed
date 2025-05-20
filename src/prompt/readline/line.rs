use std::ops::Range;

use crate::{libsh::{error::ShResult, term::{Style, Styled}}, prompt::readline::linecmd::Anchor};

use super::linecmd::{At, CharSearch, MoveCmd, Movement, Repeat, Verb, VerbCmd, Word};


#[derive(Default,Debug)]
pub struct LineBuf {
	pub buffer: Vec<char>,
	pub inserting: bool,
	pub last_insert: String,
	cursor: usize
}

impl LineBuf {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn with_initial<S: ToString>(mut self, init: S) -> Self {
		self.buffer = init.to_string().chars().collect();
		self
	}
	pub fn begin_insert(&mut self) {
		self.inserting = true;
	}
	pub fn finish_insert(&mut self) {
		self.inserting = false;
	}
	pub fn take_ins_text(&mut self) -> String {
		std::mem::take(&mut self.last_insert)
	}
	pub fn display_lines(&self) -> Vec<String> {
		let line_bullet = "∙ ".styled(Style::Dim);
		self.split_lines()
			.into_iter()
			.enumerate()
			.map(|(i, line)| {
				if i == 0 {
					line.to_string()
				} else {
					format!("{line_bullet}{line}")
				}
			})
		.collect()
	}
	pub fn repos_cursor(&mut self) {
		if self.cursor >= self.len() {
			self.cursor = self.len_minus_one();
		}
	}
	pub fn split_lines(&self) -> Vec<String> {
		let line = self.prepare_line();
		let mut lines = vec![];
		let mut cur_line = String::new();
		for ch in line.chars() {
			match ch {
				'\n' => lines.push(std::mem::take(&mut cur_line)),
				_ => cur_line.push(ch)
			}
		}
		lines.push(cur_line);
		lines
	}
	pub fn count_lines(&self) -> usize {
		self.buffer.iter().filter(|&&c| c == '\n').count()
	}
	pub fn cursor(&self) -> usize {
		self.cursor
	}
	pub fn prepare_line(&self) -> String {
		self.buffer
			.iter()
			.filter(|&&c| c != '\r')
			.collect::<String>()
	}
	pub fn clear(&mut self) {
		self.buffer.clear();
		self.cursor = 0;
	}
	pub fn cursor_display_coords(&self) -> (usize, usize) {
		let mut x = 0;
		let mut y = 0;
		for i in 0..self.cursor() {
			let ch = self.get_char(i);
			match ch {
				'\n' => {
					y += 1;
					x = 0;
				}
				'\r' => continue,
				_ => {
					x += 1;
				}
			}
		}

		(x, y)
	}
	pub fn cursor_real_coords(&self) -> (usize,usize) {
		let mut x = 0;
		let mut y = 0;
		for i in 0..self.cursor() {
			let ch = self.get_char(i);
			match ch {
				'\n' => {
					y += 1;
					x = 0;
				}
				_ => {
					x += 1;
				}
			}
		}

		(x, y)
	}
	pub fn backspace(&mut self) {
		if self.cursor() == 0 {
			return 
		}
		self.delete_pos(self.cursor() - 1);
	}
	pub fn delete(&mut self) {
		if self.cursor() >= self.buffer.len() {
			return 
		}
		self.delete_pos(self.cursor());
	}
	pub fn delete_pos(&mut self, pos: usize) {
		self.buffer.remove(pos);
		if pos < self.cursor() {
			self.cursor = self.cursor.saturating_sub(1)
		}
	}
	pub fn insert_at_pos(&mut self, pos: usize, ch: char) {
		self.buffer.insert(pos, ch)
	}
	pub fn insert_at_cursor(&mut self, ch: char) {
		self.buffer.insert(self.cursor, ch);
		self.move_cursor_right();
	}
	pub fn insert_after_cursor(&mut self, ch: char) {
		self.buffer.insert(self.cursor, ch);
	}
	pub fn backspace_at_cursor(&mut self) {
		assert!(self.cursor <= self.buffer.len());
		if self.buffer.is_empty() {
			return
		}
		self.buffer.remove(self.cursor.saturating_sub(1));
		self.move_cursor_left();
	}
	pub fn del_at_cursor(&mut self) {
		assert!(self.cursor <= self.buffer.len());
		if self.buffer.is_empty() || self.cursor == self.buffer.len() {
			return
		}
		self.buffer.remove(self.cursor);
	}
	pub fn move_cursor_left(&mut self) {
		self.cursor = self.cursor.saturating_sub(1);
	}
	pub fn move_cursor_start(&mut self) {
		self.cursor = 0;
	}
	pub fn move_cursor_end(&mut self) {
		self.cursor = self.buffer.len();
	}
	pub fn move_cursor_right(&mut self) {
		if self.cursor == self.buffer.len() {
			return
		}
		self.cursor = self.cursor.saturating_add(1);
	}
	pub fn del_from_cursor(&mut self) {
		self.buffer.truncate(self.cursor);
	}
	pub fn del_word_back(&mut self) {
		if self.cursor == 0 {
			return 
		}
		let end = self.cursor;
		let mut start = self.cursor;

		while start > 0 && self.buffer[start - 1].is_whitespace() {
			start -= 1;
		}

		while start > 0 && !self.buffer[start - 1].is_whitespace() {
			start -= 1;
		}

		self.buffer.drain(start..end);
		self.cursor = start;
	}
	pub fn len(&self) -> usize {
		self.buffer.len()
	}
	pub fn len_minus_one(&self) -> usize {
		self.buffer.len().saturating_sub(1)
	}
	pub fn is_empty(&self) -> bool {
		self.buffer.is_empty()
	}
	pub fn cursor_char(&self) -> Option<&char> {
		self.buffer.get(self.cursor())
	}
	pub fn get_char(&self, pos: usize) -> char {
		assert!((0..self.len()).contains(&pos));

		self.buffer[pos]
	}
	pub fn prev_char(&self) -> Option<char> {
		if self.cursor() == 0 {
			None
		} else {
			Some(self.get_char(self.cursor() - 1))
		}
	}
	pub fn next_char(&self) -> Option<char> {
		if self.cursor() == self.len_minus_one() {
			None
		} else {
			Some(self.get_char(self.cursor() + 1))
		}
	}
	pub fn on_word_bound_left(&self) -> bool {
		if self.cursor() == 0 {
			return false
		}
		let Some(ch) = self.cursor_char() else {
			return false
		};
		let cur_char_class = CharClass::from(*ch);
		let prev_char_pos = self.cursor().saturating_sub(1).max(0);
		cur_char_class.is_opposite(self.get_char(prev_char_pos)) 
	}
	pub fn on_word_bound_right(&self) -> bool {
		if self.cursor() >= self.len_minus_one() {
			return false
		}
		let Some(ch) = self.cursor_char() else {
			return false
		};
		let cur_char_class = CharClass::from(*ch);
		let next_char_pos = self.cursor().saturating_add(1).min(self.len());
		cur_char_class.is_opposite(self.get_char(next_char_pos))
	}
	fn backward_until<F: Fn(usize) -> bool>(&self, mut start: usize, cond: F) -> usize {
		while start > 0 && !cond(start) {
			start -= 1;
		}
		start
	}
	fn forward_until<F: Fn(usize) -> bool>(&self, mut start: usize, cond: F) -> usize {
		while start < self.len() && !cond(start) {
			start += 1;
		}
		start
	}
	pub fn calc_range(&mut self, movement: &Movement) -> Range<usize> {
		let mut start = self.cursor();
		let mut end = self.cursor();

		match movement {
			Movement::WholeLine => {
				start = self.backward_until(start, |pos| self.buffer[pos] == '\n');
				if self.buffer.get(start) == Some(&'\n') {
					start += 1; // Exclude the previous newline
				}
				end = self.forward_until(end, |pos| self.buffer[pos] == '\n');
			}
			Movement::BeginningOfLine => {
				start = self.backward_until(start, |pos| self.buffer[pos] == '\n');
			}
			Movement::BeginningOfFirstWord => {
				let start_of_line = self.backward_until(start, |pos| self.buffer[pos] == '\n');
				start = self.forward_until(start_of_line, |pos| !self.buffer[pos].is_whitespace());
			}
			Movement::EndOfLine => {
				end = self.forward_until(end, |pos| self.buffer[pos] == '\n');
			}
			Movement::BackwardWord(word) => {
				match word {
					Word::Big => {
						if self.cursor_char().is_none() {
							self.cursor = self.cursor.saturating_sub(1);
							start = start.saturating_sub(1)
						}
						// Skip whitespace
						let Some(cur_char) = self.cursor_char() else {
							return start..end
						};
						if cur_char.is_whitespace() {
							start = self.backward_until(start, |pos| !self.buffer[pos].is_whitespace())
						}

						let ch_class = CharClass::from(self.get_char(start));
						let should_step = self.prev_char().is_some_and(|ch| ch_class.is_opposite(ch));
						// If we are on a word boundary, move forward one character
						// If we are now on whitespace, skip it
						if should_step {
							start = start.saturating_sub(1).max(0);
							if self.get_char(start).is_whitespace() {
								start = self.backward_until(start, |pos| !self.buffer[pos].is_whitespace())
							}
						}
						start = self.backward_until(start, |pos| self.buffer[pos].is_whitespace());
						if self.get_char(start).is_whitespace() {
							start += 1;
						}
					}
					Word::Normal => {
						if self.cursor_char().is_none() {
							self.cursor = self.cursor.saturating_sub(1);
							start = start.saturating_sub(1)
						}
						let Some(cur_char) = self.cursor_char() else {
							return start..end
						};
						// Skip whitespace
						if cur_char.is_whitespace() {
							start = self.backward_until(start, |pos| !self.get_char(pos).is_whitespace())
						}

						let ch_class = CharClass::from(self.get_char(start));
						let should_step = self.prev_char().is_some_and(|ch| ch_class.is_opposite(ch));
						// If we are on a word boundary, move forward one character
						// If we are now on whitespace, skip it
						if should_step {
							start = start.saturating_sub(1).max(0);
							if self.get_char(start).is_whitespace() {
								start = self.backward_until(start, |pos| !self.get_char(pos).is_whitespace())
							}
						}

						// Find an alternate charclass to stop at
						let cur_char = self.get_char(start);
						let cur_char_class = CharClass::from(cur_char);
						start = self.backward_until(start, |pos| cur_char_class.is_opposite(self.get_char(pos)));
						if cur_char_class.is_opposite(self.get_char(start)) {
							start += 1;
						}
					}
				}
			}
			Movement::ForwardWord(at, word) => {
				let Some(cur_char) = self.cursor_char() else {
					return start..end
				};
				let is_ws = |pos: usize| self.buffer[pos].is_whitespace();
				let not_ws = |pos: usize| !self.buffer[pos].is_whitespace();

				match word {
					Word::Big => {
						if cur_char.is_whitespace() {
							end = self.forward_until(end, not_ws);
						} 

						let ch_class = CharClass::from(self.buffer[end]);
						let should_step = self.next_char().is_some_and(|ch| ch_class.is_opposite(ch));

						if should_step {
							end = end.saturating_add(1).min(self.len_minus_one());
							if self.get_char(end).is_whitespace() {
								end = self.forward_until(end, |pos| !self.get_char(pos).is_whitespace())
							}
						}

						match at {
							At::Start => {
								if !should_step {
									end = self.forward_until(end, is_ws);
									end = self.forward_until(end, not_ws);
								}
							}
							At::AfterEnd => {
								end = self.forward_until(end, is_ws);
							}
							At::BeforeEnd => {
								end = self.forward_until(end, is_ws);
								if self.buffer.get(end).is_some_and(|ch| ch.is_whitespace()) {
									end = end.saturating_sub(1);
								}
							}
						}
					}
					Word::Normal => {
						if cur_char.is_whitespace() {
							end = self.forward_until(end, not_ws);
						} 

						let ch_class = CharClass::from(self.buffer[end]);
						let should_step = self.next_char().is_some_and(|ch| ch_class.is_opposite(ch));

						if should_step {
							end = end.saturating_add(1).min(self.len_minus_one());
							if self.get_char(end).is_whitespace() {
								end = self.forward_until(end, |pos| !self.get_char(pos).is_whitespace())
							}
						}

						match at {
							At::Start => {
								if !should_step {
									end = self.forward_until(end, |pos| ch_class.is_opposite(self.buffer[pos]));
									if self.get_char(end).is_whitespace() {
										end = self.forward_until(end, |pos| !self.get_char(pos).is_whitespace())
									}
								}
							}
							At::AfterEnd => {
								end = self.forward_until(end, |pos| ch_class.is_opposite(self.buffer[pos]));
							}
							At::BeforeEnd => {
								end = self.forward_until(end, |pos| ch_class.is_opposite(self.buffer[pos]));
								if self.buffer.get(end).is_some_and(|ch| ch.is_whitespace()) {
									end = end.saturating_sub(1);
								}
							}
						}
					}
				}
			}
			Movement::BackwardChar => {
				start = start.saturating_sub(1);
			}
			Movement::ForwardChar => {
				end = end.saturating_add(1);
			}
			Movement::TextObj(text_obj, bound) => todo!(),
			Movement::CharSearch(char_search) => {
				match char_search {
					CharSearch::FindFwd(ch) => {
						let ch = ch.unwrap();
						end = end.saturating_add(1).min(self.len_minus_one());
						let search = self.forward_until(end, |pos| self.buffer[pos] == ch);

						// we check anyway because it may have reached the end without finding anything
						if self.buffer.get(search).is_some_and(|&s_ch| s_ch == ch) { 
							end = search;
						}
					}
					CharSearch::FwdTo(ch) => {
						let ch = ch.unwrap();
						end = end.saturating_add(1).min(self.len_minus_one());
						let search = self.forward_until(end, |pos| self.buffer[pos] == ch);

						// we check anyway because it may have reached the end without finding anything
						if self.buffer.get(search).is_some_and(|&s_ch| s_ch == ch) { 
							end = search.saturating_sub(1);
						}
					}
					CharSearch::FindBkwd(ch) => {
						let ch = ch.unwrap();
						start = start.saturating_sub(1);
						let search = self.backward_until(start, |pos| self.buffer[pos] == ch);

						// we check anyway because it may have reached the end without finding anything
						if self.buffer.get(search).is_some_and(|&s_ch| s_ch == ch) { 
							start = search;
						}
					}
					CharSearch::BkwdTo(ch) => {
						let ch = ch.unwrap();
						start = start.saturating_sub(1);
						let search = self.backward_until(start, |pos| self.buffer[pos] == ch);

						// we check anyway because it may have reached the end without finding anything
						if self.buffer.get(search).is_some_and(|&s_ch| s_ch == ch) { 
							start = search.saturating_add(1);
						}
					}
				}
			}
			Movement::LineUp => todo!(),
			Movement::LineDown => todo!(),
			Movement::WholeBuffer => {
				start = 0;
				end = self.len_minus_one();
			}
			Movement::BeginningOfBuffer => {
				start = 0;
			}
			Movement::EndOfBuffer => {
				end = self.len_minus_one();
			}
			Movement::Null => {/* nothing */}
		}

		end = end.min(self.len());

		start..end
	}
	pub fn exec_vi_cmd(&mut self, verb: Option<Verb>, move_cmd: Option<MoveCmd>) -> ShResult<()> {
		match (verb, move_cmd) {
			(Some(v), None) => self.exec_vi_verb(v),
			(None, Some(m)) => self.exec_vi_movement(m),
			(Some(v), Some(m)) => self.exec_vi_moveverb(v,m),
			(None, None) => unreachable!()
		}
	}
	pub fn exec_vi_verb(&mut self, verb: Verb) -> ShResult<()> {
		assert!(!verb.needs_movement());
		match verb {
			Verb::DeleteOne(anchor) => {
				match anchor {
					Anchor::After => {
						self.delete();
					}
					Anchor::Before => {
						self.backspace();
					}
				}
			}
			Verb::Breakline(anchor) => {
				match anchor {
					Anchor::Before => {
						let last_newline = self.backward_until(self.cursor(), |pos| self.get_char(pos) == '\n');
						self.cursor = last_newline;
						self.insert_at_cursor('\n');
						self.insert_at_cursor('\r');
					}
					Anchor::After => {
						let next_newline = self.forward_until(self.cursor(), |pos| self.get_char(pos) == '\n');
						self.cursor = next_newline;
						self.insert_at_cursor('\n');
						self.insert_at_cursor('\r');
					}
				}
			}
			Verb::InsertChar(ch) => {
				if self.inserting {
					self.last_insert.push(ch);
				}
				self.insert_at_cursor(ch)
			}
			Verb::Insert(text) => {
				for ch in text.chars() {
					if self.inserting {
						self.last_insert.push(ch);
					}
					self.insert_at_cursor(ch);
				}
			}
			Verb::InsertMode => todo!(),
			Verb::JoinLines => todo!(),
			Verb::ToggleCase => todo!(),
			Verb::OverwriteMode => todo!(),
			Verb::Substitute => todo!(),
			Verb::Put(_) => todo!(),
			Verb::Undo => todo!(),
			Verb::RepeatLast => todo!(),
			Verb::Dedent => {
				let mut start_pos = self.backward_until(self.cursor(), |pos| self.get_char(pos) == '\n');
				if self.get_char(start_pos) == '\n' {
					start_pos += 1;
				}
				if self.get_char(start_pos) == '\t' {
					self.delete_pos(start_pos);
				}
			}
			Verb::Indent => {
				let mut line_start = self.backward_until(self.cursor(), |pos| self.get_char(pos) == '\n');
				if self.get_char(line_start) == '\n' {
					line_start += 1;
				}
				self.insert_at_pos(line_start, '\t');
			}
			Verb::ReplaceChar(_) => todo!(),
			_ => unreachable!()
		}
		Ok(())
	}
	pub fn exec_vi_movement(&mut self, move_cmd: MoveCmd) -> ShResult<()> {
		let MoveCmd { move_count, movement } = move_cmd;
		for _ in 0..move_count {
			let range = self.calc_range(&movement);
			if range.start != self.cursor() {
				self.cursor = range.start.max(0);
			} else {
				self.cursor = range.end.min(self.len());
			}
		}
		Ok(())
	}
	pub fn exec_vi_moveverb(&mut self, verb: Verb, move_cmd: MoveCmd) -> ShResult<()> {
		let MoveCmd { move_count, movement } = move_cmd;
		match verb {
			Verb::Delete => {
				(0..move_count).for_each(|_| {
					let range = self.calc_range(&movement);
					let range = range.start..(range.end + 1).min(self.len());
					self.buffer.drain(range);
					self.repos_cursor();
				});
			}
			Verb::Change => {
				(0..move_count).for_each(|_| {
					let range = self.calc_range(&movement);
					let range = range.start..(range.end + 1).min(self.len());
					self.buffer.drain(range);
					self.repos_cursor();
				});
			}
			Verb::Repeat(rep) => {

			}
			Verb::DeleteOne(anchor) => todo!(),
			Verb::Breakline(anchor) => todo!(),
			Verb::Yank => todo!(),
			Verb::ReplaceChar(_) => todo!(),
			Verb::Substitute => todo!(),
			Verb::ToggleCase => todo!(),
			Verb::Undo => todo!(),
			Verb::RepeatLast => todo!(),
			Verb::Put(anchor) => todo!(),
			Verb::OverwriteMode => todo!(),
			Verb::InsertMode => todo!(),
			Verb::JoinLines => todo!(),
			Verb::InsertChar(_) => todo!(),
			Verb::Indent => todo!(),
			Verb::Dedent => todo!(),
		}
		Ok(())
	} 
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CharClass {
	AlphaNum,
	Symbol,
}

impl CharClass {
	pub fn is_opposite(&self, other: char) -> bool {
		let other_class = CharClass::from(other);
		other_class != *self
	}
}

impl From<char> for CharClass {
	fn from(value: char) -> Self {
		if value.is_alphanumeric() || value == '_' { 
			CharClass::AlphaNum 
		} else {
			CharClass::Symbol 
		}
	}
}


pub fn strip_ansi_codes_and_escapes(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	let mut chars = s.chars().peekable();

	while let Some(c) = chars.next() {
		if c == '\x1b' && chars.peek() == Some(&'[') {
			// Skip over the escape sequence
			chars.next(); // consume '['
			while let Some(&ch) = chars.peek() {
				if ch.is_ascii_lowercase() || ch.is_ascii_uppercase() {
					chars.next(); // consume final letter
					break;
				}
				chars.next(); // consume intermediate characters
			}
		} else {
			match c {
				'\n' |
				'\r' => { /* Continue */ }
				_ => out.push(c)
			}
		}
	}
	out
}
