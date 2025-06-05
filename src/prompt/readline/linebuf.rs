use std::{ops::{Range, RangeBounds, RangeInclusive}, string::Drain};

use unicode_segmentation::UnicodeSegmentation;

use super::{term::Layout, vicmd::{Direction, Motion, MotionBehavior, RegisterName, To, Verb, ViCmd, Word}};
use crate::{libsh::error::ShResult, prelude::*};

#[derive(PartialEq,Eq,Debug,Clone,Copy)]
pub enum CharClass {
	Alphanum,
	Symbol,
	Whitespace,
	Other
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

fn is_whitespace(a: &str) -> bool {
	CharClass::from(a) == CharClass::Whitespace
}

fn is_other_class(a: &str, b: &str) -> bool {
	let a = CharClass::from(a);
	let b = CharClass::from(b);
	a != b
}

fn is_other_class_not_ws(a: &str, b: &str) -> bool {
	if is_whitespace(a) || is_whitespace(b) {
		false
	} else {
		is_other_class(a, b)
	}
}

fn is_other_class_or_is_ws(a: &str, b: &str) -> bool {
	if is_whitespace(a) || is_whitespace(b) {
		true
	} else {
		is_other_class(a, b)
	}
}

fn is_other_class_and_is_ws(a: &str, b: &str) -> bool {
	is_other_class(a, b) && (is_whitespace(a) || is_whitespace(b))
}

#[derive(Default,Clone,Copy,PartialEq,Eq,Debug)]
pub enum SelectAnchor {
	#[default]
	End,
	Start
}

#[derive(Clone,Copy,PartialEq,Eq,Debug)]
pub enum SelectMode {
	Char(SelectAnchor),
	Line(SelectAnchor),
	Block(SelectAnchor),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionKind {
	To(usize), // Absolute position, exclusive
	On(usize), // Absolute position, inclusive
	Inclusive((usize,usize)), // Range, inclusive
	Exclusive((usize,usize)), // Range, exclusive
	Null
}

impl MotionKind {
	pub fn inclusive(range: RangeInclusive<usize>) -> Self {
		Self::Inclusive((*range.start(),*range.end()))
	}
	pub fn exclusive(range: Range<usize>) -> Self {
		Self::Exclusive((range.start,range.end))
	}
}

#[derive(Default,Debug)]
pub struct Edit {
	pub pos: usize,
	pub cursor_pos: usize,
	pub old: String,
	pub new: String,
	pub merging: bool,
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
				merging: false,
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
			merging: false
		}
	}
	pub fn start_merge(&mut self) {
		self.merging = true
	}
	pub fn stop_merge(&mut self) {
		self.merging = false
	}
	pub fn is_empty(&self) -> bool {
		self.new.is_empty() &&
		self.old.is_empty()
	}
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
/// A usize which will always exist between 0 and a given upper bound
///
/// * The upper bound can be both inclusive and exclusive
/// * Used for the LineBuf cursor to enforce the `0 <= cursor < self.buffer.len()` invariant.
pub struct ClampedUsize {
	value: usize,
	max: usize,
	exclusive: bool
}

impl ClampedUsize {
	pub fn new(value: usize, max: usize, exclusive: bool) -> Self {
		let mut c = Self { value: 0, max, exclusive };
		c.set(value); 
		c
	}
	pub fn get(self) -> usize {
		self.value
	}
	pub fn upper_bound(&self) -> usize {
		if self.exclusive {
			self.max.saturating_sub(1)
		} else {
			self.max
		}
	}
	pub fn inc(&mut self) -> bool {
		let max = self.upper_bound();
		if self.value == max {
			return false;
		}
		self.add(1);
		true
	}
	pub fn dec(&mut self) -> bool {
		if self.value == 0 {
			return false;
		}
		self.sub(1);
		true
	}
	pub fn set(&mut self, value: usize) {
		let max = self.upper_bound();
		self.value = value.clamp(0,max);
	}
	pub fn set_max(&mut self, max: usize) {
		self.max = max;
		self.set(self.get()); // Enforces the new maximum
	}
	pub fn add(&mut self, value: usize) {
		let max = self.upper_bound();
		self.value = (self.value + value).clamp(0,max)
	}
	pub fn sub(&mut self, value: usize) {
		self.value = self.value.saturating_sub(value)
	}
	/// Add a value to the wrapped usize, return the result
	///
	/// Returns the result instead of mutating the inner value
	pub fn ret_add(&self, value: usize) -> usize {
		let max = self.upper_bound();
		(self.value + value).clamp(0,max)
	}
	/// Add a value to the wrapped usize, forcing inclusivity
	pub fn ret_add_inclusive(&self, value: usize) -> usize {
		let max = self.max;
		(self.value + value).clamp(0,max)
	}
	/// Subtract a value from the wrapped usize, return the result
	///
	/// Returns the result instead of mutating the inner value
	pub fn ret_sub(&self, value: usize) -> usize {
		self.value.saturating_sub(value)
	}
}

#[derive(Default,Debug)]
pub struct LineBuf {
	pub buffer: String,
	pub hint: Option<String>,
	pub grapheme_indices: Option<Vec<usize>>, // Used to slice the buffer
	pub cursor: ClampedUsize, // Used to index grapheme_indices

