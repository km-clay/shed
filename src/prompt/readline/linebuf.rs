use std::{cmp::Ordering, fmt::Display, ops::{Deref, DerefMut, Range, RangeBounds, RangeInclusive}, str::FromStr, sync::Arc};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::libsh::{error::ShResult, sys::sh_quit, term::{Style, Styled}};
use crate::prelude::*;

use super::vicmd::{Anchor, Bound, Dest, Direction, Motion, RegisterName, TextObj, To, Verb, ViCmd, Word};

#[derive(Debug, PartialEq, Eq)]
pub enum CharClass {
	Alphanum,
	Symbol,
	Whitespace,
	Other
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionKind {
	Forward(usize),
	To(usize),
	Backward(usize),
	Range((usize,usize)),
	Line(isize), // positive = up line, negative = down line
	ToLine(usize), 
	Null,

	/// Absolute position based on display width of characters
	/// Factors in the length of the prompt, and skips newlines
	ToScreenPos(usize), 
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
			Self::Range((start,end))
		} else {
			Self::Range((end,start))
		}
	}
}

impl From<&str> for CharClass {
	fn from(value: &str) -> Self {
		if value.len() > 1 {
			return Self::Symbol // Multi-byte grapheme
		}

		if value.chars().all(char::is_alphanumeric) {
			CharClass::Alphanum
		} else if value.chars().all(char::is_whitespace) {
			CharClass::Whitespace
		} else if !value.chars().all(char::is_alphanumeric) {
			CharClass::Symbol
		} else {
			Self::Other
		}
	}
}


fn is_other_class_or_ws(a: &str, b: &str) -> bool {
	let a = CharClass::from(a);
	let b = CharClass::from(b);
	if a == CharClass::Whitespace || b == CharClass::Whitespace {
		true
	} else {
		a != b
	}
}

pub struct UndoPayload {
	buffer: String,
	cursor: usize
}

#[derive(Default,Debug)]
pub struct Edit {
	pub pos: usize,
	pub cursor_pos: usize,
	pub old: String,
	pub new: String,
}

impl Edit {
	pub fn diff(a: &str, b: &str, old_cursor_pos: usize) -> Edit {
		use std::cmp::min;

		let mut start = 0;
		let max_start = min(a.len(), b.len());

		// Calculate the prefix of the edit
		while start < max_start && a.as_bytes()[start] == b.as_bytes()[start] {
			start += 1;
		}

		if start == a.len() && start == b.len() {
			return Edit {
				pos: start,
				cursor_pos: old_cursor_pos,
				old: String::new(),
				new: String::new(),
			};
		}

		let mut end_a = a.len();
		let mut end_b = b.len();

		// Calculate the suffix of the edit
		while end_a > start && end_b > start && a.as_bytes()[end_a - 1] == b.as_bytes()[end_b - 1] {
			end_a -= 1;
			end_b -= 1;
		}

		// Slice off the prefix and suffix for both (safe because start/end are byte offsets)
		let old = a[start..end_a].to_string();
		let new = b[start..end_b].to_string();

		Edit {
			pos: start,
			cursor_pos: old_cursor_pos,
			old,
			new,
		}
	}
}

#[derive(Default,Debug)]
pub struct LineBuf {
	buffer: String,
	cursor: usize,
	clamp_cursor: bool,
	first_line_offset: usize,
	merge_edit: bool,
	undo_stack: Vec<Edit>,
	redo_stack: Vec<Edit>,
}

