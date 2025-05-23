use std::{fmt::Display, ops::{Deref, DerefMut, Range, RangeBounds, RangeInclusive}, sync::Arc};

use unicode_width::UnicodeWidthStr;

use crate::libsh::{error::ShResult, sys::sh_quit, term::{Style, Styled}};
use crate::prelude::*;

use super::vicmd::{Anchor, Bound, Dest, Direction, Motion, RegisterName, TextObj, To, Verb, ViCmd, Word};

#[derive(Debug, PartialEq, Eq)]
pub enum CharClass {
	Alphanum,
	Symbol
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionKind {
	Forward(usize),
	To(usize),
	Backward(usize),
	Range(Range<usize>),
	Null
}

impl MotionKind {
	pub fn range<R: RangeBounds<usize>>(range: R) -> Self {
		let start = match range.start_bound() {
			std::ops::Bound::Included(&start) => start,
			std::ops::Bound::Excluded(&start) => start + 1,
			std::ops::Bound::Unbounded => 0
		};
		let end = match range.end_bound() {
			std::ops::Bound::Included(&end) => end,
			std::ops::Bound::Excluded(&end) => end + 1,
			std::ops::Bound::Unbounded => panic!("called range constructor with no upper bound")
		};
		if end > start {
			Self::Range(start..end)
		} else {
			Self::Range(end..start)
		}
	}
}

#[derive(Clone,Default,Debug)]
pub struct TermCharBuf(pub Vec<TermChar>);

impl Deref for TermCharBuf {
	type Target = Vec<TermChar>;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl DerefMut for TermCharBuf {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.0
	}
}

impl Display for TermCharBuf {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		for ch in &self.0 {
			match ch {
				TermChar::Grapheme(str) => write!(f, "{str}")?,
				TermChar::Newline => write!(f, "\r\n")?,
			}
		}
		Ok(())
	}
}

impl FromIterator<TermChar> for TermCharBuf {
	fn from_iter<T: IntoIterator<Item = TermChar>>(iter: T) -> Self {
		let mut buf = vec![];
		for item in iter {
			buf.push(item)
		}
		Self(buf)
	}
}

impl From<TermCharBuf> for String {
	fn from(value: TermCharBuf) -> Self {
		let mut string = String::new();
		for char in value.0 {
			match char {
				TermChar::Grapheme(str) => string.push_str(&str),
				TermChar::Newline => {
					string.push('\r');
					string.push('\n');
				}
			}
		}
		string
	}
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TermChar {
	Grapheme(Arc<str>),
	// Treated as '\n' in the code, printed as '\r\n' to the terminal
	Newline 
}

impl TermChar {
	pub fn is_whitespace(&self) -> bool {
		match self {
			TermChar::Newline => true,
			TermChar::Grapheme(ch) => {
				ch.chars().next().is_some_and(|c| c.is_whitespace())
			}
		}
	}
	pub fn matches(&self, other: &str) -> bool {
		match self {
			TermChar::Grapheme(ch) => {
				ch.as_ref() == other
			}
			TermChar::Newline => other == "\n"
		}
	}
}

impl From<Arc<str>> for TermChar {
	fn from(value: Arc<str>) -> Self {
		Self::Grapheme(value)
	}
}

impl From<char> for TermChar {
	fn from(value: char) -> Self {
		match value {
			'\n' => Self::Newline,
			ch => Self::Grapheme(Arc::from(ch.to_string()))
		}
	}
}

impl Display for TermChar {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			TermChar::Grapheme(str) => {
				write!(f,"{str}")
			}
			TermChar::Newline => {
				write!(f,"\r\n")
			}
		}
	}
}

impl From<&TermChar> for CharClass {
	fn from(value: &TermChar) -> Self {
		match value {
			TermChar::Newline => Self::Symbol,
			TermChar::Grapheme(ch) => {
				if ch.chars().next().is_some_and(|c| c.is_alphanumeric()) {
					Self::Alphanum
				} else {
					Self::Symbol
				}
			}
		}
	}
}

impl From<char> for CharClass {
	fn from(value: char) -> Self {
		if value.is_alphanumeric() {
			Self::Alphanum
		} else {
			Self::Symbol
		}
	}
}

fn is_other_class_or_ws(a: &TermChar, b: &TermChar) -> bool {
	if a.is_whitespace() || b.is_whitespace() {
		return true;
	}

	CharClass::from(a) != CharClass::from(b)
}

pub struct UndoPayload {
	buffer: TermCharBuf,
	cursor: usize
}

