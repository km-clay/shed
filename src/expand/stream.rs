use smallvec::SmallVec;

use crate::{state::vars::VarStr, util::QuoteState};

/// A stream of bytes and markers.
///
/// The markers represent various contextual metadata, like the start of a variable substitution, or a command sub, etc.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SegStream(Vec<StreamSeg>);

impl SegStream {
  /// Build a stream from a resolved *value*'s bytes, translating the value-layer
  /// separators (`ARG_SEP`/`NULL_EXPAND`, byte-encoded inside a `VarStr`) into
  /// real `Marker`s so word splitting sees element boundaries. Ordinary source
  /// bytes never contain these sequences, so this is a no-op for them.
  fn from_value_bytes(bytes: &[u8]) -> Self {
    let mut out = SegStream::new();
    let mut i = 0;
    let mut start = 0;

    while i < bytes.len() {
      // the markers are \xEF\xB7\x96 (ARG_SEP) and \xEF\xB7\x95 (NULL_EXPAND)
      if i + 3 <= bytes.len() && bytes[i] == 0xEF && bytes[i + 1] == 0xB7 {
        let marker = match bytes[i + 2] {
          0x96 => Some(Marker::ArgSep),
          0x95 => Some(Marker::NullExpand),
          _ => None,
        };
        if let Some(marker) = marker {
          out.push_bytes(&bytes[start..i]);
          out.push_marker(marker);
          i += 3;
          start = i;
          continue;
        }
      }
      i += 1;
    }
    out.push_bytes(&bytes[start..]);
    out
  }
}

impl PartialEq<str> for SegStream {
  fn eq(&self, other: &str) -> bool {
    self.to_bytes() == other.as_bytes()
  }
}
impl PartialEq<&str> for SegStream {
  fn eq(&self, other: &&str) -> bool {
    self.to_bytes() == other.as_bytes()
  }
}
impl PartialEq<String> for SegStream {
  fn eq(&self, other: &String) -> bool {
    self.to_bytes() == other.as_bytes()
  }
}