impl LineBuf {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn with_initial(mut self, initial: &str) -> Self {
		self.buffer = initial.to_string();
		self
	}
	pub fn as_str(&self) -> &str {
		&self.buffer
	}
	pub fn take(&mut self) -> String {
		let line = std::mem::take(&mut self.buffer);
		*self = Self::default();
		line
	}
	pub fn byte_pos(&self) -> usize {
		self.cursor
	}
	pub fn byte_len(&self) -> usize {
		self.buffer.len()
	}
	pub fn is_empty(&self) -> bool {
		self.buffer.is_empty()
	}
	pub fn clamp_cursor(&mut self) {
		// Normal mode does not allow you to sit on the edge of the buffer, you must be hovering over a character
		// Insert mode does let you set on the edge though, so that you can append new characters
		// This method is used in Normal mode
		dbg!("clamping");
		if self.cursor == self.byte_len() {
			self.cursor_back(1);
		}
	}
	pub fn grapheme_len(&self) -> usize {
		self.buffer.grapheme_indices(true).count()
	}
	pub fn slice_from_cursor(&self) -> &str {
		&self.buffer[self.cursor..]
	}
	pub fn slice_to_cursor(&self) -> &str {
		if let Some(slice) = self.buffer.get(..self.cursor) {
			slice
		} else {
			&self.buffer
		}
		
	}
	pub fn slice_from_cursor_to_end_of_line(&self) -> &str {
		let end = self.end_of_line();
		&self.buffer[self.cursor..end]
	}
	pub fn slice_from_start_of_line_to_cursor(&self) -> &str {
		let start = self.start_of_line();
		&self.buffer[start..self.cursor]
	}
	pub fn slice_from(&self, pos: usize) -> &str {
		&self.buffer[pos..]
	}
	pub fn slice_to(&self, pos: usize) -> &str {
		&self.buffer[..pos]
	}
	pub fn set_cursor_clamp(&mut self, yn: bool) {
		self.clamp_cursor = yn
	}
	pub fn grapheme_at_cursor(&self) -> Option<&str> {
		if self.cursor == self.byte_len() {
			None
		} else {
			self.slice_from_cursor().graphemes(true).next() 
		}
	}
	pub fn grapheme_at_cursor_offset(&self, offset: isize) -> Option<&str> {
		match offset.cmp(&0) {
			Ordering::Equal => {
				return self.grapheme_at(self.cursor);
			}
			Ordering::Less => {
				// Walk backward from the start of the line or buffer up to the cursor
				// and count graphemes in reverse.
				let rev_graphemes: Vec<&str> = self.slice_to_cursor().graphemes(true).collect();
				let idx = rev_graphemes.len().checked_sub((-offset) as usize)?;
				rev_graphemes.get(idx).copied()
			}
			Ordering::Greater => {
				self.slice_from_cursor()
					.graphemes(true)
					.nth(offset as usize)
			}
		}
	}
	pub fn grapheme_at(&self, pos: usize) -> Option<&str> {
		if pos >= self.byte_len() {
			None
		} else {
			self.buffer.graphemes(true).nth(pos)
		}
	}
	pub fn is_whitespace(&self, pos: usize) -> bool {
		let Some(g) = self.grapheme_at(pos) else {
			return false
		};
		g.chars().all(char::is_whitespace)
	}
	pub fn on_whitespace(&self) -> bool {
		self.is_whitespace(self.cursor)
	}
	pub fn next_pos(&self, n: usize) -> Option<usize> {
		if self.cursor == self.byte_len() {
			None
		} else {
			self.slice_from_cursor()
				.grapheme_indices(true)
				.take(n)
				.last()
				.map(|(i,s)| i + self.cursor + s.len())
		}
	}
	pub fn prev_pos(&self, n: usize) -> Option<usize> {
		if self.cursor == 0 {
			None
		} else {
			self.slice_to_cursor()
				.grapheme_indices(true)
				.rev()                      // <- walk backward
				.take(n)
				.last()
				.map(|(i, _)| i)
		}
	}
	pub fn cursor_back(&mut self, dist: usize) -> bool {
		let Some(pos) = self.prev_pos(dist) else {
			return false
		};
		self.cursor = pos;
		true
	}
	/// Up to but not including 'dist'
	pub fn cursor_back_to(&mut self, dist: usize) -> bool {
		let dist = dist.saturating_sub(1);
		let Some(pos) = self.prev_pos(dist) else {
			return false
		};
		self.cursor = pos;
		true
	}
	pub fn cursor_fwd(&mut self, dist: usize) -> bool {
		let Some(pos) = self.next_pos(dist) else {
			return false
		};
		self.cursor = pos;
		true
	}
	pub fn cursor_fwd_to(&mut self, dist: usize) -> bool {
		let dist = dist.saturating_sub(1);
		let Some(pos) = self.next_pos(dist) else {
			return false
		};
		self.cursor = pos;
		true
	}
	pub fn count_display_lines(&self, offset: usize, term_width: usize) -> usize {
		let mut lines = 0;
		let mut col = offset.max(1);
		for ch in self.buffer.chars() {
			match ch {
				'\n' => {
					lines += 1;
					col = 1;
				}
				_ => {
					col += 1;
					if col > term_width {
						lines += 1;
						col = 1
					}
				}
			}
		}
		lines
	}
	pub fn cursor_display_line_position(&self, offset: usize, term_width: usize) -> usize {
		let mut lines = 0;
		let mut col = offset.max(1);
		for ch in self.slice_to_cursor().chars() {
			match ch {
				'\n' => {
					lines += 1;
					col = 1;
				}
				_ => {
					col += 1;
					if col > term_width {
						lines += 1;
						col = 1
					}
				}
			}
		}
		lines
	}
	pub fn display_coords(&self, term_width: usize) -> (usize,usize) {
		let mut chars = self.slice_to_cursor().chars();

		let mut lines = 0;
		let mut col = 0;
		for ch in chars {
			match ch {
				'\n' => {
					lines += 1;
					col = 1;
				}
				_ => {
					col += 1;
					if col > term_width {
						lines += 1;
						col = 1
					}
				}
			}
		}
		(lines,col)
	}
	pub fn cursor_display_coords(&self, first_ln_offset: usize, term_width: usize) -> (usize,usize) {
		let (d_line,mut d_col) = self.display_coords(term_width);
		let line = self.count_display_lines(first_ln_offset, term_width) - d_line;

		if line == self.count_lines() {
			d_col += first_ln_offset;
		}

		(line,d_col)
	}
	pub fn insert(&mut self, ch: char) {
		if self.buffer.is_empty() {
			self.buffer.push(ch)
		} else {
			self.buffer.insert(self.cursor, ch);
		}
	}
	pub fn move_to(&mut self, pos: usize) -> bool {
		if self.cursor == pos {
			false
		} else {
			self.cursor = pos;
			true
		}
	}
	pub fn move_buf_start(&mut self) -> bool {
		self.move_to(0)
	}
	pub fn move_buf_end(&mut self) -> bool {
		self.move_to(self.byte_len())
	}
	pub fn move_home(&mut self) -> bool {
		let start = self.start_of_line();
		self.move_to(start)
	}
	pub fn move_end(&mut self) -> bool {
		let end = self.end_of_line();
		self.move_to(end)
	}
	pub fn start_of_line(&self) -> usize {
		if let Some(i) = self.slice_to_cursor().rfind('\n') {
			i + 1 // Land on start of this line, instead of the end of the last one
		} else {
			0
		}
	}
	pub fn end_of_line(&self) -> usize {
		if let Some(i) = self.slice_from_cursor().find('\n') {
			i + self.cursor
		} else {
			self.byte_len()
		}
	}
	pub fn this_line(&self) -> (usize,usize) {
		(
			self.start_of_line(),
			self.end_of_line()
		)
	}
	pub fn count_lines(&self) -> usize {
		self.buffer
			.chars()
			.filter(|&c| c == '\n')
			.count()
	}
	pub fn line_no(&self) -> usize {
		self.slice_to_cursor()
			.chars()
			.filter(|&c| c == '\n')
			.count()
	}
	/// Returns the (start, end) byte range for the given line number.
	/// 
	/// - Line 0 starts at the beginning of the buffer and ends at the first newline (or end of buffer).
	/// - Line 1 starts just after the first newline, ends at the second, etc.
	/// 
	/// Returns `None` if the line number is beyond the last line in the buffer.
	pub fn select_line(&self, n: usize) -> Option<(usize, usize)> {
		let mut start = 0;

		let bytes = self.as_str(); // or whatever gives the full buffer as &str
		let mut line_iter = bytes.match_indices('\n').map(|(i, _)| i + 1);

		// Advance to the nth newline (start of line n)
		for _ in 0..n {
			start = line_iter.next()?;
		}

		// Find the next newline (end of line n), or end of buffer
		let end = line_iter.next().unwrap_or(bytes.len());

		Some((start, end))
	}
	/// Find the span from the start of the nth line above the cursor, to the end of the current line.
	///
	/// Returns (start,end)
	/// 'start' is the first character after the previous newline, or the start of the buffer
	/// 'end' is the index of the newline after the nth line
	///
	/// The caller can choose whether to include the newline itself in the selection by using either
	/// * `(start..end)` to exclude it
	/// * `(start..=end)` to include it
	pub fn select_lines_up(&self, n: usize) -> (usize,usize) {
		let end = self.end_of_line();
		let mut start = self.start_of_line();
		if start == 0 {
			return (start,end)
		}

		for _ in 0..n {
			if let Some(prev_newline) = self.slice_to(start - 1).rfind('\n') {
				start = prev_newline + 1;
			} else {
				start = 0;
				break
			}
		}

		(start,end)
	}
	/// Find the range from the start of this line, to the end of the nth line after the cursor
	///
	/// Returns (start,end)
	/// 'start' is the first character after the previous newline, or the start of the buffer
	/// 'end' is the index of the newline after the nth line
	///
	/// The caller can choose whether to include the newline itself in the selection by using either
	/// * `(start..end)` to exclude it
	/// * `(start..=end)` to include it
	pub fn select_lines_down(&self, n: usize) -> (usize,usize) {
		let mut end = self.end_of_line();
		let start = self.start_of_line();
		if end == self.byte_len() {
			return (start,end)
		}

		for _ in 0..n {
			if let Some(next_newline) = self.slice_from(end).find('\n') {
				end = next_newline
			} else {
				end = self.byte_len();
				break
			}
		}

		(start,end)
	}
	pub fn select_lines_to(&self, line_no: usize) -> (usize,usize) {
		let cursor_line_no = self.line_no();
		let offset = (cursor_line_no as isize) - (line_no as isize);
		match offset.cmp(&0) {
			Ordering::Less => self.select_lines_down(offset.unsigned_abs()),
			Ordering::Equal => self.this_line(),
			Ordering::Greater => self.select_lines_up(offset as usize)
		}
	}
	fn on_start_of_word(&self, size: Word) -> bool {
		self.is_start_of_word(size, self.cursor)
	}
	fn on_end_of_word(&self, size: Word) -> bool {
		self.is_end_of_word(size, self.cursor)
	}
	fn is_start_of_word(&self, size: Word, pos: usize) -> bool {
		if self.grapheme_at(pos).is_some_and(|g| g.chars().all(char::is_whitespace)) {
			return false
		}
		match size {
			Word::Big => {
				let Some(prev_g) = self.grapheme_at(pos.saturating_sub(1)) else {
					return true // We are on the very first grapheme, so it is the start of a word
				};
				prev_g.chars().all(char::is_whitespace)
			}
			Word::Normal => {
				let Some(cur_g) = self.grapheme_at(pos) else {
					return false // We aren't on a character to begin with
				};
				let Some(prev_g) = self.grapheme_at(pos.saturating_sub(1)) else {
					return true 
				};
				is_other_class_or_ws(cur_g, prev_g)
			}
		}
	}
	fn is_end_of_word(&self, size: Word, pos: usize) -> bool {
		if self.grapheme_at(pos).is_some_and(|g| g.chars().all(char::is_whitespace)) {
			return false
		}
		match size {
			Word::Big => {
				let Some(next_g) = self.grapheme_at(pos + 1) else {
					return false
				};
				next_g.chars().all(char::is_whitespace)
			}
			Word::Normal => {
				let Some(cur_g) = self.grapheme_at(pos) else {
					return false 
				};
				let Some(next_g) = self.grapheme_at(pos + 1) else {
					return false 
				};
				is_other_class_or_ws(cur_g, next_g)
			}
		}
	}
	pub fn find_word_pos(&self, word: Word, to: To, dir: Direction) -> Option<usize> {
		let mut pos = self.cursor;
		match word {
			Word::Big => {
				match dir {
					Direction::Forward => {
						match to {
							To::Start => {
								if self.on_start_of_word(word) {
									pos += 1;
									if pos >= self.byte_len() {
										return None
									}
								}
								let ws_pos = self.find_from(pos, |c| CharClass::from(c) == CharClass::Whitespace)?;
								let word_start = self.find_from(ws_pos, |c| CharClass::from(c) != CharClass::Whitespace)?;
								Some(word_start)
							}
							To::End => {
								match self.on_end_of_word(word) {
									true => {
										pos += 1;
										if pos >= self.byte_len() {
											return None
										}
										let word_start = self.find_from(pos, |c| CharClass::from(c) != CharClass::Whitespace)?;
										match self.find_from(word_start, |c| CharClass::from(c) == CharClass::Whitespace) {
											Some(n) => Some(n.saturating_sub(1)), // Land on char before whitespace
											None => Some(self.byte_len()) // End of buffer
										}
									}
									false => {
										match self.find_from(pos, |c| CharClass::from(c) == CharClass::Whitespace) {
											Some(n) => Some(n.saturating_sub(1)), // Land on char before whitespace
											None => Some(self.byte_len()) // End of buffer
										}
									}
								}
							}
						}
					}
					Direction::Backward => {
						match to {
							To::Start => {
								match self.on_start_of_word(word) {
									true => {
										pos = pos.checked_sub(1)?;
										let prev_word_end = self.rfind_from(pos, |c| CharClass::from(c) != CharClass::Whitespace)?;
										match self.rfind_from(prev_word_end, |c| CharClass::from(c) == CharClass::Whitespace) {
											Some(n) => Some(n + 1), // Land on char after whitespace
											None => Some(0) // Start of buffer
										}
									}
									false => {
										let last_ws = self.rfind_from(pos, |c| CharClass::from(c) == CharClass::Whitespace)?; 
										let prev_word_end = self.rfind_from(last_ws, |c| CharClass::from(c) != CharClass::Whitespace)?;
										match self.rfind_from(prev_word_end, |c| CharClass::from(c) == CharClass::Whitespace) {
											Some(n) => Some(n + 1), // Land on char after whitespace
											None => Some(0) // Start of buffer
										}
									}
								}
							}
							To::End => {
								if self.on_end_of_word(word) {
									pos = pos.checked_sub(1)?;
								}
								let last_ws = self.rfind_from(pos, |c| CharClass::from(c) == CharClass::Whitespace)?; 
								let prev_word_end = self.rfind_from(last_ws, |c| CharClass::from(c) != CharClass::Whitespace)?;
								Some(prev_word_end)
							}
						}
					}
				}
			}
			Word::Normal => {
				match dir {
					Direction::Forward => {
						match to {
							To::Start => {
								if self.on_start_of_word(word) {
									pos += 1;
									if pos >= self.byte_len() {
										return None
									}
								}
								let cur_graph = self.grapheme_at(pos)?;
								let diff_class_pos = self.find_from(pos, |c| is_other_class_or_ws(c, cur_graph))?;
								if let CharClass::Whitespace = CharClass::from(self.grapheme_at(diff_class_pos)?) {
									let non_ws_pos = self.find_from(diff_class_pos, |c| CharClass::from(c) != CharClass::Whitespace)?;
									Some(non_ws_pos)
								} else {
									Some(diff_class_pos)
								}
							}
							To::End => {
								match self.on_end_of_word(word) {
									true => {
										pos += 1;
										if pos >= self.byte_len() {
											return None
										}
										let cur_graph = self.grapheme_at(pos)?;
										match self.find_from(pos, |c| is_other_class_or_ws(c, cur_graph)) {
											Some(n) => {
												let cur_graph = self.grapheme_at(n)?;
												if CharClass::from(cur_graph) == CharClass::Whitespace {
													let Some(non_ws_pos) = self.find_from(n, |c| CharClass::from(c) != CharClass::Whitespace) else {
														return Some(self.byte_len())
													};
													let cur_graph = self.grapheme_at(non_ws_pos)?;
													let Some(word_end_pos) = self.find_from(non_ws_pos, |c| is_other_class_or_ws(c, cur_graph)) else {
														return Some(self.byte_len())
													};
													Some(word_end_pos.saturating_sub(1))
												} else {
													Some(pos.saturating_sub(1))
												}
											}
											None => Some(self.byte_len()) // End of buffer
										}
									}
									false => {
										let cur_graph = self.grapheme_at(pos)?;
										match self.find_from(pos, |c| is_other_class_or_ws(c, cur_graph)) {
											Some(n) => Some(n.saturating_sub(1)), // Land on char before other char class
											None => Some(self.byte_len()) // End of buffer
										}
									}
								}
							}
						}
					}
					Direction::Backward => {
						match to {
							To::Start => {
								if self.on_start_of_word(word) {
									pos = pos.checked_sub(1)?;
								}
								let cur_graph = self.grapheme_at(pos)?;
								let diff_class_pos = self.rfind_from(pos, |c| is_other_class_or_ws(c, cur_graph))?;
								if let CharClass::Whitespace = self.grapheme_at(diff_class_pos)?.into() {
									let prev_word_end = self.rfind_from(diff_class_pos, |c| CharClass::from(c) != CharClass::Whitespace)?;
									let cur_graph = self.grapheme_at(prev_word_end)?;
									let Some(prev_word_start) = self.rfind_from(prev_word_end, |c| is_other_class_or_ws(c, cur_graph)) else {
										return Some(0)
									};
									Some(prev_word_start + 1)
								} else {
									let cur_graph = self.grapheme_at(diff_class_pos)?;
									let Some(prev_word_start) = self.rfind_from(diff_class_pos, |c| is_other_class_or_ws(c, cur_graph)) else {
										return Some(0)
									};
									Some(prev_word_start + 1)
								}
							}
							To::End => {
								if self.on_end_of_word(word) {
									pos = pos.checked_sub(1)?;
								}
								let cur_graph = self.grapheme_at(pos)?;
								let diff_class_pos = self.rfind_from(pos, |c|is_other_class_or_ws(c, cur_graph))?;
								if let CharClass::Whitespace = self.grapheme_at(diff_class_pos)?.into() {
									let prev_word_end = self.rfind_from(diff_class_pos, |c| CharClass::from(c) != CharClass::Whitespace).unwrap_or(0);
									Some(prev_word_end)
								} else {
									Some(diff_class_pos)
								}
							}
						}
					}
				}
			}
		}
	} 
	pub fn find<F: Fn(&str) -> bool>(&self, op: F) -> Option<usize> {
		self.find_from(self.cursor, op)
	}
	pub fn rfind<F: Fn(&str) -> bool>(&self, op: F) -> Option<usize> {
		self.rfind_from(self.cursor, op)
	}

