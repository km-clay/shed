use std::ops::Range;

use crate::{libsh::error::ShResult, prompt::readline::linecmd::Anchor};

use super::linecmd::{At, CharSearch, MoveCmd, Movement, Verb, VerbCmd, Word};


#[derive(Default,Debug)]
pub struct LineBuf {
	pub buffer: Vec<char>,
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
	pub fn count_lines(&self) -> usize {
		self.buffer.iter().filter(|&&c| c == '\n').count()
	}
	pub fn cursor(&self) -> usize {
		self.cursor
	}
	pub fn clear(&mut self) {
		self.buffer.clear();
		self.cursor = 0;
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
		if self.cursor() >= self.buffer.len() {
			self.cursor = self.buffer.len().saturating_sub(1)
		}
	}
	pub fn insert_at_cursor(&mut self, ch: char) {
		self.buffer.insert(self.cursor, ch);
		self.move_cursor_right();
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
	pub fn is_empty(&self) -> bool {
		self.buffer.is_empty()
	}
	pub fn cursor_char(&self) -> char {
		self.buffer[self.cursor]
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
	pub fn calc_range(&self, movement: &Movement) -> Range<usize> {
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
				let cur_char = self.cursor_char();
				match word {
					Word::Big => {
						if cur_char.is_whitespace() {
							start = self.backward_until(start, |pos| !self.buffer[pos].is_whitespace())
						}
						start = self.backward_until(start, |pos| self.buffer[pos].is_whitespace());
						start += 1;
					}
					Word::Normal => {
						if cur_char.is_alphanumeric() || cur_char == '_' {
							start = self.backward_until(start, |pos| !(self.buffer[pos].is_alphanumeric() || self.buffer[pos] == '_'));
							start += 1;
						} else {
							start = self.backward_until(start, |pos| (self.buffer[pos].is_alphanumeric() || self.buffer[pos] == '_'));
							start += 1;
						}
					}
				}
			}
			Movement::ForwardWord(at, word) => {
				let cur_char = self.cursor_char();
				let is_ws = |pos: usize| self.buffer[pos].is_whitespace();
				let not_ws = |pos: usize| !self.buffer[pos].is_whitespace();

				match word {
					Word::Big => {
						if cur_char.is_whitespace() {
							end = self.forward_until(end, not_ws);
						} else {
							end = self.forward_until(end, is_ws);
							end = self.forward_until(end, not_ws);
						}

						match at {
							At::Start => {/* Done */}
							At::AfterEnd => {
								end = self.forward_until(end, is_ws);
							}
							At::BeforeEnd => {
								end = self.forward_until(end, is_ws);
								end = end.saturating_sub(1);
							}
						}
					}
					Word::Normal => {
						let ch_class = CharClass::from(self.buffer[end]);
						if cur_char.is_whitespace() {
							end = self.forward_until(end, not_ws);
						} else {
							end = self.forward_until(end, |pos| ch_class.is_opposite(self.buffer[pos]))
						}

						match at {
							At::Start => {/* Done */ }
							At::AfterEnd => {
								end = self.forward_until(end, |pos| ch_class.is_opposite(self.buffer[pos]));
							}
							At::BeforeEnd => {
								end = self.forward_until(end, |pos| ch_class.is_opposite(self.buffer[pos]));
								end = end.saturating_sub(1);
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
						let search = self.forward_until(end, |pos| self.buffer[pos] == *ch);

						// we check anyway because it may have reached the end without finding anything
						if self.buffer[search] == *ch { 
							end = search;
						}
					}
					CharSearch::FwdTo(ch) => {
						let search = self.forward_until(end, |pos| self.buffer[pos] == *ch);

						// we check anyway because it may have reached the end without finding anything
						if self.buffer[search] == *ch { 
							end = search.saturating_sub(1);
						}
					}
					CharSearch::FindBkwd(ch) => {
						let search = self.forward_until(start, |pos| self.buffer[pos] == *ch);

						// we check anyway because it may have reached the end without finding anything
						if self.buffer[search] == *ch { 
							start = search;
						}
					}
					CharSearch::BkwdTo(ch) => {
						let search = self.forward_until(start, |pos| self.buffer[pos] == *ch);

						// we check anyway because it may have reached the end without finding anything
						if self.buffer[search] == *ch { 
							start = search.saturating_add(1);
						}
					}
				}
			}
			Movement::ViFirstPrint => todo!(),
			Movement::LineUp => todo!(),
			Movement::LineDown => todo!(),
			Movement::WholeBuffer => {
				start = 0;
				end = self.len().saturating_sub(1);
			}
			Movement::BeginningOfBuffer => {
				start = 0;
			}
			Movement::EndOfBuffer => {
				end = self.len().saturating_sub(1);
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
			Verb::InsertChar(ch) => self.insert_at_cursor(ch),
			Verb::InsertMode => todo!(),
			Verb::JoinLines => todo!(),
			Verb::ToggleCase => todo!(),
			Verb::OverwriteMode => todo!(),
			Verb::Substitute => todo!(),
			Verb::Put(_) => todo!(),
			Verb::Undo => todo!(),
			Verb::RepeatLast => todo!(),
			Verb::Dedent => todo!(),
			Verb::Indent => todo!(),
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
				});
			}
			Verb::DeleteOne(anchor) => todo!(),
			Verb::Change => todo!(),
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
	Symbol
}

impl CharClass {
	pub fn is_opposite(&self, ch: char) -> bool {
		let opp_class = CharClass::from(ch);
		opp_class != *self
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


pub fn strip_ansi_codes(s: &str) -> String {
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
			out.push(c);
		}
	}
	out
}