#[derive(Default,Debug)]
pub struct Edit {
	pub pos: usize,
	pub cursor_pos: usize,
	pub old: TermCharBuf,
	pub new: TermCharBuf
}

impl Edit {
	pub fn diff(a: TermCharBuf, b: TermCharBuf, old_cursor_pos: usize) -> Self {
		use std::cmp::min;

		let mut start = 0;
		let max_start = min(a.len(), b.len());

		// Calculate the prefix of the edit
		while start < max_start && a[start] == b[start] {
			start += 1;
		}

		if start == a.len() && start == b.len() {
			return Edit {
				pos: start,
				cursor_pos: old_cursor_pos,
				old: TermCharBuf(vec![]),
				new: TermCharBuf(vec![]),
			}
		}

		let mut end_a = a.len();
		let mut end_b = b.len();

		// Calculate the suffix of the edit
		while end_a > start && end_b > start && a[end_a - 1] == b[end_b - 1] {
			end_a -= 1;
			end_b -= 1;
		}

		// Slice off the prefix and suffix for both
		let old = TermCharBuf(a[start..end_a].to_vec());
		let new = TermCharBuf(b[start..end_b].to_vec());

		Edit {
			pos: start,
			cursor_pos: old_cursor_pos,
			old,
			new
		}
	}
}

#[derive(Default,Debug)]
pub struct LineBuf {
	buffer: TermCharBuf,
	cursor: usize,
	clamp_cursor: bool,
	merge_edit: bool,
	undo_stack: Vec<Edit>,
	redo_stack: Vec<Edit>,
	term_dims: (usize,usize)
}

