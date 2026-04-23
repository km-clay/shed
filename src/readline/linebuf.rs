use std::{
  cmp::Ordering,
  collections::{HashSet, VecDeque},
  fmt::Display,
  ops::{Index, IndexMut},
  slice::SliceIndex,
};

use ariadne::Span;
use itertools::Either;
use regex::Regex;
use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use super::editcmd::{
  Anchor, Bound, Dest, Direction, EditCmd, Motion, MotionCmd, TextObj, To, Verb, Word,
};
use crate::{
  builtin::stash::{Stash, StashedCmd},
  expand::alias::AliasExpander,
  match_loop, motion,
  parse::{
    ParseFlags, ParsedSrc, Redir, RedirType,
    execute::{exec_int, exec_nonint},
    lex::{self, LexFlags, LexStream, Tk, TkFlags, TkRule},
  },
  prelude::*,
  procio::{self, IoFrame, IoMode, IoStack, capture_command},
  readline::{
    editcmd::{LineAddr, ReadSrc, StashArgs, StashListArg, VerbCmd, WriteDest},
    editmode::ex::SubFlags,
    highlight::Highlighter,
    history::History,
    markers,
    register::RegisterContent,
    term::get_win_size,
  },
  sherr,
  state::{
    self, AutoCmdKind, VarFlags, VarKind, read_logic, read_shopts, read_vars, with_term,
    write_meta, write_vars,
  },
  util::{AutoCmdVecUtils, error::ShResult, guards::var_ctx_guard, strops::QuoteState},
  verb,
};

const DEFAULT_VIEWPORT_HEIGHT: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// A single grapheme. Graphemes can be composed of multiple chars, but are always treated as a single unit for display and editing purposes.
/// Using a SmallVec<[char; 4]> allows us to organize most multi-byte codepoints while maintaining both ownership and stack allocation.
/// If we ever run into a Grapheme made of more than 4 chars, just that Grapheme will gracefully spill over onto the heap
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
  /// Returns true if the Grapheme consists of exactly one char and that char is equal to `c`
  pub fn is_char(&self, c: char) -> bool {
    self.0.len() == 1 && self.0[0] == c
  }
  /// Returns the CharClass of the Grapheme, which is determined by the properties of its chars
  /// Used for things like word motions
  pub fn class(&self) -> CharClass {
    CharClass::from(self)
  }

  /// If the Grapheme consists of exactly one char, returns that char. Otherwise, returns None.
  /// All callsites that use this method operate on ascii, so never returning anything for multibyte sequences is fine.
  pub fn as_char(&self) -> Option<char> {
    if self.0.len() == 1 {
      Some(self.0[0])
    } else {
      None
    }
  }

  /// Returns true if the Grapheme is classified as whitespace
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

