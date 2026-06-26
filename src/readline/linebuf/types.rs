use std::{
  fmt::{self, Display},
  ops::{Deref, DerefMut, Index, IndexMut},
  slice::SliceIndex,
};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use super::{CharClass, Pos};
/// A single grapheme. Graphemes can be composed of multiple chars, but are always treated as a single unit for display and editing purposes.
/// The common single-codepoint case stays inline in `Single`; only true multi-codepoint clusters take a heap allocation via `Cluster`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Grapheme {
  Single(char),
  Cluster(Box<str>),
}

impl Grapheme {
  /// Returns the display width of the Grapheme. ASCII control bytes (other
  /// than `\n` and `\t`) and DEL render as 2-column caret notation (`^[`,
  /// `^M`, `^?`, etc.) in the highlighter, so their width must match.
  /// Other unprintable codepoints fall back to 0.
  pub fn width(&self) -> usize {
    match self {
      Grapheme::Single(ch) => {
        if Self::is_visualized_control(*ch) {
          2
        } else {
          ch.width().unwrap_or(0)
        }
      }
      Grapheme::Cluster(cluster) => cluster
        .chars()
        .map(|c| {
          if Self::is_visualized_control(c) {
            2
          } else {
            c.width().unwrap_or(0)
          }
        })
        .sum(),
    }
  }

  fn is_visualized_control(c: char) -> bool {
    matches!(c, '\x00'..='\x08' | '\x0b'..='\x1f' | '\x7f')
  }
  pub fn len_utf8(&self) -> usize {
    match self {
      Grapheme::Single(ch) => ch.len_utf8(),
      Grapheme::Cluster(cluster) => cluster.chars().map(|c| c.len_utf8()).sum(),
    }
  }
  /// Returns true if the Grapheme is wrapping a linefeed ('\n')
  pub fn is_lf(&self) -> bool {
    self.is_char('\n')
  }
  /// Returns true if the Grapheme consists of exactly one char and that char is equal to `c`
  pub fn is_char(&self, c: char) -> bool {
    self.as_char().is_some_and(|ch| ch == c)
  }
  /// Returns the `CharClass` of the Grapheme, which is determined by the properties of its chars
  /// Used for things like word motions
  pub fn class(&self) -> CharClass {
    CharClass::from(self)
  }

  /// If the Grapheme consists of exactly one char, returns that char. Otherwise, returns None.
  /// All callsites that use this method operate on ascii, so never returning anything for multibyte sequences is fine.
  pub fn as_char(&self) -> Option<char> {
    match self {
      Grapheme::Single(ch) => Some(*ch),
      Grapheme::Cluster(_) => None,
    }
  }

  /// Returns true if the Grapheme is classified as whitespace
  pub fn is_ws(&self) -> bool {
    self.class() == CharClass::Whitespace
  }
}

impl From<char> for Grapheme {
  fn from(value: char) -> Self {
    Self::Single(value)
  }
}

impl From<&str> for Grapheme {
  fn from(value: &str) -> Self {
    if value.chars().count() == 1 {
      Self::Single(value.chars().next().unwrap())
    } else {
      Self::Cluster(value.into())
    }
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
    match self {
      Grapheme::Single(ch) => write!(f, "{ch}"),
      Grapheme::Cluster(cluster) => write!(f, "{cluster}"),
    }
  }
}

pub fn to_graphemes(s: &str) -> Vec<Grapheme> {
  let s = s.to_string();
  s.graphemes(true).map(Grapheme::from).collect()
}

