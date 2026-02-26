use std::{
  fmt::Display,
  ops::{Range, RangeInclusive},
};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::vicmd::{
  Anchor, Bound, CmdFlags, Dest, Direction, Motion, MotionCmd, RegisterName, TextObj, To, Verb,
  ViCmd, Word,
};
use crate::{
  libsh::{
    error::ShResult,
    term::{Style, Styled},
  }, parse::lex::{LexFlags, LexStream, Tk, TkFlags, TkRule}, prelude::*, readline::{markers, register::{write_register, RegisterContent}}, state::read_shopts
};

const PUNCTUATION: [&str; 3] = ["?", "!", "."];

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Delim {
  Paren,
  Brace,
  Bracket,
  Angle,
}

#[derive(Default, PartialEq, Eq, Debug, Clone, Copy)]
pub enum CharClass {
  #[default]
  Alphanum,
  Symbol,
  Whitespace,
  Other,
}

impl From<&str> for CharClass {
  fn from(value: &str) -> Self {
    let mut chars = value.chars();

    // Empty string fallback
    let Some(first) = chars.next() else {
      return Self::Other;
    };

    if first.is_alphanumeric()
      && chars.all(|c| c.is_ascii_punctuation() || c == '\u{0301}' || c == '\u{0308}')
    {
      // Handles things like `ï`, `é`, etc., by manually allowing common diacritics
      return CharClass::Alphanum;
    }

    if value.chars().all(|c| c.is_alphanumeric() || c == '_') {
      CharClass::Alphanum
    } else if value.chars().all(char::is_whitespace) {
      CharClass::Whitespace
    } else if value.chars().all(|c| !c.is_alphanumeric()) {
      CharClass::Symbol
    } else {
      CharClass::Other
    }
  }
}