impl LineBuf {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn with_initial(mut self, initial: &str) -> Self {
		let chars = initial.chars();
		for char in chars {
			self.buffer.push(char.into())
		}
		self
	}
	pub fn set_cursor_clamp(&mut self, yn: bool) {
		self.clamp_cursor = yn
	}
	pub fn buffer(&self) -> &TermCharBuf {
		&self.buffer
	}
	pub fn cursor(&self) -> usize {
		self.cursor
	}
	pub fn cursor_char(&self) -> Option<&TermChar> {
		let tc = self.buffer.get(self.cursor())?;
		Some(tc)
	}
	pub fn get_char(&self, pos: usize) -> Option<&TermChar> {
		let tc = self.buffer.get(pos)?;
		Some(tc)
	}
	pub fn insert_at_cursor(&mut self, tc: TermChar) {
		let cursor = self.cursor();
		self.buffer.insert(cursor,tc)
	}
	pub fn count_lines(&self, first_line_offset: usize) -> usize {
		let mut cur_line_len = 0;
		let mut lines = 1;
		let first_line_max_len = self.term_dims.1.saturating_sub(first_line_offset);
		for char in self.buffer.iter() {
			match char {
				TermChar::Newline => {
					lines += 1;
					cur_line_len = 0;
				}
				TermChar::Grapheme(str) => {
					cur_line_len += str.width().max(1);
					if (lines == 1 && first_line_max_len > 0 && cur_line_len >= first_line_max_len) || cur_line_len > self.term_dims.1 {
						lines += 1;
						cur_line_len = 0;
					}
				}
			}
		}
		lines
	}
	pub fn cursor_back(&mut self, count: usize) {
		self.cursor = self.cursor.saturating_sub(count)
	}
	pub fn cursor_fwd(&mut self, count: usize) {
		self.cursor = self.num_or_len(self.cursor + count)
	}
	pub fn cursor_to(&mut self, pos: usize) {
		self.cursor = self.num_or_len(pos)
	}
	pub fn prepare_line(&self) -> String {
		self.buffer.to_string()
	}
	pub fn clamp_cursor(&mut self) {
		if self.cursor_char().is_none() && !self.buffer.is_empty() {
			self.cursor = self.cursor.saturating_sub(1)
		}
	}
	pub fn update_term_dims(&mut self, x: usize, y: usize) {
		self.term_dims = (x,y)
	}
	pub fn cursor_display_coords(&self, first_line_offset: Option<usize>) -> (usize, usize) {
		let mut x = 0;
		let mut y = 0;
		let first_line_max_len = first_line_offset.map(|fl| self.term_dims.1.saturating_sub(fl)).unwrap_or_default();
		for i in 0..self.cursor() {
			let ch = self.get_char(i).unwrap();
			match ch {
				TermChar::Grapheme(str) => {
					x += str.width().max(1);
					if (y == 0 && first_line_max_len > 0 && x >= first_line_max_len) || x > self.term_dims.1 {
						y += 1;
						x = 0;
					}
				}
				TermChar::Newline => {
					y += 1;
					x = 0;
				}
			}
		}

		(x, y)
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
	pub fn on_word_bound(&self, word: Word, pos: usize, dir: Direction) -> bool {
		let check_pos = match dir {
			Direction::Forward => self.num_or_len(pos + 1),
			Direction::Backward => pos.saturating_sub(1)
		};
		let Some(curr_char) = self.cursor_char() else {
			return false
		};
		self.get_char(check_pos).is_some_and(|c| {
			match word {
				Word::Big => c.is_whitespace(),
				Word::Normal => is_other_class_or_ws(curr_char, c)
			}
		})
	}
	fn backward_until<F: Fn(&TermChar) -> bool>(&self, mut start: usize, cond: F, inclusive: bool) -> usize {
		start = self.num_or_len_minus_one(start);
		while start > 0 && !cond(&self.buffer[start]) {
			start -= 1;
		}
		if !inclusive {
			if start > 0 {
				start.saturating_add(1)
			} else {
				start
			}
		} else {
			start
		}
	}
	fn forward_until<F: Fn(&TermChar) -> bool>(&self, mut start: usize, cond: F, inclusive: bool) -> usize {
		while start < self.buffer.len() && !cond(&self.buffer[start]) {
			start += 1;
		}
		if !inclusive {
			if start < self.buffer.len() {
				start.saturating_sub(1)
			} else {
				start
			}
		} else {
			start
		}
	}
	pub fn find_word_pos(&self, word: Word, dest: To, dir: Direction) -> usize {
		let mut pos = self.cursor();
		match dir {
			Direction::Forward => {
				match word {
					Word::Big => {
						match dest {
							To::Start => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off of the word
									pos = self.num_or_len(pos + 1);
								}
								// Pass the current word if any
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									pos = self.forward_until(pos, |c| c.is_whitespace(), true);
								}
								// Land on the start of the next word
								pos = self.forward_until(pos, |c| !c.is_whitespace(), true)
							}
							To::End => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off of the word
									pos = self.num_or_len(pos + 1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									// We are in a word
									// Go to the end of the current word
									pos = self.forward_until(pos, |c| c.is_whitespace(), false)
								} else {
									// We are outside of a word
									// Find the next word, then go to the end of it
									pos = self.forward_until(pos, |c| !c.is_whitespace(), true);
									pos = self.forward_until(pos, |c| c.is_whitespace(), false)
								}
							}
						}
					}
					Word::Normal => {
						match dest {
							To::Start => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off of the word
									pos = self.num_or_len(pos + 1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									// We are inside of a word
									// Find the next instance of whitespace or a different char class
									let this_char = self.get_char(pos).unwrap();
									pos = self.forward_until(pos, |c| is_other_class_or_ws(this_char, c), true);

									// If we found whitespace, continue until we find non-whitespace
									if self.get_char(pos).is_some_and(|c| c.is_whitespace()) {
										pos = self.forward_until(pos, |c| !c.is_whitespace(), true)
									}
								} else {
									// We are in whitespace, proceed to the next word
									pos = self.forward_until(pos, |c| !c.is_whitespace(), true)
								}
							}
							To::End => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off of the word
									pos = self.num_or_len(pos + 1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									// Proceed up until the next differing char class
									let this_char = self.get_char(pos).unwrap();
									pos = self.forward_until(pos, |c| is_other_class_or_ws(this_char, c), false);
								} else {
									// Find the next non-whitespace character
									pos = self.forward_until(pos, |c| !c.is_whitespace(), true);
									// Then proceed until a differing char class is found
									let this_char = self.get_char(pos).unwrap();
									pos = self.forward_until(pos, |c|is_other_class_or_ws(this_char, c), false);
								}
							}
						}
					}
				}
			}
			Direction::Backward => {
				match word {
					Word::Big => {
						match dest {
							To::Start => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off
									pos = pos.saturating_sub(1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									// We are in a word, go to the start of it
									pos = self.backward_until(pos, |c| c.is_whitespace(), false);
								} else {
									// We are not in a word, find one and go to the start of it
									pos = self.backward_until(pos, |c| !c.is_whitespace(), true);
									pos = self.backward_until(pos, |c| c.is_whitespace(), false);
								}
							}
							To::End => {
								if self.on_word_bound(word, pos, dir) {
									pos = pos.saturating_sub(1);
								}

								if self.get_char(pos).is_some_and(|c| c.is_whitespace()) {
									pos = self.backward_until(pos, |c| !c.is_whitespace(), true);
								} else {
									pos = self.backward_until(pos, |c| c.is_whitespace(), true);
									pos = self.backward_until(pos, |c| !c.is_whitespace(), true);
								}
							}
						}
					}
					Word::Normal => {
						match dest {
							To::Start => {
								if self.on_word_bound(word, pos, dir) {
									pos = pos.saturating_sub(1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									let this_char = self.get_char(pos).unwrap();
									pos = self.backward_until(pos, |c| is_other_class_or_ws(this_char, c), false)
								} else {
									pos = self.backward_until(pos, |c| !c.is_whitespace(), true);
									let this_char = self.get_char(pos).unwrap();
									pos = self.backward_until(pos, |c| is_other_class_or_ws(this_char, c), false);
								}
							}
							To::End => {
								if self.on_word_bound(word, pos, dir) {
									// Nudge
									pos = pos.saturating_sub(1);
								}
								// If we are on whitespace, proceed until we are not, inclusively
								if self.get_char(pos).is_some_and(|c| c.is_whitespace()) {
									pos = self.backward_until(pos, |c| !c.is_whitespace(), true)
								} else {
									// If we are not on whitespace, proceed until we hit something different, inclusively
									let this_char = self.get_char(pos).unwrap();
									pos = self.backward_until(pos, |c| is_other_class_or_ws(this_char, c), true);
									// If we landed on whitespace, proceed until we are not on whitespace
									if self.get_char(pos).is_some_and(|c| c.is_whitespace()) {
										pos = self.backward_until(pos, |c| !c.is_whitespace(), true)
									}
								}
							}
						}
					}
				}
			}
		}
		pos
	}
	pub fn eval_quote_obj(&self, target: &str, bound: Bound) -> Range<usize> {
		let mut end;
		let start;
		let cursor = self.cursor();
		let ln_start = self.backward_until(cursor, |c| c == &TermChar::Newline, false);
		let mut line_chars = self.buffer[ln_start..cursor].iter();
		let mut in_quote = false;
		while let Some(ch) = line_chars.next() {
			let TermChar::Grapheme(ch) = ch else { unreachable!() };
			match ch.as_ref() {
				"\\" => {
					line_chars.next();
				}
				"\"" => in_quote = !in_quote,
				_ => { /* continue */ }
			}
		}
		let mut start_pos = cursor;
		let end_pos;
		if !in_quote {
			start_pos = self.forward_until(start_pos, |c| c.matches("\n") || c.matches(target), true);
			if !self.get_char(start_pos).is_some_and(|c| c.matches(target)) {
				return cursor..cursor
			}
			end_pos = self.forward_until(start_pos, |c| c.matches("\n") || c.matches(target), true);
			if !self.get_char(end_pos).is_some_and(|c| c.matches(target)) {
				return cursor..cursor
			}
			start = start_pos;
			end = end_pos;
		} else {
			start_pos = self.backward_until(start_pos, |c| c.matches("\n") || c.matches(target), true);
			if !self.get_char(start_pos).is_some_and(|c| c.matches(target)) {
				return cursor..cursor
			}
			end_pos = self.forward_until(self.num_or_len(start_pos + 1), |c| c.matches("\n") || c.matches(target), true);
			if !self.get_char(end_pos).is_some_and(|c| c.matches(target)) {
				return cursor..cursor
			}
			start = start_pos;
			end = self.num_or_len(end_pos + 1);

			if bound == Bound::Around && self.get_char(end).is_some_and(|c| c.is_whitespace()) {
				end += 1;
				end = self.forward_until(end, |c| !c.is_whitespace(), true);
			}
		}
		mk_range(start,end)
	}
	pub fn eval_delim_obj(&self, obj: &TextObj, bound: Bound) -> Range<usize> {
		// FIXME: logic isn't completely robust i think
		let opener = match obj {
			TextObj::Brace => "{",
			TextObj::Bracket => "[",
			TextObj::Paren => "(",
			TextObj::Angle => "<",
			_ => unreachable!()
		};
		let closer = match obj {
			TextObj::Brace => "}",
			TextObj::Bracket => "]",
			TextObj::Paren => ")",
			TextObj::Angle => ">",
			_ => unreachable!()
		};
		let mut end = None;
		let mut start = None;
		let mut delim_count: usize = 0;
		let ln_range = self.cur_line_range();
		let cursor = self.cursor();
		let mut ln_chars = self.buffer[*ln_range.start()..cursor].iter().enumerate();
		while let Some((i,ch)) = ln_chars.next() {
			let &TermChar::Grapheme(ch) = &ch else { unreachable!() };
			match ch.as_ref() {
				"\\" => {
					ln_chars.next();
				}
				ch if ch == opener => {
					start = Some(ln_range.start() + i);
					delim_count += 1;
				}
				ch if ch == closer => delim_count -= 1,
				_ => {}
			}
		} 

		let mut start_pos = None;
		let mut end_pos = None;
		if delim_count == 0 {
			let mut ln_chars = self.buffer[cursor..*ln_range.end()].iter().enumerate();
			while let Some((i,ch)) = ln_chars.next() {
				let &TermChar::Grapheme(ch) = &ch else { unreachable!() };
				match ch.as_ref() {
					"\\" => {
						ln_chars.next();
					}
					ch if ch == opener => {
						if delim_count == 0 {
							start_pos = Some(cursor + i);
						}
						delim_count += 1;
					}
					ch if ch == closer => {
						delim_count -= 1;
						if delim_count == 0 {
							end_pos = Some(cursor + i);
						}
					}
					_ => {}
				}
			}

			if start_pos.is_none() || end_pos.is_none() {
				return cursor..cursor
			} else {
				start = start_pos;
				end = end_pos;
			}
		} else {
			let Some(strt) = start else {
				dbg!("no start");
				dbg!("no start");
				dbg!("no start");
				dbg!("no start");
				dbg!("no start");
				dbg!("no start");
				return cursor..cursor
			};
			let strt = self.num_or_len(strt + 1); // skip the paren
			let target = delim_count.saturating_sub(1);
			let mut ln_chars = self.buffer[strt..*ln_range.end()].iter().enumerate();
				dbg!(&ln_chars);
				dbg!(&ln_chars);
				dbg!(&ln_chars);
				dbg!(&ln_chars);

			while let Some((i,ch)) = ln_chars.next() {
				let &TermChar::Grapheme(ch) = &ch else { unreachable!() };
				match ch.as_ref() {
					"\\" => {
						ln_chars.next();
					}
					ch if ch == opener => {
						delim_count += 1;
					}
					ch if ch == closer => {
						delim_count -= 1;
						if delim_count == target {
							end_pos = Some(strt + i);
						}
					}
					_ => {}
				}
			}
			dbg!(end_pos);
			dbg!(end_pos);
			dbg!(end_pos);
			dbg!(start_pos);
			dbg!(start_pos);
			dbg!(start_pos);
			dbg!(start_pos);
			dbg!(start_pos);
			dbg!(start_pos);
			dbg!(start_pos);
			if end_pos.is_none() {
				return cursor..cursor
			} else {
				end = end_pos;
			}
		}

		let Some(mut start) = start else {
			return cursor..cursor
		};
		let Some(mut end) = end else {
			return cursor..cursor
		};
		match bound {
			Bound::Inside => {
				end = end.saturating_sub(1);
				start = self.num_or_len(start + 1);
				mk_range(start,end)
			}
			Bound::Around => mk_range(start,end)
		}
		
	}
	pub fn eval_text_obj(&self, obj: TextObj, bound: Bound) -> Range<usize> {
		let mut start;
		let mut end;

		match obj {
			TextObj::Word(word) => {
				start = match self.on_word_bound(word, self.cursor(), Direction::Backward) {
					true => self.cursor(),
					false => self.find_word_pos(word, To::Start, Direction::Backward),
				};
				end = match self.on_word_bound(word, self.cursor(), Direction::Forward) {
					true => self.cursor(),
					false => self.find_word_pos(word, To::End, Direction::Forward),
				};
				end = self.num_or_len(end + 1);
				if bound == Bound::Around {
					end = self.forward_until(end, |c| c.is_whitespace(), true);
					end = self.forward_until(end, |c| !c.is_whitespace(), true);
				}
				return start..end
			}
			TextObj::Line => {
				let cursor = self.cursor();
				start = self.backward_until(cursor, |c| c == &TermChar::Newline, false);
				end = self.forward_until(cursor, |c| c == &TermChar::Newline, true);
			}
			TextObj::Sentence => todo!(),
			TextObj::Paragraph => todo!(),
			TextObj::DoubleQuote => return self.eval_quote_obj("\"", bound),
			TextObj::SingleQuote => return self.eval_quote_obj("'", bound),
			TextObj::BacktickQuote => return self.eval_quote_obj("`", bound),
			TextObj::Paren |
			TextObj::Bracket |
			TextObj::Brace |
			TextObj::Angle => return self.eval_delim_obj(&obj, bound),
			TextObj::Tag => todo!(),
			TextObj::Custom(_) => todo!(),
		}

		if bound == Bound::Inside {
			start = self.num_or_len_minus_one(start + 1);
			end = end.saturating_sub(1);
		}
		start..end
	}
	pub fn validate_range(&self, range: &Range<usize>) -> bool {
		range.end < self.buffer.len()
	}
	pub fn cur_line_range(&self) -> RangeInclusive<usize> {
		let cursor = self.cursor();
		let mut line_start = self.backward_until(cursor, |c| c == &TermChar::Newline, false);
		let mut line_end = self.forward_until(cursor, |c| c == &TermChar::Newline, true);
		if self.get_char(line_start.saturating_sub(1)).is_none_or(|c| c != &TermChar::Newline) {
			line_start = 0;
		}
		if self.get_char(line_end).is_none_or(|c| c != &TermChar::Newline) {
			line_end = self.buffer.len().saturating_sub(1);
			line_start = self.backward_until(line_start, |c| c == &TermChar::Newline, true)
		}

		line_start..=self.num_or_len(line_end + 1)
	}
	/// Clamp a number to the length of the buffer
	pub fn num_or_len_minus_one(&self, num: usize) -> usize {
		num.min(self.buffer.len().saturating_sub(1))
	}
	pub fn num_or_len(&self, num: usize) -> usize {
		num.min(self.buffer.len())
	}
	pub fn eval_motion(&self, motion: Motion) -> MotionKind {
		match motion {
			Motion::WholeLine => MotionKind::range(self.cur_line_range()),
			Motion::TextObj(text_obj, bound) => {
				let range = self.eval_text_obj(text_obj, bound);
				let range = mk_range(range.start, range.end);
				let cursor = self.cursor();
				if range.start == cursor && range.end == cursor {
					MotionKind::Null
				} else {
					MotionKind::range(range)
				}
			}
			Motion::BeginningOfFirstWord => {
				let cursor = self.cursor();
				let line_start = self.backward_until(cursor, |c| c == &TermChar::Newline, true);
				let first_print = self.forward_until(line_start, |c| !c.is_whitespace(), true);
				MotionKind::To(first_print)
			}
			Motion::ToColumn(col) => {
				let rng = self.cur_line_range();
				let column = (*rng.start() + (col.saturating_sub(1))).min(*rng.end());
				MotionKind::To(column)
			}
			Motion::BeginningOfLine => {
				let cursor = self.cursor();
				let mut line_start = self.backward_until(cursor, |c| c == &TermChar::Newline, false);
				if self.get_char(line_start.saturating_sub(1)).is_some_and(|c| c != &TermChar::Newline) {
					line_start = 0; // FIXME: not sure if this logic is correct
				}
				MotionKind::To(line_start)
			}
			Motion::EndOfLine => {
				let cursor = self.cursor();
				let mut line_end = self.forward_until(cursor, |c| c == &TermChar::Newline, false);
				// If we didn't actually find a newline, we need to go to the end of the buffer
				if self.get_char(line_end + 1).is_some_and(|c| c != &TermChar::Newline) {
					line_end = self.buffer.len(); // FIXME: not sure if this logic is correct
				}
				MotionKind::To(line_end)
			}
			Motion::BackwardWord(dest, word) => MotionKind::To(self.find_word_pos(word, dest, Direction::Backward)),
			Motion::ForwardWord(dest, word) => MotionKind::To(self.find_word_pos(word, dest, Direction::Forward)),
			Motion::CharSearch(direction, dest, ch) => {
				let mut cursor = self.cursor();
				let inclusive = matches!(dest, Dest::On);

				let stop_condition = |c: &TermChar| {
					c == &TermChar::Newline ||
					c == &ch
				};
				if self.cursor_char().is_some_and(|c| c == &ch) {
					// We are already on the character we are looking for
					// Let's nudge the cursor
					match direction {
						Direction::Backward => cursor = self.cursor().saturating_sub(1),
						Direction::Forward => cursor = self.num_or_len(self.cursor() + 1),
					}
				}

				let stop_pos = match direction {
					Direction::Forward => self.forward_until(cursor, stop_condition, inclusive),
					Direction::Backward => self.backward_until(cursor, stop_condition, inclusive),
				};

				let found_char = match dest {
					Dest::On => self.get_char(stop_pos).is_some_and(|c| c == &ch),
					_ => {
						match direction {
							Direction::Forward => self.get_char(stop_pos + 1).is_some_and(|c| c == &ch),
							Direction::Backward => self.get_char(stop_pos.saturating_sub(1)).is_some_and(|c| c == &ch),
						}
					}
				};

				if found_char {
					MotionKind::To(stop_pos)
				} else {
					MotionKind::Null
				}
			}
			Motion::Range(s, e) => {
				if self.validate_range(&(s..e)) {
					let range = mk_range(s, e);
					MotionKind::range(range)
				} else {
					MotionKind::Null
				}
			}
			Motion::BackwardChar => MotionKind::Backward(1),
			Motion::ForwardChar => MotionKind::Forward(1),
			Motion::LineUp => todo!(),
			Motion::LineDown => todo!(),
			Motion::WholeBuffer => MotionKind::Range(0..self.buffer.len().saturating_sub(1)),
			Motion::BeginningOfBuffer => MotionKind::To(0),
			Motion::EndOfBuffer => MotionKind::To(self.buffer.len().saturating_sub(1)),
			Motion::Null => MotionKind::Null,
			Motion::Builder(_) => unreachable!(),
		}
	}
	pub fn exec_verb(&mut self, verb: Verb, motion: MotionKind, register: RegisterName) -> ShResult<()> {
		match verb {
			Verb::Change |
			Verb::Delete => {
				let deleted;
				match motion {
					MotionKind::Forward(n) => {
						let fwd = self.num_or_len(self.cursor() + n);
						let cursor = self.cursor();
						deleted = self.buffer.drain(cursor..=fwd).collect::<TermCharBuf>();
					}
					MotionKind::To(pos) => {
						let range = mk_range(self.cursor(), pos);
						deleted = self.buffer.drain(range.clone()).collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(range.start));
					}
					MotionKind::Backward(n) => {
						let back = self.cursor.saturating_sub(n);
						let cursor = self.cursor();
						deleted = self.buffer.drain(back..cursor).collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(back));
					}
					MotionKind::Range(r) => {
						deleted = self.buffer.drain(r.clone()).collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(r.start));
					}
					MotionKind::Null => return Ok(())
				}
				register.write_to_register(deleted);
			}
			Verb::DeleteChar(anchor) => {
				match anchor {
					Anchor::After => {
						let pos = self.num_or_len(self.cursor() + 1);
						self.buffer.remove(pos);
					}
					Anchor::Before => {
						let pos = self.cursor.saturating_sub(1);
						self.buffer.remove(pos);
						self.cursor = self.cursor.saturating_sub(1);
					}
				}
			}
			Verb::Yank => {
				let yanked;
				match motion {
					MotionKind::Forward(n) => {
						let fwd = self.num_or_len(self.cursor() + n);
						let cursor = self.cursor();
						yanked = self.buffer[cursor..=fwd]
							.iter()
							.cloned()
							.collect::<TermCharBuf>();
					}
					MotionKind::To(pos) => {
						let range = mk_range(self.cursor(), pos);
						yanked = self.buffer[range.clone()]
							.iter()
							.cloned()
							.collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(range.start));
					}
					MotionKind::Backward(n) => {
						let back = self.cursor.saturating_sub(n);
						let cursor = self.cursor();
						yanked = self.buffer[back..cursor]
							.iter()
							.cloned()
							.collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(back));
					}
					MotionKind::Range(r) => {
						yanked = self.buffer[r.start..r.end]
							.iter()
							.cloned()
							.collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(r.start));
					}
					MotionKind::Null => return Ok(())
				}
				register.write_to_register(yanked);
			}
			Verb::ReplaceChar(ch) => {
				let cursor = self.cursor();
				if let Some(c) = self.buffer.get_mut(cursor) {
					let mut tc = TermChar::from(ch);
					std::mem::swap(c, &mut tc)
				}
				self.apply_motion(motion);
			}
			Verb::Substitute => todo!(),
			Verb::ToggleCase => todo!(),
			Verb::Complete => todo!(),
			Verb::CompleteBackward => todo!(),
			Verb::Undo => {
				let Some(undo) = self.undo_stack.pop() else {
					return Ok(())
				};
				flog!(DEBUG, undo);
				let Edit { pos, cursor_pos, old, new } = undo;
				let start = pos;
				let end = pos + new.len();
				self.buffer.0.splice(start..end, old.0.clone());
				let cur_pos = self.cursor();
				self.cursor = cursor_pos;
				let redo = Edit { pos, cursor_pos: cur_pos, old: new, new: old };
				flog!(DEBUG, redo);
				self.redo_stack.push(redo);
			}
			Verb::Redo => {
				let Some(Edit { pos, cursor_pos, old, new }) = self.redo_stack.pop() else {
					return Ok(())
				};
				let start = pos;
				let end = pos + new.len();
				self.buffer.0.splice(start..end, old.0.clone());
				let cur_pos = self.cursor();
				self.cursor = cursor_pos;
				self.undo_stack.push(Edit { pos, cursor_pos: cur_pos, old: new, new: old });
			}
			Verb::RepeatLast => todo!(),
			Verb::Put(anchor) => {
				if let Some(charbuf) = register.read_from_register() {
					let chars = charbuf.0.into_iter();
					if anchor == Anchor::Before {
						self.cursor_back(1);
					}
					for char in chars {
						self.cursor_fwd(1);
						self.insert_at_cursor(char);
					}
				}
			}
			Verb::JoinLines => todo!(),
			Verb::InsertChar(ch) => {
				self.insert_at_cursor(ch);
				self.apply_motion(motion);
			}
			Verb::Insert(_) => todo!(),
			Verb::Breakline(anchor) => todo!(),
			Verb::Indent => todo!(),
			Verb::Dedent => todo!(),
			Verb::AcceptLine => todo!(),
			Verb::EndOfFile => {
				if self.buffer.is_empty() {
					sh_quit(0)
				} else {
					self.buffer.clear();
					self.cursor = 0;
				}
			}
			Verb::InsertModeLineBreak(anchor) => {
				match anchor {
					Anchor::After => {
						let rng = self.cur_line_range();
						self.apply_motion(MotionKind::To(self.num_or_len(rng.end() + 1)));
						self.insert_at_cursor('\n'.into());
						self.apply_motion(MotionKind::Forward(1));
					}
					Anchor::Before => todo!(),
				}
			}
			Verb::Equalize => {
			}
			Verb::InsertMode |
			Verb::NormalMode |
			Verb::VisualMode |
			Verb::OverwriteMode => {
				self.apply_motion(motion);
			}
		}
		Ok(())
	}
	pub fn apply_motion(&mut self, motion: MotionKind) {
		match motion {
			MotionKind::Forward(n) => self.cursor_fwd(n),
			MotionKind::To(pos) => self.cursor_to(pos),
			MotionKind::Backward(n) => self.cursor_back(n),
			MotionKind::Range(r) => self.cursor_to(r.start), // TODO: not sure if this is correct in every case
			MotionKind::Null => { /* Pass */ }
		}
	}
	pub fn handle_edit(&mut self, old: TermCharBuf, new: TermCharBuf, curs_pos: usize) {
		if self.merge_edit {
			let mut diff = Edit::diff(old, new, curs_pos);
			let Some(mut edit) = self.undo_stack.pop() else {
				self.undo_stack.push(diff);
				return
			};
			dbg!("old");
			dbg!(&edit);

			edit.new.append(&mut diff.new);
			dbg!("new");
			dbg!(&edit);

			self.undo_stack.push(edit);
		} else {
			let diff = Edit::diff(old, new, curs_pos);
			self.undo_stack.push(diff);
		}
	}
	pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
		flog!(DEBUG, cmd);
		let clear_redos = !cmd.is_undo_op() || cmd.verb.as_ref().is_some_and(|v| v.1.is_edit());
		let is_char_insert = cmd.verb.as_ref().is_some_and(|v| v.1.is_char_insert());
		let is_undo_op = cmd.is_undo_op();

		// Merge character inserts into one edit
		if self.merge_edit && cmd.verb.as_ref().is_none_or(|v| !v.1.is_char_insert()) {
			self.merge_edit = false;
		}

		let ViCmd { register, verb, motion, .. } = cmd;

		let verb_count = verb.as_ref().map(|v| v.0);
		let motion_count = motion.as_ref().map(|m| m.0);

		let before = self.buffer.clone();
		let cursor_pos = self.cursor();

		for _ in 0..verb_count.unwrap_or(1) {
			for _ in 0..motion_count.unwrap_or(1) {
				let motion = motion
					.clone()
					.map(|m| self.eval_motion(m.1))
					.unwrap_or(MotionKind::Null);

				if let Some(verb) = verb.clone() {
					self.exec_verb(verb.1, motion, register)?;
				} else {
					self.apply_motion(motion);
				}
			}
		}

		let after = self.buffer.clone();
		if clear_redos {
			self.redo_stack.clear();
		}

		if before.0 != after.0 && !is_undo_op {
			self.handle_edit(before, after, cursor_pos);
		}

		if is_char_insert {
			self.merge_edit = true;
		}

		if self.clamp_cursor {
			self.clamp_cursor();
		}
		Ok(())
	}
}

impl Display for LineBuf {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f,"{}",self.buffer)
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

fn mk_range(a: usize, b: usize) -> Range<usize> {
    std::cmp::min(a, b)..std::cmp::max(a, b)
}
