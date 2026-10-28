use std::borrow::Cow;

#[cfg(test)]
use smallvec::SmallVec;

use crate::{state::vars::VarStr, util::strops::QuoteState};

/// A stream of bytes and markers.
///
/// Literal bytes live in one contiguous buffer; markers are held in a sparse
/// parallel buffer as `(offset, marker)` pairs, where `offset` is the number of bytes
/// that precede the marker. Offsets are non-decreasing (the build path only appends),
/// and markers sharing an offset keep insertion order.
///
/// Markers are not added on creation or by `from_bytes`, so a stream created from a string is just a literal byte run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SegStream {
  content: VarStr,
  marks  : Vec<(usize, Marker)>,
}

impl SegStream {
  /// Convert `Marker::ArgSep` and `Marker::NullExpand` back into markers from
  /// their byte-sentinel representation, so a serialized value round-trips.
  pub(crate) fn as_plain_bytes(&self) -> Option<&[u8]> {
    (self.marks.is_empty() && !self.content.is_empty()).then_some(self.content.as_bytes())
  }

  pub(crate) fn has_meta(&self) -> bool {
    self.content.iter().any(|&c| {
      matches!(
        c,
        b'~' | b'\\' | b'(' | b'"' | b'\'' | b'`' | b'<' | b'>' | b'$'
      )
    })
  }

  /// Whether any marker in the stream equals `marker`.
  pub(crate) fn contains_marker(&self, marker: Marker) -> bool {
    self.marks.iter().any(|(_, m)| *m == marker)
  }

  /// If the stream is exactly one marker and no bytes, return it.
  pub(crate) fn sole_marker(&self) -> Option<Marker> {
    (self.content.is_empty() && self.marks.len() == 1).then(|| self.marks[0].1)
  }
}

impl PartialEq<str> for SegStream {
  fn eq(&self, other: &str) -> bool {
    self.content.as_bytes() == other.as_bytes()
  }
}
impl PartialEq<&str> for SegStream {
  fn eq(&self, other: &&str) -> bool {
    self.content.as_bytes() == other.as_bytes()
  }
}
impl PartialEq<String> for SegStream {
  fn eq(&self, other: &String) -> bool {
    self.content.as_bytes() == other.as_bytes()
  }
}

impl From<String> for SegStream {
  fn from(s: String) -> Self {
    Self::from_bytes(s.into_bytes())
  }
}
impl From<&str> for SegStream {
  fn from(s: &str) -> Self {
    Self::from_slice(s.as_bytes())
  }
}
impl From<Cow<'_, str>> for SegStream {
  fn from(s: Cow<'_, str>) -> Self {
    Self::from_slice(s.as_bytes())
  }
}
impl From<VarStr> for SegStream {
  fn from(v: VarStr) -> Self {
    Self::new(v)
  }
}
impl SegStream {
  pub(crate) fn new(content: VarStr) -> Self {
    Self {
      content,
      marks  : Vec::new(),
    }
  }
  pub(crate) fn from_bytes(b: Vec<u8>) -> Self {
    Self {
      content: VarStr::from(b),
      marks  : Vec::new(),
    }
  }
  pub(crate) fn from_slice(b: &[u8]) -> Self {
    Self {
      content: VarStr::from(b),
      marks  : Vec::new(),
    }
  }
  /// Reconstruct the interleaved byte/marker segments. Debug/test only.
  #[cfg(test)]
  pub(crate) fn stream(&self) -> Vec<StreamSeg> {
    let mut out: Vec<StreamSeg> = vec![];
    let mut cur                 = self.cursor();
    while let Some(unit) = cur.next() {
      match unit {
        Unit::Byte(b) => match out.last_mut() {
          Some(StreamSeg::Bytes(last)) => last.push(b),
          _ => out.push(StreamSeg::Bytes(SmallVec::from_slice(&[b]))),
        },
        Unit::Mark(m) => out.push(StreamSeg::Mark(m)),
      }
    }
    out
  }
  pub(crate) fn push(&mut self, unit: Unit) {
    match unit {
      Unit::Byte(b) => self.push_byte(b),
      Unit::Mark(m) => self.push_marker(m),
    }
  }
  pub(crate) fn push_byte(&mut self, b: u8) {
    self.content.push(b);
  }
  pub(crate) fn push_bytes(&mut self, bytes: &[u8]) {
    self.content.push_slice(bytes);
  }
  pub(crate) fn push_marker(&mut self, marker: Marker) {
    self.marks.push((self.content.len(), marker));
  }
  /// Append another stream onto this one, preserving markers and coalescing
  /// byte runs across the seam.
  pub(crate) fn append(&mut self, other: SegStream) {
    let base = self.content.len();
    self.content.push_slice(&other.content);
    self
      .marks
      .extend(other.marks.into_iter().map(|(o, m)| (o + base, m)));
  }
  pub(crate) fn is_empty(&self) -> bool {
    self.content.is_empty() && self.marks.is_empty()
  }