pub fn split_lines(mut lines: Vec<Line>, pos: Pos) -> (Vec<Line>, Vec<Line>) {
  let tail = lines[pos.row].split_off(pos.col);
  let mut rest: Vec<Line> = lines.drain(pos.row + 1..).collect();
  rest.insert(0, tail);
  (lines, rest)
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

pub fn is_prefix_lines(this: &[Line], other: &[Line]) -> bool {
  if this.is_empty() {
    return false;
  }

  let all_but_last = this[..this.len().saturating_sub(1)]
    .iter()
    .zip(other.iter())
    .all(|(l, r)| *l == *r);

  if !all_but_last {
    return false;
  }

  let last = this.len().saturating_sub(1);
  let Some(other_line) = other.get(last).map(|l| &l.0) else {
    return false;
  };
  let this_line = &this[last].0;

  this_line.len() <= other_line.len()
    && this_line.iter().zip(other_line.iter()).all(|(l, r)| l == r)
}

pub fn strip_prefix_lines(mut lines: Vec<Line>, other: &[Line]) -> Option<Vec<Line>> {
  if lines.is_empty() {
    return None;
  }

  let common_lines = lines
    .iter()
    .zip(other.iter())
    .take_while(|(l, r)| *l == *r)
    .count();

  // drain equal lines
  lines.drain(..common_lines);

  if lines.is_empty() {
    return None;
  }

  if let Some(other_line) = other.get(common_lines) {
    let common_chars = lines[0]
      .0
      .iter()
      .zip(other_line.0.iter())
      .take_while(|(l, r)| l == r)
      .count();

    // drain common characters
    lines[0].0.drain(..common_chars);
  }

  if lines.iter().all(|l| l.is_empty()) {
    None
  } else {
    Some(lines)
  }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd)]
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
  pub fn insert_str(&mut self, at: usize, other: &str) {
    let mut at = at.min(self.0.len());
    if other.contains('\n') {
      log::warn!(
        "Inserting string with newlines into a single line. Newlines will be treated as literal characters."
      );
    }
    for g in other.graphemes(true) {
      self.0.insert(at, Grapheme::from(g));
      at += 1;
    }
  }
  pub fn insert_char(&mut self, at: usize, c: char) {
    let at = at.min(self.0.len());
    self.0.insert(at, Grapheme::from(c));
  }
  pub fn insert(&mut self, at: usize, g: Grapheme) {
    let at = at.min(self.0.len());
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

  pub fn new(row: usize, col: usize) -> Self {
    Self { row, col }
  }

  pub fn row_col_add(&self, row: isize, col: isize) -> Self {
    Self {
      row: self.row.saturating_add_signed(row),
      col: self.col.saturating_add_signed(col),
    }
  }

  pub fn set(&mut self, row: usize, col: usize) {
    self.row = row;
    self.col = col;
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

impl std::ops::Add for Pos {
  type Output = Self;

  fn add(self, rhs: Self) -> Self::Output {
    Self {
      row: self.row.saturating_add(rhs.row),
      col: self.col.saturating_add(rhs.col),
    }
  }
}

#[derive(Debug, Clone)]
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
    inclusive: bool,
  },
  /// A list of lines, not necessarily contiguous. Used for things like line addresses in ex mode commands
  Lines {
    lines: Vec<usize>,
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

  pub fn swap_top(&mut self, tk: Tk) {
    if let Some(top) = self.ctx.last_mut() {
      *top = tk;
    }
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

  pub fn is_sibling(&self, other: &str) -> bool {
    let Some(last) = self.ctx.last() else {
      return false;
    };
    let last = last.as_str();
    if last == "if" || last == "elif" {
      other == "elif" || other == "else"
    } else {
      false
    }
  }

  pub fn checked_calculate(&mut self, input: &str) -> (usize, bool) {
    self.depth = 0;
    self.ctx.clear();

    let mut src = ParsedSrc::new(input.into())
      .with_lex_flags(LexFlags::LEX_UNFINISHED)
      .with_parse_flags(ParseFlags::ERR_RETURN);

    // now we parse the input
    // src.block_depth will be non-zero if the parse was stopped somewhere.
    let res = src.parse_src();

    self.depth = src.block_depth;

    (self.depth, res.is_err())
  }

  pub fn calculate(&mut self, input: &str) -> usize {
    self.checked_calculate(input).0
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

#[derive(Default, Debug, Clone)]
pub struct KillRing {
  pub kills: VecDeque<Vec<Line>>,
  pub merging: bool,
  pub selected: Option<usize>,
  pub kill_cycle_span: Option<(Pos, Pos)>,
}

impl KillRing {
  pub fn new() -> Self {
    Self {
      kills: VecDeque::new(),
      merging: false,
      selected: None,
      kill_cycle_span: None,
    }
  }
  pub fn push_back(&mut self, kill: Vec<Line>) {
    if kill.is_empty() || (kill.len() == 1 && kill[0].is_empty()) {
      return;
    }
    self.kills.push_back(kill);
    if self.kills.len() > LineBuf::MAX_KILL_RING {
      self.kills.pop_front();
    }
  }
  pub fn push_front(&mut self, kill: Vec<Line>) {
    if kill.is_empty() || (kill.len() == 1 && kill[0].is_empty()) {
      return;
    }
    self.kills.push_front(kill);
    if self.kills.len() > LineBuf::MAX_KILL_RING {
      self.kills.pop_back();
    }
  }
  pub fn pop_back(&mut self) -> Option<Vec<Line>> {
    self.kills.pop_back()
  }
  pub fn pop_front(&mut self) -> Option<Vec<Line>> {
    self.kills.pop_front()
  }
  pub fn len(&self) -> usize {
    self.kills.len()
  }
  pub fn is_empty(&self) -> bool {
    self.kills.is_empty()
  }
  pub fn next_idx(&mut self) -> usize {
    let idx = match self.selected {
      Some(0) | None => self.kills.len(),
      Some(i) => i,
    }
    .saturating_sub(1);
    self.selected = Some(idx);
    idx
  }
  pub fn reset(&mut self) {
    self.selected = None;
    self.kill_cycle_span = None;
  }
}

impl Iterator for KillRing {
  type Item = Vec<Line>;
  fn next(&mut self) -> Option<Self::Item> {
    let next_idx = self.next_idx();
    self.kills.get(next_idx).cloned()
  }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Hint {
  Override(Vec<Line>),
  History(Vec<Line>),
  Completion(Vec<Line>),
}

impl Hint {
  pub fn new_override(s: String) -> Self {
    Self::Override(to_lines(s))
  }
  pub fn new_history(s: String) -> Self {
    Self::History(to_lines(s))
  }
  pub fn new_completion(s: String) -> Self {
    Self::Completion(to_lines(s))
  }

  pub fn set_lines(&mut self, new_lines: Vec<Line>) {
    match self {
      Self::Override(lines) | Self::History(lines) | Self::Completion(lines) => {
        *lines = new_lines;
      }
    }
  }
  pub fn lines(&self) -> &[Line] {
    match self {
      Self::Override(lines) | Self::History(lines) | Self::Completion(lines) => lines,
    }
  }
  pub fn raw(&self) -> String {
    join_lines(self.lines())
  }
  pub fn take_lines(&mut self) -> Vec<Line> {
    match self {
      Self::Override(lines) | Self::History(lines) | Self::Completion(lines) => {
        std::mem::take(lines)
      }
    }
  }
  pub fn display(&self, prefix: Option<&str>) -> String {
    let mut text = self.raw();
    if let Some(prefix) = prefix
      && let Some(rest) = text.strip_prefix(prefix)
    {
      text = rest.to_string();
    }

    format!("\x1b[90m{text}\x1b[0m").replace("\n", "\n\x1b[90m")
  }
  pub fn len(&self) -> usize {
    self.lines().len()
  }
  pub fn is_empty(&self) -> bool {
    self.lines().is_empty() || (self.lines().len() == 1 && self.lines()[0].is_empty())
  }
}

impl PartialOrd for Hint {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for Hint {
  /// Defines priority for hint types.
  ///
  /// If a new hint would overwrite an old hint, the 'lesser' hint loses
  ///
  /// 'greater' hints and 'equal' hints both overwrite.
  fn cmp(&self, other: &Self) -> Ordering {
    match self {
      Self::Override(_) => {
        if matches!(other, Self::Override(_)) {
          Ordering::Equal
        } else {
          Ordering::Greater
        }
      }
      Self::History(_) => match other {
        Self::Override(_) => Ordering::Less,
        Self::History(_) => Ordering::Equal,
        Self::Completion(_) => Ordering::Greater,
      },
      Self::Completion(_) => {
        if matches!(other, Self::Completion(_)) {
          Ordering::Equal
        } else {
          Ordering::Less
        }
      }
    }
  }
}

#[derive(Debug, Clone)]
pub struct LineBuf {
  pub lines: Vec<Line>,
  pub hint: Option<Hint>,
  pub cursor: Cursor,

  pub select_mode: Option<SelectMode>,
  pub last_selection: Option<(SelectMode, Pos)>,

  pub last_substitute: Option<EditCmd>,
  pub last_global: Option<EditCmd>,

  pub insert_mode_start_pos: Option<Pos>,
  pub saved_col: Option<usize>,
  pub indent_ctx: IndentCtx,

  pub scroll_offset: usize,

  pub undo_stack: Vec<Edit>,
  pub redo_stack: Vec<Edit>,
  pub merging_undos: bool,

  pub kill_ring: KillRing,
  pub kill_cycle_pos: Option<Pos>,

  pub concat_points: VecDeque<Pos>,
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
      last_substitute: None,
      last_global: None,
      insert_mode_start_pos: None,
      saved_col: None,
      indent_ctx: IndentCtx::new(),
      scroll_offset: 0,
      undo_stack: vec![],
      redo_stack: vec![],
      merging_undos: false,
      kill_ring: KillRing::new(),
      kill_cycle_pos: None,
      concat_points: VecDeque::new(),
    }
  }
}

#[allow(dead_code, unused_variables)]
impl LineBuf {
  const MAX_KILL_RING: usize = 60;

  pub fn new() -> Self {
    Self::default()
  }
  pub fn get_viewport_height(&self) -> usize {
    let raw = read_shopts(|o| {
      let height = o.line.viewport_height.as_str();
      if let Ok(num) = height.parse::<usize>() {
        num
      } else if let Some(pre) = height.strip_suffix('%')
        && let Ok(num) = pre.parse::<usize>()
      {
        if !isatty(STDIN_FILENO).unwrap_or_default() {
          return DEFAULT_VIEWPORT_HEIGHT;
        };
        let (_, rows) = get_win_size(STDIN_FILENO);
        (rows as f64 * (num as f64 / 100.0)).round() as usize
      } else {
        log::warn!(
          "Invalid viewport height shopt value: '{}', using 50% of terminal height as default",
          height
        );
        if !isatty(STDIN_FILENO).unwrap_or_default() {
          return DEFAULT_VIEWPORT_HEIGHT;
        };
        let (_, rows) = get_win_size(STDIN_FILENO);
        (rows as f64 * 0.5).round() as usize
      }
    });
    let mut hint_lines = self.hint_lines();
    let mut buf_lines = self.lines.clone();
    attach_lines(&mut buf_lines, &mut hint_lines);
    (raw.min(100)).min(buf_lines.len())
  }
  pub fn update_scroll_offset(&mut self) {
    let height = self.get_viewport_height();
    let scrolloff = read_shopts(|o| o.line.scroll_offset);
    if self.cursor.pos.row < self.scroll_offset + scrolloff {
      self.scroll_offset = self.cursor.pos.row.saturating_sub(scrolloff);
    }
    if self.cursor.pos.row + scrolloff >= self.scroll_offset + height {
      self.scroll_offset = self.cursor.pos.row + scrolloff + 1 - height;
    }

    let max_offset = self.lines.len().saturating_sub(height);
    self.scroll_offset = self.scroll_offset.min(max_offset);
  }
  pub fn get_window(&self) -> Vec<Line> {
    let height = self.get_viewport_height();
    self
      .lines
      .iter()
      .skip(self.scroll_offset)
      .take(height)
      .cloned()
      .collect()
  }
  pub fn window_joined(&self) -> String {
    join_lines(&self.get_window())
  }
  pub fn display_window_joined(&self) -> String {
    let display = self.to_string();
    let do_hl = state::read_shopts(|s| s.highlight.enable);
    let mut highlighter = Highlighter::new();
    highlighter.only_visual(!do_hl);
    highlighter.load_input(&display, self.cursor_byte_pos());
    highlighter.expand_control_chars();
    highlighter.highlight();
    let highlighted = highlighter.take();
    let hint = self.get_hint_text();
    let lines = to_lines(format!("{highlighted}{hint}"));

    let offset = self.scroll_offset.min(lines.len());
    let (_, mid) = lines.split_at(offset);

    let height = self.get_viewport_height().min(mid.len());
    let (mid, _) = mid.split_at(height);

    join_lines(mid)
  }
  pub fn window_slice_to_cursor(&self) -> Option<String> {
    let mut result = String::new();
    let start_row = self.scroll_offset;

    for i in start_row..self.cursor.pos.row {
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
    let (start, end) = ordered(start, end);
    self.lines.iter().take(end + 1).skip(start)
  }
  fn line_iter_mut(&mut self, span: (usize, usize)) -> impl Iterator<Item = &mut Line> {
    let (start, end) = ordered(span.0, span.1);
    self.lines.iter_mut().take(end + 1).skip(start)
  }
  fn line_iter_mut_by_indices(
    &mut self,
    indices: &[usize],
  ) -> impl Iterator<Item = &mut Line> + '_ {
    let indices_set: HashSet<usize> = indices.iter().cloned().collect();
    self
      .lines
      .iter_mut()
      .enumerate()
      .filter_map(move |(i, line)| {
        if indices_set.contains(&i) {
          Some(line)
        } else {
          None
        }
      })
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
  pub fn row(&self) -> usize {
    self.cursor.pos.row
  }
  fn offset_row(&self, offset: isize) -> usize {
    let mut row = self.cursor.pos.row.saturating_add_signed(offset);
    row = row.clamp(0, self.lines.len().saturating_sub(1));
    row
  }
  pub fn col(&self) -> usize {
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
  fn offset_col_wrapping_at(&self, row: usize, offset: isize, pos: Pos) -> (usize, usize) {
    let mut row = row;
    let mut col = pos.col as isize + offset;

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
  fn offset_col_wrapping(&self, row: usize, offset: isize) -> (usize, usize) {
    self.offset_col_wrapping_at(row, offset, self.cursor.pos)
  }
  fn cursor_on_ws(&self) -> bool {
    let line = self.cur_line();
    let col = self.cursor.pos.col;
    line.graphemes().get(col).is_some_and(|g| g.is_ws())
  }
  pub fn set_cursor(&mut self, mut pos: Pos) {
    pos.clamp_row(&self.lines);
    pos.clamp_col(&self.lines[pos.row].0, self.cursor.exclusive);
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
    self.break_line_at(self.cursor.pos);
  }
  fn break_line_at(&mut self, pos: Pos) {
    let Pos { row, col } = pos;
    let rest = self.lines[row].split_off(col);

    self.lines.insert(row + 1, rest);
    let mut new_line_pos = Pos {
      row: row + 1,
      col: 0,
    };
    let level = self.calc_indent_level_for_pos(new_line_pos);
    let new_line = self.lines.get_mut(row + 1).unwrap();
    for tab in std::iter::repeat_n(Grapheme::from('\t'), level) {
      new_line.insert(0, tab);
      new_line_pos = new_line_pos.col_add(1);
    }

    self.cursor.pos = new_line_pos;
  }
  fn verb_shell_cmd(&mut self, cmd: &str, stdin: Option<&str>) -> ShResult<Option<String>> {
    let mut vars = HashSet::new();
    vars.insert("BUFFER".into());
    vars.insert("CURSOR".into());
    vars.insert("ANCHOR".into());
    let _guard = var_ctx_guard(vars);

    let mut buf = self.joined();
    let cursor_raw = self.cursor_to_flat();
    let mut cursor = cursor_raw.to_string();
    let mut anchor = self.anchor_to_flat();

    write_vars(|v| -> ShResult<()> {
      v.set_var("BUFFER", VarKind::Str(buf.clone()), VarFlags::EXPORT)?;
      v.set_var("CURSOR", VarKind::Str(cursor.to_string()), VarFlags::EXPORT)?;
      if let Some(anchor) = anchor {
        v.set_var("ANCHOR", VarKind::Str(anchor.to_string()), VarFlags::EXPORT)?;
      }
      Ok(())
    })?;

    let pre_cmd = read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd));
    let post_cmd = read_logic(|l| l.get_autocmds(AutoCmdKind::PostCmd));

    pre_cmd.exec();
    let res = if let Some(stdin) = stdin {
      Some(procio::capture_command(cmd, Some(stdin))?)
    } else {
      let _guard = with_term(|t| t.cooked_mode_guard());
      exec_int(cmd.to_string(), Some("<ex-mode-cmd>".into()))?;
      None
    };
    post_cmd.exec();

    let output = res;

    let mut new_anchor = None;

    let keys = write_vars(|v| {
      buf = v.take_var("BUFFER");
      cursor = v.take_var("CURSOR");
      if anchor.is_some() {
        new_anchor = Some(v.take_var("ANCHOR"));
      }
      v.take_var("KEYS")
    });

    self.set_buffer(buf);

    if let Some(new_anchor) = new_anchor {
      if let Ok(pos) = self.parse_pos(&new_anchor) {
        anchor = Some(self.pos_to_flat(pos));
      } else {
        log::warn!(
          "Invalid anchor position returned from shell command: '{}'",
          new_anchor
        );
        anchor = None;
      }
    }

    if let Ok(pos) = self.parse_pos(&cursor) {
      self.set_cursor(pos);
    } else {
      log::warn!(
        "Invalid cursor position returned from shell command: '{}'",
        cursor
      );
      self.set_cursor_from_flat(cursor_raw);
    }

    if let Some(anchor) = anchor
      && anchor != cursor_raw
      && self.select_mode.is_some()
    {
      self.set_anchor_from_flat(anchor);
    }
    if !keys.is_empty() {
      write_meta(|m| m.set_pending_widget_keys(&keys))
    }
    Ok(output)
  }
  fn parse_pos(&self, pos: &str) -> ShResult<Pos> {
    if let Some((row, col)) = pos.split_once(':')
      && let Ok(row) = row.parse::<usize>()
      && let Ok(col) = col.parse::<usize>()
    {
      Ok(Pos { row, col })
    } else if let Ok(num) = pos.parse::<usize>() {
      Ok(self.pos_from_flat(num))
    } else {
      Err(sherr!(
        ParseErr,
        "Invalid position format: '{pos}'. Expected 'row:col' or grapheme index.",
      ))
    }
  }
  fn insert_lines_at(&mut self, pos: Pos, mut lines: Vec<Line>) {
    if lines.is_empty() {
      return;
    }
    let row = pos.row;
    let col = pos.col;

    // Split the current line at the insertion point
    let mut right = self.lines[row].split_off(col);

    let last = lines.len() - 1;

    // First line appends to current line at the split point
    self.lines[row].append(&mut lines[0]);

    // Middle + last lines get inserted after
    for (i, line) in lines[1..].iter().cloned().enumerate() {
      self.lines.insert(row + 1 + i, line);
    }

    // Reattach right half to the last inserted line
    self.lines[row + last].append(&mut right);
  }
  fn remove_at(&mut self, pos: Pos) -> Option<Grapheme> {
    let Pos { row, col } = pos;
    let line = self.lines.get_mut(row)?;

    line.0.get(col).is_some().then(|| line.0.remove(col))
  }
  fn insert_at(&mut self, mut pos: Pos, gr: Grapheme) {
    let level = self.calc_indent_level_for_pos(pos);
    if gr.is_lf() {
      self.break_line_at(pos);
      pos = pos.row_add(1);
      pos.set(pos.row, 0);
    } else {
      let row = pos.row;
      let col = pos.col;
      self.lines[row].insert(col, gr);
      pos = pos.col_add(1);
    }
    let new_level = self.calc_indent_level_for_pos(pos);
    let line = self.cur_line().to_string();
    let trimmed = line.trim();

    if new_level < level {
      let delta = level.saturating_sub(new_level);
      let line = self.cur_line_mut();
      // check if the line is an exact match of a closer keyword
      // we don't want to dedent something like 'foo) echo bar ;;'
      // where the closer is inlined with the content
      let is_closer = lex::CLOSERS
        .iter()
        .chain(lex::MIDDLES.iter())
        .any(|closer| trimmed == *closer);

      if is_closer {
        for _ in 0..delta {
          if line.0.first().is_some_and(|c| c.as_char() == Some('\t')) {
            line.0.remove(0);
          } else {
            break;
          }
        }
      }
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
  fn insert_str_at(&mut self, pos: Pos, s: &str) {
    let mut row_offset = self.row();
    let mut col_offset = pos.col;
    for gr in s.graphemes(true) {
      let gr = Grapheme::from(gr);
      if gr.is_lf() {
        self.break_line_at(pos.row_add(row_offset));
        row_offset += 1;
        col_offset = 0;
      } else {
        self.insert_at(pos.row_add(row_offset).col_add(col_offset), gr);
        if self.cursor.pos.row == pos.row + row_offset && self.cursor.pos.col >= col_offset {
          self.cursor.pos.col += 1;
        }
        col_offset += 1;
      }
    }
  }
  pub fn pop_left(&mut self) -> bool {
    let Some(pos) = self.concat_points.pop_front() else {
      return false;
    };
    self.lines = split_lines_at(&mut self.lines, pos);
    self.fix_cursor();
    true
  }
  pub fn pop_right(&mut self) -> bool {
    let Some(pos) = self.concat_points.pop_back() else {
      return false;
    };
    split_lines_at(&mut self.lines, pos);
    self.fix_cursor();
    true
  }
  pub fn clear_concats(&mut self) {
    self.concat_points.clear();
  }
  /// Concatenate a string onto the left side of the buffer with a separator
  pub fn concat_left(&mut self, sep: &str, other: &str) {
    if self.is_empty() {
      self.lines = to_lines(other);
      return;
    }
    let joined = self.joined();
    let Some(first) = self.lines.first_mut() else {
      self.lines = to_lines(other);
      return;
    };
    let mut new_lines = to_lines(other);
    if new_lines.is_empty() {
      return;
    }
    while first.0.first().is_some_and(|l| l.is_ws()) {
      first.0.remove(0);
    }
    let Some(new_last) = new_lines.last_mut() else {
      unreachable!()
    };
    if !joined.trim_end().ends_with(sep.trim()) {
      new_last.push_str(sep);
    }
    let mut last = new_lines.pop().unwrap();
    let splice_pos = Pos {
      row: new_lines.len(),
      col: last.len(),
    };
    last.append(first);
    self.lines[0] = last;
    if !new_lines.is_empty() {
      for line in new_lines.into_iter().rev() {
        self.lines.insert(0, line);
      }
    }
    self.concat_points.push_front(splice_pos);
  }
  /// Concatenate a string onto the right side of the buffer with a separator
  pub fn concat_right(&mut self, sep: &str, other: &str) {
    if self.is_empty() {
      self.lines = to_lines(other);
      return;
    }
    let joined = self.joined();
    let last_row = self.lines.len() - 1;
    let Some(last) = self.lines.last_mut() else {
      self.lines = to_lines(other);
      return;
    };
    let mut new_lines = to_lines(other);
    if new_lines.is_empty() {
      return;
    }
    while last.0.last().is_some_and(|l| l.is_ws()) {
      last.0.pop();
    }
    let Some(new_first) = new_lines.first_mut() else {
      unreachable!()
    };
    if !joined.trim_end().ends_with(sep.trim()) {
      new_first.insert_str(0, sep);
    }
    let splice_pos = Pos {
      row: last_row,
      col: last.len(),
    };
    let mut first = new_lines.remove(0);
    last.append(&mut first);
    self.lines.extend(new_lines);
    self.concat_points.push_back(splice_pos);
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
      if col >= line.len() {
        if row < self.lines.len() - 1 {
          row += 1;
          col = 0;
          continue;
        } else {
          return None;
        }
      }
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
    self.scan_backward_from(self.cursor.pos.col_add_signed(-1), f)
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
        for (i, gr) in slice.iter().rev().enumerate() {
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

    for i in 0..count {
      let last = i == count - 1;
      let iws = ignore_trailing_ws && last; // only ignore on the last iteration
      match (to, dir) {
        (To::Start, Direction::Forward) => {
          // 'w' is a special snowflake motion so we need these two extra arguments
          // if we hit the ignore_trailing_ws path in the function,
          // inclusive is flipped to true.
          target = self
            .word_motion_w(word, target, iws, &mut inclusive)
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
    target.clamp_col(&self.lines[target.row].0, self.cursor.exclusive);

    Some(MotionKind::Char {
      start: self.cursor.pos,
      end: target,
      inclusive,
    })
  }
  fn word_motion_w(
    &self,
    word: &Word,
    start: Pos,
    ignore_trailing_ws: bool,
    inclusive: &mut bool,
  ) -> Option<Pos> {
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
        let mut last = classes.next()?;
        let first_c = last.1;
        while let Some((p, c)) = classes.next() {
          match c {
            C::Whitespace => {
              if ignore_trailing_ws {
                *inclusive = true;
                return Some(last.0);
              } else {
                break;
              }
            }
            c if !c.is_other_class_or_ws(&first_c) => {
              last = (p, c);
            }
            _ => return Some(p),
          }
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
    pos.clamp_col(&self.lines[pos.row].0, self.cursor.exclusive);
    pos
  }
  fn dispatch_text_obj(&mut self, count: u16, obj: TextObj) -> Option<MotionKind> {
    match obj {
      // text structures
      TextObj::Word(word, bound) => self.text_obj_word(count, self.cursor.pos, word, bound),
      TextObj::Sentence(_)
      | TextObj::Paragraph(_)
      | TextObj::WholeSentence(_)
      | TextObj::Tag(_)
      | TextObj::Custom(_)
      | TextObj::WholeParagraph(_) => {
        log::warn!("{:?} text objects are not implemented yet", obj);
        None
      }

      // quote stuff
      TextObj::DoubleQuote(bound) | TextObj::SingleQuote(bound) | TextObj::BacktickQuote(bound) => {
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
    from: Pos,
    word: Word,
    bound: Bound,
  ) -> Option<MotionKind> {
    use CharClass as C;
    let mut fwd_classes = self.char_classes_forward_from(from);
    let first_class = fwd_classes.next()?;
    match first_class {
      (pos, C::Whitespace) => match bound {
        Bound::Inside => {
          let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
          let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
          let mut first = (pos, C::Whitespace);
          let mut last = (pos, C::Whitespace);
          while let Some((_, c)) = bkwd_classes.peek() {
            if !c.is_ws() {
              break;
            }
            first = bkwd_classes.next()?;
          }

          while let Some((_, c)) = fwd_classes.peek() {
            if !c.is_ws() {
              break;
            }
            last = fwd_classes.next()?;
          }

          Some(MotionKind::Char {
            start: first.0,
            end: last.0,
            inclusive: true,
          })
        }
        Bound::Around => {
          let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
          let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
          let mut first = (pos, C::Whitespace);
          let mut last = (pos, C::Whitespace);
          while let Some((_, cl)) = bkwd_classes.peek() {
            if !cl.is_ws() {
              break;
            }
            first = bkwd_classes.next()?;
          }

          while let Some((_, cl)) = fwd_classes.peek() {
            if !cl.is_ws() {
              break;
            }
            last = fwd_classes.next()?;
          }
          let word_class = fwd_classes.next()?.1;
          while let Some((_, cl)) = fwd_classes.peek() {
            match word {
              Word::Big => {
                if cl.is_ws() {
                  break;
                }
              }
              Word::Normal => {
                if cl.is_other_class_or_ws(&word_class) {
                  break;
                }
              }
            }
            last = fwd_classes.next()?;
          }

          Some(MotionKind::Char {
            start: first.0,
            end: last.0,
            inclusive: true,
          })
        }
      },
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
            let mut first = (pos, c);
            let mut last = (pos, c);

            while let Some((_, cl)) = bkwd_classes.peek() {
              if break_cond(cl, &c) {
                break;
              }
              first = bkwd_classes.next()?;
            }

            while let Some((_, cl)) = fwd_classes.peek() {
              if break_cond(cl, &c) {
                break;
              }
              last = fwd_classes.next()?;
            }

            Some(MotionKind::Char {
              start: first.0,
              end: last.0,
              inclusive: true,
            })
          }
          Bound::Around => {
            let mut fwd_classes = self.char_classes_forward_from(pos).peekable();
            let mut bkwd_classes = self.char_classes_backward_from(pos).peekable();
            let mut first = (pos, c);
            let mut last = (pos, c);

            while let Some((_, cl)) = bkwd_classes.peek() {
              if break_cond(cl, &c) {
                break;
              }
              first = bkwd_classes.next()?;
            }

            while let Some((_, cl)) = fwd_classes.peek() {
              if break_cond(cl, &c) {
                break;
              }
              last = fwd_classes.next()?;
            }

            // Include trailing whitespace
            while let Some((_, cl)) = fwd_classes.peek() {
              if !cl.is_ws() {
                break;
              }
              last = fwd_classes.next()?;
            }

            Some(MotionKind::Char {
              start: first.0,
              end: last.0,
              inclusive: true,
            })
          }
        }
      }
    }
  }
  fn text_obj_quote(&mut self, count: u16, obj: TextObj, bound: Bound) -> Option<MotionKind> {
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
    let line_len = self.lines[scan_start_pos.row].len();
    scan_start_pos.col = (scan_start_pos.col + 1).min(line_len.saturating_sub(1));

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
  fn number_at_cursor(&self) -> Option<(Pos, Pos)> {
    self.number_at(self.cursor.pos)
  }
  /// Returns the start/end span of a number at a given position, if any
  fn number_at(&self, mut pos: Pos) -> Option<(Pos, Pos)> {
    let is_number_char = |gr: &Grapheme| {
      gr.as_char()
        .is_some_and(|c| c == '.' || c == '-' || c.is_ascii_digit())
    };
    let is_digit = |gr: &Grapheme| gr.as_char().is_some_and(|c| c.is_ascii_digit());

    pos = self.clamp_pos(pos);
    if !is_number_char(self.gr_at(pos)?) {
      return None;
    }

    // If cursor is on '-', advance to the first digit
    if self.gr_at(pos)?.as_char() == Some('-') {
      pos = pos.col_add(1);
    }

    let mut start = self
      .scan_backward_from(pos, |g| !is_digit(g))
      .map(|pos| Pos {
        row: pos.row,
        col: pos.col + 1,
      })
      .unwrap_or(Pos::MIN);
    let end = self
      .scan_forward_from(pos, |g| !is_digit(g))
      .map(|pos| Pos {
        row: pos.row,
        col: pos.col.saturating_sub(1),
      })
      .unwrap_or(Pos {
        row: pos.row,
        col: self.lines[pos.row].len().saturating_sub(1),
      });

    if start > Pos::MIN && self.lines[start.row][start.col.saturating_sub(1)].as_char() == Some('-')
    {
      start.col -= 1;
    }

    Some((start, end))
  }
  fn adjust_number(&mut self, inc: i64) -> Option<()> {
    let (s, e) = if let Some(range) = self.select_range() {
      match range {
        Motion::CharRange(s, e) => (s, e),
        _ => return None,
      }
    } else if let Some((s, e)) = self.number_at_cursor() {
      (s, e)
    } else {
      return None;
    };

    let word = self.pos_slice_str(s, e);

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
    } else {
      return None;
    };

    self.replace_range((s, e), &num_fmt);
    self.cursor.pos.col -= 1;
    Some(())
  }
  fn replace_range(&mut self, span: (Pos, Pos), new: &str) -> Vec<Line> {
    let s = span.0;
    let e = span.1;
    let motion = MotionKind::Char {
      start: s,
      end: e,
      inclusive: true,
    };
    let content = self.extract_range(&motion);
    self.set_cursor(s);
    self.insert_str(new);
    content
  }
  fn pos_slice_str(&self, s: Pos, e: Pos) -> String {
    let (s, e) = ordered(s, e);
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
      let mut depth = 0;
      let opener = match self.gr_at(first)?.as_char()? {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        '>' => '<',
        _ => unreachable!(),
      };
      self.scan_backward_from(first, |g| {
        if g.as_char() == self.gr_at(first).and_then(|c| c.as_char()) {
          depth += 1;
        } else if g.as_char() == Some(opener) {
          depth -= 1;
        }
        depth == 0
      })?
    } else if is_opener(self.gr_at(first)?) {
      let mut depth = 0;
      let closer = match self.gr_at(first)?.as_char()? {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        _ => unreachable!(),
      };
      self.scan_forward_from(first, |g| {
        if g.as_char() == self.gr_at(first).and_then(|c| c.as_char()) {
          depth += 1;
        } else if g.as_char() == Some(closer) {
          depth -= 1;
        }
        depth == 0
      })?
    } else {
      unreachable!()
    };

    Some(MotionKind::Char {
      start: self.cursor.pos,
      end: delim_match,
      inclusive: true,
    })
  }
  fn get_row(&self, row: usize) -> Option<&Line> {
    self.lines.get(row)
  }
  /// Given a LineAddr, resolve it to an absolute line number.
  ///
  /// This is used for commands like `:3` or `:'a` where we need to convert the address into a line number in the buffer.
  pub fn resolve_line_addr(&self, addr: &LineAddr) -> ShResult<Option<usize>> {
    match addr {
      LineAddr::Number(n) => Ok(Some(
        (n.saturating_sub(1)).min(self.lines.len().saturating_sub(1)),
      )),
      LineAddr::Current => Ok(Some(self.row())),
      LineAddr::Last => Ok(Some(self.lines.len().saturating_sub(1))),
      LineAddr::Offset(i) => Ok(Some(self.row().saturating_add_signed(*i))),
      dir @ (LineAddr::Pattern(re) | LineAddr::PatternRev(re)) => {
        let reg = Regex::new(re).map_err(|e| sherr!(ParseErr, "Invalid search pattern: {e}"))?;
        let off = if matches!(dir, LineAddr::Pattern(_)) {
          1
        } else {
          -1
        };
        let inc_acc =
          |acc: usize| (acc as isize + off).rem_euclid(self.lines.len() as isize) as usize;
        let mut acc = inc_acc(self.row());

        while let Some(row) = self.get_row(acc) {
          let row_str = row.to_string();
          if reg.is_match(&row_str) {
            return Ok(Some(acc));
          }

          if acc == self.row() {
            break;
          }
          acc = inc_acc(acc);
        }

        Ok(None)
      }
      LineAddr::Mark(ch) => {
        match ch {
          anchor @ ('<' | '>') => {
            let Some(select_range) = self.select_range() else {
              return Ok(None);
            };
            let (s, e) = match select_range {
              Motion::CharRange(s, e) => (s.row, e.row),
              Motion::LineRange(s, e) => {
                let Some(s) = self.resolve_line_addr(&s)? else {
                  return Ok(None);
                };
                let Some(e) = self.resolve_line_addr(&e)? else {
                  return Ok(None);
                };
                (s, e)
              }
              _ => unreachable!(),
            };
            match anchor {
              '<' => Ok(Some(s)),
              '>' => Ok(Some(e)),
              _ => unreachable!(),
            }
          }
          _ => Ok(None), // TODO: implement marks
        }
      }
    }
  }
  /// Perform an operation that incrementally accepts the hint if the cursor moves into it
  ///
  /// Process:
  /// * take the lines out of self.hint directly
  /// * mark end of buffer position, append hint lines to self.lines
  /// * call the function
  /// * split the buffer at `old_end_pos.max(new_cursor_pos)`
  ///
  /// Notes:
  /// * The size of the hint can never grow as a result of this function. It will only ever stay the same size or shrink.
  pub fn with_hint<F, T>(&mut self, f: F) -> T
  where
    F: FnOnce(&mut Self) -> T,
  {
    let mut hint = self.hint.take();
    let mut old_end_pos = self.end_pos();

    if self.cursor.exclusive {
      old_end_pos = old_end_pos.col_add(1);
    }

    if let Some(h) = hint.as_mut()
      && let Some(mut hint_lines) = strip_prefix_lines(h.take_lines(), &self.lines)
    {
      attach_lines(&mut self.lines, &mut hint_lines);
    }
    let old_cursor_pos = self.cursor.pos;

    let result = f(self);

    let new_cursor_pos = self.cursor.pos;

    if let Some(mut hint) = hint {
      let is_past_end = if self.cursor.exclusive {
        new_cursor_pos >= old_end_pos
      } else {
        new_cursor_pos > old_end_pos
      };
      let split_pos = if new_cursor_pos > old_cursor_pos && is_past_end {
        // our cursor moved into the hint.
        let old_len = self.count_graphemes();
        self.attempt_alias_expansion();
        let new_len = self.count_graphemes();
        let delta = new_len as isize - old_len as isize;
        new_cursor_pos.col_add_signed(delta + 1)
      } else {
        old_end_pos
      };

      let hint_lines = split_lines_at(&mut self.lines, split_pos);
      if !hint_lines.is_empty() {
        hint.set_lines(hint_lines);
        self.hint = Some(hint);
      }
    }

    result
  }
  fn display_col_to_index(&self, row: usize, target: usize) -> usize {
    let tab_width = read_shopts(|o| o.line.tab_width);
    let line = self.line(row);
    let mut col = 0;
    for (i, gr) in line.0.iter().enumerate() {
      if col >= target {
        return i;
      }
      let Some(ch) = gr.as_char() else {
        col += gr.width();
        continue;
      };

      match ch {
        '\t' => {
          col += tab_width - (col % tab_width);
        }
        c => {
          col += c.width().unwrap_or(0);
        }
      }
    }

    line.0.len()
  }
  fn calc_display_col_for(&self, pos: Pos) -> usize {
    let tab_width = read_shopts(|o| o.line.tab_width);
    let line = self.line(pos.row);
    let mut col = 0;
    for gr in &line.0[..pos.col] {
      let Some(ch) = gr.as_char() else {
        col += gr.width();
        continue;
      };

      match ch {
        '\t' => {
          col += tab_width - (col % tab_width);
        }
        c => {
          col += c.width().unwrap_or(0);
        }
      }
    }

    col
  }
  fn calc_cursor_display_col(&self) -> usize {
    self.calc_display_col_for(self.cursor.pos)
  }
  /// Wrapper for eval_motion_inner that calls it with `check_hint: false`
  fn eval_motion(&mut self, cmd: &EditCmd) -> ShResult<Option<MotionKind>> {
    self.eval_motion_inner(cmd, false)
  }
  fn eval_motion_inner(&mut self, cmd: &EditCmd, check_hint: bool) -> ShResult<Option<MotionKind>> {
    let EditCmd { verb, motion, .. } = cmd;
    let Some(MotionCmd(count, motion)) = motion.as_ref() else {
      return Ok(None);
    };

    let eval = |this: &mut Self| -> ShResult<Option<MotionKind>> {
      let kind = match motion {
        Motion::WholeLine => {
          let start = this.row();
          let end =
            (this.row() + (count.saturating_sub(1))).min(this.lines.len().saturating_sub(1));
          Some(MotionKind::Line {
            start,
            end,
            inclusive: true,
          })
        }
        Motion::TextObj(text_obj) => this.dispatch_text_obj(*count as u16, text_obj.clone()),
        Motion::EndOfLastWord => {
          let row = this.row() + (count.saturating_sub(1));
          let line = this.line_mut(row);
          let mut target = Pos { row, col: 0 };
          for (i, gr) in line.0.iter().enumerate() {
            if !gr.is_ws() {
              target.col = i;
            }
          }

          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
            end: target,
            inclusive: true,
          })
        }
        Motion::StartOfFirstWord => {
          let mut target = Pos {
            row: this.row(),
            col: 0,
          };
          let line = this.cur_line();
          for (i, gr) in line.0.iter().enumerate() {
            target.col = i;
            if !gr.is_ws() {
              break;
            }
          }

          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
            end: target,
            inclusive: true,
          })
        }
        dir @ (Motion::StartOfLine | Motion::EndOfLine) => {
          let (inclusive, off) = match dir {
            Motion::StartOfLine => (false, isize::MIN),
            Motion::EndOfLine => (true, isize::MAX),
            _ => unreachable!(),
          };
          let target = this.offset_cursor(0, off);
          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
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

          this.eval_word_motion(*count, to, word, dir, ignore_trailing_ws, inclusive)
        }
        Motion::CharSearch(dir, dest, char) => {
          let off = this.search_char(dir, dest, char);
          let target = this.offset_cursor(0, off);
          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
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
            this.offset_cursor_wrapping(0, off)
          } else {
            this.offset_cursor(0, off)
          };

          (target != this.cursor.pos).then_some(MotionKind::Char {
            start: this.cursor.pos,
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
            let row = this.row();
            let target_row = this.offset_row(off);
            let (s, e) = ordered(row, target_row);
            Some(MotionKind::Line {
              start: s,
              end: e,
              inclusive: true,
            })
          } else {
            if this.saved_col.is_none() {
              this.saved_col = Some(this.calc_cursor_display_col());
            }
            let row = this.offset_row(off);
            let limit = if this.cursor.exclusive {
              this.lines[row].len().saturating_sub(1)
            } else {
              this.lines[row].len()
            };
            let target_col = this.saved_col.unwrap();
            let col = this.display_col_to_index(row, target_col).min(limit);
            let target = Pos { row, col };
            (target != this.cursor.pos).then_some(MotionKind::Char {
              start: this.cursor.pos,
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
            let row = this.row();
            let target_row = this.offset_row(off);
            let (s, e) = ordered(row, target_row);
            Some(MotionKind::Line {
              start: s,
              end: e,
              inclusive: false,
            })
          } else {
            let target = this.offset_cursor(off, 0);
            (target != this.cursor.pos).then_some(MotionKind::Char {
              start: this.cursor.pos,
              end: target,
              inclusive: true,
            })
          }
        }
        Motion::WholeBuffer => Some(MotionKind::Line {
          start: 0,
          end: this.lines.len().saturating_sub(1),
          inclusive: false,
        }),
        Motion::ToColumn => {
          let row = this.row();
          let end = Pos {
            row,
            col: count.saturating_sub(1),
          };
          Some(MotionKind::Char {
            start: this.cursor.pos,
            end,
            inclusive: end > this.cursor.pos,
          })
        }

        Motion::ToDelimMatch => this.find_delim_match(),
        Motion::ToBracket(direction) | Motion::ToParen(direction) | Motion::ToBrace(direction) => {
          let (opener, closer) = match motion {
            Motion::ToBracket(_) => ('[', ']'),
            Motion::ToParen(_) => ('(', ')'),
            Motion::ToBrace(_) => ('{', '}'),
            _ => unreachable!(),
          };
          match direction {
            Direction::Forward => {
              let mut depth = 0;
              let Some(target_pos) = this.scan_forward(|g| {
                if g.as_char() == Some(opener) {
                  depth += 1;
                }
                if g.as_char() == Some(closer) {
                  depth -= 1;
                  if depth <= 0 {
                    return true;
                  }
                }
                false
              }) else {
                return Ok(None);
              };
              return Ok(Some(MotionKind::Char {
                start: this.cursor.pos,
                end: target_pos,
                inclusive: true,
              }));
            }
            Direction::Backward => {
              let mut depth = 0;
              let Some(target_pos) = this.scan_backward(|g| {
                if g.as_char() == Some(closer) {
                  depth += 1;
                }
                if g.as_char() == Some(opener) {
                  depth -= 1;
                  if depth <= 0 {
                    return true;
                  }
                }
                false
              }) else {
                return Ok(None);
              };
              return Ok(Some(MotionKind::Char {
                start: this.cursor.pos,
                end: target_pos,
                inclusive: true,
              }));
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
        Motion::Line(l) => {
          let Some(l) = this.resolve_line_addr(l)? else {
            return Ok(None);
          };
          Some(MotionKind::Line {
            start: l,
            end: l + 1,
            inclusive: false,
          })
        }
        Motion::LineRange(s, e) => {
          let Some(s) = this.resolve_line_addr(s)? else {
            return Ok(None);
          };
          let Some(e) = this.resolve_line_addr(e)? else {
            return Ok(None);
          };
          let (s, e) = ordered(s, e);
          Some(MotionKind::Line {
            start: s,
            end: e,
            inclusive: true,
          })
        }
        Motion::BlockRange(s, e) => {
          let (s, e) = ordered(*s, *e);
          Some(MotionKind::Block { start: s, end: e })
        }
        dir @ (Motion::HalfScreenUp | Motion::HalfScreenDown) => {
          let off = match dir {
            Motion::HalfScreenUp => -(this.get_viewport_height() as isize / 2),
            Motion::HalfScreenDown => this.get_viewport_height() as isize / 2,
            _ => unreachable!(),
          };
          let row = this.row();
          let target_row = this.offset_row(off);
          Some(MotionKind::Line {
            start: target_row,
            end: row,
            inclusive: false,
          })
        }
        Motion::RepeatMotion | Motion::RepeatMotionRev => {
          unreachable!("Repeat motions should have been resolved in readline/mod.rs")
        }
        dir @ (Motion::Global(constraint, pat) | Motion::NotGlobal(constraint, pat)) => {
          let lines =
            this.get_matching_lines(constraint, pat, matches!(dir, Motion::Global(_, _)))?;

          this.last_global = Some(cmd.clone());
          Some(MotionKind::Lines { lines })
        }
        Motion::Null => None,
      };
      Ok(kind)
    };

    if check_hint {
      self.with_hint(eval)
    } else {
      eval(self)
    }
  }
  pub fn get_matching_lines(
    &self,
    constraint: &Motion,
    re: &str,
    polarity: bool,
  ) -> ShResult<Vec<usize>> {
    let (s, e) = match constraint {
      Motion::LineRange(s, e) => {
        let Some(s) = self.resolve_line_addr(s)? else {
          return Ok(vec![]);
        };
        let Some(e) = self.resolve_line_addr(e)? else {
          return Ok(vec![]);
        };
        ordered(s, e)
      }
      Motion::Line(addr) => {
        let Some(line) = self.resolve_line_addr(addr)? else {
          return Ok(vec![]);
        };
        (line, line)
      }
      _ => (0, self.lines.len().saturating_sub(1)),
    };

    let re =
      Regex::new(re).map_err(|e| sherr!(ParseErr, "Invalid regex in global command: {e}"))?;
    let mut acc = 0;
    let mut lines = vec![];

    loop {
      if !(s..=e).contains(&acc) {
        acc += 1 % self.lines.len();
        continue;
      }
      let Some(line) = self.get_row(acc) else { break };
      let line_str = line.to_string();
      if re.is_match(&line_str) == polarity {
        lines.push(acc);
      }

      if acc == self.lines.len().saturating_sub(1) {
        break;
      }
      acc += 1 % self.lines.len();
    }

    Ok(lines)
  }
  fn move_to_start(&mut self, motion: MotionKind) {
    match motion {
      MotionKind::Char { start, end, .. } => {
        let (s, _) = ordered(start, end);
        self.set_cursor(s);
      }
      MotionKind::Line { start, end, .. } => {
        let (s, _) = ordered(start, end);
        self.set_cursor(Pos { row: s, col: 0 });
      }
      MotionKind::Lines { lines } => {
        let Some(line) = lines.first() else {
          return;
        };
        self.set_cursor(Pos { row: *line, col: 0 });
      }
      MotionKind::Block { start, end } => unimplemented!(),
    }
  }
  /// Wrapper for apply_motion_inner that calls it with `accept_hint: false`
  fn apply_motion(&mut self, motion: MotionKind) -> ShResult<()> {
    self.apply_motion_inner(motion, false)
  }
  fn apply_motion_inner(&mut self, motion: MotionKind, accept_hint: bool) -> ShResult<()> {
    let apply = |this: &mut Self| -> ShResult<()> {
      match motion {
        MotionKind::Char { end, .. } => {
          this.set_cursor(end);
        }
        MotionKind::Line { start, .. } => {
          this.set_row(start);
        }
        MotionKind::Lines { lines } => {
          let Some(line) = lines.first() else {
            return Ok(());
          };
          this.set_row(*line);
        }
        MotionKind::Block { start, end } => unimplemented!(),
      }
      Ok(())
    };

    if accept_hint {
      self.with_hint(apply)
    } else {
      apply(self)
    }
  }
  fn extract_span(&mut self, span: (Pos, Pos), inclusive: bool) -> Vec<Line> {
    let (s, e) = ordered(span.0, span.1);
    let end = if inclusive {
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
  fn yank_span(&self, span: (Pos, Pos), inclusive: bool) -> Vec<Line> {
    let mut tmp = Self {
      lines: self.lines.clone(),
      cursor: self.cursor,
      ..Default::default()
    };
    tmp.extract_span(span, inclusive)
  }
  fn extract_range(&mut self, motion: &MotionKind) -> Vec<Line> {
    let extracted = match motion {
      MotionKind::Char {
        start,
        end,
        inclusive,
      } => self.extract_span((*start, *end), *inclusive),
      MotionKind::Lines { lines } => {
        let mut extracted_lines = vec![];
        for line_no in lines.iter().rev() {
          let line = self.lines.remove(*line_no);
          extracted_lines.push(line);
        }
        extracted_lines
      }
      MotionKind::Line {
        start,
        end,
        inclusive,
      } => {
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
  pub fn checked_calc_indent_level_for_pos(&mut self, pos: Pos) -> (usize, bool) {
    let mut lines = self.lines.clone();
    split_lines_at(&mut lines, pos);
    let raw = join_lines(&lines);

    self.indent_ctx.checked_calculate(&raw)
  }
  pub fn checked_calc_indent_level(&mut self) -> (usize, bool) {
    self.checked_calc_indent_level_for_pos(self.cursor.pos)
  }
  pub fn calc_indent_level(&mut self) -> usize {
    self.calc_indent_level_for_pos(self.cursor.pos)
  }
  pub fn calc_indent_level_for_pos(&mut self, pos: Pos) -> usize {
    self.checked_calc_indent_level_for_pos(pos).0
  }
  fn motion_mutation(&mut self, motion: &MotionKind, mut f: impl FnMut(&Grapheme) -> Grapheme) {
    match motion {
      MotionKind::Char {
        start,
        end,
        inclusive,
      } => {
        let (s, e) = ordered(start, end);
        if s.row == e.row {
          let range = if *inclusive {
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
        let end = if *inclusive { e.col + 1 } else { e.col };

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
      MotionKind::Lines { lines } => {
        for line_no in lines.iter().rev() {
          let line = self.line_mut(*line_no);
          for col in 0..line.len() {
            line[col] = f(&line[col]);
          }
        }
      }
      MotionKind::Line {
        start,
        end,
        inclusive,
      } => {
        let end = if *inclusive {
          *end
        } else {
          end.saturating_sub(1)
        };
        for row in *start..=end {
          let line = self.line_mut(row);
          for col in 0..line.len() {
            line[col] = f(&line[col]);
          }
        }
      }
      MotionKind::Block { start, end } => unimplemented!(),
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
      self.motion_mutation(&motion, &f);
    }
  }
  fn exec_verb(&mut self, cmd: &EditCmd) -> ShResult<()> {
    let EditCmd {
      register,
      verb,
      motion,
      ..
    } = cmd;
    let Some(VerbCmd(_, verb)) = verb else {
      // For verb-less motions in insert mode, merge hint before evaluating
      // so motions like `w` can see into the hint text
      let result = self.eval_motion_inner(cmd, true)?;
      if let Some(motion_kind) = result {
        self.apply_motion_inner(motion_kind, true)?;
      }
      return Ok(());
    };
    let count = motion.as_ref().map(|m| m.0).unwrap_or(1);

    match verb {
      Verb::Kill => {
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        let mut content = self.delete_range(&motion);
        if self.kill_ring.merging
          && let Some(last) = self.kill_ring.kills.back_mut()
        {
          last.append(&mut content);
        } else {
          self.kill_ring.push_back(content);
          if self.kill_ring.len() > Self::MAX_KILL_RING {
            self.kill_ring.pop_front();
          }
        }

        self.kill_ring.merging = true;
      }
      Verb::Delete | Verb::Change | Verb::Yank => {
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        let content = if *verb == Verb::Yank {
          self.yank_range(&motion)
        } else if *verb == Verb::Change && matches!(motion, MotionKind::Line { .. }) {
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
          MotionKind::Lines { .. } => {
            RegisterContent::Line(vec![content.last().cloned().unwrap_or_default()])
          }
          MotionKind::Block { .. } => RegisterContent::Block(content),
        };
        register.write_to_register(reg_content);

        match motion {
          MotionKind::Char { start, end, .. } => {
            let (s, _) = ordered(start, end);
            self.set_cursor(s);
          }
          MotionKind::Line {
            start,
            end,
            inclusive,
          } => {
            let end = if inclusive {
              end
            } else {
              end.saturating_sub(1)
            };
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
          MotionKind::Lines { lines } => {
            let Some(s) = lines.first() else {
              return Ok(());
            };
            self.set_row(*s);
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
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        self.motion_mutation(&motion, |gr| {
          gr.as_char()
            .map(rot13_char)
            .map(Grapheme::from)
            .unwrap_or_else(|| gr.clone())
        });
        self.move_to_start(motion);
      }
      Verb::ReplaceChar(ch) => {
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        self.motion_mutation(&motion, |_| Grapheme::from(*ch));
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
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        self.motion_mutation(&motion, |gr| {
          gr.as_char()
            .map(toggle_case_char)
            .map(Grapheme::from)
            .unwrap_or_else(|| gr.clone())
        });
        self.move_to_start(motion);
      }
      Verb::IncrementNumber(n) => {
        self.adjust_number(*n as i64);
      }
      Verb::DecrementNumber(n) => {
        self.adjust_number(-(*n as i64));
      }
      Verb::ToLower => {
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        self.motion_mutation(&motion, |gr| {
          gr.as_char()
            .map(|c| c.to_ascii_lowercase())
            .map(Grapheme::from)
            .unwrap_or_else(|| gr.clone())
        });
        self.move_to_start(motion);
      }
      Verb::ToUpper => {
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        self.motion_mutation(&motion, |gr| {
          gr.as_char()
            .map(|c| c.to_ascii_uppercase())
            .map(Grapheme::from)
            .unwrap_or_else(|| gr.clone())
        });
        self.move_to_start(motion);
      }
      Verb::Capitalize => {
        // Emacs Alt+C capitalization
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        let mut capitalized = false;
        self.motion_mutation(&motion, |gr| {
          let Some(ch) = gr.as_char() else {
            return gr.clone();
          };
          if !ch.is_ascii_alphabetic() {
            return gr.clone();
          }

          if capitalized {
            gr.as_char()
              .map(|c| c.to_ascii_lowercase())
              .map(Grapheme::from)
              .unwrap_or_else(|| gr.clone())
          } else {
            capitalized = true;
            gr.as_char()
              .map(|c| c.to_ascii_uppercase())
              .map(Grapheme::from)
              .unwrap_or_else(|| gr.clone())
          }
        });
        self.apply_motion(motion)?;
        self.cursor.pos = self.cursor.pos.col_add(1);
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
      Verb::KillCycle => {
        let Some(content) = self.kill_ring.next() else {
          return Ok(());
        };
        let Some(span) = self.kill_ring.kill_cycle_span else {
          return Ok(());
        };
        let total_len: usize =
          content.iter().map(|l| l.len()).sum::<usize>() + content.len().saturating_sub(1); // adds the newlines too

        let (s, e) = ordered(span.0, span.1);
        let old = self.extract_span((s, e), false);

        self.set_cursor(s);
        self.insert_lines_at(s, content);
        self.cursor.pos = self.offset_cursor_wrapping(0, total_len as isize);
        self.kill_ring.kill_cycle_span = Some((s, self.cursor.pos));
      }
      Verb::KillPut => {
        let Some(content) = self.kill_ring.next() else {
          return Ok(());
        };
        let paste_pos = self.cursor.pos;
        let total_len: usize =
          content.iter().map(|l| l.len()).sum::<usize>() + content.len().saturating_sub(1); // adds the newlines too
        self.insert_lines_at(paste_pos, content);
        self.cursor.pos = self.offset_cursor_wrapping(0, total_len as isize);
        self.kill_ring.kill_cycle_span = Some((paste_pos, self.cursor.pos));
      }
      Verb::Put(anchor) => {
        let Some(content) = register.read_from_register() else {
          return Ok(());
        };
        if let Some(motion) = self.select_range() {
          // we have a selected range to replace.
          // no need to overcomplicate it, the Verb::Delete handler
          // knows exactly how to do this.
          let rec_cmd = cmd
            .new_with_verb(Some(verb!(Verb::Delete)))
            .new_with_motion(Some(motion!(motion)));

          self.exec_verb(&rec_cmd)?;
        }
        match content {
          RegisterContent::Span(lines) => {
            let move_cursor = lines.len() == 1 && lines[0].len() > 1;
            let content_len: usize = lines.iter().map(|l| l.len()).sum();
            let row = self.row();
            let col = match anchor {
              Anchor::After => (self.col() + 1).min(self.cur_line().len()),
              Anchor::Before => self.col(),
            };
            let pos = Pos {
              row: self.row(),
              col,
            };
            let start_len = self.lines[row].len();

            self.insert_lines_at(pos, lines);

            let end_len = self.lines[row].len();
            let mut delta = end_len.saturating_sub(start_len);
            if let Anchor::Before = anchor {
              delta = delta.saturating_sub(1);
            }
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
          RegisterContent::Block(lines) => unimplemented!(),
          RegisterContent::Empty => {}
        }
      }
      Verb::InsertModeLineBreak(anchor) => match anchor {
        Anchor::After => {
          let row = self.row();
          let target = (row + 1).min(self.lines.len());
          self.lines.insert(target, Line::default());

          let level = self.calc_indent_level_for_pos(Pos {
            row: target,
            col: 0,
          });
          let line = self.line_mut(target);
          let mut col = 0;
          for tab in std::iter::repeat_n(Grapheme::from('\t'), level) {
            line.insert(0, tab);
            col += 1;
          }

          self.cursor.pos = Pos { row: row + 1, col };
        }
        Anchor::Before => {
          let row = self.row();
          self.lines.insert(row, Line::default());

          let level = self.calc_indent_level_for_pos(Pos { row, col: 0 });
          let line = self.line_mut(row);
          let mut col = 0;
          for tab in std::iter::repeat_n(Grapheme::from('\t'), level) {
            line.insert(0, tab);
            col += 1;
          }

          self.cursor.pos = Pos { row, col };
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
        let mut row = self.row();
        let mut count = count;
        if self.select_range().is_some() {
          let Some(MotionKind::Line {
            start,
            end,
            inclusive,
          }) = self.eval_motion(cmd)?
          else {
            unreachable!()
          };
          let (s, e) = ordered(start, end);
          count = if inclusive { e - s + 1 } else { e - s };
          row = s;
        }
        self.cursor.exclusive = false;
        for _ in 0..count {
          let target_pos = Pos {
            row,
            col: self.offset_col(row, isize::MAX),
          };
          if row == self.lines.len() - 1 {
            break;
          }

          let mut next_line = self.lines.remove(row + 1).trim_start();
          let this_line = self.line_mut(row);
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
        self.insert(Grapheme::from(*ch));
        if let Some(motion) = self.eval_motion(cmd)? {
          self.apply_motion(motion)?;
        }
      }
      Verb::Insert(s) => self.insert_str(s),
      Verb::Indent | Verb::Dedent => {
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        let lines: Either<_, _> = match motion {
          MotionKind::Char { start, end, .. } => {
            Either::Left(self.line_iter_mut(ordered(start.row, end.row)))
          }
          MotionKind::Line { start, end, .. } => {
            Either::Left(self.line_iter_mut(ordered(start, end)))
          }
          MotionKind::Lines { lines } => Either::Right(self.line_iter_mut_by_indices(&lines)),
          MotionKind::Block { .. } => unimplemented!(),
        };
        let mut col_offset = 0;
        for line in lines {
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
        let Some(motion) = self.eval_motion(cmd)? else {
          return Ok(());
        };
        let line_nums: Either<_, _> = match motion {
          MotionKind::Char {
            start,
            end,
            inclusive,
          } => {
            let (s, e) = ordered(start.row, end.row);
            Either::Left(s..=e)
          }
          MotionKind::Line {
            start,
            end,
            inclusive,
          } => {
            let (s, e) = ordered(start, end);
            Either::Left(s..=e)
          }
          MotionKind::Lines { lines } => Either::Right(lines.into_iter()),
          MotionKind::Block { start, end } => unimplemented!(),
        };
        let line_nums: Vec<usize> = line_nums.collect();
        self.equalize_rows(line_nums);
      }
      Verb::AcceptLineOrNewline => {
        // If we are here, we did not accept the line
        // so we break to a new line
        self.break_line();
      }
      Verb::ShellCmd(sh_cmd) => {
        let Some(MotionKind::Line {
          start,
          end,
          inclusive,
        }) = self.eval_motion(cmd)?
        else {
          self.verb_shell_cmd(sh_cmd, None)?;
          return Ok(());
        };
        let (s, e) = ordered(start, end);
        let lines = self.lines.drain(s..=e).collect::<Vec<_>>();
        if self.lines.is_empty() {
          self.lines.push(Line::default());
        }
        let input = format!("{}\n", join_lines(&lines));
        let output = self.verb_shell_cmd(sh_cmd, Some(&input))?;
        let new_lines = to_lines(output.unwrap_or_default());
        self.lines.splice(s..s, new_lines);
      }
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
          let pre_cmd = read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd));
          let post_cmd = read_logic(|l| l.get_autocmds(AutoCmdKind::PostCmd));
          pre_cmd.exec();
          let output = match capture_command(cmd, None) {
            Ok(out) => out,
            Err(e) => {
              post_cmd.exec();
              e.print_error();
              return Ok(());
            }
          };
          post_cmd.exec();
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

          let pre_cmd = read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd));
          let post_cmd = read_logic(|l| l.get_autocmds(AutoCmdKind::PostCmd));
          pre_cmd.exec();
          exec_nonint(cmd.to_string(), Some(stack), Some("ex write".into()))?;
          post_cmd.exec();
        }
      },
      Verb::Edit(path) => {
        if read_vars(|v| v.try_get_var("EDITOR")).is_none() {
          write_meta(|m| m.post_system_message("$EDITOR is unset. Aborting edit.".into()));
        } else {
          let input = format!("$EDITOR {}", path.display());
          exec_int(input, Some("ex edit".into()))?;
        }
      }

      Verb::Stash(args) => {
        let Ok(stash) = Stash::new() else {
          write_meta(|m| {
            m.post_status_message("Failed to open stash - database unreachable".into())
          });
          return Ok(());
        };
        match args {
          StashArgs::Push(arg) => {
            if self.is_empty() {
              write_meta(|m| m.post_status_message("Buffer is empty, nothing to stash".into()));
              return Ok(());
            }
            let stash_len = stash.stack_len();
            let name = arg.clone().filter(|a| !a.trim().is_empty());
            let buffer = self.joined();
            let (s, e) = (self.row(), self.col());

            stash.push(name, &buffer, (s, e))?;
            self.clear_buffer();
            self.clear_hint();
            self.set_cursor(Pos::new(0, 0));
          }
          StashArgs::Pop(arg) => {
            let stack_len = stash.stack_len();
            let idx = arg
              .as_ref()
              .map(|a| a.parse::<usize>())
              .transpose()
              .ok()
              .flatten()
              .unwrap_or(stack_len.saturating_sub(1));

            let StashedCmd {
              name,
              buffer,
              cursor_pos,
            } = match stash.pop(idx) {
              Ok(ent) => match ent {
                Some(ent) => {
                  write_meta(|m| m.post_status_message("stash: Popped stack entry".to_string()));
                  ent
                }
                None => {
                  write_meta(|m| {
                    if stack_len == 0 {
                      m.post_status_message("stash: Stash is empty".into());
                    } else {
                      m.post_status_message(format!("stash: No stash entry at index '{idx}'"));
                    }
                  });
                  return Ok(());
                }
              },
              Err(e) => {
                write_meta(|m| {
                  m.post_status_message(format!("stash: Failed to pop stash entry: {e}"))
                });
                return Ok(());
              }
            };
            let cursor_pos = match self.parse_pos(&cursor_pos) {
              Ok(pos) => pos,
              Err(e) => {
                write_meta(|m| {
                  m.post_status_message(format!(
                    "stash: Failed to parse cursor position from stash: {e}"
                  ))
                });
                Pos { row: 0, col: 0 }
              }
            };

            self.set_buffer(buffer);
            self.set_cursor(cursor_pos);
          }
          StashArgs::Drop(arg) => {
            let idx = arg
              .as_ref()
              .map(|a| a.parse::<usize>())
              .transpose()
              .ok()
              .flatten()
              .unwrap_or(0);
            let stack_len = stash.stack_len();

            match stash.pop(idx).ok().flatten() {
              Some(_) => {
                write_meta(|m| {
                  m.post_status_message(format!("stash: Dropped stash entry '{idx}'"))
                });
              }
              None => {
                if stack_len == 0 {
                  write_meta(|m| {
                    m.post_status_message("stash: Stash is empty, nothing to drop".into())
                  });
                } else {
                  write_meta(|m| {
                    m.post_status_message(format!("stash: No stack entry at index '{idx}'"))
                  });
                }
              }
            }
          }
          StashArgs::Apply(arg) => {
            let stack_len = stash.stack_len();
            let name = arg
              .clone()
              .unwrap_or(stack_len.saturating_sub(1).to_string());

            let Some(StashedCmd {
              name,
              buffer,
              cursor_pos,
            }) = stash.get(&name)?
            else {
              if let Ok(idx) = name.parse::<usize>() {
                if stack_len == 0 {
                  write_meta(|m| m.post_status_message("stash: Stash is empty".into()));
                } else {
                  write_meta(|m| {
                    m.post_status_message(format!("stash: No stash entry at index '{idx}'"))
                  });
                }
              } else {
                write_meta(|m| {
                  m.post_status_message(format!("stash: No stash entry named '{name}'"))
                });
              }
              return Ok(());
            };

            if let Some(name) = name {
              write_meta(|m| {
                m.post_status_message(format!("stash: Applied stash entry '{}'", name))
              });
            }

            let cursor_pos = match self.parse_pos(&cursor_pos) {
              Ok(pos) => pos,
              Err(e) => {
                write_meta(|m| {
                  m.post_status_message(format!(
                    "stash: Failed to parse cursor position from stash: {e}"
                  ))
                });
                Pos { row: 0, col: 0 }
              }
            };

            self.set_buffer(buffer);
            self.set_cursor(cursor_pos);
          }
          StashArgs::Insert(arg) => {
            let stack_len = stash.stack_len();
            let name = arg
              .clone()
              .unwrap_or(stack_len.saturating_sub(1).to_string());

            let Some(StashedCmd {
              name,
              buffer,
              cursor_pos,
            }) = stash.get(&name)?
            else {
              if let Ok(idx) = name.parse::<usize>() {
                if stack_len == 0 {
                  write_meta(|m| m.post_status_message("stash: Stash is empty".into()));
                } else {
                  write_meta(|m| {
                    m.post_status_message(format!("stash: No stash entry at index '{idx}'"))
                  });
                }
              } else {
                write_meta(|m| {
                  m.post_status_message(format!("stash: No stash entry named '{name}'"))
                });
              }
              return Ok(());
            };

            let cursor_offset = match self.parse_pos(&cursor_pos) {
              Ok(pos) => pos,
              Err(e) => {
                write_meta(|m| {
                  m.post_system_message(format!(
                    "stash: Failed to parse cursor position from stash: {e}"
                  ))
                });
                Pos { row: 0, col: 0 }
              }
            };
            let lines = to_lines(&buffer);
            let num_lines = lines.len();
            let line_range = self.row()..self.row() + num_lines;

            self.insert_lines_at(self.cursor.pos, lines);
            self.cursor.pos = self.cursor.pos + cursor_offset;
            self.fix_cursor();
            if read_shopts(|o| o.line.auto_indent) {
              self.equalize_rows(line_range.collect());
            }
          }
          StashArgs::Swap(arg) => todo!(),
          StashArgs::List(arg) => {
            let output = match arg {
              Some(StashListArg::Stack) => {
                stash.list(/*named_only:*/ false, /*stack_only:*/ true)
              }
              Some(StashListArg::Named) => {
                stash.list(/*named_only:*/ true, /*stack_only:*/ false)
              }
              None => stash.list(/*named_only:*/ false, /*stack_only:*/ false),
            };
            if output.trim().is_empty() {
              match arg {
                Some(StashListArg::Named) => {
                  write_meta(|m| m.post_status_message("stash: No named stash entries".into()));
                }
                Some(StashListArg::Stack) => {
                  write_meta(|m| m.post_status_message("stash: Stack is empty".into()));
                }
                None => {
                  write_meta(|m| m.post_status_message("stash: No stash entries".into()));
                }
              }
            } else {
              for line in output.lines() {
                write_meta(|m| m.post_system_message(line.to_string()));
              }
            }
          }
        }
      }

      Verb::EndOfFile => {
        self.lines.clear();
      }

      Verb::PrintPosition => {
        let num_lines = self.lines.len();
        let row = self.row() + 1;
        let col = self.col() + 1;
        let total_graphemes = self.count_graphemes();
        let (left, _) = split_lines(self.lines.clone(), self.cursor.pos);
        let total_in_left = left.iter().map(|l| l.len()).sum::<usize>();
        let percentage = if total_graphemes > 0 {
          (total_in_left as f64 / total_graphemes as f64) * 100.0
        } else {
          100.0
        }
        .round() as usize;

        let msg = format!("line: {row}/{num_lines}, col: {col} --{percentage}%--");
        write_meta(|m| {
          m.post_status_message(msg);
        })
      }

      Verb::TransposeChar => {
        let Pos { row, col: c_col } = self.cursor.pos;
        let prev_char = Pos {
          row,
          col: c_col.saturating_sub(1),
        };

        let Some(gr) = self.remove_at(prev_char) else {
          return Ok(());
        };

        self.insert_at(self.cursor.pos, gr);
        self.cursor.pos = self.cursor.pos.col_add(1);
      }
      Verb::TransposeWord => {
        // Find the word at/after cursor
        let this_word = if self.cursor_on_ws() {
          let Some(pos) = self.eval_word_motion(
            1,
            &To::Start,
            &Word::Normal,
            &Direction::Forward,
            false,
            false,
          ) else {
            return Ok(());
          };
          let MotionKind::Char { end, .. } = pos else {
            unreachable!()
          };
          end
        } else {
          self.cursor.pos
        };
        let Some(MotionKind::Char {
          start,
          end,
          inclusive,
        }) = self.text_obj_word(1, this_word, Word::Normal, Bound::Inside)
        else {
          return Ok(());
        };
        let end = if inclusive { end.col_add(1) } else { end };
        let this_word_span = (start, end);

        let back_count = if self.cursor_on_ws() { 1 } else { 2 };

        // Find the previous word
        let prev_word = if let Some(pos) = self.eval_word_motion(
          back_count,
          &To::Start,
          &Word::Normal,
          &Direction::Backward,
          false,
          false,
        ) {
          let MotionKind::Char { end, .. } = pos else {
            unreachable!()
          };
          end
        } else {
          return Ok(());
        };
        let Some(MotionKind::Char {
          start,
          end,
          inclusive,
        }) = self.text_obj_word(1, prev_word, Word::Normal, Bound::Inside)
        else {
          return Ok(());
        };
        let end = if inclusive { end.col_add(1) } else { end };
        let prev_word_span = (start, end);

        // Bail if the spans overlap or are the same word
        if prev_word_span.0 >= this_word_span.0 {
          return Ok(());
        }

        // Yank both words non-destructively
        let this_content = self.yank_span(this_word_span, false);
        let prev_content = self.yank_span(prev_word_span, false);

        // Compute lengths before we move the content vecs
        let this_content_len: usize = this_content.iter().map(|l| l.len()).sum::<usize>()
          + this_content.len().saturating_sub(1);
        let prev_content_len: usize = prev_content.iter().map(|l| l.len()).sum::<usize>()
          + prev_content.len().saturating_sub(1);

        // Remove later word first so earlier positions stay valid
        self.extract_span(this_word_span, false);
        self.insert_lines_at(this_word_span.0, prev_content);

        // Remove earlier word (its positions are unaffected by later changes)
        self.extract_span(prev_word_span, false);
        self.insert_lines_at(prev_word_span.0, this_content);

        // Cursor goes after the later word, which now holds prev_content.
        // The later word's start shifted by the size difference from
        // replacing the earlier word with different-length content.
        let shift = this_content_len as isize - prev_content_len as isize;
        let new_later_start = Pos {
          row: this_word_span.0.row,
          col: (this_word_span.0.col as isize + shift) as usize,
        };
        self.set_cursor(new_later_start);
        self.cursor.pos = self.offset_cursor_wrapping(0, prev_content_len as isize);
      }

      Verb::Complete
      | Verb::ExMode
      | Verb::InsertMode
      | Verb::NormalMode
      | Verb::VisualMode
      | Verb::VerbatimMode
      | Verb::ReplaceMode
      | Verb::VisualModeLine
      | Verb::VisualModeBlock
      | Verb::CompleteBackward
      | Verb::VisualModeSelectLast => {
        let Some(motion_kind) = self.eval_motion_inner(cmd, true)? else {
          return Ok(());
        };
        self.apply_motion_inner(motion_kind, true)?;
      }
      Verb::Substitute(old, new, flags) => {
        let line_nums: Vec<usize> = match self.eval_motion(cmd)? {
          Some(MotionKind::Lines { lines }) => lines,
          Some(MotionKind::Line {
            start,
            end,
            inclusive,
          }) => {
            if inclusive {
              (start..=end).collect()
            } else {
              (start..end).collect()
            }
          }
          None => vec![self.row()],

          m => {
            return Err(sherr!(
              InternalErr,
              "Substitute verb only supports linewise motions, found {m:?}"
            ));
          }
        };

        let re = match regex::Regex::new(old) {
          Ok(re) => re,
          Err(e) => {
            write_meta(|m| m.post_status_message(format!("{e}")));
            return Ok(());
          }
        };

        // TODO: implement flag logic
        let mut changes: Vec<(usize, Vec<Line>)> = vec![];
        let lines = self
          .lines
          .iter()
          .enumerate()
          .filter(|(i, _)| line_nums.contains(i));

        for (i, line) in lines {
          let s = line.to_string();
          let res = if flags.contains(SubFlags::GLOBAL) {
            re.replace_all(&s, new)
          } else {
            re.replace(&s, new)
          };
          let lines = to_lines(res);
          changes.push((i, lines));
        }

        for (i, change) in changes.into_iter().rev() {
          self.lines.remove(i);
          for (j, new_line) in change.into_iter().enumerate() {
            self.lines.insert(i + j, new_line);
          }
        }

        self.last_substitute = Some(cmd.clone());
      }
      Verb::RepeatSubstitute => {
        if let Some(sub) = self.last_substitute.clone() {
          let merged = EditCmd {
            register: cmd.register,
            verb: sub.verb,
            motion: cmd.motion.clone().or(sub.motion),
            raw_seq: cmd.raw_seq.clone(),
            flags: cmd.flags,
          };
          self.exec_cmd(merged)?;
        }
      }
      Verb::RepeatGlobal => {
        if let Some(global) = self.last_global.clone() {
          let merged = EditCmd {
            register: cmd.register,
            verb: global.verb,
            motion: cmd.motion.clone().or(global.motion),
            raw_seq: cmd.raw_seq.clone(),
            flags: cmd.flags,
          };
          self.exec_cmd(merged)?;
        }
      }
      Verb::RepeatLast
      | Verb::Interrupt
      | Verb::Quit
      | Verb::Normal(_)
      | Verb::HistoryDown
      | Verb::HistoryUp
      | Verb::DeleteOrEof
      | Verb::ClearScreen => unreachable!("{verb:?} should be handled in readline/mod.rs"),
    }

    Ok(())
  }
  pub fn equalize_rows(&mut self, line_nums: Vec<usize>) {
    for row in line_nums {
      let line_len = self.line(row).len();

      // we are going to calculate the level twice, once at column = 0 and once at column = line.len()
      // "b-b-b-b-but muh performance" i dont care
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
  /// Provides a public interface for editing the buffer in a way that is recognized by the undo system.
  /// Any change made by the provided function will be tracked in the undo stack.
  pub fn edit<T, F: FnMut(&mut Self) -> T>(&mut self, mut f: F) -> T {
    let before = self.lines.clone();
    let old_cursor = self.cursor.pos;

    let res = f(self);

    if self.is_empty() {
      self.set_hint(None);
    }

    let new_cursor = self.cursor.pos;
    self.handle_edit(before, new_cursor, old_cursor);

    res
  }
  pub fn start_undo_merge(&mut self) {
    self.merging_undos = true;
  }
  pub fn stop_undo_merge(&mut self) {
    self.merging_undos = false;
  }
  pub fn exec_cmd(&mut self, cmd: EditCmd) -> ShResult<()> {
    let is_char_insert = cmd.verb.as_ref().is_some_and(|v| v.1.is_char_insert());
    let is_kill = cmd.verb.as_ref().is_some_and(|v| v.1 == Verb::Kill);
    let is_killring_op = cmd
      .verb
      .as_ref()
      .is_some_and(|v| matches!(v.1, Verb::KillCycle | Verb::KillPut));
    let starts_merge = cmd
      .verb
      .as_ref()
      .is_some_and(|v| matches!(v.1, Verb::Change));
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
    let is_separator = cmd.is_separator_insert();

    if !is_vertical {
      self.saved_col = None;
    }

    let before = self.lines.clone();
    let old_cursor = self.cursor.pos;

    if is_separator
      && !self.grapheme_before_cursor().is_none_or(|gr| gr.is_ws())
      && read_shopts(|o| o.prompt.expand_aliases)
    {
      self.attempt_alias_expansion();
    }

    // Execute the command
    let res = self.exec_verb(&cmd);

    if self.is_empty() {
      self.set_hint(None);
    }

    let new_cursor = self.cursor.pos;

    // Stop merging on any non-char-insert command, even if buffer didn't change
    if !self.merging_undos
      && !is_char_insert
      && !is_undo_op
      && let Some(edit) = self.undo_stack.last_mut()
    {
      edit.merging = false;
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
        if (starts_merge || self.merging_undos)
          && let Some(edit) = self.undo_stack.last_mut()
        {
          edit.merging = true;
        }
      }
    }

    self.fix_cursor();

    if !is_kill {
      self.kill_ring.merging = false;
    }

    if !is_killring_op {
      self.kill_ring.reset();
    }

    if let Some(Hint::Override(hint_lines)) = self.hint.as_ref()
      && !is_prefix_lines(&self.lines, hint_lines)
    {
      self.clear_hint();
    }

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
    // we are now going to enforce some invariants and do some bookkeeping
    if self.lines.is_empty() {
      // self.lines must always have at least one line
      self.lines.push(Line::default());
    }
    if self.cursor.pos.row >= self.lines.len() {
      // clamp this now so self.cur_line() cannot panic
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

    // update viewport scroll offset
    self.update_scroll_offset();
  }

  pub fn joined(&self) -> String {
    let mut lines = vec![];
    for line in &self.lines {
      lines.push(line.to_string());
    }
    lines.join("\n")
  }

  pub fn set_buffer(&mut self, s: String) {
    self.lines = to_lines(&s);
    if self.lines.is_empty() {
      self.lines.push(Line::default());
    }
    self.clear_concats();
    self.fix_cursor();
  }

  pub fn clear_buffer(&mut self) {
    self.lines = vec![Line::default()];
    self.clear_concats();
    self.fix_cursor();
  }

  pub fn clear_hint(&mut self) {
    self.hint = None;
  }

  pub fn set_hint(&mut self, hint: Option<Hint>) {
    if self.is_empty() {
      self.hint = None;
      return;
    }

    let Some(hint) = hint else {
      if !matches!(&self.hint, Some(Hint::Override(_))) {
        self.hint = None;
      }
      return;
    };

    if let Some(old_hint) = self.hint.as_ref()
      && *old_hint > hint
    {
      // order comparisons on hints are priority checks
      // if older hint has higher priority, keep it instead of replacing with lower-priority new hint
      return;
    }

    if !read_shopts(|o| o.line.auto_suggest) {
      self.hint = None;
      return;
    }

    self.hint = (!hint.is_empty()).then_some(hint);
  }

  pub fn has_hint(&self) -> bool {
    self
      .hint
      .as_ref()
      .is_some_and(|h| !h.lines().is_empty() && h.lines().iter().any(|l| !l.is_empty()))
  }

  pub fn hint_lines(&self) -> Vec<Line> {
    self
      .hint
      .as_ref()
      .map(|h| h.lines().to_vec())
      .unwrap_or_default()
  }

  pub fn get_hint_text(&self) -> String {
    self.try_get_hint_text().unwrap_or_default()
  }

  pub fn try_get_hint_text(&self) -> Option<String> {
    self.hint.as_ref().map(|h| h.display(Some(&self.joined())))
  }

  pub fn join_hint(&self) -> String {
    self.try_join_hint().unwrap_or_default()
  }

  pub fn try_join_hint(&self) -> Option<String> {
    self.hint.as_ref().map(|h| h.raw())
  }

  pub fn accept_hint(&mut self) {
    let Some(mut hint) = self.hint.take() else {
      return;
    };
    let Some(mut hint_lines) = strip_prefix_lines(hint.take_lines(), &self.lines) else {
      return;
    };
    attach_lines(&mut self.lines, &mut hint_lines);
    self.attempt_alias_expansion_all();

    self.set_cursor(Pos::MAX);
    self.fix_cursor();
  }

  pub fn with_initial(mut self, s: &str, cursor_pos: usize) -> Self {
    self.set_buffer(s.to_string());
    // In the flat model, cursor_pos was a flat offset. Map to col on row .
    self.cursor.pos = Pos {
      row: 0,
      col: cursor_pos.min(s.len()),
    };
    self
  }

  pub fn move_cursor_to_end(&mut self) {
    self.set_cursor(Pos::MAX);
  }

  pub fn cursor_max(&self) -> usize {
    // In single-line mode this is the length of the first line
    // In multi-line mode this returns total grapheme count (for flat compat)
    if self.lines.len() == 1 {
      self.lines[0].len()
    } else {
      self.count_graphemes()
    }
  }

  pub fn cursor_at_max(&self) -> bool {
    let last_row = self.lines.len().saturating_sub(1);
    let max = if self.cursor.exclusive {
      self.lines[last_row].len().saturating_sub(1)
    } else {
      self.lines[last_row].len()
    };
    self.cursor.pos.row == last_row && self.cursor.pos.col >= max
  }

  pub fn set_cursor_clamp(&mut self, exclusive: bool) {
    self.cursor.exclusive = exclusive;
  }

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
      && self.hint.as_ref().is_none_or(|h| h.lines().len() <= 1)
  }

  pub fn slice(&self, range: std::ops::Range<usize>) -> Option<String> {
    let joined = self.joined();
    let graphemes: Vec<&str> = joined.graphemes(true).collect();
    if range.start > graphemes.len() || range.end > graphemes.len() {
      return None;
    }
    Some(graphemes[range].join(""))
  }

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

  pub fn select_motion(&self) -> Option<MotionKind> {
    let range = self.select_range()?;
    match range {
      Motion::CharRange(s, e) => {
        let (s, e) = ordered(s, e);
        Some(MotionKind::Char {
          start: s,
          end: e,
          inclusive: true,
        })
      }
      Motion::LineRange(s, e) => {
        let s = self.resolve_line_addr(&s).ok()??;
        let e = self.resolve_line_addr(&e).ok()??;
        let (s, e) = ordered(s, e);
        Some(MotionKind::Line {
          start: s,
          end: e,
          inclusive: true,
        })
      }
      Motion::BlockRange(s, e) => todo!(),
      _ => unreachable!(),
    }
  }

  pub fn select_range(&self) -> Option<Motion> {
    let mode = self.select_mode.as_ref()?;
    match mode {
      SelectMode::Char(pos) => {
        let (s, e) = ordered(self.cursor.pos, *pos);
        Some(Motion::CharRange(s, e))
      }
      SelectMode::Line(pos) => {
        let (s, e) = ordered(self.row() + 1, pos.row + 1);
        Some(Motion::LineRange(LineAddr::Number(s), LineAddr::Number(e)))
      }
      SelectMode::Block(pos) => {
        let (s, e) = ordered(self.cursor.pos, *pos);
        Some(Motion::BlockRange(s, e))
      }
    }
  }

  pub fn is_selecting(&self) -> bool {
    self.select_mode.is_some()
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
    Pos {
      row: last_row,
      col: last_col,
    }
  }

  pub fn cursor_to_flat(&self) -> usize {
    self.pos_to_flat(self.cursor.pos)
  }

  pub fn anchor_to_flat(&self) -> Option<usize> {
    self.select_mode.map(|r| match r {
      SelectMode::Char(pos) | SelectMode::Block(pos) | SelectMode::Line(pos) => {
        self.pos_to_flat(pos)
      }
    })
  }

  pub fn set_cursor_from_flat(&mut self, flat: usize) {
    self.cursor.pos = self.pos_from_flat(flat);
    self.fix_cursor();
  }
  pub fn set_anchor_from_flat(&mut self, flat: usize) {
    let new_pos = self.pos_from_flat(flat);
    self.set_anchor(new_pos);
  }
  pub fn set_anchor(&mut self, new_pos: Pos) {
    match self.select_mode.as_mut() {
      Some(SelectMode::Line(pos)) | Some(SelectMode::Block(pos)) | Some(SelectMode::Char(pos)) => {
        *pos = new_pos
      }
      None => unreachable!(),
    }
  }

  pub fn grapheme_positions(&self) -> Vec<(Pos, Grapheme)> {
    Self::enumerate_graphemes(&self.lines)
  }

  pub fn enumerate_graphemes(lines: &[Line]) -> Vec<(Pos, Grapheme)> {
    lines
      .iter()
      .enumerate()
      .flat_map(|(row, line)| {
        line
          .graphemes()
          .iter()
          .cloned()
          .enumerate()
          .map(move |(col, g)| (Pos { row, col }, g))
      })
      .collect()
  }

  pub fn attempt_inline_expansion(&mut self, history: &History) -> bool {
    let hist_res = self.attempt_history_expansion(history);
    let alias_res = self.attempt_alias_expansion();

    hist_res || alias_res
  }

  fn word_before_cursor(&mut self) -> Option<(Pos, Pos)> {
    let word_start = self.word_motion_b(&Word::Big, self.cursor.pos)?;
    Some(ordered(word_start, self.cursor.pos))
  }

  fn get(&mut self, pos: Pos) -> Option<Grapheme> {
    self
      .lines
      .get(pos.row)
      .and_then(|line| line.graphemes().get(pos.col))
      .cloned()
  }

  fn grapheme_before_cursor(&mut self) -> Option<Grapheme> {
    self.get(self.cursor.pos.col_add_signed(-1))
  }
  fn grapheme_after_cursor(&mut self) -> Option<Grapheme> {
    self.get(self.cursor.pos)
  }

  pub fn attempt_alias_expansion_all(&mut self) -> bool {
    let raw = self.joined();
    let mut seen = HashSet::new();
    let (result, first_pos) = AliasExpander::new(raw.clone(), &mut seen).expand();
    if first_pos.is_some() {
      self.lines = to_lines(result);
      true
    } else {
      false
    }
  }

  pub fn attempt_alias_expansion(&mut self) -> bool {
    let (to_cursor, mut after_cursor) = split_lines(self.lines.clone(), self.cursor.pos);
    let raw = join_lines(&to_cursor);
    let mut tokens = LexStream::new(raw.clone().into(), LexFlags::empty())
      .filter_map(Result::ok)
      .filter(|tk| !matches!(tk.class, TkRule::SOI | TkRule::EOI | TkRule::Null))
      .collect::<Vec<_>>();
    while tokens
      .last()
      .is_some_and(|tk| !tk.flags.contains(TkFlags::IS_CMD))
    {
      tokens.pop();
    }

    let Some(last) = tokens.pop() else {
      return false;
    };
    if !last.flags.contains(TkFlags::IS_CMD) {
      return false;
    }
    let tk_start = last.span.start();
    let word = last.as_str();

    if let Some(alias) = read_logic(|l| l.aliases().get(word).cloned())
      && let alias = alias.to_string()
      && !raw[tk_start..].starts_with(&alias)
    {
      let delta = alias.graphemes(true).count() as isize - word.graphemes(true).count() as isize;
      let expanded = last.replaced(&alias);

      self.lines = to_lines(expanded);
      attach_lines(&mut self.lines, &mut after_cursor);
      self.cursor.pos = self.cursor.pos.col_add_signed(delta);

      true
    } else {
      false
    }
  }

  /// The inner logic of `attempt_history_expansion()`. This function calls itself recursively when it encounters command substitutions.
  /// This is necessary because of the following nasty edge case:
  /// ```bash
  /// echo "foo $(echo 'bar!') biz"
  /// ```
  /// The exclamation point is inside of both double and single quotes here. According to shell language though, it's really just in single quotes because the command substitution is it's own parsing context.
  /// Pressing enter on this case with a normal flat parse will attempt history expansion. But a parse that recurses into command subs will not.
  /// The easiest way to handle this is to simply do lightweight recursive descent whenever we see the start of a command sub.
  pub fn find_history_expansions(
    &mut self,
    changes: &mut Vec<((Pos, Pos), String)>,
    positions: impl Iterator<Item = (Pos, Grapheme)>,
    history: &History,
    offset: Pos,
  ) -> bool {
    let mut positions = positions.peekable();
    let mut qt_state = QuoteState::default();

    // Map a sub-buffer position to the original buffer's coordinate space.
    // On row 0 of the sub-buffer, columns are offset from the anchor point.
    // On subsequent rows, columns map directly (same line structure).
    let map_pos = |slf: &Self, sub_pos: Pos, offset: Pos| -> Pos {
      if sub_pos.row == 0 {
        let (r, c) = slf.offset_col_wrapping_at(offset.row, sub_pos.col as isize, offset);
        Pos::new(r, c)
      } else {
        Pos::new(offset.row + sub_pos.row, sub_pos.col)
      }
    };

    while let Some((pos, gr)) = positions.next() {
      let Some(ch) = gr.as_char() else { continue };
      match ch {
        symbol @ ('$' | '`') if qt_state.in_double() => {
          let mut lines = vec![];
          let mut cur_line = vec![];
          if let Some((_, gr2)) = positions.peek()
            && let Some('(') = gr2.as_char()
          {
            // command substitution. read until we find matching paren.
            let mut paren_depth = 1;
            match_loop!(positions.next() => (_,gr) => gr.as_char(), {
              Some('\\') => {
                if let Some((_,gr2)) = positions.next() {
                  cur_line.push(gr2.clone());
                }
              }
              Some('$') => {
                let Some((pos,gr2)) = positions.peek() else {
                  cur_line.push(gr.clone());
                  continue
                };
                let Some('(') = gr2.as_char() else {
                  cur_line.push(gr.clone());
                  continue
                };

                positions.next();
                paren_depth += 1;
                cur_line.push(Grapheme::from('$'));
                cur_line.push(Grapheme::from('('));
              }
              Some(')') => {
                paren_depth -= 1;
                if paren_depth == 0 {
                  break;
                }
                cur_line.push(Grapheme::from(')'));
              }

              _ if gr.is_lf() => lines.push(Line(std::mem::take(&mut cur_line))),
              _ => cur_line.push(gr.clone()),
            });

            lines.push(Line(cur_line));
            let sub_positions = Self::enumerate_graphemes(&lines).into_iter();
            // offset past "$(" - 2 chars from the '$' position
            let sub_offset = map_pos(self, pos.col_add(2), offset);

            // now we recurse.
            self.find_history_expansions(changes, sub_positions, history, sub_offset);
          } else if symbol == '`' {
            // also command substitution.
            match_loop!(positions.next() => (_,gr) => gr.as_char(), {
              Some('\\') => {
                if let Some((_,gr2)) = positions.next() {
                  cur_line.push(gr2.clone());
                }
              }
              Some('`') => break,

              _ if gr.is_lf() => lines.push(Line(std::mem::take(&mut cur_line))),
              _ => cur_line.push(gr.clone()),
            });

            lines.push(Line(cur_line));
            let sub_positions = Self::enumerate_graphemes(&lines).into_iter();
            // offset past "`" - 1 char from the backtick position
            let sub_offset = map_pos(self, pos.col_add(1), offset);

            // now we recurse.
            self.find_history_expansions(changes, sub_positions, history, sub_offset);
          } else {
            positions.next();
            continue;
          };
        }
        '\\' | '$' => {
          positions.next();
        }
        '\'' => qt_state.toggle_single(),
        '"' => qt_state.toggle_double(),
        '!' if !qt_state.in_single() => {
          let start = pos;
          let Some((pos2, gr2)) = positions.next() else {
            continue;
          };
          let Some(ch) = gr2.as_char() else {
            continue;
          };
          match ch {
            '!' => {
              if let Some(prev) = history.last() {
                let raw = prev.command();
                let start = map_pos(self, start, offset);
                changes.push(((start, start.col_add(1)), raw.to_string()));
              }
            }
            '$' => {
              if let Some(prev) = history.last() {
                let raw = prev.command();
                let start = map_pos(self, start, offset);
                if let Some(last_word) = raw.split_whitespace().last() {
                  changes.push(((start, start.col_add(1)), last_word.to_string()));
                }
              }
            }
            ch if !ch.is_whitespace() => {
              if ch == '"' && qt_state.in_double() {
                qt_state.toggle_double();
                continue;
              }
              let mut end = pos2;
              let cur_row = end.row;
              while let Some((pos3, gr3)) = positions.next() {
                if pos3.row > cur_row {
                  break;
                }; // break on linefeed
                let Some(ch) = gr3.as_char() else { break }; // break on non-ascii
                if ch.is_whitespace() {
                  break; // break on whitespace
                } else if ch == '"' && qt_state.in_double() {
                  qt_state.toggle_double();
                  break;
                };
                end = pos3;
              }
              let pos2 = map_pos(self, pos2, offset);
              let start = map_pos(self, start, offset);
              let end = map_pos(self, end, offset);

              let span = self.yank_span((pos2, end), true);
              let token = join_lines(&span);
              let cmd = history.resolve_hist_token(&token).unwrap_or(token);

              changes.push(((start, end), cmd));
            }
            _ => {}
          }
        }
        _ => {}
      }
    }

    !changes.is_empty()
  }

  pub fn attempt_history_expansion(&mut self, history: &History) -> bool {
    let mut changes: Vec<((Pos, Pos), String)> = vec![];
    let positions = self.grapheme_positions().into_iter();
    if !self.find_history_expansions(&mut changes, positions, history, Pos::MIN) {
      return false;
    };

    for (range, change) in changes.into_iter().rev() {
      let old_len = self.count_graphemes();
      self.replace_range(range, &change);
      let new_len = self.count_graphemes();
      let delta = new_len as isize - old_len as isize;
      let (nr, nc) = self.offset_col_wrapping(self.row(), delta);
      self.cursor.pos.set(nr, nc);
    }

    true
  }

  pub fn cursor_in_leading_ws(&self) -> bool {
    let line = self.line(self.row());
    let col = self.col();
    line
      .0
      .get(..col)
      .is_none_or(|grs| grs.iter().all(|g| g.is_ws()))
  }

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

  pub fn take_buf(&mut self) -> String {
    let result = self.joined();
    self.lines = vec![Line::default()];
    self.cursor.pos = Pos { row: 0, col: 0 };
    result
  }

  pub fn mark_insert_mode_start_pos(&mut self) {
    self.insert_mode_start_pos = Some(self.cursor.pos);
  }

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
        SelectMode::Block(_pos) => unimplemented!(),
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

/// Given two things that implement Ord, make sure that the left is less than the right
pub fn ordered<T: Ord>(start: T, end: T) -> (T, T) {
  if start > end {
    (end, start)
  } else {
    (start, end)
  }
}