	pub select_mode: Option<SelectMode>,
	pub select_range: Option<(usize,usize)>,
	pub last_selection: Option<(usize,usize)>,

	pub saved_col: Option<usize>,

	pub undo_stack: Vec<Edit>,
	pub redo_stack: Vec<Edit>,
}

impl LineBuf {
	pub fn new() -> Self {
		Self::default()
	}
	/// Only update self.grapheme_indices if it is None
	pub fn update_graphemes_lazy(&mut self) {
		if self.grapheme_indices.is_none() {
			self.update_graphemes();
		}
	}
	pub fn with_initial(mut self, buffer: &str, cursor: usize) -> Self {
		self.buffer = buffer.to_string();
		self.update_graphemes();
		self.cursor = ClampedUsize::new(cursor, self.grapheme_indices().len(), self.cursor.exclusive);
		self
	}
	pub fn has_hint(&self) -> bool {
		self.hint.is_some()
	}
	pub fn hint(&self) -> Option<&String> {
		self.hint.as_ref()
	}
	pub fn set_cursor_clamp(&mut self, yn: bool) {
		self.cursor.exclusive = yn;
	}
	pub fn cursor_byte_pos(&mut self) -> usize {
		self.index_byte_pos(self.cursor.get())
	}
	pub fn index_byte_pos(&mut self, index: usize) -> usize {
		self.update_graphemes_lazy();
		self.grapheme_indices()
			.get(index)
			.copied()
			.unwrap_or(self.buffer.len())
	}
	/// Update self.grapheme_indices with the indices of the current buffer
	pub fn update_graphemes(&mut self) {
		let indices: Vec<_> = self.buffer
			.grapheme_indices(true)
			.map(|(i,_)| i)
			.collect();
		self.cursor.set_max(indices.len());
		self.grapheme_indices = Some(indices)
	}
	pub fn grapheme_indices(&self) -> &[usize] {
		self.grapheme_indices.as_ref().unwrap()
	}
	pub fn grapheme_indices_owned(&self) -> Vec<usize> {
		self.grapheme_indices.as_ref().cloned().unwrap_or_default()
	}
	pub fn grapheme_at(&mut self, pos: usize) -> Option<&str> {
		self.update_graphemes_lazy();
		let indices = self.grapheme_indices();
		let start = indices.get(pos).copied()?;
		let end = indices.get(pos + 1).copied().or_else(|| {
			if pos + 1 == self.grapheme_indices().len() {
				Some(self.buffer.len())
			} else {
				None
			}
		})?;
		self.buffer.get(start..end)
	}
	pub fn grapheme_at_cursor(&mut self) -> Option<&str> {
		self.grapheme_at(self.cursor.get())
	}
	pub fn slice(&mut self, range: Range<usize>) -> Option<&str> {
		self.update_graphemes_lazy();
		let start_index = self.grapheme_indices().get(range.start).copied()?;
		let end_index = self.grapheme_indices().get(range.end).copied().or_else(|| {
			if range.end == self.grapheme_indices().len() {
				Some(self.buffer.len())
			} else {
				None
			}
		})?;
		self.buffer.get(start_index..end_index)
	}
	pub fn slice_to(&mut self, end: usize) -> Option<&str> {
		self.update_graphemes_lazy();
		flog!(DEBUG,end);
		flog!(DEBUG,self.grapheme_indices().len());
		let grapheme_index = self.grapheme_indices().get(end).copied().or_else(|| {
			if end == self.grapheme_indices().len() {
				Some(self.buffer.len())
			} else {
				None
			}
		})?;
		self.buffer.get(..grapheme_index)
	}
	pub fn slice_from(&mut self, start: usize) -> Option<&str> {
		self.update_graphemes_lazy();
		let grapheme_index = *self.grapheme_indices().get(start)?;
		self.buffer.get(grapheme_index..)
	}
	pub fn slice_to_cursor(&mut self) -> Option<&str> {
		self.slice_to(self.cursor.get())
	}
	pub fn slice_from_cursor(&mut self) -> Option<&str> {
		self.slice_from(self.cursor.get())
	}	
	pub fn drain(&mut self, start: usize, end: usize) -> String {
		let drained = if end == self.grapheme_indices().len() {
			let start = self.grapheme_indices()[start];
			self.buffer.drain(start..).collect()
		} else {
			let start = self.grapheme_indices()[start];
			let end = self.grapheme_indices()[end];
			self.buffer.drain(start..end).collect()
		};
		self.update_graphemes();
		drained
	}
	pub fn push(&mut self, ch: char) {
		self.buffer.push(ch);
		self.update_graphemes();
	}
	pub fn push_str(&mut self, slice: &str) {
		self.buffer.push_str(slice);
		self.update_graphemes();
	}
	pub fn insert_at_cursor(&mut self, ch: char) {
		let cursor_pos = self.cursor_byte_pos();
		self.buffer.insert(cursor_pos, ch);
		self.update_graphemes();
	}
	pub fn set_buffer(&mut self, buffer: String) {
		self.buffer = buffer;
		self.update_graphemes();
	}
	pub fn select_range(&self) -> Option<(usize,usize)> {
		self.select_range
	}
	pub fn start_selecting(&mut self, mode: SelectMode) {
		self.select_mode = Some(mode);
		let range_start = self.cursor;
		let mut range_end = self.cursor;
		range_end.add(1);
		self.select_range = Some((range_start.get(),range_end.get()));
	}
	pub fn stop_selecting(&mut self) {
		self.select_mode = None;
		if self.select_range.is_some() {
			self.last_selection = self.select_range.take();
		}
	}
	pub fn rfind_newlines(&mut self, n: usize) -> usize {
		self.rfind_newlines_from(self.cursor.get(), n)
	}
	pub fn find_newlines(&mut self, n: usize) -> usize {
		self.find_newlines_from(self.cursor.get(), n)
	}
	pub fn rfind_newlines_from(&mut self, start_pos: usize, n: usize) -> usize {
		let Some(slice) = self.slice_to(start_pos) else {
			return 0
		};

		let mut offset = slice.len();
		let mut count = 0;

		for (i, b) in slice.bytes().rev().enumerate() {
			if b == b'\n' {
				count += 1;
				if count == n {
					offset = slice.len() - i - 1;
					break;
				}
			}
		}

		let byte_pos = if count == n {
			offset // move to *after* the newline
		} else {
			0
		};

		self.find_index_for(byte_pos).unwrap_or(0)
	}
	pub fn find_newlines_from(&mut self, start_pos: usize, n: usize) -> usize {
		let Some(slice) = self.slice_from(start_pos) else {
			return self.cursor.max
		};

		let mut count = 0;
		for (i, b) in slice.bytes().enumerate() {
			if b == b'\n' {
				count += 1;
				if count == n {
					let byte_pos = self.index_byte_pos(start_pos) + i;
					return self.find_index_for(byte_pos).unwrap_or(self.cursor.max);
				}
			}
		}

		self.cursor.max
	}
	pub fn find_index_for(&self, byte_pos: usize) -> Option<usize> {
		self.grapheme_indices()
			.binary_search(&byte_pos)
			.ok()
	}
	pub fn start_of_cursor_line(&mut self) -> usize {
		let mut pos = self.rfind_newlines(1);
		if pos != 0 {
			pos += 1; // Don't include the newline itself
		}
		pos
	}
	pub fn end_of_cursor_line(&mut self) -> usize {
		self.find_newlines(1)
	}
	pub fn this_line(&mut self) -> (usize,usize) {
		(
			self.start_of_cursor_line(),
			self.end_of_cursor_line()
		)
	}
	pub fn prev_line(&mut self) -> Option<(usize,usize)> {
		if self.start_of_cursor_line() == 0 {
			return None
		}
		let mut start = self.rfind_newlines(2);
		if start != 0 {
			start += 1;
		}
		let end = self.find_newlines_from(start, 1);
		Some((start, end))
	}
	pub fn next_line(&mut self) -> Option<(usize,usize)> {
		if self.end_of_cursor_line() == self.cursor.max {
			return None;
		}
		let end = self.find_newlines(2);
		let start = self.rfind_newlines_from(end, 1) + 1;
		Some((start,end))
	}
	pub fn select_lines_backward(&mut self, n: usize) -> (usize,usize) {
		let mut start = self.rfind_newlines(n);
		if start != 0 {
			start += 1;
		}
		let end = self.end_of_cursor_line();
		(start,end)
	}
	pub fn select_lines_forward(&mut self, n: usize) -> (usize,usize) {
		let start = self.start_of_cursor_line();
		let end = self.find_newlines(n);
		(start,end)
	}
	pub fn handle_edit(&mut self, old: String, new: String, curs_pos: usize) {
		let edit_is_merging = self.undo_stack.last().is_some_and(|edit| edit.merging);
		if edit_is_merging {
			let diff = Edit::diff(&old, &new, curs_pos);
			if diff.is_empty() {
				return
			}
			let Some(mut edit) = self.undo_stack.pop() else {
				self.undo_stack.push(diff);
				return
			};

			edit.new.push_str(&diff.new);
			edit.old.push_str(&diff.old);

			self.undo_stack.push(edit);
		} else {
			let diff = Edit::diff(&old, &new, curs_pos);
			if !diff.is_empty() {
				self.undo_stack.push(diff);
			}
		}
	}

