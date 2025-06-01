use std::{cmp::Ordering, fmt::Display, ops::{Range, RangeBounds}};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionKind {
	Forward(usize),
	To(usize), // Land just before
	On(usize), // Land directly on
	Before(usize), // Had to make a separate one for char searches, for some reason
	Backward(usize),
	Range((usize,usize)),
	Line(isize), // positive = up line, negative = down line
	ToLine(usize), 
	Null,

	/// Absolute position based on display width of characters
	/// Factors in the length of the prompt, and skips newlines
	ScreenLine(isize)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SelectionAnchor {
	Start,
	#[default]
	End
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
	Char(SelectionAnchor),
	Line(SelectionAnchor),
	Block(SelectionAnchor)
}

impl Default for SelectionMode {
	fn default() -> Self {
	  Self::Char(Default::default())
	}
}

impl SelectionMode {
	pub fn anchor(&self) -> &SelectionAnchor {
		match self {
			SelectionMode::Char(anchor) |
			SelectionMode::Line(anchor) |
			SelectionMode::Block(anchor) => anchor
		}
	}
	pub fn invert_anchor(&mut self) {
		match self {
			SelectionMode::Char(anchor) |
			SelectionMode::Line(anchor) |
			SelectionMode::Block(anchor) => {
				*anchor = match anchor {
					SelectionAnchor::Start => SelectionAnchor::End,
					SelectionAnchor::End => SelectionAnchor::Start
				}
			}
		}
	}
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

fn is_whitespace(a: &str) -> bool {
	CharClass::from(a) == CharClass::Whitespace
}

fn is_other_class(a: &str, b: &str) -> bool {
	let a = CharClass::from(a);
	let b = CharClass::from(b);
	a != b
}

fn is_other_class_or_ws(a: &str, b: &str) -> bool {
	if is_whitespace(a) || is_whitespace(b) {
		true
	} else {
		is_other_class(a, b)
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

#[derive(Default,Debug)]
pub struct LineBuf {
	buffer: String,
	hint: Option<String>,
	cursor: usize,
	clamp_cursor: bool,
	select_mode: Option<SelectionMode>,
	selected_range: Option<Range<usize>>,
	last_selected_range: Option<Range<usize>>,
	first_line_offset: usize,
	saved_col: Option<usize>,
	term_dims: (usize,usize), // Height, width
	move_cursor_on_undo: bool,
	undo_stack: Vec<Edit>,
	redo_stack: Vec<Edit>,
	tab_stop: usize
}

impl LineBuf {
	pub fn new() -> Self {
		Self { tab_stop: 8, ..Default::default() }
	}
	pub fn with_initial(mut self, initial: &str) -> Self {
		self.buffer = initial.to_string();
		self
	}
	pub fn selected_range(&self) -> Option<&Range<usize>> {
		self.selected_range.as_ref()
	}
	pub fn is_selecting(&self) -> bool {
		self.select_mode.is_some()
	}
	pub fn stop_selecting(&mut self) {
		self.select_mode = None;
		if self.selected_range().is_some() {
			self.last_selected_range = self.selected_range.take();
		}
	}
	pub fn start_selecting(&mut self, mode: SelectionMode) {
		self.select_mode = Some(mode);
		self.selected_range = Some(self.cursor..(self.cursor + 1).min(self.byte_len().saturating_sub(1)))
	}
	pub fn has_hint(&self) -> bool {
		self.hint.is_some()
	}
	pub fn set_hint(&mut self, hint: Option<String>) {
		self.hint = hint
	}
	pub fn set_first_line_offset(&mut self, offset: usize) {
		self.first_line_offset = offset
	}
	pub fn as_str(&self) -> &str {
		&self.buffer
	}
	pub fn saved_col(&self) -> Option<usize> {
		self.saved_col
	}
	pub fn update_term_dims(&mut self, dims: (usize,usize)) {
		self.term_dims = dims
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
	pub fn at_end_of_buffer(&self) -> bool {
		if self.clamp_cursor {
			self.cursor == self.byte_len().saturating_sub(1)
		} else {
			self.cursor == self.byte_len()
		}
	}
	pub fn undos(&self) -> usize {
		self.undo_stack.len()
	}
	pub fn is_empty(&self) -> bool {
		self.buffer.is_empty()
	}
	pub fn set_move_cursor_on_undo(&mut self, yn: bool) {
		self.move_cursor_on_undo = yn;
	}
	pub fn clamp_cursor(&mut self) {
		// Normal mode does not allow you to sit on the edge of the buffer, you must be hovering over a character
		// Insert mode does let you set on the edge though, so that you can append new characters
		// This method is used in Normal mode
		if self.cursor == self.byte_len() || self.grapheme_at_cursor() == Some("\n") {
			self.cursor_back(1);
		}
	}
	pub fn clamp_range(&self, range: Range<usize>) -> Range<usize> {
		let (mut start,mut end) = (range.start,range.end);
		start = start.max(0);
		end = end.min(self.byte_len());
		start..end
	}
	pub fn grapheme_len(&self) -> usize {
		self.buffer.grapheme_indices(true).count()
	}
	pub fn slice_from_cursor(&self) -> &str {
		if let Some(slice) = &self.buffer.get(self.cursor..) {
			slice
		} else {
			""
		}
	}
	pub fn slice_to_cursor(&self) -> &str {
		if let Some(slice) = self.buffer.get(..self.cursor) {
			slice
		} else {
			&self.buffer
		}
		
	}
	pub fn into_line(self) -> String {
		self.buffer
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
	pub fn g_idx_to_byte_pos(&self, pos: usize) -> Option<usize> {
		if pos >= self.byte_len() {
			None
		} else {
			self.buffer.grapheme_indices(true).map(|(i,_)| i).nth(pos)
		}
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
				self.grapheme_at(self.cursor)
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
	pub fn sync_cursor(&mut self) {
		if !self.buffer.is_char_boundary(self.cursor) {
			self.cursor = self.prev_pos(1).unwrap_or(0)
		}
	}
	pub fn cursor_back(&mut self, dist: usize) -> bool {
		let Some(pos) = self.prev_pos(dist) else {
			return false
		};
		self.cursor = pos;
		true
	}
	/// Constrain the cursor to the current line
	pub fn cursor_back_confined(&mut self, dist: usize) -> bool {
		for _ in 0..dist {
			let Some(pos) = self.prev_pos(1) else {
				return false
			};
			if let Some("\n") = self.grapheme_at(pos) {
				return false
			}
			if !self.cursor_back(1) {
				return false
			}
		}
		true
	}
	pub fn cursor_fwd_confined(&mut self, dist: usize) -> bool {
		for _ in 0..dist {
			let Some(pos) = self.next_pos(1) else {
				return false
			};
			if let Some("\n") = self.grapheme_at(pos) {
				return false
			}
			if !self.cursor_fwd(1) {
				return false
			}
		}
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

	fn compute_display_positions<'a>(
		text: impl Iterator<Item = &'a str>,
		start_col: usize,
		tab_stop: usize,
		term_width: usize,
	) -> (usize, usize) {
		let mut lines = 0;
		let mut col = start_col;

		for grapheme in text {
			match grapheme {
				"\n" => {
					lines += 1;
					col = 1;
				}
				"\t" => {
					let spaces_to_next_tab = tab_stop - (col % tab_stop);
					if col + spaces_to_next_tab > term_width {
						lines += 1;
						col = 1;
					} else {
						col += spaces_to_next_tab;
					}

					// Don't ask why this is here
					// I don't know either
					// All I know is that it only finds the correct cursor position
					// if i add one to the column here, for literally no reason
					// Thank you linux terminal :)
					col += 1; 
				}
				_ => {
					col += grapheme.width();
					if col > term_width {
						lines += 1;
						col = 1;
					}
				}
			}
		}
		if col == term_width {
			lines += 1;
			// Don't ask why col has to be set to zero here but one everywhere else
			// I don't know either
			// All I know is that it only finds the correct cursor position
			// if I set col to 0 here, and 1 everywhere else
			// Thank you linux terminal :)
			col = 0;
		}

		(lines, col)
	}
	pub fn count_display_lines(&self, offset: usize, term_width: usize) -> usize {
		let (lines, _) = Self::compute_display_positions(
			self.buffer.graphemes(true),
			offset,
			self.tab_stop,
			term_width,
		);
		lines
	}

	pub fn cursor_display_line_position(&self, offset: usize, term_width: usize) -> usize {
		let (lines, _) = Self::compute_display_positions(
			self.slice_to_cursor().graphemes(true),
			offset,
			self.tab_stop,
			term_width,
		);
		lines
	}

	pub fn display_coords(&self, term_width: usize) -> (usize, usize) {
		Self::compute_display_positions(
			self.slice_to_cursor().graphemes(true),
			0,
			self.tab_stop,
			term_width,
		)
	}

	pub fn cursor_display_coords(&self, term_width: usize) -> (usize, usize) {
		let (d_line, mut d_col) = self.display_coords(term_width);
		let total_lines = self.count_display_lines(0, term_width);
		let is_first_line = self.start_of_line() == 0;
		let mut logical_line = total_lines - d_line;

		if is_first_line {
			d_col += self.first_line_offset;
			if d_col > term_width {
				logical_line = logical_line.saturating_sub(1);
				d_col -= term_width;
			}
		}

		(logical_line, d_col)
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
		if self.clamp_cursor {
			self.move_to(self.byte_len().saturating_sub(1))
		} else {
			self.move_to(self.byte_len())
		}
	}
	pub fn move_home(&mut self) -> bool {
		let start = self.start_of_line();
		self.move_to(start)
	}
	pub fn move_end(&mut self) -> bool {
		let end = self.end_of_line();
		self.move_to(end)
	}
	/// Consume the LineBuf and return the buffer
	pub fn pack_line(self) -> String {
		self.buffer
	}
	pub fn accept_hint(&mut self) {
		if let Some(hint) = self.hint.take() {
			let old_buf = self.buffer.clone();
			self.buffer.push_str(&hint);
			let new_buf = self.buffer.clone();
			self.handle_edit(old_buf, new_buf, self.cursor);
			self.move_buf_end();
		}
	}
	pub fn accept_hint_partial(&mut self, accept_to: usize) {
		if let Some(hint) = self.hint.take() {
			let accepted = &hint[..accept_to];
			let remainder = &hint[accept_to..];
			self.buffer.push_str(accepted);
			self.hint = Some(remainder.to_string());
		}
	}
	/// If we have a hint, then motions are able to extend into it
	/// and partially accept pieces of it, instead of the whole thing
	pub fn apply_motion_with_hint(&mut self, motion: MotionKind) {
		let buffer_end = self.byte_len().saturating_sub(1);
		flog!(DEBUG,self.hint);
		if let Some(hint) = self.hint.take() {
			self.buffer.push_str(&hint);
			flog!(DEBUG,motion);
			self.apply_motion(/*forced*/ true, motion);
			flog!(DEBUG, self.cursor);
			flog!(DEBUG, self.grapheme_at_cursor());
			if self.cursor > buffer_end {
				let remainder = if self.clamp_cursor {
					self.slice_from((self.cursor + 1).min(self.byte_len()))
				} else {
					self.slice_from_cursor()
				};
				flog!(DEBUG,remainder);
				if !remainder.is_empty() {
					self.hint = Some(remainder.to_string());
				}
				let buffer = if self.clamp_cursor {
					self.slice_to((self.cursor + 1).min(self.byte_len()))
				} else {
					self.slice_to_cursor()
				};
				flog!(DEBUG,buffer);
				self.buffer = buffer.to_string();
				flog!(DEBUG,self.hint);
			} else {
				let old_hint = self.slice_from(buffer_end + 1);
				flog!(DEBUG,old_hint);
				self.hint = Some(old_hint.to_string());
				let buffer = self.slice_to(buffer_end + 1);
				flog!(DEBUG,buffer);
				self.buffer = buffer.to_string();
			}
		}
	}
	pub fn find_prev_line_pos(&mut self) -> Option<usize> {
		if self.start_of_line() == 0 {
			return None
		};
		let col = self.saved_col.unwrap_or(self.cursor_column());
		let line = self.line_no();
		if self.saved_col.is_none() {
			self.saved_col = Some(col);
		}
		let (start,end) = self.select_line(line - 1).unwrap();
		Some((start + col).min(end.saturating_sub(1)))
	}
	pub fn find_next_line_pos(&mut self) -> Option<usize> {
		if self.end_of_line() == self.byte_len() {
			return None
		};
		let col = self.saved_col.unwrap_or(self.cursor_column());
		let line = self.line_no();
		if self.saved_col.is_none() {
			self.saved_col = Some(col);
		}
		let (start,end) = self.select_line(line + 1).unwrap();
		Some((start + col).min(end.saturating_sub(1)))
	}
	pub fn cursor_column(&self) -> usize {
		let line_start = self.start_of_line();
		self.buffer[line_start..self.cursor].graphemes(true).count()
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
	pub fn prev_line(&self, offset: usize) -> (usize,usize) {
		let (start,_) = self.select_lines_up(offset);
		let end = self.slice_from_cursor().find('\n').unwrap_or(self.byte_len());
		(start,end)
	}
	pub fn next_line(&self, offset: usize) -> Option<(usize,usize)> {
		if self.this_line().1 == self.byte_len() {
			return None
		}
		let (_,mut end) = self.select_lines_down(offset);
		end = end.min(self.byte_len().saturating_sub(1));
		let start = self.slice_to(end + 1).rfind('\n').unwrap_or(0);
		Some((start,end))
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
			let slice = self.slice_to(start - 1);
			if let Some(prev_newline) = slice.rfind('\n') {
				start = prev_newline;
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

		for _ in 0..=n {
			let next_ln_start = end + 1;
			if next_ln_start >= self.byte_len() {
				end = self.byte_len();
				break
			}
			if let Some(next_newline) = self.slice_from(next_ln_start).find('\n') {
				end += next_newline;
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
	pub fn eval_text_object(&self, obj: TextObj, bound: Bound) -> Option<Range<usize>> {
		flog!(DEBUG, obj);
		flog!(DEBUG, bound);
		match obj {
			TextObj::Word(word) => {
				match word {
					Word::Big => match bound {
						Bound::Inside => {
							let start = self.rfind(is_whitespace)
								.map(|pos| pos+1)
								.unwrap_or(0);
							let end = self.find(is_whitespace)
								.map(|pos| pos-1)
								.unwrap_or(self.byte_len());
							Some(start..end)
						}
						Bound::Around => {
							let start = self.rfind(is_whitespace)
								.map(|pos| pos+1)
								.unwrap_or(0);
							let mut end = self.find(is_whitespace)
								.unwrap_or(self.byte_len());
							if end != self.byte_len() {
								end = self.find_from(end,|c| !is_whitespace(c))
									.map(|pos| pos-1)
									.unwrap_or(self.byte_len())
							}
							Some(start..end)
						}
					}
					Word::Normal => match bound {
						Bound::Inside => {
							let cur_graph = self.grapheme_at_cursor()?;
							let start = self.rfind(|c| is_other_class(c, cur_graph))
								.map(|pos| pos+1)
								.unwrap_or(0);
							let end = self.find(|c| is_other_class(c, cur_graph))
								.map(|pos| pos-1)
								.unwrap_or(self.byte_len());
							Some(start..end)
						}
						Bound::Around => {
							let cur_graph = self.grapheme_at_cursor()?;
							let start = self.rfind(|c| is_other_class(c, cur_graph))
								.map(|pos| pos+1)
								.unwrap_or(0);
							let mut end = self.find(|c| is_other_class(c, cur_graph))
								.unwrap_or(self.byte_len());
							if end != self.byte_len() && self.is_whitespace(end) {
								end = self.find_from(end,|c| !is_whitespace(c))
									.map(|pos| pos-1)
									.unwrap_or(self.byte_len())
							} else {
								end -= 1;
							}
							Some(start..end)
						}
					}
				}
			}
			TextObj::Line => todo!(),
			TextObj::Sentence => todo!(),
			TextObj::Paragraph => todo!(),
			TextObj::DoubleQuote => todo!(),
			TextObj::SingleQuote => todo!(),
			TextObj::BacktickQuote => todo!(),
			TextObj::Paren => todo!(),
			TextObj::Bracket => todo!(),
			TextObj::Brace => todo!(),
			TextObj::Angle => todo!(),
			TextObj::Tag => todo!(),
			TextObj::Custom(_) => todo!(),
		}
	}
	pub fn get_screen_line_positions(&self) -> Vec<usize> {
		let (start,end) = self.this_line();
		let mut screen_starts = vec![start];
		let line = &self.buffer[start..end];
		let term_width = self.term_dims.1;
		let mut col = 1;
		if start == 0 {
			col = self.first_line_offset
		}

		for (byte, grapheme) in line.grapheme_indices(true) {
			let width = grapheme.width();
			if col + width > term_width {
				screen_starts.push(start + byte);
				col = width;
			} else {
				col += width;
			}
		}

		screen_starts
	}
	pub fn start_of_screen_line(&self) -> usize {
		let screen_starts = self.get_screen_line_positions();
		let mut screen_start = screen_starts[0];
		let start_of_logical_line = self.start_of_line();
		flog!(DEBUG,screen_starts);
		flog!(DEBUG,self.cursor);

		for (i,pos) in screen_starts.iter().enumerate() {
			if *pos > self.cursor {
				break
			} else {
				screen_start = screen_starts[i];
			}
		}
		if screen_start != start_of_logical_line {
			screen_start += 1; // FIXME: doesn't account for grapheme bounds
		}
		screen_start
	}
	pub fn this_screen_line(&self) -> (usize,usize) {
		let screen_starts = self.get_screen_line_positions();
		let mut screen_start = screen_starts[0];
		let mut screen_end = self.end_of_line().saturating_sub(1);
		let start_of_logical_line = self.start_of_line();
		flog!(DEBUG,screen_starts);
		flog!(DEBUG,self.cursor);

		for (i,pos) in screen_starts.iter().enumerate() {
			if *pos > self.cursor {
				screen_end = screen_starts[i].saturating_sub(1);
				break;
			} else {
				screen_start = screen_starts[i];
			}
		}
		if screen_start != start_of_logical_line {
			screen_start += 1; // FIXME: doesn't account for grapheme bounds
		}
		(screen_start,screen_end)
	}
	pub fn find_word_pos(&self, word: Word, to: To, dir: Direction) -> Option<usize> {
		// FIXME: This uses a lot of hardcoded +1/-1 offsets, but they need to account for grapheme boundaries
		let mut pos = self.cursor;
		match word {
			Word::Big => {
				match dir {
					Direction::Forward => {
						match to {
							To::Start => {
								if self.on_whitespace() {
									return self.find_from(pos, |c| CharClass::from(c) != CharClass::Whitespace)
								}
								if self.on_start_of_word(word) {
									pos += 1;
									if pos >= self.byte_len() {
										return Some(self.byte_len())
									}
								}
								let Some(ws_pos) = self.find_from(pos, |c| CharClass::from(c) == CharClass::Whitespace) else {
									return Some(self.byte_len())
								};
								let word_start = self.find_from(ws_pos, |c| CharClass::from(c) != CharClass::Whitespace)?;
								Some(word_start)
							}
							To::End => {
								if self.on_whitespace() {
									let Some(non_ws_pos) = self.find_from(pos, |c| CharClass::from(c) != CharClass::Whitespace) else {
										return Some(self.byte_len())
									};
									pos = non_ws_pos
								}
								match self.on_end_of_word(word) {
									true => {
										pos += 1;
										if pos >= self.byte_len() {
											return Some(self.byte_len())
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
								if self.on_whitespace() {
									let Some(non_ws_pos) = self.rfind_from(pos, |c| CharClass::from(c) != CharClass::Whitespace) else {
										return Some(0)
									};
									pos = non_ws_pos
								}
								match self.on_start_of_word(word) {
									true => {
										pos = pos.checked_sub(1)?;
										let Some(prev_word_end) = self.rfind_from(pos, |c| CharClass::from(c) != CharClass::Whitespace) else {
											return Some(0)
										};
										match self.rfind_from(prev_word_end, |c| CharClass::from(c) == CharClass::Whitespace) {
											Some(n) => Some(n + 1), // Land on char after whitespace
											None => Some(0) // Start of buffer
										}
									}
									false => {
										match self.rfind_from(pos, |c| CharClass::from(c) == CharClass::Whitespace) {
											Some(n) => Some(n + 1), // Land on char after whitespace
											None => Some(0) // Start of buffer
										}
									}
								}
							}
							To::End => {
								if self.on_whitespace() {
									return Some(self.rfind_from(pos, |c| CharClass::from(c) != CharClass::Whitespace).unwrap_or(0))
								}
								if self.on_end_of_word(word) {
									pos = pos.checked_sub(1)?;
								}
								let Some(last_ws) = self.rfind_from(pos, |c| CharClass::from(c) == CharClass::Whitespace) else {
									return Some(0)
								}; 
								let Some(prev_word_end) = self.rfind_from(last_ws, |c| CharClass::from(c) != CharClass::Whitespace) else {
									return Some(0)
								};
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
								if self.on_whitespace() {
									return Some(self.find_from(pos, |c| CharClass::from(c) != CharClass::Whitespace).unwrap_or(self.byte_len()))
								}
								if self.on_start_of_word(word) {
									let cur_char_class = CharClass::from(self.grapheme_at_cursor()?);
									pos += 1;
									if pos >= self.byte_len() {
										return Some(self.byte_len())
									}
									let next_char = self.grapheme_at(self.next_pos(1)?)?;
									let next_char_class = CharClass::from(next_char);
									if cur_char_class != next_char_class && next_char_class != CharClass::Whitespace {
										return Some(pos)
									}
								}
								let cur_graph = self.grapheme_at(pos)?;
								let Some(diff_class_pos) = self.find_from(pos, |c| is_other_class_or_ws(c, cur_graph)) else {
									return Some(self.byte_len())
								};
								if let CharClass::Whitespace = CharClass::from(self.grapheme_at(diff_class_pos)?) {
									let non_ws_pos = self.find_from(diff_class_pos, |c| CharClass::from(c) != CharClass::Whitespace)?;
									Some(non_ws_pos)
								} else {
									Some(diff_class_pos)
								}
							}
							To::End => {
								flog!(DEBUG,self.buffer);
								if self.on_whitespace() {
									let Some(non_ws_pos) = self.find_from(pos, |c| CharClass::from(c) != CharClass::Whitespace) else {
										return Some(self.byte_len())
									};
									pos = non_ws_pos
								}
								match self.on_end_of_word(word) {
									true => {
										flog!(DEBUG, "on end of word");
										let cur_char_class = CharClass::from(self.grapheme_at_cursor()?);
										pos += 1;
										if pos >= self.byte_len() {
											return Some(self.byte_len())
										}
										let next_char = self.grapheme_at(self.next_pos(1)?)?;
										let next_char_class = CharClass::from(next_char);
										if cur_char_class != next_char_class && next_char_class != CharClass::Whitespace {
											let Some(end_pos) = self.find_from(pos, |c| is_other_class_or_ws(c, next_char)) else {
												return Some(self.byte_len())
											};
											pos = end_pos.saturating_sub(1);
											return Some(pos)
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
										flog!(DEBUG, "not on end of word");
										let cur_graph = self.grapheme_at(pos)?;
										flog!(DEBUG,cur_graph);
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
								if self.on_whitespace() {
									pos = self.rfind_from(pos, |c| CharClass::from(c) != CharClass::Whitespace)?;
								}
								match self.on_start_of_word(word) {
									true => {
										pos = pos.checked_sub(1)?;
										let cur_char_class = CharClass::from(self.grapheme_at_cursor()?);
										let prev_char = self.grapheme_at(self.prev_pos(1)?)?;
										let prev_char_class = CharClass::from(prev_char);
										let is_diff_class = cur_char_class != prev_char_class && prev_char_class != CharClass::Whitespace;
										if is_diff_class && self.is_start_of_word(Word::Normal, self.prev_pos(1)?) {
												return Some(pos)
										}
										let prev_word_end = self.rfind_from(pos, |c| CharClass::from(c) != CharClass::Whitespace)?;
										let cur_graph = self.grapheme_at(prev_word_end)?;
										match self.rfind_from(prev_word_end, |c| is_other_class_or_ws(c, cur_graph)) {
											Some(n) => Some(n + 1), // Land on char after whitespace
											None => Some(0) // Start of buffer
										}
									}
									false => {
										let cur_graph = self.grapheme_at(pos)?;
										match self.rfind_from(pos, |c| is_other_class_or_ws(c, cur_graph)) {
											Some(n) => Some(n + 1), // Land on char after whitespace
											None => Some(0) // Start of buffer
										}
									}
								}
							}
							To::End => {
								if self.on_whitespace() {
									return Some(self.rfind_from(pos, |c| CharClass::from(c) != CharClass::Whitespace).unwrap_or(0))
								}
								if self.on_end_of_word(word) {
									pos = pos.checked_sub(1)?;
									let cur_char_class = CharClass::from(self.grapheme_at_cursor()?);
									let prev_char = self.grapheme_at(self.prev_pos(1)?)?;
									let prev_char_class = CharClass::from(prev_char);
									if cur_char_class != prev_char_class && prev_char_class != CharClass::Whitespace {
										return Some(pos)
									}
								}
								let cur_graph = self.grapheme_at(pos)?;
								let Some(diff_class_pos) = self.rfind_from(pos, |c|is_other_class_or_ws(c, cur_graph)) else {
									return Some(0)
								};
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
	pub fn eval_motion_with_hint(&mut self, motion: Motion) -> MotionKind {
		let Some(hint) = self.hint.as_ref() else {
			return MotionKind::Null
		};
		let buffer = self.buffer.clone();
		self.buffer.push_str(hint);
		let motion_eval = self.eval_motion(motion);
		self.buffer = buffer;
		motion_eval
	}
	pub fn eval_motion(&mut self, motion: Motion) -> MotionKind {
		flog!(DEBUG,self.buffer);
		flog!(DEBUG,motion);
		match motion {
			Motion::WholeLine => MotionKind::Line(0),
			Motion::TextObj(text_obj, bound) => {
				let Some(range) = self.eval_text_object(text_obj, bound) else {
					return MotionKind::Null
				};
				MotionKind::range(range)
			}
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
				match to {
					To::Start => MotionKind::To(pos),
					To::End => MotionKind::On(pos),
				}
			}
			Motion::CharSearch(direction, dest, ch) => {
				let ch = format!("{ch}");
				let saved_cursor = self.cursor;
				match direction {
					Direction::Forward => {
						if self.grapheme_at_cursor().is_some_and(|c| c == ch) {
							self.cursor_fwd(1);
						}
						let Some(pos) = self.find(|c| c == ch) else {
							self.cursor = saved_cursor;
							return MotionKind::Null
						};
						self.cursor = saved_cursor;
						match dest {
							Dest::On => MotionKind::On(pos),
							Dest::Before => MotionKind::Before(pos),
							Dest::After => todo!(),
						}
					}
					Direction::Backward => {
						if self.grapheme_at_cursor().is_some_and(|c| c == ch) {
							self.cursor_back(1);
						}
						let Some(pos) = self.rfind(|c| c == ch) else {
							self.cursor = saved_cursor;
							return MotionKind::Null
						};
						self.cursor = saved_cursor;
						match dest {
							Dest::On => MotionKind::On(pos),
							Dest::Before => MotionKind::Before(pos),
							Dest::After => todo!(),
						}
					}
				}

			}
			Motion::BackwardChar => MotionKind::Backward(1),
			Motion::ForwardChar => MotionKind::Forward(1),
			Motion::LineUp => MotionKind::Line(-1),
			Motion::LineDown => MotionKind::Line(1),
			Motion::ScreenLineUp => MotionKind::ScreenLine(-1),
			Motion::ScreenLineDown => MotionKind::ScreenLine(1),
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
			Motion::Range(start, end) => {
				let start = start.clamp(0, self.byte_len().saturating_sub(1));
				let end = end.clamp(0, self.byte_len().saturating_sub(1));
				MotionKind::range(mk_range(start, end))
			}
			Motion::EndOfLastWord => {
				let Some(search_start) = self.next_pos(1) else {
					return MotionKind::Null
				};
				let mut last_graph_pos = None;
				for (i,graph) in self.buffer[search_start..].grapheme_indices(true) {
					flog!(DEBUG, last_graph_pos);
					flog!(DEBUG, graph);
					if graph == "\n" && last_graph_pos.is_some() {
						return MotionKind::On(search_start + last_graph_pos.unwrap())
					} else if !is_whitespace(graph) {
						last_graph_pos = Some(i)
					}
				}
				flog!(DEBUG,self.byte_len());
				last_graph_pos
					.map(|pos| MotionKind::On(search_start + pos))
					.unwrap_or(MotionKind::Null)
			}
			Motion::BeginningOfScreenLine => {
				let screen_start = self.start_of_screen_line();
				MotionKind::On(screen_start) 
			}
			Motion::FirstGraphicalOnScreenLine => {
				let (start,end) = self.this_screen_line();
				flog!(DEBUG,start,end);
				let slice = &self.buffer[start..=end];
				for (i,grapheme) in slice.grapheme_indices(true) {
					if !is_whitespace(grapheme) {
						return MotionKind::On(start + i)
					}
				}
				MotionKind::On(start)
			}
			Motion::HalfOfScreen => todo!(),
			Motion::HalfOfScreenLineText => todo!(),
			Motion::Builder(_) => todo!(),
			Motion::RepeatMotion => todo!(),
			Motion::RepeatMotionRev => todo!(),
			Motion::Null => MotionKind::Null,
		}
	}
	pub fn calculate_display_offset(&self, n_lines: isize) -> Option<usize> {
		let (start,end) = self.this_line();
		let graphemes: Vec<(usize, usize, &str)> = self.buffer[start..end]
			.graphemes(true)
			.scan(start, |idx, g| {
				let current = *idx;
				*idx += g.len(); // Advance by number of bytes
				Some((g.width(), current, g))
			}).collect();

		let mut cursor_line_index = 0;
		let mut cursor_visual_col = 0;
		let mut screen_lines = vec![];
		let mut cur_line = vec![];
		let mut line_width = 0;

		for (width, byte_idx, grapheme) in graphemes {
			if byte_idx == self.cursor {
				// Save this to later find column
				cursor_line_index = screen_lines.len();
				cursor_visual_col = line_width;
			}

			let new_line_width = line_width + width;
			if new_line_width > self.term_dims.1 {
				screen_lines.push(std::mem::take(&mut cur_line));
				cur_line.push((width, byte_idx, grapheme));
				line_width = width;
			} else {
				cur_line.push((width, byte_idx, grapheme));
				line_width = new_line_width;
			}
		}

		if !cur_line.is_empty() {
			screen_lines.push(cur_line);
		}

		if screen_lines.len() == 1 {
			return None
		}

		let target_line_index = (cursor_line_index as isize + n_lines)
			.clamp(0, (screen_lines.len() - 1) as isize) as usize;

		let mut col = 0;
		for (width, byte_idx, _) in &screen_lines[target_line_index] {
			if col + width > cursor_visual_col {
				return Some(*byte_idx);
			}
			col += width;
		}

		// If you went past the end of the line
		screen_lines[target_line_index]
    .last()
    .map(|(_, byte_idx, _)| *byte_idx)
	}
	pub fn get_range_from_motion(&self, verb: &Verb, motion: &MotionKind) -> Option<Range<usize>> {
		let range = match motion {
			MotionKind::Forward(n) => {
				let pos = self.next_pos(*n)?;
				let range = self.cursor..pos;
				assert!(range.end <= self.byte_len());
				Some(range)
			}
			MotionKind::To(n) => {
				let range = mk_range(self.cursor, *n);
				assert!(range.end <= self.byte_len());
				Some(range)
			}
			MotionKind::On(n) => {
				let range = mk_range_inclusive(self.cursor, *n);
				Some(range)
			}
			MotionKind::Before(n) => {
				let n = match n.cmp(&self.cursor) {
					Ordering::Less => (n + 1).min(self.byte_len()),
					Ordering::Equal => n.saturating_sub(1),
					Ordering::Greater => *n
				};
				let range = mk_range_inclusive(n, self.cursor);
				Some(range)
			}
			MotionKind::Backward(n) => {
				let pos = self.prev_pos(*n)?;
				let range = pos..self.cursor;
				Some(range)
			}
			MotionKind::Range(range) => {
				Some(range.0..range.1)
			}
			MotionKind::Line(n) => {
				match n.cmp(&0) {
					Ordering::Less => {
						let (start,end) = self.select_lines_up(n.unsigned_abs());
						let mut range = match verb {
							Verb::Delete => mk_range_inclusive(start,end),
							_ => mk_range(start,end),
						};
						range = self.clamp_range(range);
						Some(range)
					}
					Ordering::Equal => {
						let (start,end) = self.this_line();
						let mut range = match verb {
							Verb::Delete => mk_range_inclusive(start,end),
							_ => mk_range(start,end),
						};
						range = self.clamp_range(range);
						Some(range)
					}
					Ordering::Greater => {
						let (start, mut end) = self.select_lines_down(*n as usize);
						end = (end + 1).min(self.byte_len() - 1);
						let mut range = match verb {
							Verb::Delete => mk_range_inclusive(start,end),
							_ => mk_range(start,end),
						};
						range = self.clamp_range(range);
						Some(range)
					}
				}
			}
			MotionKind::ToLine(n) => {
				let (start,end) = self.select_lines_to(*n);
				let range = match verb {
					Verb::Change => start..end,
					Verb::Delete => start..end.saturating_add(1),
					_ => unreachable!()
				};
				Some(range)
			}
			MotionKind::Null => None,
			MotionKind::ScreenLine(n) => {
				let pos = self.calculate_display_offset(*n)?;
				Some(mk_range(pos, self.cursor))
			}
		};
		range.map(|rng| self.clamp_range(rng))
	}
	pub fn indent_lines(&mut self, range: Range<usize>) {
		let (start,end) = (range.start,range.end);
		
		self.buffer.insert(start, '\t');

		let graphemes = self.buffer[start + 1..end].grapheme_indices(true);
		let mut tab_insert_indices = vec![];
		let mut next_is_tab_pos = false;
		for (i,g) in graphemes {
			if g == "\n" {
				next_is_tab_pos = true;
			} else if next_is_tab_pos {
				tab_insert_indices.push(start + i + 1);
				next_is_tab_pos = false;
			}
		}

		for i in tab_insert_indices {
			if i < self.byte_len() {
				self.buffer.insert(i, '\t');
			}
		}
	}
	pub fn dedent_lines(&mut self, range: Range<usize>) {

		todo!()
	}
	pub fn exec_verb(&mut self, verb: Verb, motion: MotionKind, register: RegisterName) -> ShResult<()> {
		match verb {
			Verb::Change |
			Verb::Delete => {
				let Some(mut range) = self.get_range_from_motion(&verb, &motion) else {
					return Ok(())
				};
				let restore_col = matches!(motion, MotionKind::Line(_)) && matches!(verb, Verb::Delete);
				if restore_col {
					self.saved_col = Some(self.cursor_column())
				}
				let deleted = self.buffer.drain(range.clone());
				register.write_to_register(deleted.collect());

				self.cursor = range.start;
				if restore_col {
					let saved = self.saved_col.unwrap();
					let line_start = self.this_line().0;

					self.cursor = line_start + saved;
				}
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
							self.cursor_back(1);
						}
					}
				}
			}
			Verb::VisualModeSelectLast => {
				if let Some(range) = self.last_selected_range.as_ref() {
					self.selected_range = Some(range.clone());
					let mode = self.select_mode.unwrap_or_default();
					self.cursor = match mode.anchor() {
						SelectionAnchor::Start => range.start,
						SelectionAnchor::End => range.end
					}
				}
			}
			Verb::SwapVisualAnchor => {
				if let Some(range) = self.selected_range() {
					if let Some(mut mode) = self.select_mode {
						mode.invert_anchor();
						self.cursor = match mode.anchor() {
							SelectionAnchor::Start => range.start,
							SelectionAnchor::End => range.end,
						};
						self.select_mode = Some(mode);
					}
				}
			}
			Verb::Yank => {
				let Some(range) = self.get_range_from_motion(&verb, &motion) else {
					return Ok(())
				};
				let yanked = &self.buffer[range.clone()];
				register.write_to_register(yanked.to_string());
				self.cursor = range.start;
			}
			Verb::ReplaceChar(c) => {
				let Some(range) = self.get_range_from_motion(&verb, &motion) else {
					return Ok(())
				};
				let delta = range.end - range.start;
				let new_range = format!("{c}").repeat(delta);
				let cursor_pos = range.end;
				self.buffer.replace_range(range, &new_range);
				self.cursor = cursor_pos
			}
			Verb::Substitute => todo!(),
			Verb::ToLower => {
				let Some(range) = self.get_range_from_motion(&verb, &motion) else {
					return Ok(())
				};
				let mut new_range = String::new();
				let slice = &self.buffer[range.clone()];
				for ch in slice.chars() {
					if ch.is_ascii_uppercase() {
						new_range.push(ch.to_ascii_lowercase())
					} else {
						new_range.push(ch)
					}
				}
				self.buffer.replace_range(range.clone(), &new_range);
				self.cursor = range.end;
			}
			Verb::ToUpper => {
				let Some(range) = self.get_range_from_motion(&verb, &motion) else {
					return Ok(())
				};
				let mut new_range = String::new();
				let slice = &self.buffer[range.clone()];
				for ch in slice.chars() {
					if ch.is_ascii_lowercase() {
						new_range.push(ch.to_ascii_uppercase())
					} else {
						new_range.push(ch)
					}
				}
				self.buffer.replace_range(range.clone(), &new_range);
				self.cursor = range.end;
			}
			Verb::ToggleCase => {
				let Some(range) = self.get_range_from_motion(&verb, &motion) else {
					return Ok(())
				};
				let mut new_range = String::new();
				let slice = &self.buffer[range.clone()];
				for ch in slice.chars() {
					if ch.is_ascii_lowercase() {
						new_range.push(ch.to_ascii_uppercase())
					} else if ch.is_ascii_uppercase() {
						new_range.push(ch.to_ascii_lowercase())
					} else {
						new_range.push(ch)
					}
				}
				self.buffer.replace_range(range.clone(), &new_range);
				self.cursor = range.end;
			}
			Verb::Complete => todo!(),
			Verb::CompleteBackward => todo!(),
			Verb::Undo => {
				let Some(undo) = self.undo_stack.pop() else {
					return Ok(())
				};
				let Edit { pos, cursor_pos, old, new, .. } = undo;
				let range = pos..pos + new.len();
				self.buffer.replace_range(range, &old);
				let redo_cursor_pos = self.cursor;
				if self.move_cursor_on_undo {
					self.cursor = cursor_pos;
				}
				let redo = Edit { pos, cursor_pos: redo_cursor_pos, old: new, new: old, merging: false };
				self.redo_stack.push(redo);
			}
			Verb::Redo => {
				let Some(redo) = self.redo_stack.pop() else {
					return Ok(())
				};
				let Edit { pos, cursor_pos, old, new, .. } = redo;
				let range = pos..pos + new.len();
				self.buffer.replace_range(range, &old);
				let undo_cursor_pos = self.cursor;
				if self.move_cursor_on_undo {
					self.cursor = cursor_pos;
				}
				let undo = Edit { pos, cursor_pos: undo_cursor_pos, old: new, new: old, merging: false };
				self.undo_stack.push(undo);
			}
			Verb::RepeatLast => todo!(),
			Verb::Put(anchor) => {
				let Some(register_content) = register.read_from_register() else {
					return Ok(())
				};
				match anchor {
					Anchor::After => {
						for ch in register_content.chars() {
							self.cursor_fwd(1); // Only difference is which one you start with
							self.insert(ch);
						}
					}
					Anchor::Before => {
						for ch in register_content.chars() {
							self.insert(ch);
							self.cursor_fwd(1);
						}
					}
				}
			}
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
			Verb::JoinLines => {
				let (start,end) = self.this_line();
				let Some((nstart,nend)) = self.next_line(1) else {
					return Ok(())
				};
				let line = &self.buffer[start..end];
				let next_line = &self.buffer[nstart..nend].trim_start().to_string(); // strip leading whitespace
				let replace_newline_with_space = !line.ends_with([' ', '\t']);
				self.cursor = end;
				if replace_newline_with_space {
					self.buffer.replace_range(end..end+1, " ");
					self.buffer.replace_range(end+1..nend, next_line);
				} else {
					self.buffer.replace_range(end..end+1, "");
					self.buffer.replace_range(end..nend, next_line);
				}
			}
			Verb::InsertChar(ch) => {
				self.insert(ch);
				self.apply_motion(/*forced*/ true, motion);
			}
			Verb::Insert(str) => {
				for ch in str.chars() {
					self.insert(ch);
					self.cursor_fwd(1);
				}
			}
			Verb::Breakline(anchor) => todo!(),
			Verb::Indent => {
				let Some(range) = self.get_range_from_motion(&verb, &motion) else {
					return Ok(())
				};
				self.indent_lines(range)
			}
			Verb::Dedent => {
				let Some(range) = self.get_range_from_motion(&verb, &motion) else {
					return Ok(())
				};
				self.dedent_lines(range)
			}
			Verb::Rot13 => {
				let Some(range) = self.get_range_from_motion(&verb, &motion) else {
					return Ok(())
				};
				let slice = &self.buffer[range.clone()];
				let rot13 = rot13(slice);
				self.buffer.replace_range(range, &rot13);
			}
			Verb::Equalize => todo!(), // I fear this one
			Verb::Builder(verb_builder) => todo!(),
			Verb::EndOfFile => {
				if !self.buffer.is_empty() {
					self.cursor = 0;
					self.buffer.clear();
				} else {
					sh_quit(0)
				}
			}

			Verb::AcceptLine |
			Verb::ReplaceMode |
			Verb::InsertMode |
			Verb::NormalMode |
			Verb::VisualModeLine |
			Verb::VisualModeBlock |
			Verb::VisualMode => {
				/* Already handled */ 
				self.apply_motion(/*forced*/ true,motion);
			}
		}
		Ok(())
	}
	pub fn apply_motion(&mut self, forced: bool, motion: MotionKind) {

		match motion {
			MotionKind::Forward(n) => {
				for _ in 0..n {
					if forced {
						if !self.cursor_fwd(1) {
							break
						}
					} else if !self.cursor_fwd_confined(1) {
						break
					}
				}
			}
			MotionKind::Backward(n) => {
				for _ in 0..n {
					if forced {
						if !self.cursor_back(1) {
							break
						}
					} else if !self.cursor_back_confined(1) {
						break
					}
				}
			}
			MotionKind::To(n) |
			MotionKind::On(n) => {
				if n > self.byte_len() {
					self.cursor = self.byte_len();
				} else {
					self.cursor = n
				}
			}
			MotionKind::Before(n) => {
				if n > self.byte_len() {
					self.cursor = self.byte_len();
				} else {
					match n.cmp(&self.cursor) {
						Ordering::Less => {
							let n = (n + 1).min(self.byte_len());
							self.cursor = n
						}
						Ordering::Equal => {
							self.cursor = n
						}
						Ordering::Greater => {
							let n = n.saturating_sub(1);
							self.cursor = n
						}
					}
				}
			}
			MotionKind::Range(range) => {
				assert!((0..self.byte_len()).contains(&range.0));
				if self.cursor != range.0 {
					self.cursor = range.0
				}
			}
			MotionKind::Line(n) => {
				match n.cmp(&0) {
					Ordering::Equal => (),
					Ordering::Less => {
						for _ in 0..n.unsigned_abs() {
							let Some(pos) = self.find_prev_line_pos() else {
								return
							};
							self.cursor = pos;
						}
					}
					Ordering::Greater => {
						for _ in 0..n.unsigned_abs() {
							let Some(pos) = self.find_next_line_pos() else {
								return
							};
							self.cursor = pos;
						}
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
			MotionKind::ScreenLine(n) => {
				let Some(pos) = self.calculate_display_offset(n) else {
					return
				};
				self.cursor = pos;
			}
		}
		if let Some(mut mode) = self.select_mode {
			let Some(range) = self.selected_range.clone() else {
				return
			};
			let (mut start,mut end) = (range.start,range.end);
			match mode {
				SelectionMode::Char(anchor) => {
					match anchor {
						SelectionAnchor::Start => {
							start = self.cursor;
						}
						SelectionAnchor::End => {
							end = self.cursor;
						}
					}
				}
				SelectionMode::Line(anchor) => todo!(),
				SelectionMode::Block(anchor) => todo!(),
			}
			if start >= end {
				mode.invert_anchor();
				std::mem::swap(&mut start, &mut end);

				self.select_mode = Some(mode);
			}
			self.selected_range = Some(start..end);
		}
	}
	pub fn edit_is_merging(&self) -> bool {
		self.undo_stack.last().is_some_and(|edit| edit.merging)
	}
	pub fn handle_edit(&mut self, old: String, new: String, curs_pos: usize) {
		if self.edit_is_merging() {
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
	pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
		let clear_redos = !cmd.is_undo_op() || cmd.verb.as_ref().is_some_and(|v| v.1.is_edit());
		let is_char_insert = cmd.verb.as_ref().is_some_and(|v| v.1.is_char_insert());
		let is_line_motion = cmd.is_line_motion();
		let is_undo_op = cmd.is_undo_op();

		// Merge character inserts into one edit
		if self.edit_is_merging() && cmd.verb.as_ref().is_none_or(|v| !v.1.is_char_insert()) {
			if let Some(edit) = self.undo_stack.last_mut() {
				edit.stop_merge();
			}
		}

		let ViCmd { register, verb, motion, .. } = cmd;

		let verb_count = verb.as_ref().map(|v| v.0);
		let motion_count = motion.as_ref().map(|m| m.0);

		let before = self.buffer.clone();
		let cursor_pos = self.cursor;

		for _ in 0..verb_count.unwrap_or(1) {
			for _ in 0..motion_count.unwrap_or(1) {
				let motion_eval = motion
					.clone()
					.map(|m| self.eval_motion(m.1))
					.unwrap_or({
						self.selected_range
							.clone()
							.map(MotionKind::range)
							.unwrap_or(MotionKind::Null)
					});

				if let Some(verb) = verb.clone() {
					self.exec_verb(verb.1, motion_eval, register)?;
				} else if self.has_hint() {
					let motion_eval = motion
						.clone()
						.map(|m| self.eval_motion_with_hint(m.1))
						.unwrap_or(MotionKind::Null);
					self.apply_motion_with_hint(motion_eval);
				} else {
					self.apply_motion(/*forced*/ false,motion_eval);
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

		if !is_line_motion {
			self.saved_col = None;
		}

		if is_char_insert {
			if let Some(edit) = self.undo_stack.last_mut() {
				edit.start_merge();
			}
		}


		if self.clamp_cursor {
			self.clamp_cursor();
		}
		self.sync_cursor();
		Ok(())
	}
}

impl Display for LineBuf {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let mut full_buf = self.buffer.clone();
		if let Some(range) = self.selected_range.clone() {
			let mode = self.select_mode.unwrap_or_default();
			match mode.anchor() {
				SelectionAnchor::Start => {
					let mut inclusive = range.start..=range.end;
					if *inclusive.end() == self.byte_len() {
						inclusive = range.start..=range.end.saturating_sub(1);
					}
					let selected = full_buf[inclusive.clone()].styled(Style::BgWhite | Style::Black);
					full_buf.replace_range(inclusive, &selected);
				}
				SelectionAnchor::End => {
					let selected = full_buf[range.clone()].styled(Style::BgWhite | Style::Black);
					full_buf.replace_range(range, &selected);
				}
			}
		}
		if let Some(hint) = self.hint.as_ref() {
			full_buf.push_str(&hint.styled(Style::BrightBlack));
		}
		write!(f,"{}",full_buf)
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

pub fn rot13(input: &str) -> String {
	input.chars()
		.map(|c| {
			if c.is_ascii_lowercase() {
				let offset = b'a';
				(((c as u8 - offset + 13) % 26) + offset) as char
			} else if c.is_ascii_uppercase() {
				let offset = b'A';
				(((c as u8 - offset + 13) % 26) + offset) as char
			} else {
				c
			}
		})
		.collect()
}

pub fn is_grapheme_boundary(s: &str, pos: usize) -> bool {
	s.is_char_boundary(pos) && s.grapheme_indices(true).any(|(i,_)| i == pos)
}

fn mk_range_inclusive(a: usize, b: usize) -> Range<usize> {
	let b = b + 1;
	std::cmp::min(a, b)..std::cmp::max(a, b)
}

fn mk_range(a: usize, b: usize) -> Range<usize> {
    std::cmp::min(a, b)..std::cmp::max(a, b)
}
