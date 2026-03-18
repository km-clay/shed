use std::{
  collections::HashSet,
  fmt::Display,
  ops::{Index, Range, RangeBounds, RangeFull, RangeInclusive}, slice::SliceIndex,
};

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
	s.graphemes(true)
		.map(Grapheme::from)
		.collect()
}

pub fn to_lines(s: impl ToString) -> Vec<Line> {
	let s = s.to_string();
	s.split("\n")
		.map(to_graphemes)
		.map(Line::from)
		.collect()
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
}

impl<T:SliceIndex<[Grapheme]>> Index<T> for Line {
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

impl From<&Grapheme> for CharClass {
	fn from(g: &Grapheme) -> Self {
		let Some(&first) = g.0.first() else {
			return Self::Other
		};

		if first.is_alphanumeric()
		&& g.0[1..].iter().all(|&c| c.is_ascii_punctuation() || c == '\u{0301}' || c == '\u{0308}') {
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
  LineNo(usize)
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
			SelectMode::Block(select_anchor) |
			SelectMode::Char(select_anchor) => {
				let SelectAnchor::Pos(_) = new_anchor else {
					panic!("Cannot switch to a Pos anchor when the new anchor is a LineNo, or vice versa");
				};
				*select_anchor = new_anchor;
			}
			SelectMode::Line(select_anchor) => {
				let SelectAnchor::LineNo(_) = new_anchor else {
					panic!("Cannot switch to a LineNo anchor when the new anchor is a Pos, or vice versa");
				};
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
	pub col: usize
}

impl Pos {
	pub fn clamp_row<T>(&mut self, other: &[T]) {
		self.row = self.row.clamp(0, other.len().saturating_sub(1));
	}
	pub fn clamp_col<T>(&mut self, other: &[T], inclusive: bool) {
		let mut max = other.len();
		if inclusive && max > 0 {
			max = max.saturating_sub(1);
		}
		self.col = self.col.clamp(0, max);
	}
}

pub enum MotionKind {
	Char { target: Pos },
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
			std::ops::Bound::Unbounded => 0
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
	pub inclusive: bool
}

#[derive(Default, Clone, Debug)]
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

#[derive(Debug, Default, Clone)]
pub struct LineBuf {
	pub lines: Vec<Line>,
	pub hint: Vec<Line>,
	pub cursor: Cursor,

	pub select_mode: Option<SelectMode>,
	pub last_selection: Option<(SelectMode,SelectAnchor)>,

	pub insert_mode_start_pos: Option<Pos>,
	pub saved_col: Option<usize>,
	pub indent_ctx: IndentCtx,

	pub undo_stack: Vec<Edit>,
	pub redo_stack: Vec<Edit>,
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
	fn row_col(&self) -> (usize,usize) {
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
		col = col.clamp(0, self.lines[row].len());
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
	fn verb_shell_cmd(&self, cmd: &str) -> ShResult<()> {

		Ok(())
	}
	fn insert(&mut self, gr: Grapheme) {
		if gr.is_lf() {
			let (row,col) = self.row_col();
			let rest = self.lines[row].split_off(col);
			self.lines.insert(row + 1, rest);
			self.cursor.pos = Pos { row: row + 1, col: 0 };
		} else {
			let (row,col) = self.row_col();
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
			} else{
				return None
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
				return None
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
							Dest::After => unreachable!()
						}
					}
				}
			}
			Direction::Backward => {
				let slice = self.line_to_cursor();
				for (i,gr) in slice.iter().rev().enumerate().skip(1) {
					if gr == char {
						match dest {
							Dest::On => return -(i as isize),
							Dest::After => return -(i as isize) + 1,
							Dest::Before => unreachable!()
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
		include_last_char: bool
	) -> Option<MotionKind> {
		todo!()
	}
	fn eval_motion(&mut self, cmd: &ViCmd) -> Option<MotionKind> {
		let ViCmd { verb, motion, .. } = cmd;
		let MotionCmd(count, motion) = motion.as_ref()?;

		match motion {
			Motion::WholeLine => {
				Some(MotionKind::Line(self.row()))
			}
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
				(target != self.cursor.pos).then_some(MotionKind::Char { target })
			}
			Motion::WordMotion(to, word, dir) => {
        // 'cw' is a weird case
        // if you are on the word's left boundary, it will not delete whitespace after
        // the end of the word
				let include_last_char = matches!(
					verb,
					Some(VerbCmd(_, Verb::Change)),
				)
				&& matches!(motion, Motion::WordMotion(
					To::Start,
					_,
					Direction::Forward,
				));

				self.eval_word_motion(*count, to, word, dir, include_last_char)
			}
			Motion::CharSearch(dir, dest, char) => {
				let off = self.search_char(dir, dest, char);
				let target = self.offset_cursor(0, off);
				(target != self.cursor.pos).then_some(MotionKind::Char { target })
			}
			dir @ (Motion::BackwardChar | Motion::ForwardChar) |
			dir @ (Motion::BackwardCharForced | Motion::ForwardCharForced) => {
				let (off,wrap) = match dir {
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

				(target != self.cursor.pos).then_some(MotionKind::Char { target })
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
					let target = self.offset_cursor(off, 0);
					(target != self.cursor.pos).then_some(MotionKind::Char { target })
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
					(target != self.cursor.pos).then_some(MotionKind::Char { target })
				}
			}
			Motion::WholeBuffer => {
				Some(MotionKind::LineRange(0..self.lines.len()))
			}
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
			Motion::Null => None
		}
	}
	fn apply_motion(&mut self, motion: MotionKind) -> ShResult<()> {
		todo!()
	}
	fn exec_verb(&mut self, cmd: &ViCmd) -> ShResult<()> {
		let ViCmd { register, verb, motion, .. } = cmd;
		let Some(VerbCmd(_, verb)) = verb else {
			let Some(motion_kind) = self.eval_motion(cmd) else {
				return Ok(())
			};
			return self.apply_motion(motion_kind);
		};
		let count = motion.as_ref().map(|m| m.0).unwrap_or(1);

		match verb {
			Verb::Delete => todo!(),
			Verb::Change => todo!(),
			Verb::Yank => todo!(),
			Verb::Rot13 => todo!(),
			Verb::ReplaceChar(_) => todo!(),
			Verb::ReplaceCharInplace(_, _) => todo!(),
			Verb::ToggleCaseInplace(_) => todo!(),
			Verb::ToggleCaseRange => todo!(),
			Verb::IncrementNumber(_) => todo!(),
			Verb::DecrementNumber(_) => todo!(),
			Verb::ToLower => todo!(),
			Verb::ToUpper => todo!(),
			Verb::Undo => todo!(),
			Verb::Redo => todo!(),
			Verb::RepeatLast => todo!(),
			Verb::Put(anchor) => todo!(),
			Verb::InsertModeLineBreak(anchor) => {
				match anchor {
					Anchor::After => {
						let row = self.row();
						self.lines.insert(row + 1, Line::default());
						self.cursor.pos = Pos { row: row + 1, col: 0 };
					}
					Anchor::Before => {
						let row = self.row();
						self.lines.insert(row, Line::default());
						self.cursor.pos = Pos { row, col: 0 };
					}
				}
			}
			Verb::SwapVisualAnchor => todo!(),
			Verb::JoinLines => todo!(),
			Verb::InsertChar(ch) => self.insert(Grapheme::from(*ch)),
			Verb::Insert(s) => self.insert_str(s),
			Verb::Indent => todo!(),
			Verb::Dedent => todo!(),
			Verb::Equalize => todo!(),
			Verb::AcceptLineOrNewline => todo!(),
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
            OpenOptions::new()
							.create(true)
							.append(true)
							.open(path_buf)

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
					return Ok(())
				};
				self.apply_motion(motion_kind)?;
			}
      Verb::Normal(_) |
			Verb::Substitute(..) |
			Verb::RepeatSubstitute |
			Verb::Quit |
			Verb::RepeatGlobal => {
				log::warn!("Verb {:?} is not implemented yet", verb);
			}
		}

		Ok(())
	}
	pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
    let clear_redos = !cmd.is_undo_op() || cmd.verb.as_ref().is_some_and(|v| v.1.is_edit());
    let is_char_insert = cmd.verb.as_ref().is_some_and(|v| v.1.is_char_insert());
    let is_line_motion = cmd.is_line_motion()
      || cmd
        .verb
        .as_ref()
        .is_some_and(|v| v.1 == Verb::AcceptLineOrNewline);
    let is_undo_op = cmd.is_undo_op();
    let edit_is_merging = self.undo_stack.last().is_some_and(|edit| edit.merging);

		self.exec_verb(&cmd)
	}

	pub fn joined(&self) -> String {
		let mut lines = vec![];
		for line in &self.lines {
			lines.push(line.to_string());
		}
		lines.join("\n")
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