	pub fn directional_indices_iter(&mut self, dir: Direction) -> Box<dyn Iterator<Item = usize>> {
		self.directional_indices_iter_from(self.cursor.get(), dir)
	}
	pub fn directional_indices_iter_from(&mut self, pos: usize, dir: Direction) -> Box<dyn Iterator<Item = usize>> {
		self.update_graphemes_lazy();
		let skip = if pos == 0 { 0 } else { pos + 1 };
		match dir {
			Direction::Forward => {
				Box::new(
					self.grapheme_indices()
					.to_vec()
					.into_iter()
					.skip(skip)
				) as Box<dyn Iterator<Item = usize>>
			}
			Direction::Backward => {
				Box::new(
					self.grapheme_indices()
					.to_vec()
					.into_iter()
					.take(pos)
					.rev()
				) as Box<dyn Iterator<Item = usize>>
			}
		}
	}

	pub fn dispatch_word_motion(&mut self, to: To, word: Word, dir: Direction) -> usize {
		// Not sorry for these method names btw
		match to {
			To::Start => {
				match dir {
					Direction::Forward => self.start_of_word_forward_or_end_of_word_backward(word, dir),
					Direction::Backward => self.end_of_word_forward_or_start_of_word_backward(word, dir)
				}
			}
			To::End => {
				match dir {
					Direction::Forward => self.end_of_word_forward_or_start_of_word_backward(word, dir),
					Direction::Backward => self.start_of_word_forward_or_end_of_word_backward(word, dir),
				}
			}
		}
	}

