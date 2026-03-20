use std::{
  collections::HashSet, fmt::Display, ops::{Index, IndexMut}, slice::SliceIndex
};

use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use super::vicmd::{
  Anchor, Bound, Dest, Direction, Motion, MotionCmd, TextObj, To, Verb,
  ViCmd, Word,
};
use crate::{
  expand::expand_cmd_sub,
  libsh::{error::ShResult, guards::{RawModeGuard, var_ctx_guard}},
  parse::{
    Redir, RedirType,
    execute::exec_input,
    lex::{LexFlags, LexStream, Tk, TkFlags},
  },
  prelude::*,
  procio::{IoFrame, IoMode, IoStack},
  readline::{
    markers,
    register::RegisterContent, vicmd::{ReadSrc, VerbCmd, WriteDest},
  },
  state::{VarFlags, VarKind, read_vars, write_meta, write_vars},
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

pub fn join_lines(lines: &[Line]) -> String {
	lines
		.iter()
		.map(|line| line.to_string())
		.collect::<Vec<String>>()
		.join("\n")
}

pub fn trim_lines(lines: &mut Vec<Line>) {
  while lines.last().is_some_and(|line| line.is_empty()) {
    lines.pop();
  }
}

pub fn split_lines_at(lines: &mut Vec<Line>, pos: Pos) -> Vec<Line> {
  let tail = lines[pos.row].split_off(pos.col);
  let mut rest: Vec<Line> = lines.drain(pos.row + 1..).collect();
  rest.insert(0, tail);
  rest
}

pub fn attach_lines(lines: &mut Vec<Line>, other: &mut Vec<Line>) {
  if other.is_empty() {
    return;
  }
  if lines.is_empty() {
    lines.append(other);
    return;
  }
  let mut head = other.remove(0);
  let mut tail = lines.pop().unwrap();
  tail.append(&mut head);
  lines.push(tail);
  lines.append(other);
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SelectMode {
  Char(Pos),
  Line(Pos),
  Block(Pos),
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
  pub row: usize,
  pub col: usize,
}

impl Pos {
  /// make sure you clamp this
  pub const MAX: Self = Pos {
    row: usize::MAX,
    col: usize::MAX,
  };
  pub const MIN: Self = Pos {
		row: usize::MIN, // just in case we discover something smaller than '0'
		col: usize::MIN,
	};

	pub fn row_col_add(&self, row: isize, col: isize) -> Self {
		Self {
			row: self.row.saturating_add_signed(row),
			col: self.col.saturating_add_signed(col),
		}
	}

	pub fn col_add(&self, rhs: usize) -> Self {
		self.row_col_add(0, rhs as isize)
	}

	pub fn col_add_signed(&self, rhs: isize) -> Self {
		self.row_col_add(0, rhs)
	}

	pub fn col_sub(&self, rhs: usize) -> Self {
		self.row_col_add(0, -(rhs as isize))
	}

	pub fn row_add(&self, rhs: usize) -> Self {
		self.row_col_add(rhs as isize, 0)
	}

	pub fn row_sub(&self, rhs: usize) -> Self {
		self.row_col_add(-(rhs as isize), 0)
	}

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

#[derive(Debug, Copy, Clone)]
pub enum MotionKind {
	/// A flat range from one grapheme position to another
	/// `start` is not necessarily less than `end`. `start` in most cases
	/// is the cursor's position.
  Char {
    start: Pos,
    end: Pos,
    inclusive: bool,
  },
	/// A range of whole lines.
  Line {
    start: usize,
    end: usize,
		inclusive: bool
  },
  Block {
    start: Pos,
    end: Pos,
  },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
  pub pos: Pos,
  pub exclusive: bool,
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
    }
  }

  pub fn calculate(&mut self, input: &str) -> usize {
    self.depth = 0;
    self.ctx.clear();

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

    self.depth
  }
}

fn extract_range_contiguous(buf: &mut Vec<Line>, start: Pos, end: Pos) -> Vec<Line> {
  let start_col = start.col.min(buf[start.row].len());
  let end_col = end.col.min(buf[end.row].len());

  if start.row == end.row {
    // single line case
    let line = &mut buf[start.row];
    let removed: Vec<Grapheme> = line.0.drain(start_col..end_col).collect();
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
  pub hint: Option<Vec<Line>>,
  pub cursor: Cursor,

  pub select_mode: Option<SelectMode>,
  pub last_selection: Option<(SelectMode, Pos)>,

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
      hint: None,
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
  pub fn is_empty(&self) -> bool {
    self.lines.len() == 0 || (self.lines.len() == 1 && self.count_graphemes() == 0)
  }
  pub fn count_graphemes(&self) -> usize {
    self.lines.iter().map(|line| line.len()).sum()
  }
  #[track_caller]
  fn cur_line(&self) -> &Line {
    let caller = std::panic::Location::caller();
    log::trace!("cur_line called from {}:{}", caller.file(), caller.line());
    &self.lines[self.cursor.pos.row]
  }
  fn cur_line_mut(&mut self) -> &mut Line {
    &mut self.lines[self.cursor.pos.row]
  }
  fn line(&self, row: usize) -> &Line {
    &self.lines[row]
  }
  fn line_mut(&mut self, row: usize) -> &mut Line {
    &mut self.lines[row]
  }
	/// Takes an inclusive range of line numbers and returns an iterator over immutable borrows of those lines.
	fn line_iter(&mut self, start: usize, end: usize) -> impl Iterator<Item = &Line> {
		let (start,end) = ordered(start,end);
		self.lines.iter().take(end + 1).skip(start)
	}
	fn line_iter_mut(&mut self, start: usize, end: usize) -> impl Iterator<Item = &mut Line> {
		let (start,end) = ordered(start,end);
		self.lines.iter_mut().take(end + 1).skip(start)
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
    self.set_cursor(Pos {
      row,
      col: self.saved_col.unwrap_or(self.cursor.pos.col),
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
		let level = self.calc_indent_level();
    let mut rest = self.lines[row].split_off(col);
		let mut col = 0;
		for tab in std::iter::repeat_n(Grapheme::from('\t'), level) {
			rest.insert(0, tab);
			col += 1;
		}

    self.lines.insert(row + 1, rest);
    self.cursor.pos = Pos {
      row: row + 1,
      col,
    };
  }
  fn verb_shell_cmd(&mut self, cmd: &str) -> ShResult<()> {
    let mut vars = HashSet::new();
    vars.insert("_BUFFER".into());
    vars.insert("_CURSOR".into());
    vars.insert("_ANCHOR".into());
    let _guard = var_ctx_guard(vars);

    let mut buf = self.joined();
    let mut cursor = self.cursor_to_flat();
    let mut anchor = self.select_mode.map(|r| {
			match r {
				SelectMode::Char(pos) |
				SelectMode::Block(pos) |
				SelectMode::Line(pos) => {
					self.pos_to_flat(pos).to_string()
				}
			}
		}).unwrap_or_default();

    write_vars(|v| {
      v.set_var("_BUFFER", VarKind::Str(buf.clone()), VarFlags::EXPORT)?;
      v.set_var(
        "_CURSOR",
        VarKind::Str(cursor.to_string()),
        VarFlags::EXPORT,
      )?;
      v.set_var(
        "_ANCHOR",
        VarKind::Str(anchor.clone()),
        VarFlags::EXPORT,
      )
    })?;

    RawModeGuard::with_cooked_mode(|| exec_input(cmd.to_string(), None, true, Some("<ex-mode-cmd>".into())))?;

    let keys = write_vars(|v| {
      buf = v.take_var("_BUFFER");
      cursor = v.take_var("_CURSOR").parse().unwrap_or(cursor);
      anchor = v.take_var("_ANCHOR");
      v.take_var("_KEYS")
    });

    self.set_buffer(buf);
    self.set_cursor_from_flat(cursor);
		if let Ok(pos) = anchor.parse()
    && pos != cursor
		&& self.select_mode.is_some() {
			let new_pos = self.pos_from_flat(pos);
			match self.select_mode.as_mut() {
				Some(SelectMode::Line(pos)) |
				Some(SelectMode::Block(pos)) |
				Some(SelectMode::Char(pos)) => {
					*pos = new_pos
				}
				None => unreachable!()
			}
    }
    if !keys.is_empty() {
      write_meta(|m| m.set_pending_widget_keys(&keys))
    }
    Ok(())
  }
  fn insert_at(&mut self, pos: Pos, gr: Grapheme) {
    if gr.is_lf() {
      self.set_cursor(pos);
      self.break_line();
    } else {
      let row = pos.row;
      let col = pos.col;
      self.lines[row].insert(col, gr);
    }
  }
  fn insert(&mut self, gr: Grapheme) {
    self.insert_at(self.cursor.pos, gr);
  }
  fn insert_str(&mut self, s: &str) {
    for gr in s.graphemes(true) {
      let gr = Grapheme::from(gr);
      if gr.is_lf() {
        self.break_line();
      } else {
        self.insert(gr);
        self.cursor.pos.col += 1;
      }
    }
  }
  fn push_str(&mut self, s: &str) {
    let mut lines = to_lines(s);
    attach_lines(&mut self.lines, &mut lines);
  }
  fn push(&mut self, gr: Grapheme) {
    let last = self.lines.last_mut();
    if let Some(last) = last {
      last.0.push(gr);
    } else {
      self.lines.push(Line::from(vec![gr]));
    }
  }
  fn scan_forward<F: FnMut(&Grapheme) -> bool>(&self, f: F) -> Option<Pos> {
    self.scan_forward_from(self.cursor.pos, f)
  }
  fn scan_forward_from<F: FnMut(&Grapheme) -> bool>(&self, mut pos: Pos, mut f: F) -> Option<Pos> {
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
  fn scan_backward<F: FnMut(&Grapheme) -> bool>(&self, f: F) -> Option<Pos> {
    self.scan_backward_from(self.cursor.pos, f)
  }
  fn scan_backward_from<F: FnMut(&Grapheme) -> bool>(&self, mut pos: Pos, mut f: F) -> Option<Pos> {
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
    mut inclusive: bool,
  ) -> Option<MotionKind> {
    let mut target = self.cursor.pos;

    for _ in 0..count {
      match (to, dir) {
        (To::Start, Direction::Forward) => {
          target = self
            .word_motion_w(word, target, ignore_trailing_ws)
            .unwrap_or_else(|| {
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

    Some(MotionKind::Char {
      start: self.cursor.pos,
      end: target,
      inclusive,
    })
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
        if let Some((_, C::Whitespace)) = classes.peek() {
          // we are on whitespace. advance to the next non-ws char class
          return classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p);
        }

        let last_non_ws = classes.find(|(_, c)| c.is_ws());
        if ignore_trailing_ws {
          return last_non_ws.map(|(p, _)| p);
        }
        classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p)
      }
      Word::Normal => {
        if let Some((_, C::Whitespace)) = classes.peek() {
          // we are on whitespace. advance to the next non-ws char class
          return classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p);
        }

        // go forward until we find some char class that isnt this one
        let first_c = classes.next()?.1;

        match classes.find(|(_, c)| c.is_other_class_or_ws(&first_c))? {
          (pos, C::Whitespace) if ignore_trailing_ws => return Some(pos),
          (_, C::Whitespace) => { /* fall through */ }
          (pos, _) => return Some(pos),
        }

        // we found whitespace previously, look for the next non-whitespace char class
        classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p)
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
        let first_non_ws = if let Some((_, C::Whitespace)) = classes.peek() {
          // we use find() to advance the iterator as usual
          // but we can also be clever and use the question mark
          // to return early if we don't find a word backwards
          classes.find(|(_, c)| !c.is_ws())?
        } else {
          classes.next()?
        };

        // ok now we are off that whitespace
        // now advance backwards until we find more whitespace, or next() is None

        let mut last = first_non_ws;
        while let Some((_, c)) = classes.peek() {
          if c.is_ws() {
            break;
          }
          last = classes.next()?;
        }
        Some(last.0)
      }
      Word::Normal => {
        classes.next();
        let first_non_ws = if let Some((_, C::Whitespace)) = classes.peek() {
          classes.find(|(_, c)| !c.is_ws())?
        } else {
          classes.next()?
        };

        // ok, off the whitespace
        // now advance until we find any different char class at all
        let mut last = first_non_ws;
        while let Some((_, c)) = classes.peek() {
          if c.is_other_class(&last.1) {
            break;
          }
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
        let first_non_ws = if let Some((_, C::Whitespace)) = classes.peek() {
          classes.find(|(_, c)| !c.is_ws())?
        } else {
          classes.next()?
        };

        let mut last = first_non_ws;
        while let Some((_, c)) = classes.peek() {
          if c.is_ws() {
            return Some(last.0);
          }
          last = classes.next()?;
        }
        None
      }
      Word::Normal => {
        classes.next();
        let first_non_ws = if let Some((_, C::Whitespace)) = classes.peek() {
          classes.find(|(_, c)| !c.is_ws())?
        } else {
          classes.next()?
        };

        let mut last = first_non_ws;
        while let Some((_, c)) = classes.peek() {
          if c.is_other_class_or_ws(&first_non_ws.1) {
            return Some(last.0);
          }
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
          classes.find(|(_, c)| c.is_ws());
        }

        classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p)
      }
      Word::Normal => {
        classes.next();
        if let Some((_, C::Whitespace)) = classes.peek() {
          return classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p);
        }

        let cur_class = classes.peek()?.1;
        let bound = classes.find(|(_, c)| c.is_other_class(&cur_class))?;

        if bound.1.is_ws() {
          classes.find(|(_, c)| !c.is_ws()).map(|(p, _)| p)
        } else {
          Some(bound.0)
        }
      }
    }
  }
  fn char_classes_forward_from(&self, pos: Pos) -> impl Iterator<Item = (Pos, CharClass)> {
    CharClassIter::new(&self.lines, pos)
  }
  fn char_classes_forward(&self) -> impl Iterator<Item = (Pos, CharClass)> {
    self.char_classes_forward_from(self.cursor.pos)
  }
  fn char_classes_backward_from(&self, pos: Pos) -> impl Iterator<Item = (Pos, CharClass)> {
    CharClassIterRev::new(&self.lines, pos)
  }
  fn char_classes_backward(&self) -> impl Iterator<Item = (Pos, CharClass)> {
    self.char_classes_backward_from(self.cursor.pos)
  }
  fn end_pos(&self) -> Pos {
    let mut pos = Pos::MAX;
    pos.clamp_row(&self.lines);
    pos.clamp_col(&self.lines[pos.row].0, false);
    pos
  }
  fn dispatch_text_obj(&mut self, count: u16, obj: TextObj) -> Option<MotionKind> {
    match obj {
      // text structures
      TextObj::Word(word, bound) => self.text_obj_word(count, word, obj, bound),
      TextObj::Sentence(_) |
      TextObj::Paragraph(_) |
      TextObj::WholeSentence(_) |
      TextObj::Tag(_) |
      TextObj::Custom(_) |
      TextObj::WholeParagraph(_) => {
				log::warn!("{:?} text objects are not implemented yet", obj);
				None
			}

      // quote stuff
      TextObj::DoubleQuote(bound) |
			TextObj::SingleQuote(bound) |
			TextObj::BacktickQuote(bound) => {
        self.text_obj_quote(count, obj, bound)
      }

      // delimited blocks
      TextObj::Paren(bound)
      | TextObj::Bracket(bound)
      | TextObj::Brace(bound)
      | TextObj::Angle(bound) => self.text_obj_delim(count, obj, bound),
    }
  }
	fn text_obj_word(
		&mut self,
		count: u16,
		word: Word,
		obj: TextObj,
		bound: Bound,
	) -> Option<MotionKind> {
		use CharClass as C;
		let mut fwd_classes = self.char_classes_forward();
		let first_class = fwd_classes.next()?;
		match first_class {
			(pos,C::Whitespace) => {
				match bound {
					Bound::Inside => {
						let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
						let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
						let mut first = (pos,C::Whitespace);
						let mut last = (pos,C::Whitespace);
						while let Some((_,c)) = bkwd_classes.peek() {
							if !c.is_ws() {
								break;
							}
							first = bkwd_classes.next()?;
						}

						while let Some((_,c)) = fwd_classes.peek() {
							if !c.is_ws() {
								break;
							}
							last = fwd_classes.next()?;
						}

						Some(MotionKind::Char {
							start: first.0,
							end: last.0,
							inclusive: true
						})
					}
					Bound::Around => {
						let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
						let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
						let mut first = (pos,C::Whitespace);
						let mut last = (pos,C::Whitespace);
						while let Some((_,cl)) = bkwd_classes.peek() {
							if !cl.is_ws() {
								break;
							}
							first = bkwd_classes.next()?;
						}

						while let Some((_,cl)) = fwd_classes.peek() {
							if !cl.is_ws() {
								break;
							}
							last = fwd_classes.next()?;
						}
						let word_class = fwd_classes.next()?.1;
						while let Some((_,cl)) = fwd_classes.peek() {
							match word {
								Word::Big => {
									if cl.is_ws() {
										break
									}
								}
								Word::Normal => {
									if cl.is_other_class_or_ws(&word_class) {
										break
									}
								}
							}
							last = fwd_classes.next()?;
						}

						Some(MotionKind::Char {
							start: first.0,
							end: last.0,
							inclusive: true
						})
					}
				}
			}
			(pos, c) => {
				let break_cond = |cl: &C, c: &C| -> bool {
					match word {
						Word::Big => cl.is_ws(),
						Word::Normal => cl.is_other_class(c),
					}
				};
				match bound {
					Bound::Inside => {
						let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
						let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
						let mut first = (pos,c);
						let mut last = (pos,c);

						while let Some((_,cl)) = bkwd_classes.peek() {
							if break_cond(cl, &c) {
								break;
							}
							first = bkwd_classes.next()?;
						}

						while let Some((_,cl)) = fwd_classes.peek() {
							if break_cond(cl, &c) {
								break;
							}
							last = fwd_classes.next()?;
						}

						Some(MotionKind::Char {
							start: first.0,
							end: last.0,
							inclusive: true
						})
					}
					Bound::Around => {
						let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
						let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
						let mut first = (pos,c);
						let mut last = (pos,c);

						while let Some((_,cl)) = bkwd_classes.peek() {
							if break_cond(cl, &c) {
								break;
							}
							first = bkwd_classes.next()?;
						}

						while let Some((_,cl)) = fwd_classes.peek() {
							if break_cond(cl, &c) {
								break;
							}
							last = fwd_classes.next()?;
						}

						// Include trailing whitespace
						while let Some((_,cl)) = fwd_classes.peek() {
							if !cl.is_ws() {
								break;
							}
							last = fwd_classes.next()?;
						}

						Some(MotionKind::Char {
							start: first.0,
							end: last.0,
							inclusive: true
						})
					}
				}
			}
		}
	}
  fn text_obj_quote(
		&mut self,
		count: u16,
		obj: TextObj,
		bound: Bound,
	) -> Option<MotionKind> {
    let q_ch = match obj {
      TextObj::DoubleQuote(_) => '"',
      TextObj::SingleQuote(_) => '\'',
      TextObj::BacktickQuote(_) => '`',
      _ => unreachable!(),
    };

    let start_pos = self
      .scan_backward(|g| g.as_char() == Some(q_ch))
      .or_else(|| self.scan_forward(|g| g.as_char() == Some(q_ch)))?;

    let mut scan_start_pos = start_pos;
    scan_start_pos.col += 1;

    let mut end_pos = self.scan_forward_from(scan_start_pos, |g| g.as_char() == Some(q_ch))?;

    match bound {
      Bound::Around => {
        // Around for quoted structures is weird. We have to include any trailing whitespace in the range.
        end_pos.col += 1;
        let mut classes = self.char_classes_forward_from(end_pos);
        end_pos = classes
          .find(|(_, c)| !c.is_ws())
          .map(|(p, _)| p)
          .unwrap_or(self.end_pos());

        (start_pos <= end_pos).then_some(MotionKind::Char {
          start: start_pos,
          end: end_pos,
          inclusive: false,
        })
      }
      Bound::Inside => {
        let mut start_pos = start_pos;
        start_pos.col += 1;
        (start_pos <= end_pos).then_some(MotionKind::Char {
          start: start_pos,
          end: end_pos,
          inclusive: false,
        })
      }
    }
  }
  fn text_obj_delim(&mut self, count: u16, obj: TextObj, bound: Bound) -> Option<MotionKind> {
    let (opener, closer) = match obj {
      TextObj::Paren(_) => ('(', ')'),
      TextObj::Bracket(_) => ('[', ']'),
      TextObj::Brace(_) => ('{', '}'),
      TextObj::Angle(_) => ('<', '>'),
      _ => unreachable!(),
    };
    let mut depth = 0;
    let start_pos = self
      .scan_backward(|g| {
        if g.as_char() == Some(closer) {
          depth += 1;
        }
        if g.as_char() == Some(opener) {
          if depth == 0 {
            return true;
          }
          depth -= 1;
        }
        false
      })
      .or_else(|| self.scan_forward(|g| g.as_char() == Some(opener)))?;

    depth = 0;
    let end_pos = self.scan_forward_from(start_pos, |g| {
      if g.as_char() == Some(opener) {
        depth += 1;
      }
      if g.as_char() == Some(closer) {
        depth -= 1;
      }
      depth == 0
    })?;

    match bound {
      Bound::Around => Some(MotionKind::Char {
        start: start_pos,
        end: end_pos,
        inclusive: true,
      }),
      Bound::Inside => {
        let mut start_pos = start_pos;
        start_pos.col += 1;
        (start_pos <= end_pos).then_some(MotionKind::Char {
          start: start_pos,
          end: end_pos,
          inclusive: false,
        })
      }
    }
  }
	fn gr_at(&self, pos: Pos) -> Option<&Grapheme> {
		self.lines.get(pos.row)?.0.get(pos.col)
	}
	fn clamp_pos(&self, mut pos: Pos) -> Pos {
		pos.clamp_row(&self.lines);
		pos.clamp_col(&self.lines[pos.row].0, false);
		pos
	}
	fn number_at_cursor(&self) -> Option<(Pos,Pos)> {
		self.number_at(self.cursor.pos)
	}
	/// Returns the start/end span of a number at a given position, if any
	fn number_at(&self, mut pos: Pos) -> Option<(Pos,Pos)> {
		let is_number_char = |gr: &Grapheme| gr.as_char().is_some_and(|c| c == '.' || c == '-' || c.is_ascii_digit());
		let is_digit = |gr: &Grapheme| gr.as_char().is_some_and(|c| c.is_ascii_digit());

		pos = self.clamp_pos(pos);
		if !is_number_char(self.gr_at(pos)?) {
			return None;
		}

		// If cursor is on '-', advance to the first digit
		if self.gr_at(pos)?.as_char() == Some('-') {
			pos = pos.col_add(1);
		}

		let mut start = self.scan_backward_from(pos, |g| !is_digit(g))
			.map(|pos| Pos { row: pos.row, col: pos.col + 1 })
			.unwrap_or(Pos::MIN);
		let end = self.scan_forward_from(pos, |g| !is_digit(g))
			.map(|pos| Pos { row: pos.row, col: pos.col.saturating_sub(1) })
			.unwrap_or(Pos { row: pos.row, col: self.lines[pos.row].len().saturating_sub(1) });

		if start > Pos::MIN && self.lines[start.row][start.col.saturating_sub(1)].as_char() == Some('-') {
			start.col -= 1;
		}

		Some((start, end))
	}
	fn adjust_number(&mut self, inc: i64) -> Option<()> {
		let (s,e) = if let Some(range) = self.select_range() {
			match range {
				Motion::CharRange(s, e) => (s,e),
				_ => return None,
			}
		} else if let Some((s,e)) = self.number_at_cursor() {
			(s,e)
		} else {
			return None;
		};

		let word = self.pos_slice_str(s,e);

		let num_fmt = if word.starts_with("0x") {
			let body = word.strip_prefix("0x").unwrap();
			let width = body.len();
			let num = i64::from_str_radix(body, 16).ok()?;
			let new_num = num + inc;
			format!("0x{new_num:0>width$x}")
		} else if word.starts_with("0b") {
			let body = word.strip_prefix("0b").unwrap();
			let width = body.len();
			let num = i64::from_str_radix(body, 2).ok()?;
			let new_num = num + inc;
			format!("0b{new_num:0>width$b}")
		} else if word.starts_with("0o") {
			let body = word.strip_prefix("0o").unwrap();
			let width = body.len();
			let num = i64::from_str_radix(body, 8).ok()?;
			let new_num = num + inc;
			format!("0o{new_num:0>width$o}")
		} else if let Ok(num) = word.parse::<i64>() {
			let width = word.len();
			let new_num = num + inc;
			if new_num < 0 {
				let abs = new_num.unsigned_abs();
				let digit_width = if num < 0 { width - 1 } else { width };
				format!("-{abs:0>digit_width$}")
			} else if num < 0 {
				let digit_width = width - 1;
				format!("{new_num:0>digit_width$}")
			} else {
				format!("{new_num:0>width$}")
			}
		} else { return None };

		self.replace_range(s, e, &num_fmt);
		self.cursor.pos.col -= 1;
		Some(())
	}
	fn replace_range(&mut self, s: Pos, e: Pos, new: &str) -> Vec<Line> {
		let motion = MotionKind::Char { start: s, end: e, inclusive: true };
		let content = self.extract_range(&motion);
		self.set_cursor(s);
		self.insert_str(new);
		content
	}
	fn pos_slice_str(&self, s: Pos, e: Pos) -> String {
		let (s,e) = ordered(s,e);
		if s.row == e.row {
			self.lines[s.row].0[s.col..=e.col]
				.iter()
				.map(|g| g.to_string())
				.collect()
		} else {
			let mut result = String::new();
			// First line from s.col to end
			for g in &self.lines[s.row].0[s.col..] {
				result.push_str(&g.to_string());
			}
			// Middle lines
			for line in &self.lines[s.row + 1..e.row] {
				result.push('\n');
				result.push_str(&line.to_string());
			}
			// Last line from start to e.col
			result.push('\n');
			for g in &self.lines[e.row].0[..=e.col] {
				result.push_str(&g.to_string());
			}
			result
		}
	}
	fn find_delim_match(&mut self) -> Option<MotionKind> {
		let is_opener = |g: &Grapheme| matches!(g.as_char(), Some(c) if "([{<".contains(c));
		let is_closer = |g: &Grapheme| matches!(g.as_char(), Some(c) if ")]}>".contains(c));
		let is_delim = |g: &Grapheme| is_opener(g) || is_closer(g);
		let first = self.scan_forward(is_delim)?;

		let delim_match = if is_closer(self.gr_at(first)?) {
			let opener = match self.gr_at(first)?.as_char()? {
				')' => '(',
				']' => '[',
				'}' => '{',
				'>' => '<',
				_ => unreachable!(),
			};
			self.scan_backward_from(first, |g| g.as_char() == Some(opener))?
		} else if is_opener(self.gr_at(first)?) {
			let closer = match self.gr_at(first)?.as_char()? {
				'(' => ')',
				'[' => ']',
				'{' => '}',
				'<' => '>',
				_ => unreachable!(),
			};
			self.scan_forward_from(first, |g| g.as_char() == Some(closer))?
		} else { unreachable!() };

		Some(MotionKind::Char {
			start: self.cursor.pos,
			end: delim_match,
			inclusive: true,
		})
	}
	/// Wrapper for eval_motion_inner that calls it with `check_hint: false`
  fn eval_motion(&mut self, cmd: &ViCmd) -> Option<MotionKind> {
    self.eval_motion_inner(cmd, false)
  }
  fn eval_motion_inner(&mut self, cmd: &ViCmd, check_hint: bool) -> Option<MotionKind> {
    let ViCmd { verb, motion, .. } = cmd;
    let MotionCmd(count, motion) = motion.as_ref()?;
    let buffer = self.lines.clone();
    if let Some(mut hint) = self.hint.clone() {
      attach_lines(&mut self.lines, &mut hint);
    }

    let kind = match motion {
      Motion::WholeLine => {
				let start = self.row();
        let end = (self.row() + (count.saturating_sub(1))).min(self.lines.len().saturating_sub(1));
        Some(MotionKind::Line {
          start,
          end,
					inclusive: true
        })
      }
      Motion::TextObj(text_obj) => self.dispatch_text_obj(*count as u16, text_obj.clone()),
      Motion::EndOfLastWord => {
        let row = self.row() + (count.saturating_sub(1));
        let line = self.line_mut(row);
        let mut target = Pos { row, col: 0 };
        for (i, gr) in line.0.iter().enumerate() {
          if !gr.is_ws() {
            target.col = i;
          }
        }

        (target != self.cursor.pos).then_some(MotionKind::Char {
          start: self.cursor.pos,
          end: target,
          inclusive: true,
        })
      }
      Motion::StartOfFirstWord => {
        let mut target = Pos {
          row: self.row(),
          col: 0,
        };
        let line = self.cur_line();
        for (i, gr) in line.0.iter().enumerate() {
          target.col = i;
          if !gr.is_ws() {
            break;
          }
        }

        (target != self.cursor.pos).then_some(MotionKind::Char {
          start: self.cursor.pos,
          end: target,
          inclusive: true,
        })
      }
      dir @ (Motion::StartOfLine | Motion::EndOfLine) => {
        let (inclusive,off) = match dir {
          Motion::StartOfLine => (false,isize::MIN),
          Motion::EndOfLine => (true,isize::MAX),
          _ => unreachable!(),
        };
        let target = self.offset_cursor(0, off);
        (target != self.cursor.pos).then_some(MotionKind::Char {
          start: self.cursor.pos,
          end: target,
          inclusive,
        })
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
        (target != self.cursor.pos).then_some(MotionKind::Char {
          start: self.cursor.pos,
          end: target,
          inclusive: true,
        })
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

        (target != self.cursor.pos).then_some(MotionKind::Char {
          start: self.cursor.pos,
          end: target,
          inclusive: false,
        })
      }
      dir @ (Motion::LineDown | Motion::LineUp) => {
        let off = match dir {
          Motion::LineUp => -(*count as isize),
          Motion::LineDown => *count as isize,
          _ => unreachable!(),
        };
        if verb.is_some() {
          let row = self.row();
          let target_row = self.offset_row(off);
          let (s, e) = ordered(row, target_row);
          Some(MotionKind::Line { start: s, end: e, inclusive: true })
        } else {
          if self.saved_col.is_none() {
            self.saved_col = Some(self.cursor.pos.col);
          }
          let row = self.offset_row(off);
          let limit = if self.cursor.exclusive {
            self.lines[row].len().saturating_sub(1)
          } else {
            self.lines[row].len()
          };
          let col = self.saved_col.unwrap().min(limit);
          let target = Pos { row, col };
          (target != self.cursor.pos).then_some(MotionKind::Char {
            start: self.cursor.pos,
            end: target,
            inclusive: true,
          })
        }
      }
      dir @ (Motion::EndOfBuffer | Motion::StartOfBuffer) => {
        let off = match dir {
          Motion::StartOfBuffer => isize::MIN,
          Motion::EndOfBuffer => isize::MAX,
          _ => unreachable!(),
        };
        if verb.is_some() {
          let row = self.row();
          let target_row = self.offset_row(off);
          let (s, e) = ordered(row, target_row);
          Some(MotionKind::Line { start: s, end: e, inclusive: false })
        } else {
          let target = self.offset_cursor(off, 0);
          (target != self.cursor.pos).then_some(MotionKind::Char {
            start: self.cursor.pos,
            end: target,
            inclusive: true,
          })
        }
      }
      Motion::WholeBuffer => Some(MotionKind::Line {
        start: 0,
        end: self.lines.len().saturating_sub(1),
				inclusive: false
      }),
      Motion::ToColumn => {
				let row = self.row();
				let end = Pos { row, col: count.saturating_sub(1) };
				Some(MotionKind::Char { start: self.cursor.pos, end, inclusive: end > self.cursor.pos })
			}

      Motion::ToDelimMatch => self.find_delim_match(),
      Motion::ToBracket(direction) |
      Motion::ToParen(direction) |
      Motion::ToBrace(direction) => {
				let (opener,closer) = match motion {
					Motion::ToBracket(_) => ('[', ']'),
					Motion::ToParen(_) => ('(', ')'),
					Motion::ToBrace(_) => ('{', '}'),
					_ => unreachable!(),
				};
				match direction {
					Direction::Forward => {
						let mut depth = 0;
						let target_pos = self.scan_forward(|g| {
							if g.as_char() == Some(opener) { depth += 1; }
							if g.as_char() == Some(closer) {
								depth -= 1;
								if depth <= 0 {
									return true;
								}
							}
							false
						})?;
						return Some(MotionKind::Char {
							start: self.cursor.pos,
							end: target_pos,
							inclusive: true,
						})
					}
					Direction::Backward => {
						let mut depth = 0;
						let target_pos = self.scan_backward(|g| {
							if g.as_char() == Some(closer) { depth += 1; }
							if g.as_char() == Some(opener) {
								depth -= 1;
								if depth <= 0 {
									return true;
								}
							}
							false
						})?;
						return Some(MotionKind::Char {
							start: self.cursor.pos,
							end: target_pos,
							inclusive: true,
						});
					}
				}
			}

      Motion::CharRange(s, e) => {
        let (s, e) = ordered(*s, *e);
        Some(MotionKind::Char {
          start: s,
          end: e,
          inclusive: true,
        })
      }
      Motion::LineRange(s, e) => {
        let (s, e) = ordered(*s, *e);
        Some(MotionKind::Line { start: s, end: e, inclusive: false })
      }
      Motion::BlockRange(s, e) => {
        let (s, e) = ordered(*s, *e);
        Some(MotionKind::Block { start: s, end: e })
      }
      Motion::RepeatMotion |
      Motion::RepeatMotionRev => unreachable!("Repeat motions should have been resolved in readline/mod.rs"),
      Motion::Global(val) |
      Motion::NotGlobal(val) => {
				log::warn!("Global motions are not implemented yet (val: {:?})", val);
				None
			}
      Motion::Null => None,
    };

    self.lines = buffer;
    kind
  }
	fn move_to_start(&mut self, motion: MotionKind) {
		match motion {
			MotionKind::Char { start, end, .. } => {
				let (s,_) = ordered(start, end);
				self.set_cursor(s);
			}
			MotionKind::Line { start, end, .. } => {
				let (s,_) = ordered(start, end);
				self.set_cursor(Pos { row: s, col: 0 });
			}
			MotionKind::Block { start, end } => todo!(),
		}
	}
	/// Wrapper for apply_motion_inner that calls it with `accept_hint: false`
  fn apply_motion(&mut self, motion: MotionKind) -> ShResult<()> {
    self.apply_motion_inner(motion, false)
  }
  fn apply_motion_inner(&mut self, motion: MotionKind, accept_hint: bool) -> ShResult<()> {
    match motion {
      MotionKind::Char { end, .. } => {
        if accept_hint && self.has_hint() && end >= self.end_pos() {
          self.accept_hint_to(end);
        } else {
          self.set_cursor(end);
        }
      }
      MotionKind::Line { start, .. } => {
        self.set_row(start);
      }
      MotionKind::Block { start, end } => todo!(),
    }
    Ok(())
  }
  fn extract_range(&mut self, motion: &MotionKind) -> Vec<Line> {
    let extracted = match motion {
      MotionKind::Char {
        start,
        end,
        inclusive,
      } => {
        let (s, e) = ordered(*start, *end);
        let end = if *inclusive {
          Pos {
            row: e.row,
            col: e.col + 1,
          }
        } else {
          e
        };
        let mut buf = std::mem::take(&mut self.lines);
        let extracted = extract_range_contiguous(&mut buf, s, end);
        self.lines = buf;
        extracted
      }
      MotionKind::Line { start, end, inclusive } => {
				let end = if *inclusive {
					*end
				} else {
					end.saturating_sub(1)
				};
				self.lines.drain(*start..=end).collect()
			}
      MotionKind::Block { start, end } => {
        let (s, e) = ordered(*start, *end);
        (s.row..=e.row)
          .map(|row| {
            let sc = s.col.min(self.lines[row].len());
            let ec = (e.col + 1).min(self.lines[row].len());
            Line(self.lines[row].0.drain(sc..ec).collect())
          })
          .collect()
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
	pub fn calc_indent_level(&mut self) -> usize {
		self.calc_indent_level_for_pos(self.cursor.pos)
	}
	pub fn calc_indent_level_for_pos(&mut self, pos: Pos) -> usize {
		let mut lines = self.lines.clone();
		split_lines_at(&mut lines, pos);
		let raw = join_lines(&lines);

		self.indent_ctx.calculate(&raw)
	}
  fn motion_mutation(&mut self, motion: MotionKind, f: impl Fn(&Grapheme) -> Grapheme) {
    match motion {
      MotionKind::Char {
        start,
        end,
        inclusive,
      } => {
        let (s, e) = ordered(start, end);
        if s.row == e.row {
          let range = if inclusive {
            s.col..e.col + 1
          } else {
            s.col..e.col
          };
          for col in range {
            if col >= self.lines[s.row].len() {
              break;
            }
            self.lines[s.row][col] = f(&self.lines[s.row][col]);
          }
          return;
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
      MotionKind::Line { start, end, inclusive } => {
				let end = if inclusive { end } else { end.saturating_sub(1) };
        let end = end.min(self.lines.len().saturating_sub(1));
        for row in start..=end {
          let line = self.line_mut(row);
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
      if first {
        first = false
      } else {
        self.cursor.pos = self.offset_cursor(0, 1);
      }
      let pos = self.cursor.pos;
      let motion = MotionKind::Char {
        start: pos,
        end: pos,
        inclusive: true,
      };
      self.motion_mutation(motion, &f);
    }
  }
  fn exec_verb(&mut self, cmd: &ViCmd) -> ShResult<()> {
    let ViCmd {
      register,
      verb,
      motion,
      ..
    } = cmd;
    let Some(VerbCmd(_, verb)) = verb else {
      // For verb-less motions in insert mode, merge hint before evaluating
      // so motions like `w` can see into the hint text
      let result = self.eval_motion_inner(cmd, true);
      if let Some(motion_kind) = result {
        self.apply_motion_inner(motion_kind, true)?;
      }
      return Ok(());
    };
    let count = motion.as_ref().map(|m| m.0).unwrap_or(1);

    match verb {
      Verb::Delete | Verb::Change | Verb::Yank => {
        let Some(motion) = self.eval_motion(cmd) else {
          return Ok(());
        };
        let content = if *verb == Verb::Yank {
          self.yank_range(&motion)
				} else if *verb == Verb::Change && matches!(motion, MotionKind::Line {..}) {
					let n_lines = self.lines.len();
          let content = self.delete_range(&motion);
					let row = self.row();
					if n_lines > 1 {
						self.lines.insert(row, Line::default());
					}
					content
        } else {
          self.delete_range(&motion)
        };
        let reg_content = match &motion {
          MotionKind::Char { .. } => RegisterContent::Span(content),
          MotionKind::Line { .. } => RegisterContent::Line(content),
          MotionKind::Block { .. } => RegisterContent::Block(content),
        };
        register.write_to_register(reg_content);

        match motion {
          MotionKind::Char { start, end, .. } => {
            let (s, _) = ordered(start, end);
            self.set_cursor(s);
          }
          MotionKind::Line { start, end, inclusive } => {
						let end = if inclusive { end } else { end.saturating_sub(1) };
            let (s, _) = ordered(start, end);
            self.set_row(s);
						if *verb == Verb::Change {
							// we've gotta indent
							let level = self.calc_indent_level();
							let line = self.cur_line_mut();
							let mut col = 0;
							for tab in std::iter::repeat_n(Grapheme::from('\t'), level) {
								line.0.insert(col, tab);
								col += 1;
							}
							self.cursor.pos = self.offset_cursor(0, col as isize);
						}
          }
          MotionKind::Block { start, .. } => {
            let (s, _) = ordered(self.cursor.pos, start);
            self.set_cursor(s);
          }
        }
      }
      Verb::Rot13 => {
        let Some(motion) = self.eval_motion(cmd) else {
          return Ok(());
        };
        self.motion_mutation(motion, |gr| {
          gr.as_char()
            .map(rot13_char)
            .map(Grapheme::from)
            .unwrap_or_else(|| gr.clone())
        });
				self.move_to_start(motion);
      }
      Verb::ReplaceChar(ch) => {
        let Some(motion) = self.eval_motion(cmd) else {
          return Ok(());
        };
        self.motion_mutation(motion, |_| Grapheme::from(*ch));
				self.move_to_start(motion);
      }
      Verb::ReplaceCharInplace(ch, count) => self.inplace_mutation(*count, |_| Grapheme::from(*ch)),
      Verb::ToggleCaseInplace(count) => {
        self.inplace_mutation(*count, |gr| {
          gr.as_char()
            .map(toggle_case_char)
            .map(Grapheme::from)
            .unwrap_or_else(|| gr.clone())
        });
				self.cursor.pos = self.cursor.pos.col_add(1);
      }
      Verb::ToggleCaseRange => {
        let Some(motion) = self.eval_motion(cmd) else {
          return Ok(());
        };
        self.motion_mutation(motion, |gr| {
          gr.as_char()
            .map(toggle_case_char)
            .map(Grapheme::from)
            .unwrap_or_else(|| gr.clone())
        });
				self.move_to_start(motion);
      }
      Verb::IncrementNumber(n) => { self.adjust_number(*n as i64); },
      Verb::DecrementNumber(n) => { self.adjust_number(-(*n as i64)); },
      Verb::ToLower => {
        let Some(motion) = self.eval_motion(cmd) else {
          return Ok(());
        };
        self.motion_mutation(motion, |gr| {
          gr.as_char()
            .map(|c| c.to_ascii_lowercase())
            .map(Grapheme::from)
            .unwrap_or_else(|| gr.clone())
        });
				self.move_to_start(motion);
      }
      Verb::ToUpper => {
        let Some(motion) = self.eval_motion(cmd) else {
          return Ok(());
        };
        self.motion_mutation(motion, |gr| {
          gr.as_char()
            .map(|c| c.to_ascii_uppercase())
            .map(Grapheme::from)
            .unwrap_or_else(|| gr.clone())
        });
				self.move_to_start(motion);
      }
      Verb::Undo => {
        if let Some(edit) = self.undo_stack.pop() {
          self.lines = edit.old.clone();
          self.cursor.pos = edit.old_cursor;
          self.redo_stack.push(edit);
        }
      }
      Verb::Redo => {
        if let Some(edit) = self.redo_stack.pop() {
          self.lines = edit.new.clone();
          self.cursor.pos = edit.new_cursor;
          self.undo_stack.push(edit);
        }
      }
      Verb::Put(anchor) => {
        let Some(content) = register.read_from_register() else {
          return Ok(());
        };
        match content {
          RegisterContent::Span(lines) => {
						let move_cursor = lines.len() == 1 && lines[0].len() > 1;
						let content_len: usize = lines.iter().map(|l| l.len()).sum();
            let row = self.row();
            let col = match anchor {
              Anchor::After => (self.col() + 1).min(self.cur_line().len()),
              Anchor::Before => self.col(),
            };
						let start_len = self.lines[row].len();
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

						let end_len = self.lines[row].len();
						let mut delta = end_len.saturating_sub(start_len);
						if let Anchor::Before = anchor { delta = delta.saturating_sub(1); }
						if move_cursor {
							self.cursor.pos = self.offset_cursor(0, delta as isize);
						} else if content_len > 1 || *anchor == Anchor::After {
							self.cursor.pos = self.offset_cursor(0, 1);
						}
          }
          RegisterContent::Line(lines) => {
            let row = match anchor {
              Anchor::After => self.row() + 1,
              Anchor::Before => self.row(),
            };
            for (i, line) in lines.iter().cloned().enumerate() {
              self.lines.insert(row + i, line);
              self.set_row(row + i);
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

					let level = self.calc_indent_level_for_pos(Pos { row: target, col: 0 });
					let line = self.line_mut(target);
					let mut col = 0;
					for tab in std::iter::repeat_n(Grapheme::from('\t'), level) {
						line.insert(0, tab);
						col += 1;
					}

          self.cursor.pos = Pos {
            row: row + 1,
            col,
          };
        }
        Anchor::Before => {
          let row = self.row();
          self.lines.insert(row, Line::default());
          self.cursor.pos = Pos { row, col: 0 };
        }
      },
      Verb::SwapVisualAnchor => {
        let cur_pos = self.cursor.pos;
        let new_anchor;
        {
          let Some(select) = self.select_mode.as_mut() else {
            return Ok(());
          };
          match select {
            SelectMode::Block(select_anchor)
            | SelectMode::Line(select_anchor)
            | SelectMode::Char(select_anchor) => {
              new_anchor = *select_anchor;
              *select_anchor = cur_pos;
            }
          }
        }

        self.set_cursor(new_anchor);
      }
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
      Verb::InsertChar(ch) => {
				let level = self.calc_indent_level();
        self.insert(Grapheme::from(*ch));
        if let Some(motion) = self.eval_motion(cmd) {
          self.apply_motion(motion)?;
        }
				let new_level = self.calc_indent_level();
				if new_level < level {
					let delta = level - new_level;
					let line = self.cur_line_mut();
					for _ in 0..delta {
						if line.0.first().is_some_and(|c| c.as_char() == Some('\t')) {
							line.0.remove(0);
						} else {
							break
						}
					}
				}
      }
      Verb::Insert(s) => self.insert_str(s),
      Verb::Indent | Verb::Dedent => {
				let Some(motion) = self.eval_motion(cmd) else {
					return Ok(());
				};
				let (s, e) = match motion {
					MotionKind::Char { start, end, .. } => ordered(start.row, end.row),
					MotionKind::Line { start, end, .. } => ordered(start, end),
					MotionKind::Block { .. } => todo!(),
				};
				let mut col_offset = 0;
				for line in self.line_iter_mut(s, e) {
					match verb {
						Verb::Indent => {
							line.insert(0, Grapheme::from('\t'));
							col_offset += 1;
						}
						Verb::Dedent => {
							if line.0.first().is_some_and(|c| c.as_char() == Some('\t')) {
								line.0.remove(0);
								col_offset -= 1;
							}
						}
						_ => unreachable!(),
					}
				}
				self.cursor.pos = self.cursor.pos.col_add_signed(col_offset)
			}
      Verb::Equalize => {
				let Some(motion) = self.eval_motion(cmd) else {
					return Ok(())
				};
				let (s,e) = match motion {
					MotionKind::Char { start, end, inclusive } => ordered(start.row, end.row),
					MotionKind::Line { start, end, inclusive } => ordered(start, end),
					MotionKind::Block { start, end } => todo!(),
				};
				for row in s..=e {
					let line_len = self.line(row).len();

					// we are going to calculate the level twice, once at column = 0 and once at column = line.len()
					// "b-b-b-b-but the performance" i dont care
					// the number of tabs we use for the line is the lesser of these two calculations
					// if level_start > level_end, the line has an closer
					// if level_end > level_start, the line has a opener
					let level_start = self.calc_indent_level_for_pos(Pos { row, col: 0 });
					let level_end = self.calc_indent_level_for_pos(Pos { row, col: line_len });
					let num_tabs = level_start.min(level_end);

					let line = self.line_mut(row);
					while line.0.first().is_some_and(|c| c.as_char() == Some('\t')) {
						line.0.remove(0);
					}
					for tab in std::iter::repeat_n(Grapheme::from('\t'), num_tabs) {
						line.insert(0, tab);
					}
				}
			}
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
          let Ok(contents) = std::fs::read_to_string(path_buf) else {
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
				if read_vars(|v| v.try_get_var("EDITOR")).is_none() {
					write_meta(|m| {
						m.post_system_message(
							"$EDITOR is unset. Aborting edit.".into(),
						)
					});
				} else {
					let input = format!("$EDITOR {}", path.display());
					exec_input(input, None, true, Some("ex edit".into()))?;
				}
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
        let Some(motion_kind) = self.eval_motion_inner(cmd, true) else {
          return Ok(());
        };
        self.apply_motion_inner(motion_kind, true)?;
      }
      Verb::Normal(_)
      | Verb::Substitute(..)
      | Verb::RepeatSubstitute
      | Verb::Quit
      | Verb::RepeatGlobal => {
        log::warn!("Verb {:?} is not implemented yet", verb);
      }
      Verb::RepeatLast => unreachable!("Verb::RepeatLast should be handled in readline/mod.rs"),
    }

    Ok(())
  }
  pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
    let is_char_insert = cmd.verb.as_ref().is_some_and(|v| v.1.is_char_insert());
    let starts_merge = cmd.verb.as_ref().is_some_and(|v| matches!(v.1, Verb::Change));
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

    // Execute the command
    let res = self.exec_verb(&cmd);

    if self.is_empty() {
      self.set_hint(None);
    }

    let new_cursor = self.cursor.pos;

    // Stop merging on any non-char-insert command, even if buffer didn't change
    if !is_char_insert && !is_undo_op {
      if let Some(edit) = self.undo_stack.last_mut() {
        edit.merging = false;
      }
    }

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
        self.handle_edit(before, new_cursor, old_cursor);
        // Change starts a new merge chain so subsequent InsertChars merge into it
        if starts_merge {
          if let Some(edit) = self.undo_stack.last_mut() {
            edit.merging = true;
          }
        }
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

  pub fn fix_cursor(&mut self) {
    if self.cursor.pos.row >= self.lines.len() {
      self.cursor.pos.row = self.lines.len().saturating_sub(1);
    }
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
    let joined = self.joined();
    self.hint = hint
      .and_then(|h| h.strip_prefix(&joined).map(|s| s.to_string()))
      .and_then(|h| (!h.is_empty()).then_some(to_lines(h)));
  }

  /// Compat shim: returns true if there is a non-empty hint.
  pub fn has_hint(&self) -> bool {
    self
      .hint
      .as_ref()
      .is_some_and(|h| !h.is_empty() && h.iter().any(|l| !l.is_empty()))
  }

  /// Compat shim: get hint text as a string.
  pub fn get_hint_text(&self) -> String {
    let text = self.get_hint_text_raw();
    let text = format!("\x1b[90m{text}\x1b[0m");

    text.replace("\n", "\n\x1b[90m")
  }

  pub fn get_hint_text_raw(&self) -> String {
    let mut lines = vec![];
    let mut hint = self.hint.clone().unwrap_or_default();
    trim_lines(&mut hint);
    for line in hint {
      lines.push(line.to_string());
    }
    lines.join("\n")
  }

  /// Accept hint text up to a given target position.
  /// Temporarily merges the hint into the buffer, moves the cursor to target,
  /// then splits: everything from cursor onward becomes the new hint.
  fn accept_hint_to(&mut self, target: Pos) {
    let Some(mut hint) = self.hint.take() else {
      self.set_cursor(target);
      return;
    };
    attach_lines(&mut self.lines, &mut hint);
    let split_col = if self.cursor.exclusive {
      target.col + 1
    } else {
      target.col
    };

    // Split after the target position so the char at target
    // becomes part of the buffer (w lands ON the next word start)
    let split_pos = Pos {
      row: target.row,
      col: target.col + 1,
    };
    // Clamp to buffer bounds
    let split_pos = Pos {
      row: split_pos.row.min(self.lines.len().saturating_sub(1)),
      col: split_pos
        .col
        .min(self.lines[split_pos.row.min(self.lines.len().saturating_sub(1))].len()),
    };

    let new_hint = split_lines_at(&mut self.lines, split_pos);
    self.hint =
      (!new_hint.is_empty() && new_hint.iter().any(|l| !l.is_empty())).then_some(new_hint);
    self.set_cursor(target);
  }

  /// Compat shim: accept the current hint by appending it to the buffer.
  pub fn accept_hint(&mut self) {
    let hint_str = self.get_hint_text_raw();
    if hint_str.is_empty() {
      return;
    }
    self.push_str(&hint_str);
    self.set_cursor(Pos::MAX);
    self.fix_cursor();
    self.hint = None;
  }

  /// Compat shim: return a constructor that sets initial buffer contents and cursor.
  pub fn with_initial(mut self, s: &str, cursor_pos: usize) -> Self {
    self.set_buffer(s.to_string());
    // In the flat model, cursor_pos was a flat offset. Map to col on row .
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
    let max = if self.cursor.exclusive {
      self.lines[last_row].len().saturating_sub(1)
    } else {
      self.lines[last_row].len()
    };
    self.cursor.pos.row == last_row && self.cursor.pos.col >= max
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
    self.select_mode = Some(SelectMode::Char(self.cursor.pos));
  }

  pub fn start_line_select(&mut self) {
    self.select_mode = Some(SelectMode::Line(self.cursor.pos));
  }

  pub fn start_block_select(&mut self) {
    self.select_mode = Some(SelectMode::Block(self.cursor.pos));
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

  pub fn select_range(&self) -> Option<Motion> {
    let mode = self.select_mode.as_ref()?;
    match mode {
      SelectMode::Char(pos) => {
        let (s, e) = ordered(self.cursor.pos, *pos);
        Some(Motion::CharRange(s, e))
      }
      SelectMode::Line(pos) => {
        let (s, e) = ordered(self.row(), pos.row);
        Some(Motion::LineRange(s, e))
      }
      SelectMode::Block(pos) => {
        let (s, e) = ordered(self.cursor.pos, *pos);
        Some(Motion::BlockRange(s, e))
      }
    }
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

	fn pos_from_flat(&self, mut flat: usize) -> Pos {
		for (i, line) in self.lines.iter().enumerate() {
			if flat <= line.len() {
				return Pos { row: i, col: flat };
			}
			flat = flat.saturating_sub(line.len() + 1); // +1 for '\n'
		}
		// If we exceed the total length, clamp to end
		let last_row = self.lines.len().saturating_sub(1);
		let last_col = self.lines[last_row].len();
		Pos { row: last_row, col: last_col }
	}

	pub fn cursor_to_flat(&self) -> usize {
		self.pos_to_flat(self.cursor.pos)
	}

	pub fn set_cursor_from_flat(&mut self, flat: usize) {
		self.cursor.pos = self.pos_from_flat(flat);
		self.fix_cursor();
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

  /// Compat shim: mark where insert mode started.
  pub fn mark_insert_mode_start_pos(&mut self) {
    self.insert_mode_start_pos = Some(self.cursor.pos);
  }

  /// Compat shim: clear insert mode start position.
  pub fn clear_insert_mode_start_pos(&mut self) {
    self.insert_mode_start_pos = None;
  }
}

impl Display for LineBuf {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    if let Some(select) = self.select_mode.as_ref() {
      let mut cloned = self.lines.clone();

      match select {
        SelectMode::Char(pos) => {
          let (s, e) = ordered(self.cursor.pos, *pos);
          if s.row == e.row {
            // Same line: insert end first to avoid shifting start index
            let line = &mut cloned[s.row];
            if e.col + 1 >= line.len() {
              line.push_char(markers::VISUAL_MODE_END);
            } else {
              line.insert(e.col + 1, markers::VISUAL_MODE_END.into());
            }
            line.insert(s.col, markers::VISUAL_MODE_START.into());
          } else {
            // Start line: highlight from s.col to end
            cloned[s.row].insert(s.col, markers::VISUAL_MODE_START.into());
            cloned[s.row].push_char(markers::VISUAL_MODE_END);

            // Middle lines: fully highlighted
            for row in cloned.iter_mut().skip(s.row + 1).take(e.row - s.row - 1) {
              row.insert(0, markers::VISUAL_MODE_START.into());
              row.push_char(markers::VISUAL_MODE_END);
            }

            // End line: highlight from start to e.col
            let end_line = &mut cloned[e.row];
            if e.col + 1 >= end_line.len() {
              end_line.push_char(markers::VISUAL_MODE_END);
            } else {
              end_line.insert(e.col + 1, markers::VISUAL_MODE_END.into());
            }
            end_line.insert(0, markers::VISUAL_MODE_START.into());
          }
        }
        SelectMode::Line(pos) => {
          let (s, e) = ordered(self.row(), pos.row);
          for row in cloned.iter_mut().take(e + 1).skip(s) {
            row.insert(0, markers::VISUAL_MODE_START.into());
          }
          cloned[e].push_char(markers::VISUAL_MODE_END);
        }
        SelectMode::Block(pos) => todo!(),
      }
      let mut lines = vec![];
      for line in &cloned {
        lines.push(line.to_string());
      }
      let joined = lines.join("\n");
      write!(f, "{joined}")
    } else {
      write!(f, "{}", self.joined())
    }
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
    Pos {
      row: self.row,
      col: self.col,
    }
  }
}

impl<'a> Iterator for CharClassIter<'a> {
  type Item = (Pos, CharClass);
  fn next(&mut self) -> Option<(Pos, CharClass)> {
    if self.exhausted {
      return None;
    }

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

    if self.row >= self.lines.len() {
      self.exhausted = true;
      return None;
    }

    let line = &self.lines[self.row];
    // Empty line = whitespace
    if line.is_empty() {
      let pos = Pos {
        row: self.row,
        col: 0,
      };
      self.row += 1;
      self.col = 0;
      return Some((pos, CharClass::Whitespace));
    }

    if self.col >= line.len() {
      self.row += 1;
      self.col = 0;
      self.at_boundary = self.row < self.lines.len();
      return self.next();
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
    let row = start_pos.row.min(lines.len().saturating_sub(1));
    let col = if lines.is_empty() || lines[row].is_empty() {
      0
    } else {
      start_pos.col.min(lines[row].len().saturating_sub(1))
    };
    Self {
      lines,
      row,
      col,
      exhausted: false,
      at_boundary: false,
    }
  }
  fn get_pos(&self) -> Pos {
    Pos {
      row: self.row,
      col: self.col,
    }
  }
}

impl<'a> Iterator for CharClassIterRev<'a> {
  type Item = (Pos, CharClass);
  fn next(&mut self) -> Option<(Pos, CharClass)> {
    if self.exhausted {
      return None;
    }

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
      let pos = Pos {
        row: self.row,
        col: 0,
      };
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