impl From<char> for CharClass {
  fn from(value: char) -> Self {
    let mut buf = [0u8; 4]; // max UTF-8 char size
    let slice = value.encode_utf8(&mut buf); // get str slice
    CharClass::from(slice as &str)
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

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectAnchor {
  #[default]
  End,
  Start,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
  Char(SelectAnchor),
  Line(SelectAnchor),
  Block(SelectAnchor),
}

impl SelectMode {
  pub fn anchor(&self) -> &SelectAnchor {
    match self {
      SelectMode::Char(anchor) | SelectMode::Line(anchor) | SelectMode::Block(anchor) => anchor,
    }
  }
  pub fn invert_anchor(&mut self) {
    match self {
      SelectMode::Char(anchor) | SelectMode::Line(anchor) | SelectMode::Block(anchor) => {
        *anchor = match anchor {
          SelectAnchor::Start => SelectAnchor::End,
          SelectAnchor::End => SelectAnchor::Start,
        }
      }
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionKind {
  To(usize), // Absolute position, exclusive
  On(usize), // Absolute position, inclusive
  Onto(usize), /* Absolute position, operations include the position but motions
              * exclude it (wtf vim) */
  Inclusive((usize, usize)), // Range, inclusive
  Exclusive((usize, usize)), // Range, exclusive

  // Used for linewise operations like 'dj', left is the selected range, right is the cursor's new
  // position on the line
  InclusiveWithTargetCol((usize, usize), usize),
  ExclusiveWithTargetCol((usize, usize), usize),
  Null,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionRange {}

impl MotionKind {
  pub fn inclusive(range: RangeInclusive<usize>) -> Self {
    Self::Inclusive((*range.start(), *range.end()))
  }
  pub fn exclusive(range: Range<usize>) -> Self {
    Self::Exclusive((range.start, range.end))
  }
}

#[derive(Default, Debug)]
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

    // Slice off the prefix and suffix for both (safe because start/end are byte
    // offsets)
    let old = a[start..end_a].to_string();
    let new = b[start..end_b].to_string();

    Edit {
      pos: start,
      cursor_pos: old_cursor_pos,
      old,
      new,
      merging: false,
    }
  }
  pub fn start_merge(&mut self) {
    self.merging = true
  }
  pub fn stop_merge(&mut self) {
    self.merging = false
  }
  pub fn is_empty(&self) -> bool {
    self.new.is_empty() && self.old.is_empty()
  }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
/// A usize which will always exist between 0 and a given upper bound
///
/// * The upper bound can be both inclusive and exclusive
/// * Used for the LineBuf cursor to enforce the `0 <= cursor <
///   self.buffer.len()` invariant.
pub struct ClampedUsize {
  value: usize,
  max: usize,
  exclusive: bool,
}

impl ClampedUsize {
  pub fn new(value: usize, max: usize, exclusive: bool) -> Self {
    let mut c = Self {
      value: 0,
      max,
      exclusive,
    };
    c.set(value);
    c
  }
  pub fn get(self) -> usize {
    self.value
  }
  pub fn cap(&self) -> usize {
    self.max
  }
  pub fn upper_bound(&self) -> usize {
    if self.exclusive {
      self.max.saturating_sub(1)
    } else {
      self.max
    }
  }
  /// Increment the ClampedUsize value
  ///
  /// Returns false if the attempted increment is rejected by the clamp
  pub fn inc(&mut self) -> bool {
    let max = self.upper_bound();
    if self.value == max {
      return false;
    }
    self.add(1);
    true
  }
  /// Decrement the ClampedUsize value
  ///
  /// Returns false if the attempted decrement would cause underflow
  pub fn dec(&mut self) -> bool {
    if self.value == 0 {
      return false;
    }
    self.sub(1);
    true
  }
  pub fn set(&mut self, value: usize) {
    let max = self.upper_bound();
    self.value = value.clamp(0, max);
  }
  pub fn set_max(&mut self, max: usize) {
    self.max = max;
    self.set(self.get()); // Enforces the new maximum
  }
  pub fn add(&mut self, value: usize) {
    let max = self.upper_bound();
    self.value = (self.value + value).clamp(0, max)
  }
  pub fn sub(&mut self, value: usize) {
    self.value = self.value.saturating_sub(value)
  }
  /// Add a value to the wrapped usize, return the result
  ///
  /// Returns the result instead of mutating the inner value
  pub fn ret_add(&self, value: usize) -> usize {
    let max = self.upper_bound();
    (self.value + value).clamp(0, max)
  }
  /// Add a value to the wrapped usize, forcing inclusivity
  pub fn ret_add_inclusive(&self, value: usize) -> usize {
    let max = self.max;
    (self.value + value).clamp(0, max)
  }
  /// Subtract a value from the wrapped usize, return the result
  ///
  /// Returns the result instead of mutating the inner value
  pub fn ret_sub(&self, value: usize) -> usize {
    self.value.saturating_sub(value)
  }
}

#[derive(Default, Debug)]
pub struct LineBuf {
  pub buffer: String,
  pub hint: Option<String>,
  pub grapheme_indices: Option<Vec<usize>>, // Used to slice the buffer
  pub cursor: ClampedUsize,                 // Used to index grapheme_indices

  pub select_mode: Option<SelectMode>,
  pub select_range: Option<(usize, usize)>,
  pub last_selection: Option<(usize, usize)>,

  pub insert_mode_start_pos: Option<usize>,
  pub saved_col: Option<usize>,
	pub auto_indent_level: usize,

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
  pub fn take_buf(&mut self) -> String {
    std::mem::take(&mut self.buffer)
  }
  pub fn has_hint(&self) -> bool {
    self.hint.is_some()
  }
  pub fn hint(&self) -> Option<&String> {
    self.hint.as_ref()
  }
  pub fn set_hint(&mut self, hint: Option<String>) {
    if let Some(hint) = hint {
      if let Some(hint) = hint.strip_prefix(&self.buffer) {
        if !hint.is_empty() {
          self.hint = Some(hint.to_string())
        } else {
          self.hint = None
        }
      }
    } else {
      self.hint = None
    }
  }
  pub fn accept_hint(&mut self) {
    let Some(hint) = self.hint.take() else { return };

    let old = self.buffer.clone();
    let cursor_pos = self.cursor.get();
    self.push_str(&hint);
    let new = self.as_str();
    let edit = Edit::diff(&old, new, cursor_pos);
    self.undo_stack.push(edit);
    self.cursor.add(hint.len());
  }
  pub fn set_cursor_clamp(&mut self, yn: bool) {
    self.cursor.exclusive = yn;
  }
	pub fn move_cursor_to_end(&mut self) {
		self.move_cursor(MotionKind::To(self.grapheme_indices().len()))
	}
	pub fn move_cursor_to_start(&mut self) {
		self.move_cursor(MotionKind::To(0))
	}
  pub fn cursor_byte_pos(&mut self) -> usize {
    self.index_byte_pos(self.cursor.get())
  }
  pub fn index_byte_pos(&mut self, index: usize) -> usize {
    self.update_graphemes_lazy();
    self
      .grapheme_indices()
      .get(index)
      .copied()
      .unwrap_or(self.buffer.len())
  }
  pub fn read_idx_byte_pos(&self, index: usize) -> usize {
    self
      .grapheme_indices()
      .get(index)
      .copied()
      .unwrap_or(self.buffer.len())
  }
  /// Update self.grapheme_indices with the indices of the current buffer
  pub fn update_graphemes(&mut self) {
    let indices: Vec<_> = self.buffer.grapheme_indices(true).map(|(i, _)| i).collect();
    self.cursor.set_max(indices.len());
    self.grapheme_indices = Some(indices)
  }
  pub fn grapheme_indices(&self) -> &[usize] {
    self.grapheme_indices.as_ref().unwrap()
  }
  pub fn grapheme_indices_owned(&self) -> Vec<usize> {
    self.grapheme_indices.as_ref().cloned().unwrap_or_default()
  }
  pub fn grapheme_is_escaped(&mut self, pos: usize) -> bool {
    let mut pos = ClampedUsize::new(pos, self.cursor.max, false);
    let mut escaped = false;

    while pos.dec() {
      let Some(gr) = self.grapheme_at(pos.get()) else {
        return escaped;
      };
      if gr == "\\" {
        escaped = !escaped;
      } else {
        return escaped;
      }
    }

    escaped
  }
  /// Does not update graphemes
  /// Useful in cases where you have to check many graphemes at once
  /// And don't want to trigger any mutable borrowing issues
  pub fn read_grapheme_at(&self, pos: usize) -> Option<&str> {
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
  pub fn read_grapheme_before(&self, pos: usize) -> Option<&str> {
    if pos == 0 {
      return None;
    }
    let pos = ClampedUsize::new(pos, self.cursor.max, false);
    self.read_grapheme_at(pos.ret_sub(1))
  }
  pub fn grapheme_before(&mut self, pos: usize) -> Option<&str> {
    if pos == 0 {
      return None;
    }
    let pos = ClampedUsize::new(pos, self.cursor.max, false);
    self.grapheme_at(pos.ret_sub(1))
  }
  pub fn read_grapheme_after(&self, pos: usize) -> Option<&str> {
    if pos == self.cursor.max {
      return None;
    }
    let pos = ClampedUsize::new(pos, self.cursor.max, false);
    self.read_grapheme_at(pos.ret_add(1))
  }
  pub fn grapheme_after(&mut self, pos: usize) -> Option<&str> {
    if pos == self.cursor.max {
      return None;
    }
    let pos = ClampedUsize::new(pos, self.cursor.max, false);
    self.grapheme_at(pos.ret_add(1))
  }
  pub fn grapheme_at_cursor(&mut self) -> Option<&str> {
    self.grapheme_at(self.cursor.get())
  }
	pub fn grapheme_before_cursor(&mut self) -> Option<&str> {
		if self.cursor.get() == 0 {
			return None;
		}
		self.grapheme_at(self.cursor.ret_sub(1))
	}
  pub fn mark_insert_mode_start_pos(&mut self) {
    self.insert_mode_start_pos = Some(self.cursor.get())
  }
  pub fn clear_insert_mode_start_pos(&mut self) {
    self.insert_mode_start_pos = None
  }
  pub fn slice(&mut self, range: Range<usize>) -> Option<&str> {
    self.update_graphemes_lazy();
    let start_index = self.grapheme_indices().get(range.start).copied()?;
    let end_index = self
      .grapheme_indices()
      .get(range.end)
      .copied()
      .or_else(|| {
        if range.end == self.grapheme_indices().len() {
          Some(self.buffer.len())
        } else {
          None
        }
      })?;
    self.buffer.get(start_index..end_index)
  }
  pub fn slice_inclusive(&mut self, range: RangeInclusive<usize>) -> Option<&str> {
    self.update_graphemes_lazy();
    let start_index = self.grapheme_indices().get(*range.start()).copied()?;
    let end_index = self
      .grapheme_indices()
      .get(*range.end())
      .copied()
      .or_else(|| {
        if *range.end() == self.grapheme_indices().len() {
          Some(self.buffer.len())
        } else {
          None
        }
      })?;
    self.buffer.get(start_index..end_index)
  }
  pub fn slice_to(&mut self, end: usize) -> Option<&str> {
    self.update_graphemes_lazy();
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
  pub fn slice_to_cursor_inclusive(&mut self) -> Option<&str> {
    self.slice_to(self.cursor.ret_add(1))
  }
  pub fn slice_from_cursor(&mut self) -> Option<&str> {
    self.slice_from(self.cursor.get())
  }
  pub fn remove(&mut self, pos: usize) {
    let idx = self.index_byte_pos(pos);
    if idx >= self.buffer.len() {
      return;
    }
    self.buffer.remove(idx);
    self.update_graphemes();
  }
  pub fn drain(&mut self, range: Range<usize>) -> String {
    let start = range.start.max(0);
    let end = range.end.min(self.grapheme_indices().len());
    let drained = if end == self.grapheme_indices().len() {
      if start == self.grapheme_indices().len() {
        return String::new();
      }
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
	pub fn drain_inclusive(&mut self, range: RangeInclusive<usize>) -> String {
		self.drain(*range.start()..range.end().saturating_add(1))
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
    self.insert_at(self.cursor.get(), ch);
  }
  pub fn insert_at(&mut self, pos: usize, ch: char) {
    let pos = self.index_byte_pos(pos);
    self.buffer.insert(pos, ch);
    self.update_graphemes();
  }
  pub fn set_buffer(&mut self, buffer: String) {
    self.buffer = buffer;
    self.update_graphemes();
  }
  pub fn select_range(&self) -> Option<(usize, usize)> {
    self.select_range
  }
  pub fn start_selecting(&mut self, mode: SelectMode) {
    self.select_mode = Some(mode);
    let range_start = self.cursor;
    let mut range_end = self.cursor;
    range_end.add(1);
    self.select_range = Some((range_start.get(), range_end.get()));
  }
  pub fn stop_selecting(&mut self) {
    self.select_mode = None;
    if self.select_range.is_some() {
      self.last_selection = self.select_range.take();
    }
  }
  pub fn total_lines(&mut self) -> usize {
    self.buffer.graphemes(true).filter(|g| *g == "\n").count()
  }
  pub fn cursor_line_number(&mut self) -> usize {
    self
      .slice_to_cursor()
      .map(|slice| slice.graphemes(true).filter(|g| *g == "\n").count())
      .unwrap_or(0)
  }
  pub fn is_sentence_punctuation(&self, pos: usize) -> bool {
    self.next_sentence_start_from_punctuation(pos).is_some()
  }

  /// If position is at sentence-ending punctuation, returns the position of the
  /// next sentence start. Handles closing delimiters (`)`, `]`, `"`, `'`)
  /// after punctuation.
  #[allow(clippy::collapsible_if)]
  pub fn next_sentence_start_from_punctuation(&self, pos: usize) -> Option<usize> {
    if let Some(gr) = self.read_grapheme_at(pos) {
      if PUNCTUATION.contains(&gr) && self.read_grapheme_after(pos).is_some() {
        let mut fwd_indices = (pos + 1..self.cursor.max).peekable();
        // Skip any closing delimiters after the punctuation
        if self
          .read_grapheme_after(pos)
          .is_some_and(|gr| [")", "]", "\"", "'"].contains(&gr))
        {
          while let Some(idx) = fwd_indices.peek() {
            if self
              .read_grapheme_at(*idx)
              .is_some_and(|gr| [")", "]", "\"", "'"].contains(&gr))
            {
              fwd_indices.next();
            } else {
              break;
            }
          }
        }
        // Now we should be at whitespace - skip it to find sentence start
        if let Some(idx) = fwd_indices.next() {
          if let Some(gr) = self.read_grapheme_at(idx) {
            if is_whitespace(gr) {
              if gr == "\n" {
                return Some(idx);
              }
              // Skip remaining whitespace to find actual sentence start
              while let Some(idx) = fwd_indices.next() {
                if let Some(gr) = self.read_grapheme_at(idx) {
                  if is_whitespace(gr) {
                    if gr == "\n" {
                      return Some(idx);
                    }
                    continue;
                  } else {
                    return Some(idx);
                  }
                }
              }
            }
          }
        }
      }
    }
    None
  }
  pub fn is_sentence_start(&mut self, pos: usize) -> bool {
    if self.grapheme_before(pos).is_some_and(is_whitespace) {
      let pos = pos.saturating_sub(1);
      let mut bkwd_indices = (0..pos).rev().peekable();
      while let Some(idx) = bkwd_indices.next() {
        let Some(gr) = self.read_grapheme_at(idx) else {
          break;
        };
        if [")", "]", "\"", "'"].contains(&gr) {
          while let Some(idx) = bkwd_indices.peek() {
            let Some(gr) = self.read_grapheme_at(*idx) else {
              break;
            };
            if [")", "]", "\"", "'"].contains(&gr) {
              bkwd_indices.next();
            } else {
              break;
            }
          }
        }
        if !is_whitespace(gr) {
          if [".", "?", "!"].contains(&gr) {
            return true;
          } else {
            break;
          }
        }
      }
    }
    false
  }
  pub fn nth_next_line(&mut self, n: usize) -> Option<(usize, usize)> {
    let line_no = self.cursor_line_number() + n;
    if line_no > self.total_lines() {
      return None;
    }
    Some(self.line_bounds(line_no))
  }
  pub fn nth_prev_line(&mut self, n: usize) -> Option<(usize, usize)> {
    let cursor_line_no = self.cursor_line_number();
    if cursor_line_no == 0 {
      return None;
    }
    let line_no = cursor_line_no.saturating_sub(n);
    if line_no > self.total_lines() {
      return None;
    }
    Some(self.line_bounds(line_no))
  }
  pub fn this_line(&mut self) -> (usize, usize) {
    let line_no = self.cursor_line_number();
    self.line_bounds(line_no)
  }
  pub fn start_of_line(&mut self) -> usize {
    self.this_line().0
  }
  pub fn end_of_line(&mut self) -> usize {
    self.this_line().1
  }
  pub fn select_lines_up(&mut self, n: usize) -> Option<(usize, usize)> {
    if self.start_of_line() == 0 {
      return None;
    }
    let target_line = self.cursor_line_number().saturating_sub(n);
    let end = self.end_of_line();
    let (start, _) = self.line_bounds(target_line);

    Some((start, end))
  }
  pub fn select_lines_down(&mut self, n: usize) -> Option<(usize, usize)> {
    if self.end_of_line() == self.cursor.max {
      return None;
    }
    let target_line = self.cursor_line_number() + n;
    let start = self.start_of_line();
    let (_, end) = self.line_bounds(target_line);

    Some((start, end))
  }
  pub fn line_bounds(&mut self, n: usize) -> (usize, usize) {
    if n > self.total_lines() {
      panic!(
        "Attempted to find line {n} when there are only {} lines",
        self.total_lines()
      )
    }

    let mut grapheme_index = 0;
    let mut start = 0;

    // Fine the start of the line
    for _ in 0..n {
      for (_, g) in self.buffer.grapheme_indices(true).skip(grapheme_index) {
        grapheme_index += 1;
        if g == "\n" {
          start = grapheme_index;
          break;
        }
      }
    }

    let mut end = start;
    // Find the end of the line
    for (_, g) in self.buffer.grapheme_indices(true).skip(start) {
      end += 1;
      if g == "\n" {
        break;
      }
    }

    (start, end)
  }
  pub fn handle_edit(&mut self, old: String, new: String, curs_pos: usize) {
    let edit_is_merging = self.undo_stack.last().is_some_and(|edit| edit.merging);
    if edit_is_merging {
      let diff = Edit::diff(&old, &new, curs_pos);
      if diff.is_empty() {
        return;
      }
      let Some(mut edit) = self.undo_stack.pop() else {
        self.undo_stack.push(diff);
        return;
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
  pub fn directional_indices_iter_from(
    &mut self,
    pos: usize,
    dir: Direction,
  ) -> Box<dyn Iterator<Item = usize>> {
    self.update_graphemes_lazy();
    let skip = pos + 1;
    match dir {
      Direction::Forward => Box::new(self.grapheme_indices().to_vec().into_iter().skip(skip))
        as Box<dyn Iterator<Item = usize>>,
      Direction::Backward => Box::new(self.grapheme_indices().to_vec().into_iter().take(pos).rev())
        as Box<dyn Iterator<Item = usize>>,
    }
  }
  pub fn is_word_bound(&mut self, pos: usize, word: Word, dir: Direction) -> bool {
    let clamped_pos = ClampedUsize::new(pos, self.cursor.max, true);
    let cur_char = self
      .grapheme_at(clamped_pos.get())
      .map(|c| c.to_string())
      .unwrap();
    let other_pos = match dir {
      Direction::Forward => clamped_pos.ret_add(1),
      Direction::Backward => clamped_pos.ret_sub(1),
    };
    if other_pos == clamped_pos.get() {
      return true;
    }

    let other_char = self.grapheme_at(other_pos).unwrap();
    match word {
      Word::Big => is_whitespace(other_char),
      Word::Normal => is_other_class_or_is_ws(other_char, &cur_char),
    }
  }
  pub fn dispatch_text_obj(&mut self, count: usize, text_obj: TextObj) -> Option<(usize, usize)> {
    match text_obj {
      // Text groups
      TextObj::Word(word, bound) => self.text_obj_word(count, bound, word),
      TextObj::Sentence(dir) => {
        let (start, end) = self.text_obj_sentence(self.cursor.get(), count, Bound::Around)?;
        let cursor = self.cursor.get();
        match dir {
          Direction::Forward => Some((cursor, end)),
          Direction::Backward => Some((start, cursor)),
        }
      }
      TextObj::Paragraph(dir) => {
        let (start, end) = self.text_obj_paragraph(count, Bound::Around)?;
        let cursor = self.cursor.get();
        match dir {
          Direction::Forward => Some((cursor, end)),
          Direction::Backward => Some((start, cursor)),
        }
      }
      TextObj::WholeSentence(bound) => self.text_obj_sentence(self.cursor.get(), count, bound),
      TextObj::WholeParagraph(bound) => self.text_obj_paragraph(count, bound),

      // Quoted blocks
      TextObj::DoubleQuote(bound) | TextObj::SingleQuote(bound) | TextObj::BacktickQuote(bound) => {
        self.text_obj_quote(count, text_obj, bound)
      }

      // Delimited blocks
      TextObj::Paren(bound)
      | TextObj::Bracket(bound)
      | TextObj::Brace(bound)
      | TextObj::Angle(bound) => self.text_obj_delim(count, text_obj, bound),

      // Other stuff
      TextObj::Tag(bound) => todo!(),
      TextObj::Custom(_) => todo!(),
    }
  }
  pub fn text_obj_word(
    &mut self,
    count: usize,
    bound: Bound,
    word: Word,
  ) -> Option<(usize, usize)> {
    match bound {
      Bound::Inside => {
        let start = if self.is_word_bound(self.cursor.get(), word, Direction::Backward) {
          self.cursor.get()
        } else {
          self.start_of_word_backward(self.cursor.get(), word)
        };
        let end = self.dispatch_word_motion(count, To::Start, word, Direction::Forward, true);
        Some((start, end))
      }
      Bound::Around => {
        let start = if self.is_word_bound(self.cursor.get(), word, Direction::Backward) {
          self.cursor.get()
        } else {
          self.start_of_word_backward(self.cursor.get(), word)
        };
        let end = self.dispatch_word_motion(count, To::Start, word, Direction::Forward, false);
        Some((start, end))
      }
    }
  }
  pub fn text_obj_sentence(
    &mut self,
    start_pos: usize,
    count: usize,
    bound: Bound,
  ) -> Option<(usize, usize)> {
    let mut start = None;
    let mut end = None;
    let mut fwd_indices = (start_pos..self.cursor.max).peekable();
    while let Some(idx) = fwd_indices.next() {
      if self.grapheme_at(idx).is_none() {
        break;
      }

      if let Some(next_sentence_start) = self.next_sentence_start_from_punctuation(idx) {
        match bound {
          Bound::Inside => {
            end = Some(idx);
            break;
          }
          Bound::Around => {
            end = Some(next_sentence_start);
            break;
          }
        }
      }
    }
    let mut end = end.unwrap_or(self.cursor.max);

    let mut bkwd_indices = (0..end).rev();
    while let Some(idx) = bkwd_indices.next() {
      if self.is_sentence_start(idx) {
        start = Some(idx);
        break;
      }
    }
    let start = start.unwrap_or(0);

    if count > 1
      && let Some((_, new_end)) = self.text_obj_sentence(end, count - 1, bound)
    {
      end = new_end;
    }

    Some((start, end))
  }
  pub fn text_obj_paragraph(&mut self, count: usize, bound: Bound) -> Option<(usize, usize)> {
    todo!()
  }
  pub fn text_obj_delim(
    &mut self,
    count: usize,
    text_obj: TextObj,
    bound: Bound,
  ) -> Option<(usize, usize)> {
    let mut backward_indices = (0..self.cursor.get()).rev();
    let (opener, closer) = match text_obj {
      TextObj::Paren(_) => ("(", ")"),
      TextObj::Bracket(_) => ("[", "]"),
      TextObj::Brace(_) => ("{", "}"),
      TextObj::Angle(_) => ("<", ">"),
      _ => unreachable!(),
    };

    let mut start_pos = None;
    let mut closer_count: u32 = 0;
    while let Some(idx) = backward_indices.next() {
      let gr = self.grapheme_at(idx)?.to_string();
      if (gr != closer && gr != opener) || self.grapheme_is_escaped(idx) {
        continue;
      }

      if gr == closer {
        closer_count += 1;
      } else if closer_count == 0 {
        start_pos = Some(idx);
        break;
      } else {
        closer_count = closer_count.saturating_sub(1)
      }
    }

    let (mut start, mut end) = if let Some(pos) = start_pos {
      let start = pos;
      let mut forward_indices = start + 1..self.cursor.max;
      let mut end = None;
      let mut opener_count: u32 = 0;

      while let Some(idx) = forward_indices.next() {
        if self.grapheme_is_escaped(idx) {
          continue;
        }
        match self.grapheme_at(idx)? {
          gr if gr == opener => opener_count += 1,
          gr if gr == closer => {
            if opener_count == 0 {
              end = Some(idx);
              break;
            } else {
              opener_count = opener_count.saturating_sub(1);
            }
          }
          _ => { /* Continue */ }
        }
      }

      (start, end?)
    } else {
      let mut forward_indices = self.cursor.get()..self.cursor.max;
      let mut start = None;
      let mut end = None;
      let mut opener_count: u32 = 0;

      while let Some(idx) = forward_indices.next() {
        if self.grapheme_is_escaped(idx) {
          continue;
        }
        match self.grapheme_at(idx)? {
          gr if gr == opener => {
            if opener_count == 0 {
              start = Some(idx);
            }
            opener_count += 1;
          }
          gr if gr == closer => {
            if opener_count == 1 {
              end = Some(idx);
              break;
            } else {
              opener_count = opener_count.saturating_sub(1)
            }
          }
          _ => { /* Continue */ }
        }
      }

      (start?, end?)
    };

    match bound {
      Bound::Inside => {
        // Start includes the quote, so push it forward
        start += 1;
      }
      Bound::Around => {
        // End excludes the quote, so push it forward
        end += 1;

        // We also need to include any trailing whitespace
        let end_of_line = self.end_of_line();
        let remainder = end..end_of_line;
        for idx in remainder {
          let Some(gr) = self.grapheme_at(idx) else {
            break;
          };
          if is_whitespace(gr) {
            end += 1;
          } else {
            break;
          }
        }
      }
    }

    Some((start, end))
  }
  pub fn text_obj_quote(
    &mut self,
    count: usize,
    text_obj: TextObj,
    bound: Bound,
  ) -> Option<(usize, usize)> {
    let (start, end) = self.this_line(); // Only operates on the current line

    // Get the grapheme indices backward from the cursor
    let mut backward_indices = (start..self.cursor.get()).rev();
    let target = match text_obj {
      TextObj::DoubleQuote(_) => "\"",
      TextObj::SingleQuote(_) => "'",
      TextObj::BacktickQuote(_) => "`",
      _ => unreachable!(),
    };
    let mut start_pos = None;
    while let Some(idx) = backward_indices.next() {
      match self.grapheme_at(idx)? {
        gr if gr == target => {
          // We are going backwards, so we need to handle escapes differently
          // These things were not meant to be read backwards, so it's a little fucked up
          let mut escaped = false;
          while let Some(idx) = backward_indices.next() {
            // Keep consuming indices as long as they refer to a backslash
            let Some("\\") = self.grapheme_at(idx) else {
              break;
            };
            // On each backslash, flip this boolean
            escaped = !escaped
          }

          // If there are an even number of backslashes, we are not escaped
          // Therefore, we have found the start position
          if !escaped {
            start_pos = Some(idx);
            break;
          }
        }
        _ => { /* Continue */ }
      }
    }

    // Try to find a quote backwards
    let (mut start, mut end) = if let Some(pos) = start_pos {
      // Found one, only one more to go
      let start = pos;
      let mut forward_indices = start + 1..end;
      let mut end = None;

      while let Some(idx) = forward_indices.next() {
        match self.grapheme_at(idx)? {
          "\\" => {
            forward_indices.next();
          }
          gr if gr == target => {
            end = Some(idx);
            break;
          }
          _ => { /* Continue */ }
        }
      }
      let end = end?;

      (start, end)
    } else {
      // Did not find one, have two find two of them forward now
      let mut forward_indices = self.cursor.get()..end;
      let mut start = None;
      let mut end = None;

      while let Some(idx) = forward_indices.next() {
        match self.grapheme_at(idx)? {
          "\\" => {
            forward_indices.next();
          }
          gr if gr == target => {
            start = Some(idx);
            break;
          }
          _ => { /* Continue */ }
        }
      }
      let start = start?;

      while let Some(idx) = forward_indices.next() {
        match self.grapheme_at(idx)? {
          "\\" => {
            forward_indices.next();
          }
          gr if gr == target => {
            end = Some(idx);
            break;
          }
          _ => { /* Continue */ }
        }
      }
      let end = end?;

      (start, end)
    };

    match bound {
      Bound::Inside => {
        // Start includes the quote, so push it forward
        start += 1;
      }
      Bound::Around => {
        // End excludes the quote, so push it forward
        end += 1;

        // We also need to include any trailing whitespace
        let end_of_line = self.end_of_line();
        let remainder = end..end_of_line;
        for idx in remainder {
          let Some(gr) = self.grapheme_at(idx) else {
            break;
          };
          if is_whitespace(gr) {
            end += 1;
          } else {
            break;
          }
        }
      }
    }

    Some((start, end))
  }
  pub fn find_next_matching_delim(&mut self) -> Option<usize> {
    let delims = ["[", "]", "{", "}", "(", ")", "<", ">"];
    let mut fwd_indices = self.cursor.get()..self.cursor.max;
    let idx = fwd_indices.find(|idx| {
      self
        .grapheme_at(*idx)
        .is_some_and(|gr| delims.contains(&gr))
    })?;
    let search_direction = match self.grapheme_at(idx)? {
      "[" | "{" | "(" | "<" => Direction::Forward,
      "]" | "}" | ")" | ">" => Direction::Backward,
      _ => unreachable!(),
    };
    let target_delim = match self.grapheme_at(idx)? {
      "[" => "]",
      "]" => "[",
      "{" => "}",
      "}" => "{",
      "(" => ")",
      ")" => "(",
      "<" => ">",
      ">" => "<",
      _ => unreachable!(),
    };

    match search_direction {
      Direction::Forward => {
        let mut fwd_indices = idx..self.cursor_max();
        fwd_indices.find(|idx| {
          self.grapheme_at(*idx).is_some_and(|gr| gr == target_delim)
            && !self.grapheme_is_escaped(*idx)
        })
      }
      Direction::Backward => {
        let mut bkwd_indices = 0..idx;
        bkwd_indices.find(|idx| {
          self.grapheme_at(*idx).is_some_and(|gr| gr == target_delim)
            && !self.grapheme_is_escaped(*idx)
        })
      }
    }
  }
  pub fn find_unmatched_delim(&mut self, delim: Delim, dir: Direction) -> Option<usize> {
    let (opener, closer) = match delim {
      Delim::Paren => ("(", ")"),
      Delim::Brace => ("{", "}"),
      Delim::Bracket => ("[", "]"),
      Delim::Angle => ("<", ">"),
    };
    match dir {
      Direction::Forward => {
        let mut fwd_indices = self.cursor.get()..self.cursor.max;
        let mut depth = 0;

        while let Some(idx) = fwd_indices.next() {
          if self.grapheme_is_escaped(idx) {
            continue;
          }
          let gr = self.grapheme_at(idx)?;
          match gr {
            _ if gr == opener => depth += 1,
            _ if gr == closer => {
              if depth == 0 {
                return Some(idx);
              } else {
                depth -= 1;
              }
            }
            _ => { /* Continue */ }
          }
        }

        None
      }
      Direction::Backward => {
        let mut bkwd_indices = (0..self.cursor.get()).rev();
        let mut depth = 0;

        while let Some(idx) = bkwd_indices.next() {
          if self.grapheme_is_escaped(idx) {
            continue;
          }
          let gr = self.grapheme_at(idx)?;
          match gr {
            _ if gr == closer => depth += 1,
            _ if gr == opener => {
              if depth == 0 {
                return Some(idx);
              } else {
                depth -= 1;
              }
            }
            _ => { /* Continue */ }
          }
        }

        None
      }
    }
  }
  pub fn dispatch_word_motion(
    &mut self,
    count: usize,
    to: To,
    word: Word,
    dir: Direction,
    include_last_char: bool,
  ) -> usize {
    let mut pos = ClampedUsize::new(self.cursor.get(), self.cursor.max, false);
    for i in 0..count {
      // We alter 'include_last_char' to only be true on the last iteration
      // Therefore, '5cw' will find the correct range for the first four and stop on
      // the end of the fifth word
      let include_last_char_and_is_last_word = include_last_char && i == count.saturating_sub(1);
      pos.set(match to {
        To::Start => {
          match dir {
            Direction::Forward => {
              self.start_of_word_forward(pos.get(), word, include_last_char_and_is_last_word)
            }
            Direction::Backward => 'backward: {
              // We also need to handle insert mode's Ctrl+W behaviors here
              let target = self.start_of_word_backward(pos.get(), word);

              // Check to see if we are in insert mode
              let Some(start_pos) = self.insert_mode_start_pos else {
                break 'backward target;
              };
              // If we are in front of start_pos, and we would cross start_pos to reach target
              // then stop at start_pos
              if start_pos > target && self.cursor.get() > start_pos {
                return start_pos;
              } else {
                // We are behind start_pos, now we just reset it
                if self.cursor.get() < start_pos {
                  self.clear_insert_mode_start_pos();
                }
                break 'backward target;
              }
            }
          }
        }
        To::End => match dir {
          Direction::Forward => self.end_of_word_forward(pos.get(), word),
          Direction::Backward => self.end_of_word_backward(pos.get(), word, false),
        },
      });
    }
    pos.get()
  }

  /// Find the start of the next word forward
  pub fn start_of_word_forward(
    &mut self,
    mut pos: usize,
    word: Word,
    include_last_char: bool,
  ) -> usize {
    let default = self.grapheme_indices().len();
    let mut indices_iter = (pos..self.cursor.max).peekable();

    match word {
      Word::Big => {
        let Some(next) = indices_iter.peek() else {
          return default;
        };
        let on_boundary = self.grapheme_at(*next).is_none_or(is_whitespace);
        if on_boundary {
          let Some(idx) = indices_iter.next() else {
            return default;
          };
          if include_last_char {
            return idx;
          } else {
            pos = idx;
          }
        }

        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let on_whitespace = is_whitespace(&cur_char);

        if !on_whitespace {
          let Some(ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace))
          else {
            return default;
          };
          if include_last_char {
            return ws_pos;
          }
        }

        indices_iter
          .find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c)))
          .unwrap_or(default)
      }
      Word::Normal => {
        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let Some(next_idx) = indices_iter.peek() else {
          return default;
        };
        let on_boundary = !is_whitespace(&cur_char)
          && self
            .grapheme_at(*next_idx)
            .is_none_or(|c| is_other_class_or_is_ws(c, &cur_char));
        if on_boundary {
          if include_last_char {
            return *next_idx;
          } else {
            pos = *next_idx;
          }
        }

        let Some(next_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        if is_other_class_not_ws(&cur_char, &next_char) {
          return pos;
        }
        let on_whitespace = is_whitespace(&cur_char);

        if !on_whitespace {
          let other_class_pos = indices_iter.find(|i| {
            self
              .grapheme_at(*i)
              .is_some_and(|c| is_other_class_or_is_ws(c, &next_char))
          });
          let Some(other_class_pos) = other_class_pos else {
            return default;
          };
          if self
            .grapheme_at(other_class_pos)
            .is_some_and(|c| !is_whitespace(c))
            || include_last_char
          {
            return other_class_pos;
          }
        }

        indices_iter
          .find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c)))
          .unwrap_or(default)
      }
    }
  }

  /// Find the end of the previous word backward
  pub fn end_of_word_backward(
    &mut self,
    mut pos: usize,
    word: Word,
    include_last_char: bool,
  ) -> usize {
    let default = self.grapheme_indices().len();
    let mut indices_iter = (0..pos).rev().peekable();

    match word {
      Word::Big => {
        let Some(next) = indices_iter.peek() else {
          return default;
        };
        let on_boundary = self.grapheme_at(*next).is_none_or(is_whitespace);
        if on_boundary {
          let Some(idx) = indices_iter.next() else {
            return default;
          };
          if include_last_char {
            return idx;
          } else {
            pos = idx;
          }
        }

        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let on_whitespace = is_whitespace(&cur_char);

        if !on_whitespace {
          let Some(ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace))
          else {
            return default;
          };
          if include_last_char {
            return ws_pos;
          }
        }

        indices_iter
          .find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c)))
          .unwrap_or(default)
      }
      Word::Normal => {
        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let Some(next_idx) = indices_iter.peek() else {
          return default;
        };
        let on_boundary = !is_whitespace(&cur_char)
          && self
            .grapheme_at(*next_idx)
            .is_none_or(|c| is_other_class_or_is_ws(c, &cur_char));
        if on_boundary {
          if include_last_char {
            return *next_idx;
          } else {
            pos = *next_idx;
          }
        }

        let Some(next_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        if is_other_class_not_ws(&cur_char, &next_char) {
          return pos;
        }
        let on_whitespace = is_whitespace(&cur_char);

        if !on_whitespace {
          let other_class_pos = indices_iter.find(|i| {
            self
              .grapheme_at(*i)
              .is_some_and(|c| is_other_class_or_is_ws(c, &next_char))
          });
          let Some(other_class_pos) = other_class_pos else {
            return default;
          };
          if self
            .grapheme_at(other_class_pos)
            .is_some_and(|c| !is_whitespace(c))
            || include_last_char
          {
            return other_class_pos;
          }
        }

        indices_iter
          .find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c)))
          .unwrap_or(default)
      }
    }
  }

  /// Find the end of the current/next word forward
  pub fn end_of_word_forward(&mut self, mut pos: usize, word: Word) -> usize {
    let default = self.cursor.max;
    if pos >= default {
      return default;
    }
    let mut fwd_indices = (pos + 1..default).peekable();

    match word {
      Word::Big => {
        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let Some(next_idx) = fwd_indices.peek() else {
          return default;
        };
        let on_boundary =
          !is_whitespace(&cur_char) && self.grapheme_at(*next_idx).is_none_or(is_whitespace);
        if on_boundary {
          let Some(idx) = fwd_indices.next() else {
            return default;
          };
          pos = idx;
        }

        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let on_whitespace = is_whitespace(&cur_char);

        if on_whitespace {
          let Some(_non_ws_pos) =
            fwd_indices.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c)))
          else {
            return default;
          };
        }

        let Some(next_ws_pos) =
          fwd_indices.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace))
        else {
          return default;
        };
        pos = next_ws_pos;

        if pos == self.grapheme_indices().len() {
          pos
        } else {
          pos.saturating_sub(1)
        }
      }
      Word::Normal => {
        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let Some(next_idx) = fwd_indices.peek() else {
          return default;
        };
        let on_boundary = !is_whitespace(&cur_char)
          && self
            .grapheme_at(*next_idx)
            .is_none_or(|c| is_other_class_or_is_ws(c, &cur_char));
        if on_boundary {
          let next_idx = fwd_indices.next().unwrap();
          pos = next_idx;
        }

        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let on_whitespace = is_whitespace(&cur_char);

        if on_whitespace {
          let Some(non_ws_pos) =
            fwd_indices.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c)))
          else {
            return default;
          };
          pos = non_ws_pos;
        }

        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return self.grapheme_indices().len();
        };
        let Some(next_ws_pos) = fwd_indices.find(|i| {
          self
            .grapheme_at(*i)
            .is_some_and(|c| is_other_class_or_is_ws(c, &cur_char))
        }) else {
          return default;
        };
        pos = next_ws_pos;

        if pos == self.grapheme_indices().len() {
          pos
        } else {
          pos.saturating_sub(1)
        }
      }
    }
  }

  /// Find the start of the current/previous word backward
  pub fn start_of_word_backward(&mut self, mut pos: usize, word: Word) -> usize {
    let default = 0;
    let mut indices_iter = (0..pos).rev().peekable();

    match word {
      Word::Big => {
        let on_boundary = 'bound_check: {
          let Some(next_idx) = indices_iter.peek() else {
            break 'bound_check false;
          };
          self.grapheme_at(*next_idx).is_none_or(is_whitespace)
        };
        if on_boundary {
          let Some(idx) = indices_iter.next() else {
            return default;
          };
          pos = idx;
        }

        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let on_whitespace = is_whitespace(&cur_char);

        if on_whitespace {
          let Some(_non_ws_pos) =
            indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c)))
          else {
            return default;
          };
        }

        let Some(next_ws_pos) =
          indices_iter.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace))
        else {
          return default;
        };
        pos = next_ws_pos;

        if pos == self.grapheme_indices().len() {
          pos
        } else {
          pos + 1
        }
      }
      Word::Normal => {
        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let on_boundary = 'bound_check: {
          let Some(next_idx) = indices_iter.peek() else {
            break 'bound_check false;
          };
          !is_whitespace(&cur_char)
            && self
              .grapheme_at(*next_idx)
              .is_some_and(|c| is_other_class_or_is_ws(c, &cur_char))
        };
        if on_boundary {
          let next_idx = indices_iter.next().unwrap();
          pos = next_idx;
        }

        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return default;
        };
        let on_whitespace = is_whitespace(&cur_char);

        if on_whitespace {
          let Some(non_ws_pos) =
            indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c)))
          else {
            return default;
          };
          pos = non_ws_pos;
        }

        let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
          return self.grapheme_indices().len();
        };
        let Some(next_ws_pos) = indices_iter.find(|i| {
          self
            .grapheme_at(*i)
            .is_some_and(|c| is_other_class_or_is_ws(c, &cur_char))
        }) else {
          return default;
        };
        pos = next_ws_pos;

        if pos == 0 { pos } else { pos + 1 }
      }
    }
  }

  fn grapheme_index_for_display_col(&self, line: &str, target_col: usize) -> usize {
    let mut col = 0;
    for (grapheme_index, g) in line.graphemes(true).enumerate() {
      if g == "\n" {
        if self.cursor.exclusive {
          return grapheme_index.saturating_sub(1);
        } else {
          return grapheme_index;
        }
      }
      let w = g.width();
      if col + w > target_col {
        return grapheme_index;
      }
      col += w;
    }
    // If we reach here, the target_col is past end of line
    line.graphemes(true).count()
  }
  pub fn cursor_max(&self) -> usize {
    self.cursor.max
  }
  pub fn cursor_at_max(&self) -> bool {
    self.cursor.get() == self.cursor.upper_bound()
  }
  pub fn cursor_col(&mut self) -> usize {
    let start = self.start_of_line();
    let end = self.cursor.get();
    let Some(slice) = self.slice_inclusive(start..=end) else {
      return start;
    };

    slice.graphemes(true).map(|g| g.width()).sum()
  }
  pub fn rfind_from<F: Fn(&str) -> bool>(&mut self, pos: usize, op: F) -> usize {
    let Some(slice) = self.slice_to(pos) else {
      return self.grapheme_indices().len();
    };
    for (offset, grapheme) in slice.grapheme_indices(true).rev() {
      if op(grapheme) {
        return pos + offset;
      }
    }
    self.grapheme_indices().len()
  }
  pub fn rfind_from_optional<F: Fn(&str) -> bool>(&mut self, pos: usize, op: F) -> Option<usize> {
    let slice = self.slice_to(pos)?;
    for (offset, grapheme) in slice.grapheme_indices(true).rev() {
      if op(grapheme) {
        return Some(pos + offset);
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
      return self.grapheme_indices().len();
    };
    for (offset, grapheme) in slice.grapheme_indices(true) {
      if op(grapheme) {
        return pos + offset;
      }
    }
    self.grapheme_indices().len()
  }
  pub fn find_from_optional<F: Fn(&str) -> bool>(&mut self, pos: usize, op: F) -> Option<usize> {
    let slice = self.slice_from(pos)?;
    for (offset, grapheme) in slice.grapheme_indices(true) {
      if op(grapheme) {
        return Some(pos + offset);
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
  pub fn insert_str_at(&mut self, pos: usize, new: &str) {
    let idx = self.index_byte_pos(pos);
    self.buffer.insert_str(idx, new);
    self.update_graphemes();
  }
  pub fn replace_at_cursor(&mut self, new: &str) {
    self.replace_at(self.cursor.get(), new);
  }
  pub fn force_replace_at(&mut self, pos: usize, new: &str) {
    let Some(gr) = self.grapheme_at(pos).map(|gr| gr.to_string()) else {
      self.buffer.push_str(new);
      return;
    };
    let start = self.index_byte_pos(pos);
    let end = start + gr.len();
    self.buffer.replace_range(start..end, new);
  }
  pub fn replace_at(&mut self, pos: usize, new: &str) {
    let Some(gr) = self.grapheme_at(pos).map(|gr| gr.to_string()) else {
      self.buffer.push_str(new);
      return;
    };
    if &gr == "\n" {
      // Do not replace the newline, push it forward instead
      let byte_pos = self.index_byte_pos(pos);
      self.buffer.insert_str(byte_pos, new);
      return;
    }
    let start = self.index_byte_pos(pos);
    let end = start + gr.len();
    self.buffer.replace_range(start..end, new);
  }
	pub fn calc_indent_level(&mut self) {
		let to_cursor = self
			.slice_to_cursor()
			.map(|s| s.to_string())
			.unwrap_or(self.buffer.clone());
		let input = Arc::new(to_cursor);
		let Ok(tokens) = LexStream::new(input, LexFlags::LEX_UNFINISHED).collect::<ShResult<Vec<Tk>>>() else {
			log::error!("Failed to lex buffer for indent calculation");
			return;
		};
		let mut level: usize = 0;
		for tk in tokens {
			if tk.flags.contains(TkFlags::KEYWORD) {
				match tk.as_str() {
					"then" | "do" => level += 1,
					"done" | "fi" | "esac" => level = level.saturating_sub(1),
					_ => { /* Continue */ }
				}
			} else if tk.class == TkRule::BraceGrpStart {
				level += 1;
			} else if tk.class == TkRule::BraceGrpEnd {
				level = level.saturating_sub(1);
			}
		}

		self.auto_indent_level = level;
	}
  pub fn eval_motion(&mut self, verb: Option<&Verb>, motion: MotionCmd) -> MotionKind {
    let buffer = self.buffer.clone();
    if self.has_hint() {
      let hint = self.hint.clone().unwrap();
      self.push_str(&hint);
    }

    let eval = match motion {
      MotionCmd(count, motion @ (Motion::WholeLineInclusive | Motion::WholeLineExclusive)) => {
				let exclusive = matches!(motion, Motion::WholeLineExclusive);

        let Some((start, mut end)) = (if count == 1 {
          Some(self.this_line())
        } else {
          self.select_lines_down(count)
        }) else {
          return MotionKind::Null;
        };

				if exclusive && self.grapheme_before(end).is_some_and(|gr| gr == "\n") {
					end = end.saturating_sub(1);
				}

        let target_col = if let Some(col) = self.saved_col {
          col
        } else {
          let col = self.cursor_col();
          self.saved_col = Some(col);
          col
        };

        let Some(line) = self.slice(start..end).map(|s| s.to_string()) else {
          return MotionKind::Null;
        };
        let mut target_pos = self.grapheme_index_for_display_col(&line, target_col);
        if self.cursor.exclusive
          && line.ends_with("\n")
          && self.grapheme_at(target_pos) == Some("\n")
          && line != "\n" // Allow landing on newline for empty lines
        {
          target_pos = target_pos.saturating_sub(1); // Don't land on the
          // newline
        }
        MotionKind::InclusiveWithTargetCol((start, end), target_pos)
      }
      MotionCmd(count, Motion::WordMotion(to, word, dir)) => {
        // 'cw' is a weird case
        // if you are on the word's left boundary, it will not delete whitespace after
        // the end of the word
        let include_last_char = verb == Some(&Verb::Change)
          && matches!(
            motion.1,
            Motion::WordMotion(To::Start, _, Direction::Forward)
          );

        let pos = self.dispatch_word_motion(count, to, word, dir, include_last_char);
        let pos = ClampedUsize::new(pos, self.cursor.max, false);
        // End-based operations must include the last character
        // But the cursor must also stop just before it when moving
        // So we have to do some weird shit to reconcile this behavior
        if to == To::End {
          match dir {
            Direction::Forward => MotionKind::Onto(pos.get()),
            Direction::Backward => {
              let (start, end) = ordered(self.cursor.get(), pos.get());
              MotionKind::Inclusive((start, end))
            }
          }
        } else {
          MotionKind::On(pos.get())
        }
      }
      MotionCmd(count, Motion::TextObj(text_obj)) => {
        let Some((start, end)) = self.dispatch_text_obj(count, text_obj.clone()) else {
          return MotionKind::Null;
        };
        match text_obj {
          TextObj::Sentence(dir) | TextObj::Paragraph(dir) => {
            match dir {
              Direction::Forward => MotionKind::On(end),
              Direction::Backward => {
                let cur_sentence_start = start;
                let mut start_pos = self.cursor.get();
                for _ in 0..count {
                  if self.is_sentence_start(start_pos) {
                    // We know there is some punctuation before us now
                    // Let's find it
                    let mut bkwd_indices = (0..start_pos).rev();
                    let punct_pos = bkwd_indices
                      .find(|idx| {
                        self
                          .grapheme_at(*idx)
                          .is_some_and(|gr| PUNCTUATION.contains(&gr))
                      })
                      .unwrap();
                    if self.grapheme_before(punct_pos).is_some() {
                      let Some((new_start, _)) =
                        self.text_obj_sentence(punct_pos - 1, count, Bound::Inside)
                      else {
                        return MotionKind::Null;
                      };
                      start_pos = new_start;
                      continue;
                    } else {
                      return MotionKind::Null;
                    }
                  } else {
                    start_pos = cur_sentence_start;
                  }
                }
                MotionKind::On(start_pos)
              }
            }
          }
          TextObj::Word(_, bound)
          | TextObj::WholeSentence(bound)
          | TextObj::WholeParagraph(bound) => match bound {
            Bound::Inside => MotionKind::Inclusive((start, end)),
            Bound::Around => MotionKind::Exclusive((start, end)),
          },
          TextObj::DoubleQuote(_)
          | TextObj::SingleQuote(_)
          | TextObj::BacktickQuote(_)
          | TextObj::Paren(_)
          | TextObj::Bracket(_)
          | TextObj::Brace(_)
          | TextObj::Angle(_) => MotionKind::Exclusive((start, end)),
          _ => todo!(),
        }
      }
      MotionCmd(_, Motion::ToDelimMatch) => {
        // Just ignoring the count here, it does some really weird stuff in Vim
        // try doing something like '5%' in vim, it is really strange
        let Some(pos) = self.find_next_matching_delim() else {
          return MotionKind::Null;
        };
        MotionKind::On(pos)
      }
      MotionCmd(_, Motion::ToBrace(direction))
      | MotionCmd(_, Motion::ToBracket(direction))
      | MotionCmd(_, Motion::ToParen(direction)) => {
        // Counts don't seem to do anything significant for these either
        let delim = match motion.1 {
          Motion::ToBrace(_) => Delim::Brace,
          Motion::ToBracket(_) => Delim::Bracket,
          Motion::ToParen(_) => Delim::Paren,
          _ => unreachable!(),
        };
        let Some(pos) = self.find_unmatched_delim(delim, direction) else {
          return MotionKind::Null;
        };

        MotionKind::On(pos)
      }
      MotionCmd(count, Motion::EndOfLastWord) => {
        let start = self.start_of_line();
        let mut newline_count = 0;
        let mut indices = self.directional_indices_iter_from(start, Direction::Forward);
        let mut last_graphical = None;
        while let Some(idx) = indices.next() {
          let grapheme = self.grapheme_at(idx).unwrap();
          if !is_whitespace(grapheme) {
            last_graphical = Some(idx);
          }
          if grapheme == "\n" {
            newline_count += 1;
            if newline_count == count {
              break;
            }
          }
        }
        let Some(last) = last_graphical else {
          return MotionKind::Null;
        };
        MotionKind::On(last)
      }
      MotionCmd(_, Motion::BeginningOfFirstWord) => {
        let start = self.start_of_line();
        self.update_graphemes_lazy();
        let indices = self.grapheme_indices().to_vec();
        let mut first_graphical = None;
        for &idx in indices.iter().skip(start) {
          let grapheme = self.grapheme_at(idx).unwrap();
          if !is_whitespace(grapheme) {
            first_graphical = Some(idx);
            break;
          }
          if grapheme == "\n" {
            break;
          }
        }
        let Some(first) = first_graphical else {
          return MotionKind::Null;
        };
        MotionKind::On(first)
      }
      MotionCmd(_, Motion::BeginningOfLine) => MotionKind::On(self.start_of_line()),
      MotionCmd(count, Motion::EndOfLine) => {
        let pos = if count == 1 {
					self.end_of_line()
        } else if let Some((_, end)) = self.select_lines_down(count) {
          end
        } else {
          self.end_of_line()
        };

        MotionKind::On(pos)
      }
      MotionCmd(count, Motion::CharSearch(direction, dest, ch)) => {
        let ch_str = &format!("{ch}");
        let mut pos = self.cursor;
        for _ in 0..count {
          let mut indices_iter = self.directional_indices_iter_from(pos.get(), direction);

          let Some(ch_pos) = indices_iter.position(|i| self.grapheme_at(i) == Some(ch_str)) else {
            return MotionKind::Null;
          };
          match direction {
            Direction::Forward => pos.add(ch_pos + 1),
            Direction::Backward => pos.sub(ch_pos + 1),
          }

          if dest == Dest::Before {
            match direction {
              Direction::Forward => pos.sub(1),
              Direction::Backward => pos.add(1),
            }
          }
        }
        MotionKind::Onto(pos.get())
      }
      MotionCmd(count, motion @ (Motion::ForwardChar | Motion::BackwardChar)) => {
        let mut target = self.cursor;
        target.exclusive = false;
        for _ in 0..count {
          match motion {
            Motion::BackwardChar => target.sub(1),
            Motion::ForwardChar => {
              if self.cursor.exclusive && self.grapheme_at(target.ret_add(1)) == Some("\n") {
                return MotionKind::Null;
              }
              target.add(1);
              continue;
            }
            _ => unreachable!(),
          }
          if self.grapheme_at(target.get()) == Some("\n") {
            return MotionKind::Null;
          }
        }
        MotionKind::On(target.get())
      }
      MotionCmd(count, Motion::ForwardCharForced) => MotionKind::On(self.cursor.ret_add(count)),
      MotionCmd(count, Motion::BackwardCharForced) => MotionKind::On(self.cursor.ret_sub(count)),
      MotionCmd(count, Motion::LineDown) | MotionCmd(count, Motion::LineUp) => {
        let Some((start, end)) = (match motion.1 {
          Motion::LineUp => self.nth_prev_line(count),
          Motion::LineDown => self.nth_next_line(count),
          _ => unreachable!(),
        }) else {
          return MotionKind::Null;
        };

        let target_col = if let Some(col) = self.saved_col {
          col
        } else {
          let col = self.cursor_col();
          self.saved_col = Some(col);
          col
        };

        let Some(line) = self.slice(start..end).map(|s| s.to_string()) else {
					log::warn!("Failed to get line slice for motion, start: {start}, end: {end}");
          return MotionKind::Null;
        };
        let mut target_pos = self.grapheme_index_for_display_col(&line, target_col);
        if self.cursor.exclusive
          && line.ends_with("\n")
          && self.grapheme_at(target_pos) == Some("\n")
          && line != "\n" // Allow landing on newline for empty lines
        {
          target_pos = target_pos.saturating_sub(1); // Don't land on the
          // newline
        }

        let (start, end) = match motion.1 {
          Motion::LineUp => (start, self.end_of_line()),
          Motion::LineDown => (self.start_of_line(), end),
          _ => unreachable!(),
        };


        MotionKind::InclusiveWithTargetCol((start, end), target_pos)
      }
      MotionCmd(count, Motion::LineDownCharwise) | MotionCmd(count, Motion::LineUpCharwise) => {
        let Some((start, end)) = (match motion.1 {
          Motion::LineUpCharwise => self.nth_prev_line(count),
          Motion::LineDownCharwise => self.nth_next_line(count),
          _ => unreachable!(),
        }) else {
          return MotionKind::Null;
        };

        let target_col = if let Some(col) = self.saved_col {
          col
        } else {
          let col = self.cursor_col();
          self.saved_col = Some(col);
          col
        };

        let Some(line) = self.slice(start..end).map(|s| s.to_string()) else {
          return MotionKind::Null;
        };
        let target_pos = start + self.grapheme_index_for_display_col(&line, target_col);

        MotionKind::On(target_pos)
      }
      MotionCmd(count, Motion::ScreenLineUp) => todo!(),
      MotionCmd(count, Motion::ScreenLineUpCharwise) => todo!(),
      MotionCmd(count, Motion::ScreenLineDown) => todo!(),
      MotionCmd(count, Motion::ScreenLineDownCharwise) => todo!(),
      MotionCmd(count, Motion::BeginningOfScreenLine) => todo!(),
      MotionCmd(count, Motion::FirstGraphicalOnScreenLine) => todo!(),
      MotionCmd(count, Motion::HalfOfScreen) => todo!(),
      MotionCmd(count, Motion::HalfOfScreenLineText) => todo!(),
      MotionCmd(_count, Motion::WholeBuffer) => {
        MotionKind::Exclusive((0, self.grapheme_indices().len()))
      }
      MotionCmd(_count, Motion::BeginningOfBuffer) => MotionKind::On(0),
      MotionCmd(_count, Motion::EndOfBuffer) => MotionKind::To(self.grapheme_indices().len()),
      MotionCmd(_count, Motion::ToColumn) => todo!(),
      MotionCmd(count, Motion::Range(start, end)) => {
        let mut final_end = end;
        if self.cursor.exclusive {
          final_end += 1;
        }
        let delta = end - start;
        let count = count.saturating_sub(1); // Becomes number of times to multiply the range

        for _ in 0..count {
          final_end += delta;
        }

        final_end = final_end.min(self.cursor.max);
        MotionKind::Exclusive((start, final_end))
      }
      MotionCmd(count, Motion::RepeatMotion) => todo!(),
      MotionCmd(count, Motion::RepeatMotionRev) => todo!(),
      MotionCmd(count, Motion::Null) => MotionKind::Null,
    };

    self.set_buffer(buffer);
    eval
  }
  pub fn apply_motion(&mut self, motion: MotionKind) {
    let last_grapheme_pos = self.grapheme_indices().len().saturating_sub(1);

    if self.has_hint() {
      let hint = self.hint.take().unwrap();
      let saved_buffer = self.buffer.clone(); // cringe
      self.push_str(&hint);
      self.move_cursor(motion);

      let has_consumed_hint = (self.cursor.exclusive && self.cursor.get() >= last_grapheme_pos)
        || (!self.cursor.exclusive && self.cursor.get() > last_grapheme_pos);

      if has_consumed_hint {
        let buf_end = if self.cursor.exclusive {
          self.cursor.ret_add_inclusive(1)
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
        let Some(old_hint) = self.slice_from(last_grapheme_pos + 1) else {
          self.set_buffer(format!("{saved_buffer}{hint}"));
          self.hint = None;
          return;
        };

        self.hint = Some(old_hint.to_string());
        self.set_buffer(old_buffer);
      }
    } else {
      self.move_cursor(motion);
    }
    self.update_graphemes();
    self.update_select_range();
  }
  pub fn update_select_range(&mut self) {
    if let Some(mut mode) = self.select_mode {
      let Some((mut start, mut end)) = self.select_range else {
        return;
      };
      match mode {
        SelectMode::Char(anchor) => match anchor {
          SelectAnchor::Start => {
            start = self.cursor.get();
          }
          SelectAnchor::End => {
            end = self.cursor.get();
          }
        },
        SelectMode::Line(anchor) => match anchor {
					SelectAnchor::Start => {
						start = self.start_of_line();
					}
					SelectAnchor::End => {
						end = self.end_of_line();
					}
				}
        SelectMode::Block(anchor) => todo!(),
      }
      if start >= end {
        mode.invert_anchor();
        std::mem::swap(&mut start, &mut end);

        self.select_mode = Some(mode);
      }
      self.select_range = Some((start, end));
    }
  }
  pub fn move_cursor(&mut self, motion: MotionKind) {
    match motion {
			MotionKind::Onto(pos) | // Onto follows On's behavior for cursor movements
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
			MotionKind::ExclusiveWithTargetCol((_,_),col) |
			MotionKind::InclusiveWithTargetCol((_,_),col) => {
				let (start,end) = self.this_line();
				let end = end.min(col);
				self.cursor.set(start + end)
			}
			MotionKind::Inclusive((start,end)) => {
				if self.select_range().is_none() {
					self.cursor.set(start)
				} else {
					self.cursor.set(end);
					self.select_mode = Some(SelectMode::Char(SelectAnchor::End));
					self.select_range = Some((start,end));
				}
			}
			MotionKind::Exclusive((start,end)) => {
				if self.select_range().is_none() {
					self.cursor.set(start)
				} else {
					let end = end.saturating_sub(1);
					self.cursor.set(end);
					self.select_mode = Some(SelectMode::Char(SelectAnchor::End));
					self.select_range = Some((start,end));
				}
			}
			MotionKind::Null => { /* Do nothing */ }
		}
  }
  pub fn range_from_motion(&mut self, motion: &MotionKind) -> Option<(usize, usize)> {
    let range = match motion {
      MotionKind::On(pos) => ordered(self.cursor.get(), *pos),
      MotionKind::Onto(pos) => {
        // For motions which include the character at the cursor during operations
        // but exclude the character during movements
        let mut pos = ClampedUsize::new(*pos, self.cursor.max, false);
        let mut cursor_pos = self.cursor;

        // The end of the range must be incremented by one
        match pos.get().cmp(&self.cursor.get()) {
          std::cmp::Ordering::Less => cursor_pos.add(1),
          std::cmp::Ordering::Greater => pos.add(1),
          std::cmp::Ordering::Equal => {}
        }
        ordered(cursor_pos.get(), pos.get())
      }
      MotionKind::To(pos) => {
        let pos = match pos.cmp(&self.cursor.get()) {
          std::cmp::Ordering::Less => *pos + 1,
          std::cmp::Ordering::Greater => *pos - 1,
          std::cmp::Ordering::Equal => *pos,
        };
        ordered(self.cursor.get(), pos)
      }
      MotionKind::InclusiveWithTargetCol((start, end), _) | MotionKind::Exclusive((start, end)) => {
        ordered(*start, *end)
      }
      MotionKind::ExclusiveWithTargetCol((start, end), _) | MotionKind::Inclusive((start, end)) => {
        let (start, mut end) = ordered(*start, *end);
        end = ClampedUsize::new(end, self.cursor.max, false).ret_add(1);
        (start, end)
      }
      MotionKind::Null => return None,
    };
    Some(range)
  }
  #[allow(clippy::unnecessary_to_owned)]
  pub fn exec_verb(
    &mut self,
    verb: Verb,
    motion: MotionKind,
    register: RegisterName,
  ) -> ShResult<()> {
    match verb {
      Verb::Delete | Verb::Yank | Verb::Change => {
        let Some((mut start, mut end)) = self.range_from_motion(&motion) else {
          return Ok(());
        };

				let mut do_indent = false;
				if verb == Verb::Change && (start,end) == self.this_line() {
					do_indent = read_shopts(|o| o.prompt.auto_indent);
				}

        let text = if verb == Verb::Yank {
          self
            .slice(start..end)
            .map(|c| c.to_string())
            .unwrap_or_default()
        } else {
          let drained = self.drain(start..end);
          self.update_graphemes();
          drained
        };
        let is_linewise = matches!(
          motion,
          MotionKind::InclusiveWithTargetCol(..) | MotionKind::ExclusiveWithTargetCol(..)
        );
        let register_content = if is_linewise {
          RegisterContent::Line(text)
        } else {
          RegisterContent::Span(text)
        };
        register.write_to_register(register_content);
				self.cursor.set(start);
				if do_indent {
					self.calc_indent_level();
					let tabs = (0..self.auto_indent_level).map(|_| '\t');
					for tab in tabs {
						self.insert_at_cursor(tab);
						self.cursor.add(1);
					}
				} else if verb != Verb::Change
				&& let MotionKind::InclusiveWithTargetCol((_,_), col) = motion {
					self.cursor.add(col);
				}
      }
      Verb::Rot13 => {
        let Some((start, end)) = self.range_from_motion(&motion) else {
          return Ok(());
        };
        let slice = self.slice(start..end).unwrap_or_default();
        let rot13 = rot13(slice);
        self.buffer.replace_range(start..end, &rot13);
        self.cursor.set(start);
      }
      Verb::ReplaceChar(ch) => {
        let mut buf = [0u8; 4];
        let new = ch.encode_utf8(&mut buf);
        self.replace_at_cursor(new);
        self.apply_motion(motion);
      }
      Verb::ReplaceCharInplace(ch, count) => {
        for i in 0..count {
          let mut buf = [0u8; 4];
          let new = ch.encode_utf8(&mut buf);
          self.replace_at_cursor(new);

          // try to increment the cursor until we are on the last iteration
          // or until we hit the end of the buffer
          if i != count.saturating_sub(1) && !self.cursor.inc() {
            break;
          }
        }
      }
      Verb::ToggleCaseInplace(count) => {
        for i in 0..count {
          let Some(gr) = self.grapheme_at_cursor() else {
            return Ok(());
          };
          if gr.len() > 1 || gr.is_empty() {
            return Ok(());
          }
          let ch = gr.chars().next().unwrap();
          if !ch.is_alphabetic() {
            return Ok(());
          }
          let mut buf = [0u8; 4];
          let new = if ch.is_ascii_lowercase() {
            ch.to_ascii_uppercase().encode_utf8(&mut buf)
          } else {
            ch.to_ascii_lowercase().encode_utf8(&mut buf)
          };
          self.replace_at_cursor(new);

          // try to increment the cursor until we are on the last iteration
          // or until we hit the end of the buffer
          if i != count.saturating_sub(1) && !self.cursor.inc() {
            break;
          }
        }
      }
      Verb::ToggleCaseRange => {
        let Some((start, end)) = self.range_from_motion(&motion) else {
          return Ok(());
        };
        for i in start..end {
          let Some(gr) = self.grapheme_at(i) else {
            continue;
          };
          if gr.len() > 1 || gr.is_empty() {
            continue;
          }
          let ch = gr.chars().next().unwrap();
          if !ch.is_alphabetic() {
            continue;
          }
          let mut buf = [0u8; 4];
          let new = if ch.is_ascii_lowercase() {
            ch.to_ascii_uppercase().encode_utf8(&mut buf)
          } else {
            ch.to_ascii_lowercase().encode_utf8(&mut buf)
          };
          self.replace_at(i, new);
        }
      }
      Verb::ToLower => {
        let Some((start, end)) = self.range_from_motion(&motion) else {
          return Ok(());
        };
        for i in start..end {
          let Some(gr) = self.grapheme_at(i) else {
            continue;
          };
          if gr.len() > 1 || gr.is_empty() {
            continue;
          }
          let ch = gr.chars().next().unwrap();
          if !ch.is_alphabetic() {
            continue;
          }
          let mut buf = [0u8; 4];
          let new = if ch.is_ascii_uppercase() {
            ch.to_ascii_lowercase().encode_utf8(&mut buf)
          } else {
            ch.encode_utf8(&mut buf)
          };
          self.replace_at(i, new);
        }
      }
      Verb::ToUpper => {
        let Some((start, end)) = self.range_from_motion(&motion) else {
          return Ok(());
        };
        for i in start..end {
          let Some(gr) = self.grapheme_at(i) else {
            continue;
          };
          if gr.len() > 1 || gr.is_empty() {
            continue;
          }
          let ch = gr.chars().next().unwrap();
          if !ch.is_alphabetic() {
            continue;
          }
          let mut buf = [0u8; 4];
          let new = if ch.is_ascii_lowercase() {
            ch.to_ascii_uppercase().encode_utf8(&mut buf)
          } else {
            ch.encode_utf8(&mut buf)
          };
          self.replace_at(i, new);
        }
      }
      Verb::Redo | Verb::Undo => {
        let (edit_provider, edit_receiver) = match verb {
          // Redo = pop from redo stack, push to undo stack
          Verb::Redo => (&mut self.redo_stack, &mut self.undo_stack),
          // Undo = pop from undo stack, push to redo stack
          Verb::Undo => (&mut self.undo_stack, &mut self.redo_stack),
          _ => unreachable!(),
        };
        let Some(edit) = edit_provider.pop() else {
          return Ok(());
        };
        let Edit {
          pos,
          cursor_pos,
          old,
          new,
          merging: _,
        } = edit;

        self.buffer.replace_range(pos..pos + new.len(), &old);
        let new_cursor_pos = self.cursor.get();

				self.cursor.set(cursor_pos);
        let new_edit = Edit {
          pos,
          cursor_pos: new_cursor_pos,
          old: new,
          new: old,
          merging: false,
        };
        edit_receiver.push(new_edit);
        self.update_graphemes();
      }
      Verb::RepeatLast => todo!(),
      Verb::Put(anchor) => {
        let Some(content) = register.read_from_register() else {
          return Ok(());
        };
        if content.is_empty() {
          return Ok(());
        }
				if let Some(range) = self.select_range {
					let register_text = self.drain_inclusive(range.0..=range.1);
					write_register(None, RegisterContent::Span(register_text)); // swap deleted text into register

					let text = content.as_str();
					self.insert_str_at(range.0, text);
					self.cursor.set(range.0 + content.char_count());
					self.select_range = None;
					self.update_graphemes();
					return Ok(());
				}
        match content {
          RegisterContent::Span(ref text) => {
            let insert_idx = match anchor {
              Anchor::After => self.cursor.ret_add(1),
              Anchor::Before => self.cursor.get(),
            };
            self.insert_str_at(insert_idx, text);
            self.cursor.add(text.len().saturating_sub(1));
          }
          RegisterContent::Line(ref text) => {
            let insert_idx = match anchor {
              Anchor::After => self.end_of_line(),
              Anchor::Before => self.start_of_line(),
            };
            let needs_newline = self.grapheme_before(insert_idx).is_some_and(|gr| gr != "\n");
            if needs_newline {
              let full = format!("\n{}", text);
              self.insert_str_at(insert_idx, &full);
              self.cursor.set(insert_idx + 1);
            } else {
              self.insert_str_at(insert_idx, text);
              self.cursor.set(insert_idx);
            }
          }
          RegisterContent::Empty => {}
        }
      }
      Verb::SwapVisualAnchor => {
        if let Some((start, end)) = self.select_range()
          && let Some(mut mode) = self.select_mode
        {
          mode.invert_anchor();
          let new_cursor_pos = match mode.anchor() {
            SelectAnchor::Start => start,
            SelectAnchor::End => end,
          };
          self.cursor.set(new_cursor_pos);
          self.select_mode = Some(mode)
        }
      }
      Verb::JoinLines => {
        let start = self.start_of_line();
        let Some((_, mut end)) = self.nth_next_line(1) else {
          return Ok(());
        };
        end = end.saturating_sub(1); // exclude the last newline
        let mut last_was_whitespace = false;
        for i in start..end {
          let Some(gr) = self.grapheme_at(i) else {
            continue;
          };
          if gr == "\n" {
            if last_was_whitespace {
              self.remove(i);
            } else {
              self.force_replace_at(i, " ");
            }
            last_was_whitespace = false;
            continue;
          }
          last_was_whitespace = is_whitespace(gr);
        }
      }
      Verb::InsertChar(ch) => {
        self.insert_at_cursor(ch);
        self.cursor.add(1);
      }
      Verb::Insert(string) => {
        self.push_str(&string);
        let graphemes = string.graphemes(true).count();
        self.cursor.add(graphemes);
      }
      Verb::Indent => {
        let Some((start, end)) = self.range_from_motion(&motion) else {
          return Ok(());
        };
				let move_cursor = self.cursor.get() == start;
        self.insert_at(start, '\t');
				if move_cursor {
					self.cursor.add(1);
				}
        let mut range_indices = self.grapheme_indices()[start..end].to_vec().into_iter();
        while let Some(idx) = range_indices.next() {
          let gr = self.grapheme_at(idx).unwrap();
          if gr == "\n" {
            let Some(idx) = range_indices.next() else {
              self.push('\t');
              break;
            };
            self.insert_at(idx, '\t');
          }
        }

        match motion {
          MotionKind::ExclusiveWithTargetCol((_, _), pos)
          | MotionKind::InclusiveWithTargetCol((_, _), pos) => {
            self.cursor.set(start);
            let end = self.end_of_line();
            self.cursor.add(end.min(pos));
          }
          _ => self.cursor.set(start),
        }
      }
      Verb::Dedent => {
        let Some((start, mut end)) = self.range_from_motion(&motion) else {
          return Ok(());
        };
        if self.grapheme_at(start) == Some("\t") {
          self.remove(start);
        }
				end = end.min(self.grapheme_indices().len().saturating_sub(1));
        let mut range_indices = self.grapheme_indices()[start..end].to_vec().into_iter();
        while let Some(idx) = range_indices.next() {
          let gr = self.grapheme_at(idx).unwrap();
          if gr == "\n" {
            let Some(idx) = range_indices.next() else {
              if self.grapheme_at(self.grapheme_indices().len().saturating_sub(1)) == Some("\t") {
                self.remove(self.grapheme_indices().len().saturating_sub(1));
              }
              break;
            };
            if self.grapheme_at(idx) == Some("\t") {
              self.remove(idx);
            }
          }
        }

        match motion {
          MotionKind::ExclusiveWithTargetCol((_, _), pos)
          | MotionKind::InclusiveWithTargetCol((_, _), pos) => {
            self.cursor.set(start);
            let end = self.end_of_line();
            self.cursor.add(end.min(pos));
          }
          _ => self.cursor.set(start),
        }
      }
      Verb::Equalize => todo!(),
      Verb::InsertModeLineBreak(anchor) => {
        let (mut start, end) = self.this_line();
				let auto_indent = read_shopts(|o| o.prompt.auto_indent);
        if start == 0 && end == self.cursor.max {
          match anchor {
            Anchor::After => {
              self.push('\n');
							if auto_indent {
								self.calc_indent_level();
								let tabs = (0..self.auto_indent_level).map(|_| '\t');
								for tab in tabs {
									self.push(tab);
								}
							}
              self.cursor.set(self.cursor_max());
              return Ok(());
            }
            Anchor::Before => {
							if auto_indent {
								self.calc_indent_level();
								let tabs = (0..self.auto_indent_level).map(|_| '\t');
								for tab in tabs {
									self.insert_at(0, tab);
								}
							}
              self.insert_at(0, '\n');
              self.cursor.set(0);
              return Ok(());
            }
          }
        }
        // We want the position of the newline, or start of buffer
        start = start.saturating_sub(1).min(self.cursor.max);
        match anchor {
          Anchor::After => {
            self.cursor.set(end);
            self.insert_at_cursor('\n');
            self.cursor.add(1);
						if auto_indent {
							self.calc_indent_level();
							let tabs = (0..self.auto_indent_level).map(|_| '\t');
							for tab in tabs {
								self.insert_at_cursor(tab);
								self.cursor.add(1);
							}
						}
          }
          Anchor::Before => {
            self.cursor.set(start);
            self.insert_at_cursor('\n');
            self.cursor.add(1);
						if auto_indent {
							self.calc_indent_level();
							let tabs = (0..self.auto_indent_level).map(|_| '\t');
							for tab in tabs {
								self.insert_at_cursor(tab);
								self.cursor.add(1);
							}
						}
          }
        }
      }
      Verb::AcceptLineOrNewline => {
				// If this verb has reached this function, it means we have incomplete input
				// and therefore must insert a newline instead of accepting the input
				if self.cursor.exclusive {
					// in this case we are in normal/visual mode, so we don't insert anything
					// and just move down a line
					let motion = self.eval_motion(None, MotionCmd(1, Motion::LineDownCharwise));
					self.apply_motion(motion);
					return Ok(());
				}
				let auto_indent = read_shopts(|o| o.prompt.auto_indent);
				self.insert_at_cursor('\n');
				self.cursor.add(1);
				if auto_indent {
					self.calc_indent_level();
					let tabs = (0..self.auto_indent_level).map(|_| '\t');
					for tab in tabs {
						self.insert_at_cursor(tab);
						self.cursor.add(1);
					}
				}
			}

      Verb::Complete
      | Verb::EndOfFile
      | Verb::InsertMode
      | Verb::NormalMode
      | Verb::VisualMode
      | Verb::ReplaceMode
      | Verb::VisualModeLine
      | Verb::VisualModeBlock
      | Verb::CompleteBackward
      | Verb::VisualModeSelectLast => self.apply_motion(motion), // Already handled logic for these
    }
    Ok(())
  }
  pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
    let clear_redos = !cmd.is_undo_op() || cmd.verb.as_ref().is_some_and(|v| v.1.is_edit());
    let is_char_insert = cmd.verb.as_ref().is_some_and(|v| v.1.is_char_insert());
    let is_line_motion = cmd.is_line_motion() || cmd.verb.as_ref().is_some_and(|v| v.1 == Verb::AcceptLineOrNewline);
    let is_undo_op = cmd.is_undo_op();
    let edit_is_merging = self.undo_stack.last().is_some_and(|edit| edit.merging);

    // Merge character inserts into one edit
    if edit_is_merging
      && cmd.verb.as_ref().is_none_or(|v| !v.1.is_char_insert())
      && let Some(edit) = self.undo_stack.last_mut()
    {
      edit.stop_merge();
    }

    let ViCmd {
      register,
      verb,
      motion,
      flags,
      raw_seq: _,
    } = cmd;

    let verb_cmd_ref = verb.as_ref();
    let verb_ref = verb_cmd_ref.map(|v| v.1.clone());

    let before = self.buffer.clone();
    let cursor_pos = self.cursor.get();

    /*
     * Let's evaluate the motion now
     * If we got some weird command like 'dvw' we will have to simulate a visual
     * selection to get the range If motion is None, we will try to use
     * self.select_range If self.select_range is None, we will use
     * MotionKind::Null
     */
    let motion_eval =
      if flags.intersects(CmdFlags::VISUAL | CmdFlags::VISUAL_LINE | CmdFlags::VISUAL_BLOCK) {
        let motion = motion
          .clone()
          .map(|m| self.eval_motion(verb_ref.as_ref(), m))
          .unwrap_or(MotionKind::Null);
        let mode = match flags {
          CmdFlags::VISUAL => SelectMode::Char(SelectAnchor::End),
          CmdFlags::VISUAL_LINE => SelectMode::Line(SelectAnchor::End),
          CmdFlags::VISUAL_BLOCK => SelectMode::Block(SelectAnchor::End),
          _ => unreachable!(),
        };
        // Start a selection
        self.start_selecting(mode);
        // Apply the cursor motion
        self.apply_motion(motion);

        // Use the selection range created by the motion
        self
          .select_range
          .map(MotionKind::Inclusive)
          .unwrap_or(MotionKind::Null)
      } else {
        motion
          .clone()
          .map(|m| self.eval_motion(verb_ref.as_ref(), m))
          .unwrap_or({
            self
              .select_range
              .map(MotionKind::Inclusive)
              .unwrap_or(MotionKind::Null)
          })
      };

    if let Some(verb) = verb.clone() {
      self.exec_verb(verb.1, motion_eval, register)?;
    } else {
      self.apply_motion(motion_eval);
    }

		if self.cursor.exclusive
		&& self.grapheme_at_cursor().is_some_and(|gr| gr == "\n")
		&& self.grapheme_before_cursor().is_some_and(|gr| gr != "\n") {
			// we landed on a newline, and we aren't inbetween two newlines.
			self.cursor.sub(1);
			self.update_select_range();
		}

    /* Done executing, do some cleanup */

    let after = self.buffer.clone();
    if clear_redos {
      self.redo_stack.clear();
    }

    if before != after {
      if !is_undo_op {
        self.handle_edit(before, after, cursor_pos);
      }
      /*
       * The buffer has been edited,
       * which invalidates the grapheme_indices vector
       * We set it to None now, so that self.update_graphemes_lazy()
       * will update it when it is needed again
       */
      self.update_graphemes();
    }

    if !is_line_motion {
      self.saved_col = None;
    }

    if is_char_insert && let Some(edit) = self.undo_stack.last_mut() {
      edit.start_merge();
    }

    Ok(())
  }
  pub fn as_str(&self) -> &str {
    &self.buffer // FIXME: this will have to be fixed up later
  }

  pub fn get_hint_text(&self) -> String {
    let text = self
      .hint
      .clone()
			.map(|h| format!("\x1b[90m{h}\x1b[0m"))
      .unwrap_or_default();

		text.replace("\n", "\n\x1b[90m")
  }
}

impl Display for LineBuf {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let mut full_buf = self.buffer.clone();
    if let Some((start, end)) = self.select_range {
      let mode = self.select_mode.unwrap();
      let start_byte = self.read_idx_byte_pos(start);
      let end_byte = self.read_idx_byte_pos(end);

			if start_byte >= full_buf.len() || end_byte >= full_buf.len() {
				log::warn!("Selection range '{:?}' is out of bounds for buffer of length {}, clearing selection", (start, end), full_buf.len());
				return write!(f, "{}", full_buf);
			}

      match mode.anchor() {
        SelectAnchor::Start => {
          let mut inclusive = start_byte..=end_byte;
          if *inclusive.end() == full_buf.len() {
            inclusive = start_byte..=end_byte.saturating_sub(1);
          }
          let selected = format!("{}{}{}", markers::VISUAL_MODE_START, &full_buf[inclusive.clone()], markers::VISUAL_MODE_END);
          full_buf.replace_range(inclusive, &selected);
        }
        SelectAnchor::End => {
          let selected = format!("{}{}{}", markers::VISUAL_MODE_START, &full_buf[start..end], markers::VISUAL_MODE_END);
          full_buf.replace_range(start_byte..end_byte, &selected);
        }
      }
    }
    write!(f, "{}", full_buf)
  }
}

/// Rotate alphabetic characters by 13 alphabetic positions
pub fn rot13(input: &str) -> String {
  input
    .chars()
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

pub fn ordered(start: usize, end: usize) -> (usize, usize) {
  if start > end {
    (end, start)
  } else {
    (start, end)
  }
}
