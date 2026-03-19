use std::{
  collections::HashSet,
  fmt::Display,
  ops::{Index, IndexMut, Range, RangeBounds, RangeFull, RangeInclusive},
  slice::SliceIndex,
};

use itertools::Itertools;
use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::vicmd::{
  Anchor, Bound, CmdFlags, Dest, Direction, Motion, MotionCmd, RegisterName, TextObj, To, Verb,
  ViCmd, Word,
};
use crate::{
  expand::expand_cmd_sub,
  libsh::{error::ShResult, guards::var_ctx_guard},
  parse::{
    Redir, RedirType,
    execute::exec_input,
    lex::{LexFlags, LexStream, QuoteState, Tk, TkFlags, TkRule},
  },
  prelude::*,
  procio::{IoFrame, IoMode, IoStack},
  readline::{
    history::History,
    markers,
    register::{RegisterContent, write_register},
    term::{RawModeGuard, get_win_size},
    vicmd::{ReadSrc, VerbCmd, WriteDest},
  },
  state::{VarFlags, VarKind, read_shopts, write_meta, write_vars},
};

const PUNCTUATION: [&str; 3] = ["?", "!", "."];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Grapheme(SmallVec<[char; 4]>);

impl Grapheme {
  pub fn chars(&self) -> &[char] {
    &self.0
  }
  /// Returns the display width of the Grapheme, treating unprintable chars as width 0
  pub fn width(&self) -> usize {
    self.0.iter().map(|c| c.width().unwrap_or(0)).sum()
  }
  /// Returns true if the Grapheme is wrapping a linefeed ('\n')
  pub fn is_lf(&self) -> bool {
    self.is_char('\n')
  }
  /// Returns true if the Grapheme consists of exactly one char and that char is `c`
  pub fn is_char(&self, c: char) -> bool {
    self.0.len() == 1 && self.0[0] == c
  }
  /// Returns the CharClass of the Grapheme, which is determined by the properties of its chars
  pub fn class(&self) -> CharClass {
    CharClass::from(self)
  }

	pub fn as_char(&self) -> Option<char> {
		if self.0.len() == 1 {
			Some(self.0[0])
		} else {
			None
		}
	}

  /// Returns true if the Grapheme is classified as whitespace (i.e. all chars are whitespace)
  pub fn is_ws(&self) -> bool {
    self.class() == CharClass::Whitespace
  }
}

impl From<char> for Grapheme {
  fn from(value: char) -> Self {
    let mut new = SmallVec::<[char; 4]>::new();
    new.push(value);
    Self(new)
  }
}

impl From<&str> for Grapheme {
  fn from(value: &str) -> Self {
    assert_eq!(value.graphemes(true).count(), 1);
    let mut new = SmallVec::<[char; 4]>::new();
    for char in value.chars() {
      new.push(char);
    }
    Self(new)
  }
}

impl From<String> for Grapheme {
  fn from(value: String) -> Self {
    Into::<Self>::into(value.as_str())
  }
}

impl From<&String> for Grapheme {
  fn from(value: &String) -> Self {
    Into::<Self>::into(value.as_str())
  }
}

impl Display for Grapheme {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for ch in &self.0 {
      write!(f, "{ch}")?;
    }
    Ok(())
  }
}

pub fn to_graphemes(s: impl ToString) -> Vec<Grapheme> {
  let s = s.to_string();
  s.graphemes(true).map(Grapheme::from).collect()
}

pub fn to_lines(s: impl ToString) -> Vec<Line> {
  let s = s.to_string();
  s.split("\n").map(to_graphemes).map(Line::from).collect()
}

pub fn trim_lines(lines: &mut Vec<Line>) {
  while lines.last().is_some_and(|line| line.is_empty()) {
    lines.pop();
  }
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Line(Vec<Grapheme>);

impl Line {
  pub fn graphemes(&self) -> &[Grapheme] {
    &self.0
  }
  pub fn len(&self) -> usize {
    self.0.len()
  }
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }
  pub fn push_str(&mut self, s: &str) {
    for g in s.graphemes(true) {
      self.0.push(Grapheme::from(g));
    }
  }
  pub fn push_char(&mut self, c: char) {
    self.0.push(Grapheme::from(c));
  }
  pub fn split_off(&mut self, at: usize) -> Line {
    if at > self.0.len() {
      return Line::default();
    }
    Line(self.0.split_off(at))
  }
  pub fn append(&mut self, other: &mut Line) {
    self.0.append(&mut other.0);
  }
  pub fn insert_char(&mut self, at: usize, c: char) {
    self.0.insert(at, Grapheme::from(c));
  }
  pub fn insert(&mut self, at: usize, g: Grapheme) {
    self.0.insert(at, g);
  }
  pub fn width(&self) -> usize {
    self.0.iter().map(|g| g.width()).sum()
  }
  pub fn trim_start(&mut self) -> Line {
    let mut clone = self.clone();
    while clone.0.first().is_some_and(|g| g.is_ws()) {
      clone.0.remove(0);
    }
    clone
  }
}

impl IndexMut<usize> for Line {
	fn index_mut(&mut self, index: usize) -> &mut Self::Output {
		&mut self.0[index]
	}
}

impl<T: SliceIndex<[Grapheme]>> Index<T> for Line {
  type Output = T::Output;
  fn index(&self, index: T) -> &Self::Output {
    &self.0[index]
  }
}

impl From<Vec<Grapheme>> for Line {
  fn from(value: Vec<Grapheme>) -> Self {
    Self(value)
  }
}

impl Display for Line {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for gr in &self.0 {
      write!(f, "{gr}")?;
    }
    Ok(())
  }
}

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

impl CharClass {
	pub fn is_other_class(&self, other: &CharClass) -> bool {
		!self.eq(other)
	}
	pub fn is_other_class_not_ws(&self, other: &CharClass) -> bool {
		if self.is_ws() || other.is_ws() {
			false
		} else {
			self.is_other_class(other)
		}
	}
	pub fn is_other_class_or_ws(&self, other: &CharClass) -> bool {
		if self.is_ws() || other.is_ws() {
			true
		} else {
			self.is_other_class(other)
		}
	}
	pub fn is_ws(&self) -> bool {
		*self == CharClass::Whitespace
	}
}

impl From<&Grapheme> for CharClass {
  fn from(g: &Grapheme) -> Self {
    let Some(&first) = g.0.first() else {
      return Self::Other;
    };

    if first.is_alphanumeric()
      && g.0[1..]
        .iter()
        .all(|&c| c.is_ascii_punctuation() || c == '\u{0301}' || c == '\u{0308}')
    {
      // Handles things like `ï`, `é`, etc., by manually allowing common diacritics
      return CharClass::Alphanum;
    }

    if g.0.iter().all(|&c| c.is_alphanumeric() || c == '_') {
      CharClass::Alphanum
    } else if g.0.iter().all(|c| c.is_whitespace()) {
      CharClass::Whitespace
    } else if g.0.iter().all(|c| !c.is_alphanumeric()) {
      CharClass::Symbol
    } else {
      CharClass::Other
    }
  }
}

fn is_whitespace(a: &Grapheme) -> bool {
  CharClass::from(a) == CharClass::Whitespace
}

fn is_other_class(a: &Grapheme, b: &Grapheme) -> bool {
  let a = CharClass::from(a);
  let b = CharClass::from(b);
  a != b
}

fn is_other_class_not_ws(a: &Grapheme, b: &Grapheme) -> bool {
  if is_whitespace(a) || is_whitespace(b) {
    false
  } else {
    is_other_class(a, b)
  }
}