	/// Finds the start of a word forward, or the end of a word backward
	///
	/// Finding the start of a word in the forward direction, and finding the end of a word in the backward direction
	/// are logically the same operation, if you use a reversed iterator for the backward motion.
	pub fn start_of_word_forward_or_end_of_word_backward(&mut self, word: Word, dir: Direction) -> usize {
		let mut pos = self.cursor.get();
		let default = match dir {
			Direction::Backward => 0,
			Direction::Forward => self.grapheme_indices().len()
		};
		let mut indices_iter = self.directional_indices_iter(dir).peekable(); // And make it peekable

		match word {
			Word::Big => {
				let on_boundary = self.grapheme_at(pos).is_none_or(is_whitespace);
				flog!(DEBUG,on_boundary);
				flog!(DEBUG,pos);
				if on_boundary {
					let Some(idx) = indices_iter.next() else { return default };
					pos = idx;
				}
				flog!(DEBUG,pos);

				// Check current grapheme
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);
				flog!(DEBUG,on_whitespace);

				// Find the next whitespace
				if !on_whitespace {
					let Some(_ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace)) else {
						return default
					};
				}

				// Return the next visible grapheme position
				let non_ws_pos = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c))).unwrap_or(default);
				non_ws_pos
			}
			Word::Normal => {
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else { return default };
				let Some(next_idx) = indices_iter.next() else { return default };
				let on_boundary = self.grapheme_at(next_idx).is_none_or(|c| is_other_class_or_is_ws(c, &cur_char));
				if on_boundary {
					pos = next_idx
				}

				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);

				// Advance until hitting whitespace or a different character class
				if !on_whitespace {
					let other_class_pos = indices_iter.find(
						|i| {
							self.grapheme_at(*i)
								.is_some_and(|c| is_other_class_or_is_ws(c, &cur_char))
						}
					);
					let Some(other_class_pos) = other_class_pos else {
						return default
					};
					// If we hit a different character class, we return here
					if self.grapheme_at(other_class_pos).is_some_and(|c| !is_whitespace(c)) {
						return other_class_pos
					}
				}

				// We are now certainly on a whitespace character. Advance until a non-whitespace character.
				let non_ws_pos = indices_iter.find(
					|i| {
						self.grapheme_at(*i)
							.is_some_and(|c| !is_whitespace(c))
					}
				).unwrap_or(default);
				non_ws_pos
			}
		}
	}
	/// Finds the end of a word forward, or the start of a word backward
	///
	/// Finding the end of a word in the forward direction, and finding the start of a word in the backward direction
	/// are logically the same operation, if you use a reversed iterator for the backward motion.
	pub fn end_of_word_forward_or_start_of_word_backward(&mut self, word: Word, dir: Direction) -> usize {
		let mut pos = self.cursor.get();
		let default = match dir {
			Direction::Backward => 0,
			Direction::Forward => self.grapheme_indices().len()
		};

		let mut indices_iter = self.directional_indices_iter(dir).peekable(); 

		match word {
			Word::Big => {
				let Some(next_idx) = indices_iter.next() else { return default };
				let on_boundary = self.grapheme_at(next_idx).is_none_or(is_whitespace);
				if on_boundary {
					let Some(idx) = indices_iter.next() else { return default };
					pos = idx;
				}

				// Check current grapheme
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);

				// Advance iterator to next visible grapheme
				if on_whitespace {
					let Some(_non_ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c))) else {
						return default
					};
				}

				// The position of the next whitespace will tell us where the end (or start) of the word is
					let Some(next_ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace)) else {
						return default
					};
					pos = next_ws_pos;

				if pos == self.grapheme_indices().len() {
					// We reached the end of the buffer
					pos
				} else {
					// We hit some whitespace, so we will go back one
					match dir {
						Direction::Forward => pos.saturating_sub(1),
						Direction::Backward => pos + 1,
					}
				}
			}
			Word::Normal => {
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else { return default };
				let Some(next_idx) = indices_iter.next() else { return default };
				let on_boundary = self.grapheme_at(next_idx).is_none_or(|c| is_other_class_or_is_ws(c, &cur_char));
				if on_boundary {
					pos = next_idx
				}

				// Check current grapheme
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);

				// Proceed to next visible grapheme
				if on_whitespace {
					let Some(non_ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c))) else {
						return default
					};
					pos = non_ws_pos
				}

				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return self.grapheme_indices().len()
				};
				// The position of the next differing character class will tell us where the end (or start) of the word is
				let Some(next_ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| is_other_class_or_is_ws(c, &cur_char))) else {
					return default
				};
				pos = next_ws_pos;

				if pos == self.grapheme_indices().len() {
					// We reached the end of the buffer
					pos
				} else {
					// We hit some other character class, so we go back one
					match dir {
						Direction::Forward => pos.saturating_sub(1),
						Direction::Backward => pos + 1,
					}
				}
			}
		}
	}
	pub fn rfind_from<F: Fn(&str) -> bool>(&mut self, pos: usize, op: F) -> usize {
		let Some(slice) = self.slice_to(pos) else {
			return self.grapheme_indices().len()
		};
		for (offset,grapheme) in slice.grapheme_indices(true).rev() {
			if op(grapheme) {
				return pos + offset
			}
		}
		self.grapheme_indices().len()
	}
	pub fn rfind_from_optional<F: Fn(&str) -> bool>(&mut self, pos: usize, op: F) -> Option<usize> {
		let slice = self.slice_to(pos)?;
		for (offset,grapheme) in slice.grapheme_indices(true).rev() {
			if op(grapheme) {
				return Some(pos + offset)
			}
		}
		None
	}
	pub fn rfind<F: Fn(&str) -> bool>(&mut self, op: F) -> usize {
		self.rfind_from(self.cursor.get(), op)
	}
	pub fn rfind_optional<F: Fn(&str) -> bool>(&mut self, op: F) -> Option<usize> {
		self.rfind_from_optional(self.cursor.get(), op)
	}
	pub fn find_from<F: Fn(&str) -> bool>(&mut self, pos: usize, op: F) -> usize {
		let Some(slice) = self.slice_from(pos) else {
			return self.grapheme_indices().len()
		};
		for (offset,grapheme) in slice.grapheme_indices(true) {
			if op(grapheme) {
				return pos + offset
			}
		}
		self.grapheme_indices().len()
	}
	pub fn find_from_optional<F: Fn(&str) -> bool>(&mut self, pos: usize, op: F) -> Option<usize> {
		let slice = self.slice_from(pos)?; 
		for (offset,grapheme) in slice.grapheme_indices(true) {
			if op(grapheme) {
				return Some(pos + offset)
			}
		}
		None
	}
	pub fn find_optional<F: Fn(&str) -> bool>(&mut self, op: F) -> Option<usize> {
		self.find_from_optional(self.cursor.get(), op)
	}
	pub fn find<F: Fn(&str) -> bool>(&mut self, op: F) -> usize {
		self.find_from(self.cursor.get(), op)
	}
	pub fn eval_motion(&mut self, motion: Motion) -> MotionKind {
		let buffer = self.buffer.clone();
		if self.has_hint() {
			let hint = self.hint.clone().unwrap();
			self.push_str(&hint);
		}

		let eval = match motion {
			Motion::WholeLine => {
				let (start,end) = self.this_line();
				MotionKind::Inclusive((start,end))
			}
			Motion::WordMotion(to, word, dir) => MotionKind::On(self.dispatch_word_motion(to, word, dir)),
			Motion::TextObj(text_obj, bound) => todo!(),
			Motion::EndOfLastWord => {
				let start = self.start_of_cursor_line();
				let mut indices = self.directional_indices_iter_from(start,Direction::Forward);
				let mut last_graphical = None;
				while let Some(idx) = indices.next() {
					let grapheme = self.grapheme_at(idx).unwrap();
					if !is_whitespace(grapheme) {
						last_graphical = Some(idx);
					}
					if grapheme == "\n" {
						break
					}
				}
				let Some(last) = last_graphical else {
					return MotionKind::Null
				};
				MotionKind::On(last)
			}
			Motion::BeginningOfFirstWord => {
				let start = self.start_of_cursor_line();
				let mut indices = self.directional_indices_iter_from(start,Direction::Forward);
				let mut first_graphical = None;
				while let Some(idx) = indices.next() {
					let grapheme = self.grapheme_at(idx).unwrap();
					if !is_whitespace(grapheme) {
						flog!(DEBUG,grapheme);
						first_graphical = Some(idx);
						break
					}
					if grapheme == "\n" {
						break
					}
				}
				let Some(first) = first_graphical else {
					return MotionKind::Null
				};
				MotionKind::On(first)
			}
			Motion::BeginningOfLine => MotionKind::On(self.start_of_cursor_line()),
			Motion::EndOfLine => MotionKind::On(self.end_of_cursor_line()),
			Motion::CharSearch(direction, dest, ch) => todo!(),
			Motion::BackwardChar => MotionKind::On(self.cursor.ret_sub(1)),
			Motion::ForwardChar => MotionKind::On(self.cursor.ret_add_inclusive(1)),
			Motion::LineUp => todo!(),
			Motion::LineUpCharwise => todo!(),
			Motion::ScreenLineUp => todo!(),
			Motion::ScreenLineUpCharwise => todo!(),
			Motion::LineDown => todo!(),
			Motion::LineDownCharwise => todo!(),
			Motion::ScreenLineDown => todo!(),
			Motion::ScreenLineDownCharwise => todo!(),
			Motion::BeginningOfScreenLine => todo!(),
			Motion::FirstGraphicalOnScreenLine => todo!(),
			Motion::HalfOfScreen => todo!(),
			Motion::HalfOfScreenLineText => todo!(),
			Motion::WholeBuffer => todo!(),
			Motion::BeginningOfBuffer => todo!(),
			Motion::EndOfBuffer => todo!(),
			Motion::ToColumn(col) => todo!(),
			Motion::ToDelimMatch => todo!(),
			Motion::ToBrace(direction) => todo!(),
			Motion::ToBracket(direction) => todo!(),
			Motion::ToParen(direction) => todo!(),
			Motion::Range(start, end) => todo!(),
			Motion::RepeatMotion => todo!(),
			Motion::RepeatMotionRev => todo!(),
			Motion::Null => MotionKind::Null
		};

		self.set_buffer(buffer);
		eval
	}
	pub fn apply_motion(&mut self, motion: MotionKind) {
		let last_grapheme_pos = self
			.grapheme_indices()
			.len()
			.saturating_sub(1);

		if self.has_hint() {
			let hint = self.hint.take().unwrap();
			self.push_str(&hint);
			self.move_cursor(motion);

			if self.cursor.get() > last_grapheme_pos {
				let buf_end = if self.cursor.exclusive {
					self.cursor.ret_add(1)
				} else {
					self.cursor.get()
				};
				let remainder = self.slice_from(buf_end);

				if remainder.is_some_and(|slice| !slice.is_empty()) {
					let remainder = remainder.unwrap().to_string();
					self.hint = Some(remainder);
				}

				let buffer = self.slice_to(buf_end).unwrap_or_default();
				self.buffer = buffer.to_string();
	 		} else {
				let old_buffer = self.slice_to(last_grapheme_pos + 1).unwrap().to_string();
				let old_hint = self.slice_from(last_grapheme_pos + 1).unwrap().to_string();

				self.hint = Some(old_hint);
				self.set_buffer(old_buffer);
			}
		} else {
			self.move_cursor(motion);
		}
	}
	pub fn move_cursor(&mut self, motion: MotionKind) {
		match motion {
			MotionKind::On(pos) => self.cursor.set(pos),
			MotionKind::To(pos) => {
				self.cursor.set(pos);

				match pos.cmp(&self.cursor.get()) {
					std::cmp::Ordering::Less => {
						self.cursor.add(1);
					}
					std::cmp::Ordering::Greater => {
						self.cursor.sub(1);
					}
					std::cmp::Ordering::Equal => { /* Do nothing */ }
				}
			}
			MotionKind::Inclusive((start,_)) |
			MotionKind::Exclusive((start,_)) => {
				self.cursor.set(start)
			}
			MotionKind::Null => { /* Do nothing */ }
		}
	}
	pub fn exec_verb(&mut self, verb: Verb, motion: MotionKind, register: RegisterName) -> ShResult<()> {
		match verb {
			Verb::Delete |
			Verb::Yank |
			Verb::Change => {
				let (start,end) = match motion {
					MotionKind::On(pos) => ordered(self.cursor.get(), pos),
					MotionKind::To(pos) => {
						let pos = match pos.cmp(&self.cursor.get()) {
							std::cmp::Ordering::Less => pos + 1,
							std::cmp::Ordering::Greater => pos - 1,
							std::cmp::Ordering::Equal => pos,
						};
						ordered(self.cursor.get(), pos)
					}
					MotionKind::Inclusive((start,end)) => {
						let (start, mut end) = ordered(start, end);
						end = ClampedUsize::new(end, self.cursor.max, false).ret_add(1);
						(start,end)
					}
					MotionKind::Exclusive((start,end)) => ordered(start, end),
					MotionKind::Null => return Ok(())
				};
				flog!(DEBUG,start,end);
				let register_text = if verb == Verb::Yank {
					self.slice(start..end)
						.map(|c| c.to_string())
						.unwrap_or_default()
				} else {
					self.drain(start, end)
				};
				register.write_to_register(register_text);
				self.cursor.set(start);
			}
			Verb::Rot13 => todo!(),
			Verb::ReplaceChar(_) => todo!(),
			Verb::ToggleCase => todo!(),
			Verb::ToLower => todo!(),
			Verb::ToUpper => todo!(),
			Verb::Complete => todo!(),
			Verb::CompleteBackward => todo!(),
			Verb::Undo => todo!(),
			Verb::Redo => todo!(),
			Verb::RepeatLast => todo!(),
			Verb::Put(anchor) => todo!(),
			Verb::SwapVisualAnchor => todo!(),
			Verb::JoinLines => todo!(),
			Verb::InsertChar(ch) => {
				self.insert_at_cursor(ch);
				self.cursor.add(1);
			}
			Verb::Insert(string) => {
				self.push_str(&string);
				let graphemes = string.graphemes(true).count();
				self.cursor.add(graphemes);
			}
			Verb::Breakline(anchor) => todo!(),
			Verb::Indent => todo!(),
			Verb::Dedent => todo!(),
			Verb::Equalize => todo!(),
			Verb::AcceptLineOrNewline => todo!(),
			Verb::EndOfFile => {
				if self.buffer.is_empty() {

				}
			}
			Verb::InsertModeLineBreak(anchor) => todo!(),

			Verb::ReplaceMode |
			Verb::InsertMode |
			Verb::NormalMode |
			Verb::VisualMode |
			Verb::VisualModeLine |
			Verb::VisualModeBlock |
			Verb::VisualModeSelectLast => {
				/* Already handled */ 
				self.apply_motion(motion);
			}
		}
		Ok(())
	}
	pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
		let clear_redos = !cmd.is_undo_op() || cmd.verb.as_ref().is_some_and(|v| v.1.is_edit());
		let is_char_insert = cmd.verb.as_ref().is_some_and(|v| v.1.is_char_insert());
		let is_line_motion = cmd.is_line_motion();
		let is_undo_op = cmd.is_undo_op();
		let edit_is_merging = self.undo_stack.last().is_some_and(|edit| edit.merging);

		// Merge character inserts into one edit
		if edit_is_merging && cmd.verb.as_ref().is_none_or(|v| !v.1.is_char_insert()) {
			if let Some(edit) = self.undo_stack.last_mut() {
				edit.stop_merge();
			}
		}

		let ViCmd { register, verb, motion, raw_seq: _ } = cmd;

		let verb_count = verb.as_ref().map(|v| v.0);
		let motion_count = motion.as_ref().map(|m| m.0);

		let before = self.buffer.clone();
		let cursor_pos = self.cursor.get();

		for _ in 0..verb_count.unwrap_or(1) {
			for _ in 0..motion_count.unwrap_or(1) {
				/*
				 * Let's evaluate the motion now
				 * If motion is None, we will try to use self.select_range
				 * If self.select_range is None, we will use MotionKind::Null
				 */
				let motion_eval = motion
					.clone()
					.map(|m| self.eval_motion(m.1))
					.unwrap_or({
						self.select_range
							.map(MotionKind::Inclusive)
							.unwrap_or(MotionKind::Null)
					});

				if let Some(verb) = verb.clone() {
					self.exec_verb(verb.1, motion_eval, register)?;
				} else {
					self.apply_motion(motion_eval);
				}
			}
		}

		let after = self.buffer.clone();
		if clear_redos {
			self.redo_stack.clear();
		}

		if before != after && !is_undo_op {
			self.handle_edit(before, after, cursor_pos);
			/*
			 * The buffer has been edited,
			 * which invalidates the grapheme_indices vector
			 * We set it to None now, so that self.update_graphemes_lazy()
			 * will update it when it is needed again
			 */
			self.grapheme_indices = None; 
		}

		if !is_line_motion {
			self.saved_col = None;
		}

		if is_char_insert {
			if let Some(edit) = self.undo_stack.last_mut() {
				edit.start_merge();
			}
		}

		Ok(())
	}
	pub fn as_str(&self) -> &str {
		&self.buffer // FIXME: this will have to be fixed up later
	}
}

pub fn ordered(start: usize, end: usize) -> (usize,usize) {
	if start > end {
		(end,start)
	} else {
		(start,end)
	}
}