impl From<String> for SegStream {
  fn from(s: String) -> Self {
    Self::from_value_bytes(s.as_bytes())
  }
}
impl From<&str> for SegStream {
  fn from(s: &str) -> Self {
    Self::from_value_bytes(s.as_bytes())
  }
}
impl From<std::borrow::Cow<'_, str>> for SegStream {
  fn from(s: std::borrow::Cow<'_, str>) -> Self {
    Self::from_value_bytes(s.as_bytes())
  }
}
impl From<VarStr> for SegStream {
  fn from(v: VarStr) -> Self {
    Self::from_value_bytes(v.as_bytes())
  }
}
impl SegStream {
  pub fn new() -> Self {
    Self(vec![])
  }
  pub fn with_capacity(cap: usize) -> Self {
    Self(Vec::with_capacity(cap))
  }
  pub fn from_bytes(b: &[u8]) -> Self {
    let mut s = Self::with_capacity(b.len());
    s.push_bytes(b);
    s
  }
  pub fn stream(&self) -> &[StreamSeg] {
    &self.0
  }
  pub fn push(&mut self, unit: Unit) {
    match unit {
      Unit::Byte(b) => self.push_byte(b),
      Unit::Mark(m) => self.push_marker(m),
    }
  }
  pub fn push_byte(&mut self, b: u8) {
    self.push_bytes(&[b]);
  }
  pub fn push_bytes(&mut self, bytes: &[u8]) {
    if bytes.is_empty() {
      return;
    }
    match self.0.last_mut() {
      Some(StreamSeg::Bytes(last)) => last.extend_from_slice(bytes),
      _ => self.0.push(StreamSeg::Bytes(SmallVec::from_slice(bytes))),
    }
  }
  pub fn push_marker(&mut self, marker: Marker) {
    self.0.push(StreamSeg::Mark(marker));
  }
  /// Append another stream onto this one, preserving markers and coalescing
  /// byte runs across the seam.
  pub fn append(&mut self, other: SegStream) {
    for seg in other.0 {
      match seg {
        StreamSeg::Bytes(b) => self.push_bytes(&b),
        StreamSeg::Mark(m) => self.push_marker(m),
      }
    }
  }
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
  /// The leading run of literal bytes, up to the first marker (empty if the
  /// stream starts with a marker). Used to match a parameter-expansion operator
  /// prefix without an escaped/quoted char (which introduces a marker) being
  /// mistaken for a doubled operator like `//` or `##`.
  pub fn leading_bytes(&self) -> &[u8] {
    match self.0.first() {
      Some(StreamSeg::Bytes(b)) => b,
      _ => &[],
    }
  }
  /// A copy of this stream with every occurrence of `marker` removed.
  pub fn without_marker(&self, marker: Marker) -> SegStream {
    let mut out = SegStream::new();
    for seg in &self.0 {
      match seg {
        StreamSeg::Bytes(b) => out.push_bytes(b),
        StreamSeg::Mark(m) if *m == marker => {}
        StreamSeg::Mark(m) => out.push_marker(*m),
      }
    }
    out
  }
  /// Split at the first `sep` byte that is not escaped (preceded by an `Escape`
  /// marker) and not inside a quote region, returning `(before, after)` with
  /// the separator consumed. `None` if no such separator exists.
  pub fn split_once_unescaped(&self, sep: u8) -> Option<(SegStream, SegStream)> {
    let mut before = SegStream::new();
    let mut cursor = self.cursor();
    let mut qt = QuoteState::default();
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
          let mut after = SegStream::new();
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
  /// Peel `n` leading bytes off the front (markers before that point stay with
  /// the left half), returning `(front_bytes, remainder)`. Used to strip an
  /// ASCII operator prefix off a parameter-expansion operand.
  pub fn split_off_front(&self, n: usize) -> (Vec<u8>, SegStream) {
    let mut front = Vec::new();
    let mut rest = SegStream::new();
    let mut cursor = self.cursor();
    while let Some(unit) = cursor.next() {
      match unit {
        Unit::Byte(b) if front.len() < n => front.push(b),
        _ => rest.push(unit),
      }
    }
    (front, rest)
  }
  /// Collect the byte content (dropping markers) without consuming the stream.
  pub fn to_bytes(&self) -> Vec<u8> {
    let mut out = vec![];
    for seg in &self.0 {
      if let StreamSeg::Bytes(bytes) = seg {
        out.extend_from_slice(bytes);
      }
    }
    out
  }
  pub fn into_bytes(self) -> Vec<u8> {
    let mut out = vec![];

    for seg in self.0 {
      if let StreamSeg::Bytes(bytes) = seg {
        out.extend_from_slice(&bytes);
      }
    }

    out
  }
  pub fn cursor(&self) -> SegCursor<'_> {
    SegCursor::new(self.stream())
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegCursor<'a> {
  stream: &'a [StreamSeg],
  pos: usize, // position in stream
  off: usize, // position in current byte run
}

impl<'a> SegCursor<'a> {
  pub fn new(stream: &'a [StreamSeg]) -> Self {
    Self {
      stream,
      pos: 0,
      off: 0,
    }
  }
  pub fn peek(&self) -> Option<Unit> {
    match self.stream.get(self.pos)? {
      StreamSeg::Bytes(b) => Some(Unit::Byte(*b.get(self.off)?)),
      StreamSeg::Mark(m) => Some(Unit::Mark(*m)),
    }
  }
  pub fn peek_byte(&self) -> Option<u8> {
    match self.peek()? {
      Unit::Byte(b) => Some(b),
      Unit::Mark(_) => None,
    }
  }
  /// Consume and return the next unit's byte, but only if it is a byte.
  /// A marker (or end of stream) leaves the cursor untouched and returns None.
  pub fn next_byte(&mut self) -> Option<u8> {
    match self.peek()? {
      Unit::Byte(b) => {
        self.next();
        Some(b)
      }
      Unit::Mark(_) => None,
    }
  }
  pub fn next(&mut self) -> Option<Unit> {
    let u = self.peek()?;

    match self.stream.get(self.pos) {
      Some(StreamSeg::Bytes(b)) if self.off + 1 < b.len() => self.off += 1,
      _ => {
        self.pos += 1;
        self.off = 0;
      }
    }

    Some(u)
  }
  pub fn eat(&mut self, u: Unit) -> bool {
    let is_match = self.peek() == Some(u);
    if is_match {
      self.next();
    }
    is_match
  }
  pub fn eat_byte(&mut self, b: u8) -> bool {
    self.eat(Unit::Byte(b))
  }
  pub fn bump(&mut self) {
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