fn is_other_class_or_is_ws(a: &Grapheme, b: &Grapheme) -> bool {
  if is_whitespace(a) || is_whitespace(b) {
    true
  } else {
    is_other_class(a, b)
  }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectAnchor {
  Pos(Pos),
  LineNo(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
  Char(SelectAnchor),
  Line(SelectAnchor),
  Block(SelectAnchor),
}

impl SelectMode {
  pub fn invert_anchor(&mut self, new_anchor: SelectAnchor) {
    match self {
      SelectMode::Block(select_anchor) | SelectMode::Char(select_anchor) => {
        *select_anchor = new_anchor;
      }
      SelectMode::Line(select_anchor) => {
        *select_anchor = new_anchor;
      }
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CaseTransform {
  Toggle,
  Lower,
  Upper,
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
  pub row: usize,
  pub col: usize,
}

impl Pos {
	/// make sure you clamp this
	pub const MAX: Self = Pos { row: usize::MAX, col: usize::MAX };
	pub const MIN: Self = Pos { row: 0, col: 0 };

  pub fn clamp_row<T>(&mut self, other: &[T]) {
    self.row = self.row.clamp(0, other.len().saturating_sub(1));
  }
  pub fn clamp_col<T>(&mut self, other: &[T], exclusive: bool) {
    let mut max = other.len();
    if exclusive && max > 0 {
      max = max.saturating_sub(1);
    }
    self.col = self.col.clamp(0, max);
  }
}

#[derive(Debug, Clone)]
pub enum MotionKind {
  Char { target: Pos, inclusive: bool },
  Line(usize),
  LineRange(Range<usize>),
  LineOffset(isize),
  Block { start: Pos, end: Pos },
}

impl MotionKind {
  /// Normalizes any given max-bounded range (1..2, 2..=5, ..10 etc) into a Range<usize>
  ///
  /// Examples:
  /// ```rust
  /// let range = MotionKind::line(1..=5);
  /// assert_eq!(range, 1..6);
  /// ```
  ///
  /// ```rust
  /// let range = MotionKind::line(..10);
  /// assert_eq!(range, 0..10);
  /// ```
  ///
  /// Panics if the given range is max-unbounded (e.g. '5..').
  pub fn line<R: RangeBounds<usize>>(range: R) -> Range<usize> {
    let start = match range.start_bound() {
      std::ops::Bound::Included(&start) => start,
      std::ops::Bound::Excluded(&start) => start + 1,
      std::ops::Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
      std::ops::Bound::Excluded(&end) => end,
      std::ops::Bound::Included(&end) => end + 1,
      std::ops::Bound::Unbounded => panic!("Unbounded end is not allowed for MotionKind::Line"),
    };
    start..end
  }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
  pub pos: Pos,
  pub exclusive: bool,
}

impl Cursor {
  /// Compat shim: returns the flat column position (col on row 0 in single-line mode)
  pub fn get(&self) -> usize {
    self.pos.col
  }
  /// Compat shim: sets the flat column position
  pub fn set(&mut self, col: usize) {
    self.pos.col = col;
  }
  /// Compat shim: returns cursor.col - n without mutating, clamped to 0
  pub fn ret_sub(&self, n: usize) -> usize {
    self.pos.col.saturating_sub(n)
  }
}

#[derive(Default, Clone, Debug)]
pub struct Edit {
	pub old_cursor: Pos,
  pub new_cursor: Pos,
  pub old: Vec<Line>,
  pub new: Vec<Line>,
  pub merging: bool,
}

impl Edit {
  pub fn start_merge(&mut self) {
    self.merging = true
  }
  pub fn stop_merge(&mut self) {
    self.merging = false
  }
  pub fn is_empty(&self) -> bool {
    self.old == self.new
  }
}

#[derive(Default, Clone, Debug)]
pub struct IndentCtx {
  depth: usize,
  ctx: Vec<Tk>,
  in_escaped_line: bool,
}

impl IndentCtx {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn depth(&self) -> usize {
    self.depth
  }

  pub fn ctx(&self) -> &[Tk] {
    &self.ctx
  }

  pub fn descend(&mut self, tk: Tk) {
    self.ctx.push(tk);
    self.depth += 1;
  }

  pub fn ascend(&mut self) {
    self.depth = self.depth.saturating_sub(1);
    self.ctx.pop();
  }

  pub fn reset(&mut self) {
    std::mem::take(self);
  }

  pub fn check_tk(&mut self, tk: Tk) {
    if tk.is_opener() {
      self.descend(tk);
    } else if self.ctx.last().is_some_and(|t| tk.is_closer_for(t)) {
      self.ascend();
    } else if matches!(tk.class, TkRule::Sep) && self.in_escaped_line {
      self.in_escaped_line = false;
      self.depth = self.depth.saturating_sub(1);
    }
  }

  pub fn calculate(&mut self, input: &str) -> usize {
    self.depth = 0;
    self.ctx.clear();
    self.in_escaped_line = false;

    let input_arc = Arc::new(input.to_string());
    let Ok(tokens) =
      LexStream::new(input_arc, LexFlags::LEX_UNFINISHED).collect::<ShResult<Vec<Tk>>>()
    else {
      log::error!("Lexing failed during depth calculation: {:?}", input);
      return 0;
    };

    for tk in tokens {
      self.check_tk(tk);
    }

    if input.ends_with("\\\n") {
      self.in_escaped_line = true;
      self.depth += 1;
    }

    self.depth
  }
}

fn extract_range_contiguous(buf: &mut Vec<Line>, start: Pos, end: Pos) -> Vec<Line> {
	let start_col = start.col.min(buf[start.row].len());
	let end_col = end.col.min(buf[end.row].len());

	if start.row == end.row {
		// single line case
		let line = &mut buf[start.row];
		let removed: Vec<Grapheme> = line.0
			.drain(start_col..end_col)
			.collect();
		return vec![Line(removed)];
	}

	// multi line case
	// tail of first line
	let first_tail: Line = buf[start.row].split_off(start_col);

	// all inbetween lines. extracts nothing if only two rows
	let middle: Vec<Line> = buf.drain(start.row + 1..end.row).collect();

	// head of last line
	let last_col = end_col.min(buf[start.row + 1].len());
	let last_head: Line = Line::from(buf[start.row + 1].0.drain(..last_col).collect::<Vec<_>>());

	// tail of last line
	let mut last_remainder = buf.remove(start.row + 1);

	// attach tail of last line to head of first line
	buf[start.row].append(&mut last_remainder);

	// construct vector of extracted content
	let mut extracts = vec![first_tail];
	extracts.extend(middle);
	extracts.push(last_head);
	extracts
}

#[derive(Debug, Clone)]
pub struct LineBuf {
  pub lines: Vec<Line>,
  pub hint: Vec<Line>,
  pub cursor: Cursor,

  pub select_mode: Option<SelectMode>,
  pub last_selection: Option<(SelectMode, SelectAnchor)>,

  pub insert_mode_start_pos: Option<Pos>,
  pub saved_col: Option<usize>,
  pub indent_ctx: IndentCtx,

  pub undo_stack: Vec<Edit>,
  pub redo_stack: Vec<Edit>,
}

impl Default for LineBuf {
  fn default() -> Self {
    Self {
      lines: vec![Line::from(vec![])],
      hint: vec![],
      cursor: Cursor {
        pos: Pos { row: 0, col: 0 },
        exclusive: false,
      },
      select_mode: None,
      last_selection: None,
      insert_mode_start_pos: None,
      saved_col: None,
      indent_ctx: IndentCtx::new(),
      undo_stack: vec![],
      redo_stack: vec![],
    }
  }
}

impl LineBuf {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn count_graphemes(&self) -> usize {
    self.lines.iter().map(|line| line.len()).sum()
  }
  fn cur_line(&self) -> &Line {
    &self.lines[self.cursor.pos.row]
  }
  fn cur_line_mut(&mut self) -> &mut Line {
    &mut self.lines[self.cursor.pos.row]
  }
	fn line_mut(&mut self, row: usize) -> &mut Line {
		&mut self.lines[row]
	}
  fn line_to_cursor(&self) -> &[Grapheme] {
    let line = self.cur_line();
    let col = self.cursor.pos.col.min(line.len());
    &line[..col]
  }
  fn line_from_cursor(&self) -> &[Grapheme] {
    let line = self.cur_line();
    let col = self.cursor.pos.col.min(line.len());
    &line[col..]
  }
  fn row_col(&self) -> (usize, usize) {
    (self.row(), self.col())
  }
  fn row(&self) -> usize {
    self.cursor.pos.row
  }
  fn offset_row(&self, offset: isize) -> usize {
    let mut row = self.cursor.pos.row.saturating_add_signed(offset);
    row = row.clamp(0, self.lines.len().saturating_sub(1));
    row
  }
  fn col(&self) -> usize {
    self.cursor.pos.col
  }
  fn offset_col(&self, row: usize, offset: isize) -> usize {
    let mut col = self.cursor.pos.col.saturating_add_signed(offset);
    let max = if self.cursor.exclusive {
      self.lines[row].len().saturating_sub(1)
    } else {
      self.lines[row].len()
    };
    col = col.clamp(0, max);
    col
  }
  fn offset_col_wrapping(&self, row: usize, offset: isize) -> (usize, usize) {
    let mut row = row;
    let mut col = self.cursor.pos.col as isize + offset;

    while col < 0 {
      if row == 0 {
        col = 0;
        break;
      }
      row -= 1;
      col += self.lines[row].len() as isize + 1;
    }
    while col > self.lines[row].len() as isize {
      if row >= self.lines.len() - 1 {
        col = self.lines[row].len() as isize;
        break;
      }
      col -= self.lines[row].len() as isize + 1;
      row += 1;
    }

    (row, col as usize)
  }
  fn set_cursor(&mut self, mut pos: Pos) {
    pos.clamp_row(&self.lines);
    pos.clamp_col(&self.lines[pos.row].0, false);
    self.cursor.pos = pos;
  }
  fn set_row(&mut self, row: usize) {
    let target_col = self.saved_col.unwrap_or(self.cursor.pos.col);

    self.set_cursor(Pos {
      row,
      col: self.saved_col.unwrap(),
    });
  }
  fn set_col(&mut self, col: usize) {
    self.set_cursor(Pos {
      row: self.cursor.pos.row,
      col,
    });
  }
  fn offset_cursor(&self, row_offset: isize, col_offset: isize) -> Pos {
    let row = self.offset_row(row_offset);
    let col = self.offset_col(row, col_offset);
    Pos { row, col }
  }
  fn offset_cursor_wrapping(&self, row_offset: isize, col_offset: isize) -> Pos {
    let row = self.offset_row(row_offset);
    let (row, col) = self.offset_col_wrapping(row, col_offset);
    Pos { row, col }
  }
  fn break_line(&mut self) {
    let (row, col) = self.row_col();
    let rest = self.lines[row].split_off(col);
    self.lines.insert(row + 1, rest);
    self.cursor.pos = Pos {
      row: row + 1,
      col: 0,
    };
  }
  fn verb_shell_cmd(&self, cmd: &str) -> ShResult<()> {
    Ok(())
  }
  fn insert(&mut self, gr: Grapheme) {
    if gr.is_lf() {
      self.break_line();
    } else {
      let (row, col) = self.row_col();
      self.lines[row].insert(col, gr);
      self.cursor.pos = self.offset_cursor(0, 1);
    }
  }
  fn insert_str(&mut self, s: &str) {
    for gr in s.graphemes(true) {
      let gr = Grapheme::from(gr);
      self.insert(gr);
    }
  }
  fn push_str(&mut self, s: &str) {
    let lines = to_lines(s);
    self.lines.extend(lines);
  }
  fn push(&mut self, gr: Grapheme) {
    let last = self.lines.last_mut();
    if let Some(last) = last {
      last.push_str(&gr.to_string());
    } else {
      self.lines.push(Line::from(vec![gr]));
    }
  }
  fn scan_forward<F: Fn(&Grapheme) -> bool>(&self, f: F) -> Option<Pos> {
    self.scan_forward_from(self.cursor.pos, f)
  }
  fn scan_forward_from<F: Fn(&Grapheme) -> bool>(&self, mut pos: Pos, f: F) -> Option<Pos> {
    pos.clamp_row(&self.lines);
    pos.clamp_col(&self.lines[pos.row].0, false);
    let Pos { mut row, mut col } = pos;

    loop {
      let line = &self.lines[row];
      if !line.is_empty() && f(&line[col]) {
        return Some(Pos { row, col });
      }
      if col < self.lines[row].len().saturating_sub(1) {
        col += 1;
      } else if row < self.lines.len().saturating_sub(1) {
        row += 1;
        col = 0;
      } else {
        return None;
      }
    }
  }
  fn scan_backward<F: Fn(&Grapheme) -> bool>(&self, f: F) -> Option<Pos> {
    self.scan_backward_from(self.cursor.pos, f)
  }
  fn scan_backward_from<F: Fn(&Grapheme) -> bool>(&self, mut pos: Pos, f: F) -> Option<Pos> {
    pos.clamp_row(&self.lines);
    pos.clamp_col(&self.lines[pos.row].0, false);
    let Pos { mut row, mut col } = pos;

    loop {
      let line = &self.lines[row];
      if !line.is_empty() && f(&line[col]) {
        return Some(Pos { row, col });
      }
      if col > 0 {
        col -= 1;
      } else if row > 0 {
        row -= 1;
        col = self.lines[row].len().saturating_sub(1);
      } else {
        return None;
      }
    }
  }
  fn search_char(&self, dir: &Direction, dest: &Dest, char: &Grapheme) -> isize {
    match dir {
      Direction::Forward => {
        let slice = self.line_from_cursor();
        for (i, gr) in slice.iter().enumerate().skip(1) {
          if gr == char {
            match dest {
              Dest::On => return i as isize,
              Dest::Before => return (i as isize - 1).max(0),
              Dest::After => unreachable!(),
            }
          }
        }
      }
      Direction::Backward => {
        let slice = self.line_to_cursor();
        for (i, gr) in slice.iter().rev().enumerate().skip(1) {
          if gr == char {
            match dest {
              Dest::On => return -(i as isize) - 1,
              Dest::Before => return -(i as isize),
              Dest::After => unreachable!(),
            }
          }
        }
      }
    }

    0
  }
  fn eval_word_motion(
    &self,
    count: usize,
    to: &To,
    word: &Word,
    dir: &Direction,
    ignore_trailing_ws: bool,
		mut inclusive: bool
  ) -> Option<MotionKind> {
		let mut target = self.cursor.pos;

		for _ in 0..count {
			match (to, dir) {
				(To::Start, Direction::Forward) => {
					target = self.word_motion_w(word, target, ignore_trailing_ws).unwrap_or_else(|| {
						// we set inclusive to true so that we catch the entire word
						// instead of ignoring the last character
						inclusive = true;
						Pos::MAX
					});
				}
				(To::End, Direction::Forward) => {
					inclusive = true;
					target = self.word_motion_e(word, target).unwrap_or(Pos::MAX);
				}
				(To::Start, Direction::Backward) => {
					target = self.word_motion_b(word, target).unwrap_or(Pos::MIN);
				}
				(To::End, Direction::Backward) => {
					inclusive = true;
					target = self.word_motion_ge(word, target).unwrap_or(Pos::MIN);
				}
			}
		}

		target.clamp_row(&self.lines);
		target.clamp_col(&self.lines[target.row].0, true);

		Some(MotionKind::Char { target, inclusive })
  }
	fn word_motion_w(&self, word: &Word, start: Pos, ignore_trailing_ws: bool) -> Option<Pos> {
		use CharClass as C;

		// get our iterator of char classes
		// we dont actually care what the chars are
		// just what they look like.
		// we are going to use .find() a lot to advance the iterator
		let mut classes = self.char_classes_forward_from(start).peekable();

		match word {
			Word::Big => {
				if let Some((_,C::Whitespace)) = classes.peek() {
					// we are on whitespace. advance to the next non-ws char class
					return classes.find(|(_,c)| !c.is_ws()).map(|(p,_)| p);
				}

				let last_non_ws = classes.find(|(_,c)| c.is_ws());
				if ignore_trailing_ws {
					return last_non_ws.map(|(p,_)| p);
				}
				classes.find(|(_,c)| !c.is_ws()).map(|(p,_)| p)
			}
			Word::Normal => {
				if let Some((_,C::Whitespace)) = classes.peek() {
					// we are on whitespace. advance to the next non-ws char class
					return classes.find(|(_,c)| !c.is_ws()).map(|(p,_)| p);
				}

				// go forward until we find some char class that isnt this one
				let first_c = classes.next()?.1;


				match classes.find(|(_,c)| c.is_other_class_or_ws(&first_c))? {
					(pos, C::Whitespace) if ignore_trailing_ws => return Some(pos),
					(_, C::Whitespace) => { /* fall through */ }
					(pos, _) => return Some(pos)
				}

				// we found whitespace previously, look for the next non-whitespace char class
				classes.find(|(_,c)| !c.is_ws()).map(|(p,_)| p)
			}
		}
	}
	fn word_motion_b(&self, word: &Word, start: Pos) -> Option<Pos> {
		use CharClass as C;
		// get our iterator again
		let mut classes = self.char_classes_backward_from(start).peekable();

		match word {
			Word::Big => {
				classes.next();
				// for 'b', we handle starting on whitespace differently than 'w'
				// we don't return immediately if find() returns Some() here.
				let first_non_ws = if let Some((_,C::Whitespace)) = classes.peek() {
					// we use find() to advance the iterator as usual
					// but we can also be clever and use the question mark
					// to return early if we don't find a word backwards
					classes.find(|(_,c)| !c.is_ws())?
				} else {
					classes.next()?
				};

				// ok now we are off that whitespace
				// now advance backwards until we find more whitespace, or next() is None

				let mut last = first_non_ws;
				while let Some((_,c)) = classes.peek() {
					if c.is_ws() { break; }
					last = classes.next()?;
				}
				Some(last.0)
			}
			Word::Normal => {
				classes.next();
				let first_non_ws = if let Some((_,C::Whitespace)) = classes.peek() {
					classes.find(|(_,c)| !c.is_ws())?
				} else {
					classes.next()?
				};

				// ok, off the whitespace
				// now advance until we find any different char class at all
				let mut last = first_non_ws;
				while let Some((_,c)) = classes.peek() {
					if c.is_other_class(&last.1) { break; }
					last = classes.next()?;
				}

				Some(last.0)
			}
		}
	}
	fn word_motion_e(&self, word: &Word, start: Pos) -> Option<Pos> {
		use CharClass as C;
		let mut classes = self.char_classes_forward_from(start).peekable();

		match word {
			Word::Big => {
				classes.next(); // unconditionally skip first position for 'e'
				let first_non_ws = if let Some((_,C::Whitespace)) = classes.peek() {
					classes.find(|(_,c)| !c.is_ws())?
				} else {
					classes.next()?
				};

				let mut last = first_non_ws;
				while let Some((_, c)) = classes.peek() {
					if c.is_other_class_or_ws(&first_non_ws.1) { return Some(last.0); }
					last = classes.next()?;
				}
				None
			}
			Word::Normal => {
				classes.next();
				let first_non_ws = if let Some((_,C::Whitespace)) = classes.peek() {
					classes.find(|(_,c)| !c.is_ws())?
				} else {
					classes.next()?
				};

				let mut last = first_non_ws;
				while let Some((_, c)) = classes.peek() {
					if c.is_other_class_or_ws(&first_non_ws.1) { return Some(last.0); }
					last = classes.next()?;
				}
				None
			}
		}
	}
	fn word_motion_ge(&self, word: &Word, start: Pos) -> Option<Pos> {
		use CharClass as C;
		let mut classes = self.char_classes_backward_from(start).peekable();

		match word {
			Word::Big => {
				classes.next(); // unconditionally skip first position for 'ge'
				if matches!(classes.peek(), Some((_, c)) if !c.is_ws()) {
					classes.find(|(_,c)| c.is_ws());
				}

				classes.find(|(_,c)| !c.is_ws()).map(|(p,_)| p)
			}
			Word::Normal => {
				classes.next();
				if let Some((_,C::Whitespace)) = classes.peek() {
					return classes.find(|(_,c)| !c.is_ws()).map(|(p,_)| p);
				}

				let cur_class = classes.peek()?.1;
				let bound = classes.find(|(_,c)| c.is_other_class(&cur_class))?;

				if bound.1.is_ws() {
					classes.find(|(_,c)| !c.is_ws()).map(|(p,_)| p)
				} else {
					Some(bound.0)
				}
			}
		}
	}
	fn char_classes_forward_from(&self, pos: Pos) -> impl Iterator<Item = (Pos,CharClass)> {
		CharClassIter::new(&self.lines, pos)
	}
	fn char_classes_forward(&self) -> impl Iterator<Item = (Pos,CharClass)> {
		self.char_classes_forward_from(self.cursor.pos)
	}
	fn char_classes_backward_from(&self, pos: Pos) -> impl Iterator<Item = (Pos,CharClass)> {
		CharClassIterRev::new(&self.lines, pos)
	}
	fn char_classes_backward(&self) -> impl Iterator<Item = (Pos,CharClass)> {
		self.char_classes_backward_from(self.cursor.pos)
	}
  fn eval_motion(&mut self, cmd: &ViCmd) -> Option<MotionKind> {
    let ViCmd { verb, motion, .. } = cmd;
    let MotionCmd(count, motion) = motion.as_ref()?;

    match motion {
      Motion::WholeLine => Some(MotionKind::Line(self.row())),
      Motion::TextObj(text_obj) => todo!(),
      Motion::EndOfLastWord => todo!(),
      Motion::BeginningOfFirstWord => todo!(),
      dir @ (Motion::BeginningOfLine | Motion::EndOfLine) => {
        let off = match dir {
          Motion::BeginningOfLine => isize::MIN,
          Motion::EndOfLine => isize::MAX,
          _ => unreachable!(),
        };
        let target = self.offset_cursor(0, off);
        (target != self.cursor.pos).then_some(MotionKind::Char { target, inclusive: true })
      }
      Motion::WordMotion(to, word, dir) => {
        // 'cw' is a weird case
        // if you are on the word's left boundary, it will not delete whitespace after
        // the end of the word
        let ignore_trailing_ws = matches!(verb, Some(VerbCmd(_, Verb::Change)),)
          && matches!(
            motion,
            Motion::WordMotion(To::Start, _, Direction::Forward,)
          );
				let inclusive = verb.is_none();

        self.eval_word_motion(*count, to, word, dir, ignore_trailing_ws, inclusive)
      }
      Motion::CharSearch(dir, dest, char) => {
        let off = self.search_char(dir, dest, char);
        let target = self.offset_cursor(0, off);
				let inclusive = matches!(dest, Dest::On);
        (target != self.cursor.pos).then_some(MotionKind::Char { target, inclusive })
      }
      dir @ (Motion::BackwardChar | Motion::ForwardChar)
      | dir @ (Motion::BackwardCharForced | Motion::ForwardCharForced) => {
        let (off, wrap) = match dir {
          Motion::BackwardChar => (-(*count as isize), false),
          Motion::ForwardChar => (*count as isize, false),
          Motion::BackwardCharForced => (-(*count as isize), true),
          Motion::ForwardCharForced => (*count as isize, true),
          _ => unreachable!(),
        };
        let target = if wrap {
          self.offset_cursor_wrapping(0, off)
        } else {
          self.offset_cursor(0, off)
        };

        (target != self.cursor.pos).then_some(MotionKind::Char { target, inclusive: false })
      }
      dir @ (Motion::LineDown | Motion::LineUp) => {
        let off = match dir {
          Motion::LineUp => -(*count as isize),
          Motion::LineDown => *count as isize,
          _ => unreachable!(),
        };
        if verb.is_some() {
          Some(MotionKind::LineOffset(off))
        } else {
          if self.saved_col.is_none() {
            self.saved_col = Some(self.cursor.pos.col);
          }
          let row = self.offset_row(off);
          let col = self.saved_col.unwrap().min(self.lines[row].len());
          let target = Pos { row, col };
          (target != self.cursor.pos).then_some(MotionKind::Char { target, inclusive: true })
        }
      }
      dir @ (Motion::EndOfBuffer | Motion::StartOfBuffer) => {
        let off = match dir {
          Motion::StartOfBuffer => isize::MIN,
          Motion::EndOfBuffer => isize::MAX,
          _ => unreachable!(),
        };
        if verb.is_some() {
          Some(MotionKind::LineOffset(off))
        } else {
          let target = self.offset_cursor(off, 0);
          (target != self.cursor.pos).then_some(MotionKind::Char { target, inclusive: true })
        }
      }
      Motion::WholeBuffer => Some(MotionKind::LineRange(0..self.lines.len())),
      Motion::ToColumn => todo!(),
      Motion::ToDelimMatch => todo!(),
      Motion::ToBrace(direction) => todo!(),
      Motion::ToBracket(direction) => todo!(),
      Motion::ToParen(direction) => todo!(),
      Motion::Range(_, _) => todo!(),
      Motion::RepeatMotion => todo!(),
      Motion::RepeatMotionRev => todo!(),
      Motion::Global(val) => todo!(),
      Motion::NotGlobal(val) => todo!(),
      Motion::Null => None,
    }
  }
  fn apply_motion(&mut self, motion: MotionKind) -> ShResult<()> {
    match motion {
      MotionKind::Char { target, inclusive: _ } => {
        self.set_cursor(target);
      }
      MotionKind::Line(ln) => {
        self.set_row(ln);
      }
      MotionKind::LineRange(range) => {
        let pos = Pos {
          row: range.start,
          col: 0,
        };
        self.set_cursor(pos);
      }
      MotionKind::LineOffset(off) => {
        self.set_row(self.offset_row(off));
      }
      MotionKind::Block { start, end } => todo!(),
    }
    Ok(())
  }
	fn extract_range(&mut self, motion: &MotionKind) -> Vec<Line> {
		let extracted = match motion {
			MotionKind::Char { target, inclusive } => {
				let (s, e) = ordered(self.cursor.pos, *target);
				let end = if *inclusive {
					Pos { row: e.row, col: e.col + 1 }
				} else {
					e
				};
				let mut buf = std::mem::take(&mut self.lines);
				let extracted = extract_range_contiguous(&mut buf, s, end);
				self.lines = buf;
				extracted
			}
			MotionKind::Line(lineno) => {
				vec![self.lines.remove(*lineno)]
			}
			MotionKind::LineRange(range) => {
				self.lines.drain(range.clone()).collect()
			}
			MotionKind::LineOffset(off) => {
				let row = self.row();
				let end = row.saturating_add_signed(*off);
				let (s, e) = ordered(row, end);
				self.lines.drain(s..=e).collect()
			}
			MotionKind::Block { start, end } => {
				let (s, e) = ordered(*start, *end);
				(s.row..=e.row).map(|row| {
					let sc = s.col.min(self.lines[row].len());
					let ec = (e.col + 1).min(self.lines[row].len());
					Line(self.lines[row].0.drain(sc..ec).collect())
				}).collect()
			}
		};
		if self.lines.is_empty() {
			self.lines.push(Line::default());
		}
		extracted
	}
	fn yank_range(&self, motion: &MotionKind) -> Vec<Line> {
		let mut tmp = Self {
			lines: self.lines.clone(),
			cursor: self.cursor,
			..Default::default()
		};
		tmp.extract_range(motion)
	}
	fn delete_range(&mut self, motion: &MotionKind) -> Vec<Line> {
		self.extract_range(motion)
	}
	fn motion_mutation(&mut self, motion: MotionKind, f: impl Fn(&Grapheme) -> Grapheme) {
		match motion {
			MotionKind::Char { target, inclusive } => {
				let (s,e) = ordered(self.cursor.pos,target);
				if s.row == e.row {
					let range = if inclusive { s.col..e.col + 1 } else { s.col..e.col };
					for col in range {
						if col >= self.lines[s.row].len() {
							break;
						}
						self.lines[s.row][col] = f(&self.lines[s.row][col]);
					}
					return
				}
				let end = if inclusive { e.col + 1 } else { e.col };

				for col in s.col..self.lines[s.row].len() {
					self.lines[s.row][col] = f(&self.lines[s.row][col]);
				}
				for row in s.row + 1..e.row {
					for col in 0..self.lines[row].len() {
						self.lines[row][col] = f(&self.lines[row][col]);
					}
				}
				for col in 0..end {
					if col >= self.lines[e.row].len() {
						break;
					}
					self.lines[e.row][col] = f(&self.lines[e.row][col]);
				}
			}
			MotionKind::Line(lineno) => {
				if lineno >= self.lines.len() {
					return;
				}
				let line = self.line_mut(lineno);
				for col in 0..line.len() {
					line[col] = f(&line[col]);
				}
			}
			MotionKind::LineRange(range) => {
				for line in range {
					if line >= self.lines.len() {
						break;
					}
					let line = self.line_mut(line);
					for col in 0..line.len() {
						line[col] = f(&line[col]);
					}
				}
			}
			MotionKind::LineOffset(off) => {
				let row = self.row();
				let end = row.saturating_add_signed(off);
				let (s,mut e) = ordered(row, end);
				e = e.min(self.lines.len().saturating_sub(1));

				for line in s..=e {
					let line = self.line_mut(line);
					for col in 0..line.len() {
						line[col] = f(&line[col]);
					}
				}
			}
			MotionKind::Block { start, end } => todo!(),
		}
	}
	fn inplace_mutation(&mut self, count: u16, f: impl Fn(&Grapheme) -> Grapheme) {
		let mut first = true;
		for i in 0..count {
			let motion = MotionKind::Char {
				target: self.cursor.pos,
				inclusive: false,
			};
			self.motion_mutation(motion, &f);
			if !first {
				first = false
			} else {
				self.cursor.pos = self.offset_cursor(0, 1);
			}
		}
	}
  fn exec_verb(&mut self, cmd: &ViCmd) -> ShResult<()> {
    let ViCmd {
      register,
      verb,
      motion,
      ..
    } = cmd;
    let Some(VerbCmd(count, verb)) = verb else {
      let Some(motion_kind) = self.eval_motion(cmd) else {
        return Ok(());
      };
      return self.apply_motion(motion_kind);
    };
    let count = motion.as_ref().map(|m| m.0).unwrap_or(1);

    match verb {
      Verb::Delete |
      Verb::Change |
      Verb::Yank => {
				let Some(motion) = self.eval_motion(cmd) else {
					return Ok(())
				};
				let content = if *verb == Verb::Yank {
					self.yank_range(&motion)
				} else {
					self.delete_range(&motion)
				};
				let reg_content = match &motion {
					MotionKind::Char { .. } => RegisterContent::Span(content),
					MotionKind::Line(_) | MotionKind::LineRange(_) | MotionKind::LineOffset(_) => RegisterContent::Line(content),
					MotionKind::Block { .. } => RegisterContent::Block(content),
				};
				register.write_to_register(reg_content);

				match motion {
					MotionKind::Char { target, .. } => {
						let (start, _) = ordered(self.cursor.pos, target);
						self.set_cursor(start);
					}
					MotionKind::Line(line_no) => {
						self.set_cursor_clamp(self.cursor.exclusive);
					}
					MotionKind::LineRange(_) | MotionKind::LineOffset(_) => {
						self.set_cursor_clamp(self.cursor.exclusive);
					}
					MotionKind::Block { start, .. } => {
						let (s, _) = ordered(self.cursor.pos, start);
						self.set_cursor(s);
					}
				}
			}
      Verb::Rot13 => {
				let Some(motion) = self.eval_motion(cmd) else { return Ok(()) };
				self.motion_mutation(motion, |gr| {
					gr.as_char()
						.map(rot13_char)
						.map(Grapheme::from)
						.unwrap_or_else(|| gr.clone())
				});
			}
      Verb::ReplaceChar(ch) => {
				let Some(motion) = self.eval_motion(cmd) else { return Ok(()) };
				self.motion_mutation(motion, |_| Grapheme::from(*ch));
			}
      Verb::ReplaceCharInplace(ch, count) => self.inplace_mutation(*count, |_| Grapheme::from(*ch)),
      Verb::ToggleCaseInplace(count) => {
				self.inplace_mutation(*count, |gr| {
					gr.as_char()
						.map(toggle_case_char)
						.map(Grapheme::from)
						.unwrap_or_else(|| gr.clone())
				});
			}
      Verb::ToggleCaseRange => {
				let Some(motion) = self.eval_motion(cmd) else { return Ok(()) };
				self.motion_mutation(motion, |gr| {
					gr.as_char()
						.map(toggle_case_char)
						.map(Grapheme::from)
						.unwrap_or_else(|| gr.clone())
				});
			}
      Verb::IncrementNumber(_) => todo!(),
      Verb::DecrementNumber(_) => todo!(),
      Verb::ToLower => {
				let Some(motion) = self.eval_motion(cmd) else { return Ok(()) };
				self.motion_mutation(motion, |gr| {
					gr.as_char()
						.map(|c| c.to_ascii_uppercase())
						.map(Grapheme::from)
						.unwrap_or_else(|| gr.clone())
				})
			}
      Verb::ToUpper => {
				let Some(motion) = self.eval_motion(cmd) else { return Ok(()) };
				self.motion_mutation(motion, |gr| {
					gr.as_char()
						.map(|c| c.to_ascii_uppercase())
						.map(Grapheme::from)
						.unwrap_or_else(|| gr.clone())
				})
			}
      Verb::Undo => {
				if let Some(edit) = self.undo_stack.pop() {
					self.lines = edit.old.clone();
					self.cursor.pos = edit.old_cursor;
					self.redo_stack.push(edit);
				}
			}
      Verb::Redo => if let Some(edit) = self.redo_stack.pop() {
				self.lines = edit.new.clone();
				self.cursor.pos = edit.new_cursor;
				self.undo_stack.push(edit);
			}
      Verb::RepeatLast => todo!(),
      Verb::Put(anchor) => {
				let Some(content) = register.read_from_register() else {
					return Ok(())
				};
				match content {
					RegisterContent::Span(lines) => {
						let row = self.row();
						let col = match anchor {
							Anchor::After => (self.col() + 1).min(self.cur_line().len()),
							Anchor::Before => self.col(),
						};
						let mut right = self.lines[row].split_off(col);

						let mut lines = lines.clone();
						let last = lines.len() - 1;

						// First line appends to current line
						self.lines[row].append(&mut lines[0]);

						// Middle + last lines get inserted after
						for (i, line) in lines[1..].iter().cloned().enumerate() {
							self.lines.insert(row + 1 + i, line);
						}

						// Reattach right half to the last inserted line
						self.lines[row + last].append(&mut right);
					}
					RegisterContent::Line(lines) => {
						let row = match anchor {
							Anchor::After => self.row() + 1,
							Anchor::Before => self.row(),
						};
						for (i,line) in lines.iter().cloned().enumerate() {
							self.lines.insert(row + i, line);
						}
					}
					RegisterContent::Block(lines) => todo!(),
					RegisterContent::Empty => {}
				}
			}
      Verb::InsertModeLineBreak(anchor) => match anchor {
        Anchor::After => {
          let row = self.row();
          let target = (row + 1).min(self.lines.len());
          self.lines.insert(target, Line::default());
          self.cursor.pos = Pos {
            row: row + 1,
            col: 0,
          };
        }
        Anchor::Before => {
          let row = self.row();
          self.lines.insert(row, Line::default());
          self.cursor.pos = Pos { row, col: 0 };
        }
      },
      Verb::SwapVisualAnchor => todo!(),
      Verb::JoinLines => {
        let old_exclusive = self.cursor.exclusive;
        self.cursor.exclusive = false;
        for _ in 0..count {
          let row = self.row();
          let target_pos = Pos {
            row,
            col: self.offset_col(row, isize::MAX),
          };
          if row == self.lines.len() - 1 {
            break;
          }

          let mut next_line = self.lines.remove(row + 1).trim_start();
          let this_line = self.cur_line_mut();
          let this_has_ws = this_line.0.last().is_some_and(|g| g.is_ws());
          let join_with_space = !this_has_ws && !this_line.is_empty() && !next_line.is_empty();

          if join_with_space {
            next_line.insert_char(0, ' ');
          }

          this_line.append(&mut next_line);
          self.set_cursor(target_pos);
        }

        self.cursor.exclusive = old_exclusive;
      }
      Verb::InsertChar(ch) => self.insert(Grapheme::from(*ch)),
      Verb::Insert(s) => self.insert_str(s),
      Verb::Indent => todo!(),
      Verb::Dedent => todo!(),
      Verb::Equalize => todo!(),
      Verb::AcceptLineOrNewline => {
        // If we are here, we did not accept the line
        // so we break to a new line
        self.break_line();
      }
      Verb::ShellCmd(cmd) => self.verb_shell_cmd(cmd)?,
      Verb::Read(src) => match src {
        ReadSrc::File(path_buf) => {
          if !path_buf.is_file() {
            write_meta(|m| m.post_system_message(format!("{} is not a file", path_buf.display())));
            return Ok(());
          }
          let Ok(contents) = std::fs::read_to_string(&path_buf) else {
            write_meta(|m| {
              m.post_system_message(format!("Failed to read file {}", path_buf.display()))
            });
            return Ok(());
          };
          self.insert_str(&contents);
        }
        ReadSrc::Cmd(cmd) => {
          let output = match expand_cmd_sub(&cmd) {
            Ok(out) => out,
            Err(e) => {
              e.print_error();
              return Ok(());
            }
          };

          self.insert_str(&output);
        }
      },
      Verb::Write(dest) => match dest {
        WriteDest::FileAppend(path_buf) | WriteDest::File(path_buf) => {
          let Ok(mut file) = (if matches!(dest, WriteDest::File(_)) {
            OpenOptions::new()
              .create(true)
              .truncate(true)
              .write(true)
              .open(path_buf)
          } else {
            OpenOptions::new().create(true).append(true).open(path_buf)
          }) else {
            write_meta(|m| {
              m.post_system_message(format!("Failed to open file {}", path_buf.display()))
            });
            return Ok(());
          };

          if let Err(e) = file.write_all(self.joined().as_bytes()) {
            write_meta(|m| {
              m.post_system_message(format!(
                "Failed to write to file {}: {e}",
                path_buf.display()
              ))
            });
          }
          return Ok(());
        }
        WriteDest::Cmd(cmd) => {
          let buf = self.joined();
          let io_mode = IoMode::Buffer {
            tgt_fd: STDIN_FILENO,
            buf,
            flags: TkFlags::IS_HEREDOC | TkFlags::LIT_HEREDOC,
          };
          let redir = Redir::new(io_mode, RedirType::Input);
          let mut frame = IoFrame::new();

          frame.push(redir);
          let mut stack = IoStack::new();
          stack.push_frame(frame);

          exec_input(cmd.to_string(), Some(stack), false, Some("ex write".into()))?;
        }
      },
      Verb::Edit(path) => {
        let input = format!("$EDITOR {}", path.display());
        exec_input(input, None, true, Some("ex edit".into()))?;
      }

      Verb::Complete
      | Verb::ExMode
      | Verb::EndOfFile
      | Verb::InsertMode
      | Verb::NormalMode
      | Verb::VisualMode
      | Verb::VerbatimMode
      | Verb::ReplaceMode
      | Verb::VisualModeLine
      | Verb::VisualModeBlock
      | Verb::CompleteBackward
      | Verb::VisualModeSelectLast => {
        let Some(motion_kind) = self.eval_motion(cmd) else {
          return Ok(());
        };
        self.apply_motion(motion_kind)?;
      }
      Verb::Normal(_)
      | Verb::Substitute(..)
      | Verb::RepeatSubstitute
      | Verb::Quit
      | Verb::RepeatGlobal => {
        log::warn!("Verb {:?} is not implemented yet", verb);
      }
    }

    Ok(())
  }
  pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
    let is_char_insert = cmd.verb.as_ref().is_some_and(|v| v.1.is_char_insert());
    let is_line_motion = cmd.is_line_motion()
      || cmd
        .verb
        .as_ref()
        .is_some_and(|v| v.1 == Verb::AcceptLineOrNewline);
    let is_undo_op = cmd.is_undo_op();
    let is_vertical = matches!(
      cmd.motion().map(|m| &m.1),
      Some(Motion::LineUp | Motion::LineDown)
    );

    if !is_vertical {
      self.saved_col = None;
    }

		let before = self.lines.clone();
		let old_cursor = self.cursor.pos;

    let res = self.exec_verb(&cmd);

		let new_cursor = self.cursor.pos;

		if self.lines != before && !is_undo_op {
			self.redo_stack.clear();
			if is_char_insert {
				// Merge consecutive char inserts into one undo entry
				if let Some(edit) = self.undo_stack.last_mut().filter(|e| e.merging) {
					edit.new = self.lines.clone();
					edit.new_cursor = new_cursor;
				} else {
					self.undo_stack.push(Edit {
						old_cursor,
						new_cursor,
						old: before,
						new: self.lines.clone(),
						merging: true,
					});
				}
			} else {
				// Stop merging on any non-insert edit
				if let Some(edit) = self.undo_stack.last_mut() {
					edit.merging = false;
				}
				self.handle_edit(before, new_cursor, old_cursor);
			}
		}

		self.fix_cursor();
		res
  }

  pub fn handle_edit(&mut self, old: Vec<Line>, new_cursor: Pos, old_cursor: Pos) {
    let edit_is_merging = self.undo_stack.last().is_some_and(|edit| edit.merging);
    if edit_is_merging {
      // Update the `new` snapshot on the existing edit
      if let Some(edit) = self.undo_stack.last_mut() {
        edit.new = self.lines.clone();
      }
    } else {
      self.undo_stack.push(Edit {
				new_cursor,
        old_cursor,
        old,
        new: self.lines.clone(),
        merging: false,
      });
    }
  }

	fn fix_cursor(&mut self) {
		if self.cursor.exclusive {
			let line = self.cur_line();
			let col = self.col();
			if col > 0 && col >= line.len() {
				self.cursor.pos.col = line.len().saturating_sub(1);
			}
		} else {
			let line = self.cur_line();
			let col = self.col();
			if col > 0 && col > line.len() {
				self.cursor.pos.col = line.len();
			}
		}
	}

  pub fn joined(&self) -> String {
    let mut lines = vec![];
    for line in &self.lines {
      lines.push(line.to_string());
    }
    lines.join("\n")
  }

  // ───── Compatibility shims for old flat-string interface ─────

  /// Compat shim: replace buffer contents from a string, parsing into lines.
  pub fn set_buffer(&mut self, s: String) {
    self.lines = to_lines(&s);
    if self.lines.is_empty() {
      self.lines.push(Line::default());
    }
    // Clamp cursor to valid position
    self.cursor.pos.row = self.cursor.pos.row.min(self.lines.len().saturating_sub(1));
    let max_col = self.lines[self.cursor.pos.row].len();
    self.cursor.pos.col = self.cursor.pos.col.min(max_col);
  }

  /// Compat shim: set hint text. None clears the hint.
  pub fn set_hint(&mut self, hint: Option<String>) {
    match hint {
      Some(s) => self.hint = to_lines(&s),
      None => self.hint.clear(),
    }
  }

  /// Compat shim: returns true if there is a non-empty hint.
  pub fn has_hint(&self) -> bool {
    !self.hint.is_empty() && self.hint.iter().any(|l| !l.is_empty())
  }

  /// Compat shim: get hint text as a string.
  pub fn get_hint_text(&self) -> String {
    let mut lines = vec![];
    let mut hint = self.hint.clone();
    trim_lines(&mut hint);
    for line in hint {
      lines.push(line.to_string());
    }
    lines.join("\n")
  }

  /// Compat shim: accept the current hint by appending it to the buffer.
  pub fn accept_hint(&mut self) {
    if self.hint.is_empty() {
      return;
    }
    let hint_str = self.get_hint_text();
    self.push_str(&hint_str);
    self.hint.clear();
  }

  /// Compat shim: return a constructor that sets initial buffer contents and cursor.
  pub fn with_initial(mut self, s: &str, cursor_pos: usize) -> Self {
    self.set_buffer(s.to_string());
    // In the flat model, cursor_pos was a flat offset. Map to col on row 0.
    self.cursor.pos = Pos {
      row: 0,
      col: cursor_pos.min(s.len()),
    };
    self
  }

  /// Compat shim: move cursor to end of buffer.
  pub fn move_cursor_to_end(&mut self) {
    let last_row = self.lines.len().saturating_sub(1);
    let last_col = self.lines[last_row].len();
    self.cursor.pos = Pos {
      row: last_row,
      col: last_col,
    };
  }

  /// Compat shim: returns the maximum cursor position (flat grapheme count).
  pub fn cursor_max(&self) -> usize {
    // In single-line mode this is the length of the first line
    // In multi-line mode this returns total grapheme count (for flat compat)
    if self.lines.len() == 1 {
      self.lines[0].len()
    } else {
      self.count_graphemes()
    }
  }

  /// Compat shim: returns true if cursor is at the max position.
  pub fn cursor_at_max(&self) -> bool {
    let last_row = self.lines.len().saturating_sub(1);
    self.cursor.pos.row == last_row && self.cursor.pos.col >= self.lines[last_row].len()
  }

  /// Compat shim: set cursor with clamping.
  pub fn set_cursor_clamp(&mut self, exclusive: bool) {
    self.cursor.exclusive = exclusive;
  }

  /// Compat shim: returns the flat column of the start of the current line.
  /// In the old flat model this returned 0 for single-line; for multi-line it's the
  /// flat offset of the beginning of the current row.
  pub fn start_of_line(&self) -> usize {
    // Return 0-based flat offset of start of current row
    let mut offset = 0;
    for i in 0..self.cursor.pos.row {
      offset += self.lines[i].len() + 1; // +1 for '\n'
    }
    offset
  }

  pub fn on_last_line(&self) -> bool {
    self.cursor.pos.row == self.lines.len().saturating_sub(1)
  }

  /// Compat shim: returns slice of joined buffer from grapheme indices.
  pub fn slice(&self, range: std::ops::Range<usize>) -> Option<String> {
    let joined = self.joined();
    let graphemes: Vec<&str> = joined.graphemes(true).collect();
    if range.start > graphemes.len() || range.end > graphemes.len() {
      return None;
    }
    Some(graphemes[range].join(""))
  }

  /// Compat shim: returns the string from buffer start to cursor position.
  pub fn slice_to_cursor(&self) -> Option<String> {
    let mut result = String::new();
    for i in 0..self.cursor.pos.row {
      result.push_str(&self.lines[i].to_string());
      result.push('\n');
    }
    let line = &self.lines[self.cursor.pos.row];
    let col = self.cursor.pos.col.min(line.len());
    for g in &line.graphemes()[..col] {
      result.push_str(&g.to_string());
    }
    Some(result)
  }

  /// Compat shim: returns cursor byte position in the joined string.
  pub fn cursor_byte_pos(&self) -> usize {
    let mut pos = 0;
    for i in 0..self.cursor.pos.row {
      pos += self.lines[i].to_string().len() + 1; // +1 for '\n'
    }
    let line_str = self.lines[self.cursor.pos.row].to_string();
    let col = self
      .cursor
      .pos
      .col
      .min(self.lines[self.cursor.pos.row].len());
    // Sum bytes of graphemes up to col
    let mut byte_count = 0;
    for (i, g) in line_str.graphemes(true).enumerate() {
      if i >= col {
        break;
      }
      byte_count += g.len();
    }
    pos + byte_count
  }

  pub fn start_char_select(&mut self) {
    self.select_mode = Some(SelectMode::Char(SelectAnchor::Pos(self.cursor.pos)));
  }

  pub fn start_line_select(&mut self) {
    self.select_mode = Some(SelectMode::Line(SelectAnchor::LineNo(self.cursor.pos.row)));
  }

  pub fn start_block_select(&mut self) {
    self.select_mode = Some(SelectMode::Block(SelectAnchor::Pos(self.cursor.pos)));
  }

  /// Compat shim: stop visual selection.
  pub fn stop_selecting(&mut self) {
    if self.select_mode.is_some() {
      self.last_selection = self.select_mode.map(|m| {
        let anchor = match m {
          SelectMode::Char(a) | SelectMode::Block(a) | SelectMode::Line(a) => a,
        };
        (m, anchor)
      });
    }
    self.select_mode = None;
  }

  /// Compat shim: return current selection range as flat (start, end) offsets.
  pub fn select_range(&self) -> Option<(usize, usize)> {
    let mode = self.select_mode.as_ref()?;
    let anchor_pos = match mode {
      SelectMode::Char(SelectAnchor::Pos(p)) => *p,
      SelectMode::Line(SelectAnchor::LineNo(l)) => Pos { row: *l, col: 0 },
      SelectMode::Block(SelectAnchor::Pos(p)) => *p,
      _ => return None,
    };
    let cursor_pos = self.cursor.pos;
    // Convert both to flat offsets
    let flat_anchor = self.pos_to_flat(anchor_pos);
    let flat_cursor = self.pos_to_flat(cursor_pos);
    let (start, end) = ordered(flat_anchor, flat_cursor);
    Some((start, end))
  }

  /// Helper: convert a Pos to a flat grapheme offset.
  fn pos_to_flat(&self, pos: Pos) -> usize {
    let mut offset = 0;
    let row = pos.row.min(self.lines.len().saturating_sub(1));
    for i in 0..row {
      offset += self.lines[i].len() + 1; // +1 for '\n'
    }
    offset + pos.col.min(self.lines[row].len())
  }

  /// Compat shim: attempt history expansion. Stub that returns false.
  pub fn attempt_history_expansion(&mut self, _history: &super::history::History) -> bool {
    // TODO: implement history expansion for 2D buffer
    false
  }

  /// Compat shim: check if cursor is on an escaped char.
  pub fn cursor_is_escaped(&self) -> bool {
    if self.cursor.pos.col == 0 {
      return false;
    }
    let line = &self.lines[self.cursor.pos.row];
    if self.cursor.pos.col > line.len() {
      return false;
    }
    line
      .graphemes()
      .get(self.cursor.pos.col.saturating_sub(1))
      .is_some_and(|g| g.is_char('\\'))
  }

  /// Compat shim: take buffer contents and reset.
  pub fn take_buf(&mut self) -> String {
    let result = self.joined();
    self.lines = vec![Line::default()];
    self.cursor.pos = Pos { row: 0, col: 0 };
    result
  }

  /// Compat shim: calculate indent level.
  pub fn calc_indent_level(&mut self) {
    let joined = self.joined();
    self.indent_ctx.calculate(&joined);
  }

  /// Compat shim: mark where insert mode started.
  pub fn mark_insert_mode_start_pos(&mut self) {
    self.insert_mode_start_pos = Some(self.cursor.pos);
  }

  /// Compat shim: clear insert mode start position.
  pub fn clear_insert_mode_start_pos(&mut self) {
    self.insert_mode_start_pos = None;
  }
}

impl std::fmt::Display for LineBuf {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.joined())
  }
}

struct CharClassIter<'a> {
	lines: &'a [Line],
	row: usize,
	col: usize,
	exhausted: bool,
	at_boundary: bool,
}

impl<'a> CharClassIter<'a> {
	pub fn new(lines: &'a [Line], start_pos: Pos) -> Self {
		Self {
			lines,
			row: start_pos.row,
			col: start_pos.col,
			exhausted: false,
			at_boundary: false,
		}
	}
	fn get_pos(&self) -> Pos {
		Pos { row: self.row, col: self.col }
	}
}

impl<'a> Iterator for CharClassIter<'a> {
	type Item = (Pos, CharClass);
	fn next(&mut self) -> Option<(Pos, CharClass)> {
		if self.exhausted { return None; }

		// Synthetic whitespace for line boundary
		if self.at_boundary {
			self.at_boundary = false;
			let pos = self.get_pos();
			return Some((pos, CharClass::Whitespace));
		}

		if self.row >= self.lines.len() {
			self.exhausted = true;
			return None;
		}

		let line = &self.lines[self.row];
		// Empty line = whitespace
		if line.is_empty() {
			let pos = Pos { row: self.row, col: 0 };
			self.row += 1;
			self.col = 0;
			return Some((pos, CharClass::Whitespace));
		}

		let pos = self.get_pos();
		let class = line[self.col].class();

		self.col += 1;
		if self.col >= line.len() {
			self.row += 1;
			self.col = 0;
			self.at_boundary = self.row < self.lines.len();
		}

		Some((pos, class))
	}
}

struct CharClassIterRev<'a> {
	lines: &'a [Line],
	row: usize,
	col: usize,
	exhausted: bool,
	at_boundary: bool,
}