#[derive(Default, Debug, Clone, PartialEq, Eq, PartialOrd)]
pub struct Line(pub(super) Vec<Grapheme>);

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
    if other.contains('\n') {
      log::warn!(
        "Inserting string with newlines into a single line. Newlines will be treated as literal characters."
      );
    }
    let start = at.min(self.0.len());
    for (at, g) in (start..).zip(other.graphemes(true)) {
      self.0.insert(at, Grapheme::from(g));
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
  pub fn trim_start(&mut self) -> Line {
    let mut clone = self.clone();
    while clone.0.first().is_some_and(Grapheme::is_ws) {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lines(pub(super) Vec<Line>);
impl Lines {
  pub fn into_vec(self) -> Vec<Line> {
    self.0
  }
  pub fn to_lines(s: &str) -> Lines {
    let s = s.to_string();
    let mut new: Lines = s.split('\n').map(to_graphemes).map(Line::from).collect();
    new.push_empty();
    new
  }

  /// Ensure that the underlying Vec<Line> is not empty
  fn push_empty(&mut self) {
    if self.is_empty() {
      self.push(Line::default());
    }
  }

  pub fn join(&self) -> String {
    self
      .0
      .iter()
      .map(ToString::to_string)
      .collect::<Vec<String>>()
      .join("\n")
  }

  pub fn split_lines_at(&mut self, pos: Pos) -> Lines {
    let tail = self[pos.row].split_off(pos.col);
    let mut rest: Lines = self.drain(pos.row + 1..).collect();
    rest.insert(0, tail);
    self.push_empty();
    rest
  }

  pub fn split_lines(mut self, pos: Pos) -> (Lines, Lines) {
    let tail = self[pos.row].split_off(pos.col);
    let mut rest: Lines = self.drain(pos.row + 1..).collect();
    self.push_empty();
    rest.insert(0, tail);
    (self, rest)
  }

  pub fn attach_lines(&mut self, other: &mut Lines) {
    if other.is_empty() {
      return;
    }
    if self.is_empty() {
      self.append(other);
      self.push_empty();
      return;
    }
    let mut head = other.remove(0);
    let mut tail = self.pop().unwrap();
    tail.append(&mut head);
    self.push(tail);
    self.append(other);
    self.push_empty();
  }

  pub fn is_prefix_lines(&self, other: &Lines) -> bool {
    if self.is_empty() {
      return false;
    }

    let all_but_last = self.0[..self.len().saturating_sub(1)]
      .iter()
      .zip(other.iter())
      .all(|(l, r)| *l == *r);

    if !all_but_last {
      return false;
    }

    let last = self.len().saturating_sub(1);
    let Some(other_line) = other.get(last).map(|l| &l.0) else {
      return false;
    };
    let this_line = &self[last].0;

    this_line.len() <= other_line.len()
      && this_line.iter().zip(other_line.iter()).all(|(l, r)| l == r)
  }

  pub fn strip_prefix_lines(mut self, other: &Lines) -> Option<Lines> {
    if self.is_empty() {
      return None;
    }

    let common_lines = self
      .0
      .iter()
      .zip(other.iter())
      .take_while(|(l, r)| *l == *r)
      .count();

    // drain equal lines
    self.drain(..common_lines);

    if self.is_empty() {
      self.push_empty();
      return None;
    }

    if let Some(other_line) = other.get(common_lines) {
      let common_chars = self[0]
        .0
        .iter()
        .zip(other_line.0.iter())
        .take_while(|(l, r)| l == r)
        .count();

      // drain common characters
      self[0].0.drain(..common_chars);
    } else if common_lines > 0 {
      // every line matched on the hint's prefix, and there is no partial match at the boundary.
      // the remaining content is just complete lines, so we have to add an empty line for those
      // to attach to, otherwise it attaches to our last line.
      self.0.insert(0, Line::default());
    }

    if self.iter().all(Line::is_empty) {
      None
    } else {
      Some(self)
    }
  }

  pub fn byte_len(&self) -> usize {
    self
      .0
      .iter()
      .map(|line| {
        line.0.iter().map(Grapheme::len_utf8).sum::<usize>() + 1 // +1 for '\n'
      })
      .sum()
  }
}

impl Default for Lines {
  fn default() -> Self {
    Self(vec![Line::default()])
  }
}

impl Display for Lines {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let mut iter = self.0.iter();
    if let Some(first) = iter.next() {
      write!(f, "{first}")?;
      for line in iter {
        write!(f, "\n{line}")?;
      }
    }
    Ok(())
  }
}

impl std::iter::FromIterator<Line> for Lines {
  fn from_iter<T: IntoIterator<Item = Line>>(iter: T) -> Self {
    Self(iter.into_iter().collect())
  }
}

impl From<Vec<Line>> for Lines {
  fn from(value: Vec<Line>) -> Self {
    Self(value)
  }
}

impl Deref for Lines {
  type Target = Vec<Line>;
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl DerefMut for Lines {
  fn deref_mut(&mut self) -> &mut Vec<Line> {
    &mut self.0
  }
}

impl Index<usize> for Lines {
  type Output = Line;
  fn index(&self, index: usize) -> &Self::Output {
    let index = index.min(self.0.len().saturating_sub(1));
    &self.0[index]
  }
}

impl IndexMut<usize> for Lines {
  fn index_mut(&mut self, index: usize) -> &mut Line {
    let index = index.min(self.0.len().saturating_sub(1));
    &mut self.0[index]
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn line(s: &str) -> Line {
    let mut l = Line::default();
    l.push_str(s);
    l
  }

  fn lines_from(rows: &[&str]) -> Lines {
    Lines(rows.iter().map(|s| line(s)).collect())
  }

  // ---------------------------------------------------------------- Grapheme

  #[test]
  fn grapheme_width_ascii_printable_is_one() {
    assert_eq!(Grapheme::from('A').width(), 1);
    assert_eq!(Grapheme::from(' ').width(), 1);
    assert_eq!(Grapheme::from('~').width(), 1);
  }

  #[test]
  fn grapheme_width_wide_char_is_two() {
    // CJK ideographs are 2 columns wide in a terminal cell.
    assert_eq!(Grapheme::from('世').width(), 2);
  }

  #[test]
  fn grapheme_width_visualized_control_is_two() {
    // ASCII control bytes render as `^X` caret notation → 2 columns.
    assert_eq!(Grapheme::from('\x00').width(), 2);
    assert_eq!(Grapheme::from('\x01').width(), 2);
    assert_eq!(Grapheme::from('\x08').width(), 2);
    assert_eq!(Grapheme::from('\x0b').width(), 2);
    assert_eq!(Grapheme::from('\x1b').width(), 2); // ESC
    assert_eq!(Grapheme::from('\x1f').width(), 2);
    assert_eq!(Grapheme::from('\x7f').width(), 2); // DEL
  }

  #[test]
  fn grapheme_width_excludes_tab_and_lf_from_visualization() {
    // \t (0x09) and \n (0x0a) are handled specially by the renderer,
    // not visualized as caret notation, so they must NOT report width 2.
    assert_eq!(Grapheme::from('\t').width(), 0);
    assert_eq!(Grapheme::from('\n').width(), 0);
  }

  #[test]
  fn grapheme_width_combining_mark_is_zero() {
    // U+0301 (combining acute) on its own has zero display width.
    assert_eq!(Grapheme::from('\u{0301}').width(), 0);
  }

  #[test]
  fn grapheme_width_composed_grapheme_sums_chars() {
    // "é" = 'e' + U+0301 → width 1 + 0 = 1.
    let g = Grapheme::from("e\u{0301}");
    assert_eq!(g.width(), 1);
  }

  #[test]
  fn grapheme_is_visualized_control_boundaries() {
    // Inclusive range 0x00..=0x08
    assert!(Grapheme::is_visualized_control('\x00'));
    assert!(Grapheme::is_visualized_control('\x08'));
    // Excluded: \t (0x09), \n (0x0a)
    assert!(!Grapheme::is_visualized_control('\t'));
    assert!(!Grapheme::is_visualized_control('\n'));
    // Inclusive range 0x0b..=0x1f
    assert!(Grapheme::is_visualized_control('\x0b'));
    assert!(Grapheme::is_visualized_control('\x1f'));
    // Excluded: printable ASCII
    assert!(!Grapheme::is_visualized_control(' '));
    assert!(!Grapheme::is_visualized_control('A'));
    assert!(!Grapheme::is_visualized_control('~'));
    // DEL
    assert!(Grapheme::is_visualized_control('\x7f'));
    // Above DEL (non-ASCII)
    assert!(!Grapheme::is_visualized_control('世'));
  }

  #[test]
  fn grapheme_as_char_returns_none_for_multi_char_cluster() {
    // Composed grapheme cluster "é" stores two chars internally.
    let g = Grapheme::from("e\u{0301}");
    assert_eq!(g.as_char(), None);
  }

  #[test]
  fn grapheme_as_char_returns_some_for_single_char() {
    assert_eq!(Grapheme::from('A').as_char(), Some('A'));
  }

  // -------------------------------------------------------------------- Line

  #[test]
  fn line_insert_str_at_start() {
    let mut l = line("world");
    l.insert_str(0, "hello ");
    assert_eq!(l.to_string(), "hello world");
  }

  #[test]
  fn line_insert_str_in_middle() {
    let mut l = line("heo");
    l.insert_str(2, "ll");
    assert_eq!(l.to_string(), "hello");
  }

  #[test]
  fn line_insert_str_at_end() {
    let mut l = line("hello");
    let end = l.len();
    l.insert_str(end, " world");
    assert_eq!(l.to_string(), "hello world");
  }

  #[test]
  fn line_insert_str_beyond_end_clamps() {
    let mut l = line("hi");
    l.insert_str(999, "!");
    assert_eq!(l.to_string(), "hi!");
  }

  #[test]
  fn line_insert_str_empty_is_noop() {
    let mut l = line("hello");
    l.insert_str(2, "");
    assert_eq!(l.to_string(), "hello");
  }

  #[test]
  fn line_insert_str_with_newline_inserts_literal() {
    // Inserting a string containing '\n' logs a warning but still inserts
    // the chars as literal graphemes (newlines become literal chars in the
    // line; line-splitting is the caller's responsibility).
    let mut l = line("ab");
    l.insert_str(1, "X\nY");
    assert_eq!(l.len(), 5);
    assert_eq!(l[0].as_char(), Some('a'));
    assert_eq!(l[1].as_char(), Some('X'));
    assert_eq!(l[2].as_char(), Some('\n'));
    assert_eq!(l[3].as_char(), Some('Y'));
    assert_eq!(l[4].as_char(), Some('b'));
  }

  #[test]
  fn line_insert_str_multibyte_grapheme() {
    // Composed graphemes are inserted as single units.
    let mut l = line("ab");
    l.insert_str(1, "e\u{0301}");
    assert_eq!(l.len(), 3); // a, é, b
    assert_eq!(l[0].as_char(), Some('a'));
    assert_eq!(l[1].as_char(), None); // é is multi-char
    assert_eq!(l[2].as_char(), Some('b'));
  }

  // ------------------------------------------------------------------ Lines

  #[test]
  fn attach_lines_into_empty_self_takes_other_contents() {
    // Truly empty Lines (vec![]), not the Default which has one empty line.
    let mut sink = Lines(vec![]);
    let mut other = Lines::to_lines("hello\nworld");
    let other_len_before = other.len();
    sink.attach_lines(&mut other);

    // push_empty only fires if self is still empty after append; here other
    // had content, so it's moved into self verbatim with no trailing line.
    assert_eq!(sink.join(), "hello\nworld");
    // `other` should have been drained by Vec::append.
    assert!(other.is_empty());
    assert!(other_len_before > 0); // sanity
  }

  #[test]
  fn attach_lines_early_returns_when_other_is_empty() {
    let mut sink = lines_from(&["foo"]);
    let mut other = Lines(vec![]);
    sink.attach_lines(&mut other);
    assert_eq!(sink.join(), "foo");
  }

  #[test]
  fn is_prefix_lines_false_when_self_is_empty() {
    let empty = Lines(vec![]);
    let other = Lines::to_lines("anything");
    assert!(!empty.is_prefix_lines(&other));
  }

  #[test]
  fn is_prefix_lines_true_when_self_equals_other() {
    let a = lines_from(&["hello", "world"]);
    let b = lines_from(&["hello", "world"]);
    assert!(a.is_prefix_lines(&b));
  }

  #[test]
  fn is_prefix_lines_true_for_partial_last_line() {
    // self last line is a string prefix of other's last line.
    let a = lines_from(&["hello", "wo"]);
    let b = lines_from(&["hello", "world"]);
    assert!(a.is_prefix_lines(&b));
  }

  #[test]
  fn is_prefix_lines_true_when_self_has_fewer_lines() {
    let a = lines_from(&["hello"]);
    let b = lines_from(&["hello", "world"]);
    assert!(a.is_prefix_lines(&b));
  }

  #[test]
  fn is_prefix_lines_false_when_earlier_line_differs() {
    let a = lines_from(&["heXlo", "world"]);
    let b = lines_from(&["hello", "world"]);
    assert!(!a.is_prefix_lines(&b));
  }

  #[test]
  fn is_prefix_lines_false_when_last_line_longer_than_other() {
    // self last line is longer than the corresponding line in other.
    let a = lines_from(&["hello", "worldly"]);
    let b = lines_from(&["hello", "world"]);
    assert!(!a.is_prefix_lines(&b));
  }

  #[test]
  fn is_prefix_lines_false_when_self_has_more_lines_than_other() {
    let a = lines_from(&["hello", "world", "extra"]);
    let b = lines_from(&["hello", "world"]);
    assert!(!a.is_prefix_lines(&b));
  }

  #[test]
  fn is_prefix_lines_false_when_last_line_diverges() {
    let a = lines_from(&["hello", "woXld"]);
    let b = lines_from(&["hello", "world"]);
    assert!(!a.is_prefix_lines(&b));
  }

  #[test]
  fn strip_prefix_lines_returns_none_when_self_is_empty() {
    let empty = Lines(vec![]);
    let other = Lines::to_lines("hello");
    assert!(empty.strip_prefix_lines(&other).is_none());
  }

  // ------------------------------------------------------------- From impls

  #[test]
  fn from_impls_produce_equivalent_results() {
    // All four Grapheme From impls should produce the same single-char Grapheme.
    let g_char = Grapheme::from('A');
    let g_str = Grapheme::from("A");
    let g_string = Grapheme::from(String::from("A"));
    let g_string_ref = Grapheme::from(&String::from("A"));
    assert_eq!(g_char, g_str);
    assert_eq!(g_str, g_string);
    assert_eq!(g_string, g_string_ref);
    assert_eq!(g_char.as_char(), Some('A'));

    // Line::from(Vec<Grapheme>) wraps the vec directly.
    let gs = vec![
      Grapheme::from('a'),
      Grapheme::from('b'),
      Grapheme::from('c'),
    ];
    let l = Line::from(gs);
    assert_eq!(l.len(), 3);
    assert_eq!(l.to_string(), "abc");

    // Lines::from(Vec<Line>) wraps the vec directly.
    let ls = Lines::from(vec![line("foo"), line("bar")]);
    assert_eq!(ls.len(), 2);
    assert_eq!(ls.join(), "foo\nbar");

    // FromIterator<Line> for Lines
    let collected: Lines = vec![line("x"), line("y")].into_iter().collect();
    assert_eq!(collected.join(), "x\ny");
  }
}