	/// Find the first grapheme at or after `pos` for which `op` returns true.
	/// Returns the byte index of that grapheme in the buffer.
	pub fn find_from<F: Fn(&str) -> bool>(&self, pos: usize, op: F) -> Option<usize> {
		assert!(is_grapheme_boundary(&self.buffer, pos));

		// Iterate over grapheme indices starting at `pos`
		let slice = &self.slice_from(pos);
		for (offset, grapheme) in slice.grapheme_indices(true) {
			if op(grapheme) {
				return Some(pos + offset);
			}
		}
		None
	}
	/// Find the last grapheme at or before `pos` for which `op` returns true.
	/// Returns the byte index of that grapheme in the buffer.
	pub fn rfind_from<F: Fn(&str) -> bool>(&self, pos: usize, op: F) -> Option<usize> {
		assert!(is_grapheme_boundary(&self.buffer, pos));

		// Iterate grapheme boundaries backward up to pos
		let slice = &self.slice_to(pos);
		let graphemes = slice.grapheme_indices(true).rev();

		for (offset, grapheme) in graphemes {
			if op(grapheme) {
				return Some(offset);
			}
		}
		None
	}
	pub fn eval_motion(&self, motion: Motion) -> MotionKind {
		match motion {
			Motion::WholeLine => {
				let (start,end) = self.this_line();
				MotionKind::range(start..=end)
			}
			Motion::TextObj(text_obj, bound) => todo!(),
			Motion::BeginningOfFirstWord => {
				let (start,_) = self.this_line();
				let first_graph_pos = self.find_from(start, |c| CharClass::from(c) != CharClass::Whitespace).unwrap_or(start);
				MotionKind::To(first_graph_pos)
			}
			Motion::BeginningOfLine => MotionKind::To(self.this_line().0),
			Motion::EndOfLine => MotionKind::To(self.this_line().1),
			Motion::BackwardWord(to, word) => {
				let Some(pos) = self.find_word_pos(word, to, Direction::Backward) else {
					return MotionKind::Null
				};
				MotionKind::To(pos)
			}
			Motion::ForwardWord(to, word) => {
				let Some(pos) = self.find_word_pos(word, to, Direction::Forward) else {
					return MotionKind::Null
				};
				MotionKind::To(pos)
			}
			Motion::CharSearch(direction, dest, ch) => {
				match direction {
					Direction::Forward => {
						let Some(pos) = self.slice_from_cursor().find(ch) else {
							return MotionKind::Null
						};
						match dest {
							Dest::On => MotionKind::To(pos),
							Dest::Before => MotionKind::To(pos.saturating_sub(1)),
							Dest::After => todo!(),
						}
					}
					Direction::Backward => {
						let Some(pos) = self.slice_to_cursor().rfind(ch) else {
							return MotionKind::Null
						};
						match dest {
							Dest::On => MotionKind::To(pos),
							Dest::Before => MotionKind::To(pos + 1),
							Dest::After => todo!(),
						}
					}
				}

			}
			Motion::BackwardChar => MotionKind::Backward(1),
			Motion::ForwardChar => MotionKind::Forward(1),
			Motion::LineUp => todo!(),
			Motion::LineDown => todo!(),
			Motion::WholeBuffer => todo!(),
			Motion::BeginningOfBuffer => MotionKind::To(0),
			Motion::EndOfBuffer => MotionKind::To(self.byte_len()),
			Motion::ToColumn(n) => {
				let (start,end) = self.this_line();
				let pos = start + n;
				if pos > end {
					MotionKind::To(end)
				} else {
					MotionKind::To(pos)
				}
			}
			Motion::Range(_, _) => todo!(),
			Motion::Builder(motion_builder) => todo!(),
			Motion::RepeatMotion => todo!(),
			Motion::RepeatMotionRev => todo!(),
			Motion::Null => todo!(),
		}
	}
	pub fn exec_verb(&mut self, verb: Verb, motion: MotionKind, register: RegisterName) -> ShResult<()> {
		match verb {
			Verb::Change |
			Verb::Delete => {
				let deleted;
				match motion {
					MotionKind::Forward(n) => {
						let Some(pos) = self.next_pos(n) else {
							return Ok(())
						}; 
						let range = self.cursor..pos;
						assert!(range.end < self.byte_len());
						deleted = self.buffer.drain(range);
					}
					MotionKind::To(n) => {
						let range = mk_range(self.cursor, n);
						assert!(range.end < self.byte_len());
						deleted = self.buffer.drain(range);
					}
					MotionKind::Backward(n) => {
						let Some(back) = self.prev_pos(n) else {
							return Ok(())
						}; 
						let range = back..self.cursor;
						dbg!(&range);
						deleted = self.buffer.drain(range);
					}
					MotionKind::Range(range) => {
						deleted = self.buffer.drain(range.0..range.1);
					}
					MotionKind::Line(n) => {
						let (start,end) = match n.cmp(&0) {
							Ordering::Less => self.select_lines_up(n.abs() as usize),
							Ordering::Equal => self.this_line(),
							Ordering::Greater => self.select_lines_down(n as usize)
						};
						let range = match verb {
							Verb::Change => start..end,
							Verb::Delete => start..end.saturating_add(1),
							_ => unreachable!()
						};
						deleted = self.buffer.drain(range);
					}
					MotionKind::ToLine(n) => {
						let (start,end) = self.select_lines_to(n);
						let range = match verb {
							Verb::Change => start..end,
							Verb::Delete => start..end.saturating_add(1),
							_ => unreachable!()
						};
						deleted = self.buffer.drain(range);
					}
					MotionKind::Null => return Ok(()),
					MotionKind::ToScreenPos(n) => todo!(),
				}
				register.write_to_register(deleted.collect());
				self.apply_motion(motion);
			}
			Verb::DeleteChar(anchor) => {
				match anchor {
					Anchor::After => {
						if self.grapheme_at(self.cursor).is_some() {
							self.buffer.remove(self.cursor);
						}
					}
					Anchor::Before => {
						if self.grapheme_at(self.cursor.saturating_sub(1)).is_some() {
							self.buffer.remove(self.cursor.saturating_sub(1));
						}
					}
				}
			}
			Verb::Yank => todo!(),
			Verb::ReplaceChar(_) => todo!(),
			Verb::Substitute => todo!(),
			Verb::ToggleCase => todo!(),
			Verb::Complete => todo!(),
			Verb::CompleteBackward => todo!(),
			Verb::Undo => todo!(),
			Verb::Redo => todo!(),
			Verb::RepeatLast => todo!(),
			Verb::Put(anchor) => todo!(),
			Verb::InsertModeLineBreak(anchor) => {
				match anchor {
					Anchor::After => {
						let (_,end) = self.this_line();
						self.cursor = end;
						self.insert('\n');
						self.cursor_fwd(1);
					}
					Anchor::Before => {
						let (start,_) = self.this_line();
						self.cursor = start;
						self.insert('\n');
					}
				}
			}
			Verb::JoinLines => todo!(),
			Verb::InsertChar(ch) => {
				self.insert(ch);
				self.apply_motion(motion);
			}
			Verb::Insert(_) => todo!(),
			Verb::Breakline(anchor) => todo!(),
			Verb::Indent => todo!(),
			Verb::Dedent => todo!(),
			Verb::Equalize => todo!(),
			Verb::AcceptLine => todo!(),
			Verb::Builder(verb_builder) => todo!(),
			Verb::EndOfFile => todo!(),

			Verb::OverwriteMode |
			Verb::InsertMode |
			Verb::NormalMode |
			Verb::VisualMode => {
				/* Already handled */ 
				self.apply_motion(motion);
			}
		}
		Ok(())
	}
	pub fn apply_motion(&mut self, motion: MotionKind) {
		dbg!(&motion);
		match motion {
			MotionKind::Forward(n) => {
				for _ in 0..n {
					if !self.cursor_fwd(1) {
						break
					}
				}
			}
			MotionKind::Backward(n) => {
				for _ in 0..n {
					if !self.cursor_back(1) {
						break
					}
				}
			}
			MotionKind::To(n) => {
				assert!((0..=self.byte_len()).contains(&n));
				self.cursor = n
			}
			MotionKind::Range(range) => {
				assert!((0..self.byte_len()).contains(&range.0));
				if self.cursor != range.0 {
					self.cursor = range.0
				}
			}
			MotionKind::Line(n) => {
				match n.cmp(&0) {
					Ordering::Equal => {
						let (start,_) = self.this_line();
						self.cursor = start;
					}
					Ordering::Less => {
						let (start,_) = self.select_lines_up(n.abs() as usize);
						self.cursor = start;
					}
					Ordering::Greater => {
						let (_,end) = self.select_lines_down(n.abs() as usize);
						self.cursor = end.saturating_sub(1);
						let (start,_) = self.this_line();
						self.cursor = start;
					}
				}
			}
			MotionKind::ToLine(n) => {
				let Some((start,_)) = self.select_line(n) else {
					return 
				};
				self.cursor = start;
			}
			MotionKind::Null => { /* Pass */ }
			MotionKind::ToScreenPos(_) => todo!(),
		}
	}
	pub fn handle_edit(&mut self, old: String, new: String, curs_pos: usize) {
		if self.merge_edit {
			let diff = Edit::diff(&old, &new, curs_pos);
			let Some(mut edit) = self.undo_stack.pop() else {
				self.undo_stack.push(diff);
				return
			};

			edit.new.push_str(&diff.new);

			self.undo_stack.push(edit);
		} else {
			let diff = Edit::diff(&old, &new, curs_pos);
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
		let cursor_pos = self.cursor;

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

		if before != after && !is_undo_op {
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

pub fn is_grapheme_boundary(s: &str, pos: usize) -> bool {
	s.is_char_boundary(pos) && s.grapheme_indices(true).any(|(i,_)| i == pos)
}

fn mk_range(a: usize, b: usize) -> Range<usize> {
    std::cmp::min(a, b)..std::cmp::max(a, b)
}