impl<'a> CharClassIterRev<'a> {
	pub fn new(lines: &'a [Line], start_pos: Pos) -> Self {
		Self {
			lines,
			row: start_pos.row,
			col: start_pos.col,
			exhausted: false,
			at_boundary: false,
		}
	}
	fn get_pos(&self) -> Pos {
		Pos { row: self.row, col: self.col }
	}
}

impl<'a> Iterator for CharClassIterRev<'a> {
	type Item = (Pos, CharClass);
	fn next(&mut self) -> Option<(Pos, CharClass)> {
		if self.exhausted { return None; }

		// Synthetic whitespace for line boundary
		if self.at_boundary {
			self.at_boundary = false;
			let pos = self.get_pos();
			return Some((pos, CharClass::Whitespace));
		}

		if self.row >= self.lines.len() {
			self.exhausted = true;
			return None;
		}

		let line = &self.lines[self.row];
		// Empty line = whitespace
		if line.is_empty() {
			let pos = Pos { row: self.row, col: 0 };
			if self.row == 0 {
				self.exhausted = true;
			} else {
				self.row -= 1;
				self.col = self.lines[self.row].len().saturating_sub(1);
			}
			return Some((pos, CharClass::Whitespace));
		}

		let pos = self.get_pos();
		let class = line[self.col].class();

		if self.col == 0 {
			if self.row == 0 {
				self.exhausted = true;
			} else {
				self.row -= 1;
				self.col = self.lines[self.row].len().saturating_sub(1);
				self.at_boundary = true;
			}
		} else {
			self.col -= 1;
		}

		Some((pos, class))
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

pub fn rot13_char(c: char) -> char {
	let offset = if c.is_ascii_lowercase() {
		b'a'
	} else if c.is_ascii_uppercase() {
		b'A'
	} else {
		return c;
	};
	(((c as u8 - offset + 13) % 26) + offset) as char
}

pub fn toggle_case_char(c: char) -> char {
	if c.is_ascii_lowercase() {
		c.to_ascii_uppercase()
	} else if c.is_ascii_uppercase() {
		c.to_ascii_lowercase()
	} else {
		c
	}
}

pub fn ordered<T: Ord>(start: T, end: T) -> (T, T) {
  if start > end {
    (end, start)
  } else {
    (start, end)
  }
}