  /// Check if this word has glob characters
  pub(crate) fn has_glob_meta(&self) -> bool {
    self.content.iter().any(|c| matches!(c, b'*' | b'?' | b'['))
  }
  /// The leading run of literal bytes, up to the first marker
  pub(crate) fn leading_bytes(&self) -> &[u8] {
    let end = self.marks.first().map_or(self.content.len(), |(o, _)| *o);
    &self.content[..end]
  }
  /// A copy of this stream with every occurrence of `marker` removed.
  pub(crate) fn without_marker(&self, marker: Marker) -> SegStream {
    SegStream {
      content: self.content.clone(),
      marks  : self
        .marks
        .iter()
        .filter(|(_, m)| *m != marker)
        .copied()
        .collect(),
    }
  }
  /// Split at the first `sep` byte that is not escaped (preceded by an `Escape`
  /// marker) and not inside a quote region, returning `(before, after)` with
  /// the separator consumed. `None` if no such separator exists.
  pub(crate) fn split_once_unescaped(&self, sep: u8) -> Option<(SegStream, SegStream)> {
    let mut before = SegStream::default();
    let mut cursor = self.cursor();
    let mut qt     = QuoteState::default();
    while let Some(unit) = cursor.next() {
      match unit {
        Unit::Mark(Marker::Escape) => {
          before.push_marker(Marker::Escape);
          if let Some(next) = cursor.next() {
            before.push(next);
          }
        }
        Unit::Mark(Marker::Quote(Quote::Single)) if !qt.in_double() => {
          qt.toggle_single();
          before.push(unit);
        }
        Unit::Mark(Marker::Quote(Quote::Double)) if !qt.in_single() => {
          qt.toggle_double();
          before.push(unit);
        }
        Unit::Byte(b) if b == sep && qt.outside() => {
          let mut after = SegStream::default();
          while let Some(u) = cursor.next() {
            after.push(u);
          }
          return Some((before, after));
        }
        _ => before.push(unit),
      }
    }
    None
  }
  /// Peel `n` leading bytes off the front (markers within that span move to the
  /// remainder), returning `(front_bytes, remainder)`. Used to strip an ASCII
  /// operator prefix off a parameter-expansion operand.
  pub(crate) fn split_off_front(&self, n: usize) -> (VarStr, SegStream) {
    let cut   = n.min(self.content.len());
    let front = self.content[..cut].into();
    let rest = SegStream {
      content: self.content[cut..].into(),
      marks  : self
        .marks
        .iter()
        .map(|(o, m)| (o.saturating_sub(cut), *m))
        .collect(),
    };
    (front, rest)
  }
  /// Collect the byte content (dropping markers) without consuming the stream.
  pub(crate) fn as_bytes(&self) -> &[u8] {
    &self.content
  }
  pub(crate) fn content(&self) -> VarStr {
    self.content.clone()
  }
  pub(crate) fn cursor(&self) -> SegCursor<'_> {
    SegCursor::new(&self.content, &self.marks)
  }
}

impl From<Vec<u8>> for SegStream {
  fn from(v: Vec<u8>) -> Self {
    Self::from_bytes(v)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegCursor<'a> {
  bytes: &'a [u8],
  marks: &'a [(usize, Marker)],
  bpos : usize, // next byte index
  mpos : usize, // next marker index
}

impl<'a> SegCursor<'a> {
  pub(crate) fn new(bytes: &'a [u8], marks: &'a [(usize, Marker)]) -> Self {
    Self {
      bytes,
      marks,
      bpos: 0,
      mpos: 0,
    }
  }
  pub(crate) fn bpos(&self) -> usize {
    self.bpos
  }
  /// A marker is pending if the next unconsumed marker sits at the current byte
  /// position; those are emitted before the byte at that position.
  fn pending_mark(&self) -> Option<Marker> {
    match self.marks.get(self.mpos) {
      Some((off, m)) if *off == self.bpos => Some(*m),
      _ => None,
    }
  }
  pub(crate) fn peek(&self) -> Option<Unit> {
    if let Some(m) = self.pending_mark() {
      return Some(Unit::Mark(m));
    }
    self.bytes.get(self.bpos).map(|b| Unit::Byte(*b))
  }
  pub(crate) fn peek_nth(&self, n: usize) -> Option<u8> {
    let mut cursor = *self;
    for _ in 0..n {
      cursor.next()?;
    }
    cursor.peek_byte()
  }
  pub(crate) fn peek_byte(&self) -> Option<u8> {
    match self.peek()? {
      Unit::Byte(b) => Some(b),
      Unit::Mark(_) => None,
    }
  }
  /// Consume and return the next unit's byte, but only if it is a byte.
  /// A marker (or end of stream) leaves the cursor untouched and returns None.
  pub(crate) fn next_byte(&mut self) -> Option<u8> {
    match self.peek()? {
      Unit::Byte(b) => {
        self.next();
        Some(b)
      }
      Unit::Mark(_) => None,
    }
  }
  pub(crate) fn next(&mut self) -> Option<Unit> {
    if let Some(m) = self.pending_mark() {
      self.mpos += 1;
      return Some(Unit::Mark(m));
    }
    let b = *self.bytes.get(self.bpos)?;
    self.bpos += 1;
    Some(Unit::Byte(b))
  }
  pub(crate) fn eat(&mut self, u: Unit) -> bool {
    let is_match = self.peek() == Some(u);
    if is_match {
      self.next();
    }
    is_match
  }
  pub(crate) fn eat_byte(&mut self, b: u8) -> bool {
    self.eat(Unit::Byte(b))
  }
  pub(crate) fn bump(&mut self) {
    self.next();
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Quote {
  Single,
  Double,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ProcSubKind {
  In,
  Out,
}

impl From<bool> for ProcSubKind {
  /// Convert a boolean to a `ProcSubKind`. `true` maps to `In`, and `false` maps to `Out`.
  fn from(value: bool) -> Self {
    if value { Self::In } else { Self::Out }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Unit {
  Byte(u8),
  Mark(Marker),
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamSeg {
  Bytes(SmallVec<[u8; 32]>),
  Mark(Marker),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Marker {
  Subshell,
  VarSub,
  Escape,
  TildeSub,
  Quote(Quote),
  ProcSub(ProcSubKind),
  NullExpand,
  ArgSep,
  ExpandStart,
  ExpandEnd,
}
