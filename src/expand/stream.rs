use bstr::ByteSlice;
#[cfg(test)]
use smallvec::SmallVec;

use crate::{state::vars::VarStr, util::strops::QuoteState};

/// A stream of bytes and markers.
///
/// Literal bytes live in one contiguous buffer; markers are held in a sparse
/// parallel buffer as `(offset, marker)` pairs, where `offset` is the number of bytes
/// that precede the marker. Offsets are non-decreasing (the build path only appends),
/// and markers sharing an offset keep insertion order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SegStream {
  bytes: Vec<u8>,
  marks: Vec<(usize, Marker)>,
}

impl SegStream {
  /// Convert `Marker::ArgSep` and `Marker::NullExpand` back into markers from
  /// their byte-sentinel representation, so a serialized value round-trips.
  pub(crate) fn from_split_value(bytes: &[u8]) -> Self {
    let mut out = SegStream::new();
    let mut rest = bytes;

    // The markers are \xEF\xB7\x96 (ARG_SEP) and \xEF\xB7\x95 (NULL_EXPAND), so
    // \xEF is the only lead byte worth looking at (they both start with it).
    while let Some(ef) = rest.find_byte(0xEF) {
      let marker = (ef + 3 <= rest.len() && rest[ef + 1] == 0xB7)
        .then(|| match rest[ef + 2] {
          0x96 => Some(Marker::ArgSep),
          0x95 => Some(Marker::NullExpand),
          _ => None,
        })
        .flatten();

      if let Some(marker) = marker {
        out.push_bytes(&rest[..ef]);
        out.push_marker(marker);
        rest = &rest[ef + 3..];
      } else {
        // A \xEF that isn't a marker: emit up to and including it, keep scanning.
        out.push_bytes(&rest[..=ef]);
        rest = &rest[ef + 1..];
      }
    }
    out.push_bytes(rest);
    out
  }

  pub(crate) fn as_plain_bytes(&self) -> Option<&[u8]> {
    (self.marks.is_empty() && !self.bytes.is_empty()).then_some(self.bytes.as_slice())
  }

  pub(crate) fn has_meta(&self) -> bool {
    self.bytes.iter().any(|&c| {
      matches!(
        c,
        b'~' | b'\\' | b'(' | b'"' | b'\'' | b'`' | b'<' | b'>' | b'$'
      )
    })
  }

  pub(crate) fn has_sentinel(&self) -> bool {
    self
      .marks
      .iter()
      .any(|(_, m)| matches!(m, Marker::ArgSep | Marker::NullExpand))
  }

  /// Whether any marker in the stream equals `marker`.
  pub(crate) fn contains_marker(&self, marker: Marker) -> bool {
    self.marks.iter().any(|(_, m)| *m == marker)
  }

  /// If the stream is exactly one marker and no bytes, return it.
  pub(crate) fn sole_marker(&self) -> Option<Marker> {
    (self.bytes.is_empty() && self.marks.len() == 1).then(|| self.marks[0].1)
  }

  /// Convert any `ARG_SEP`/`NULL_EXPAND` byte sentinels embedded in the byte
  /// runs into `Marker::ArgSep`/`NullExpand`, preserving existing markers.
  pub(crate) fn reinterpret_sentinels(self) -> Self {
    if self.has_sentinel() {
      return self;
    }

    if self.bytes.find_byte(0xEF).is_none() {
      return self;
    }
    let mut out = SegStream::new();
    let mut run: Vec<u8> = Vec::new();
    let mut cursor = self.cursor();
    while let Some(unit) = cursor.next() {
      match unit {
        Unit::Byte(b) => run.push(b),
        Unit::Mark(m) => {
          if !run.is_empty() {
            out.append(Self::from_split_value(&run));
            run.clear();
          }
          out.push_marker(m);
        }
      }
    }
    if !run.is_empty() {
      out.append(Self::from_split_value(&run));
    }
    out
  }
}

impl PartialEq<str> for SegStream {
  fn eq(&self, other: &str) -> bool {
    self.bytes == other.as_bytes()
  }
}
impl PartialEq<&str> for SegStream {
  fn eq(&self, other: &&str) -> bool {
    self.bytes == other.as_bytes()
  }
}
impl PartialEq<String> for SegStream {
  fn eq(&self, other: &String) -> bool {
    self.bytes == other.as_bytes()
  }
}

impl From<String> for SegStream {
  fn from(s: String) -> Self {
    Self::from_bytes(s.as_bytes())
  }
}
impl From<&str> for SegStream {
  fn from(s: &str) -> Self {
    Self::from_bytes(s.as_bytes())
  }
}
impl From<std::borrow::Cow<'_, str>> for SegStream {
  fn from(s: std::borrow::Cow<'_, str>) -> Self {
    Self::from_bytes(s.as_bytes())
  }
}
impl From<VarStr> for SegStream {
  fn from(v: VarStr) -> Self {
    Self::from_bytes(v.as_bytes())
  }
}
impl SegStream {
  pub(crate) fn new() -> Self {
    Self {
      bytes: Vec::new(),
      marks: Vec::new(),
    }
  }
  pub(crate) fn from_bytes(b: &[u8]) -> Self {
    Self {
      bytes: b.to_vec(),
      marks: Vec::new(),
    }
  }
  /// Reconstruct the interleaved byte/marker segments. Debug/test only.
  #[cfg(test)]
  pub(crate) fn stream(&self) -> Vec<StreamSeg> {
    let mut out: Vec<StreamSeg> = vec![];
    let mut cur = self.cursor();
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
    self.bytes.push(b);
  }
  pub(crate) fn push_bytes(&mut self, bytes: &[u8]) {
    self.bytes.extend_from_slice(bytes);
  }
  pub(crate) fn push_marker(&mut self, marker: Marker) {
    self.marks.push((self.bytes.len(), marker));
  }
  /// Append another stream onto this one, preserving markers and coalescing
  /// byte runs across the seam.
  pub(crate) fn append(&mut self, other: SegStream) {
    let base = self.bytes.len();
    self.bytes.extend_from_slice(&other.bytes);
    self
      .marks
      .extend(other.marks.into_iter().map(|(o, m)| (o + base, m)));
  }
  pub(crate) fn is_empty(&self) -> bool {
    self.bytes.is_empty() && self.marks.is_empty()
  }

  /// Check if this word has glob characters
  pub(crate) fn has_glob_meta(&self) -> bool {
    self.bytes.iter().any(|c| matches!(c, b'*' | b'?' | b'['))
  }
  /// The leading run of literal bytes, up to the first marker
  pub(crate) fn leading_bytes(&self) -> &[u8] {
    let end = self.marks.first().map_or(self.bytes.len(), |(o, _)| *o);
    &self.bytes[..end]
  }
  /// A copy of this stream with every occurrence of `marker` removed.
  pub(crate) fn without_marker(&self, marker: Marker) -> SegStream {
    SegStream {
      bytes: self.bytes.clone(),
      marks: self
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
  /// Peel `n` leading bytes off the front (markers within that span move to the
  /// remainder), returning `(front_bytes, remainder)`. Used to strip an ASCII
  /// operator prefix off a parameter-expansion operand.
  pub(crate) fn split_off_front(&self, n: usize) -> (Vec<u8>, SegStream) {
    let cut = n.min(self.bytes.len());
    let front = self.bytes[..cut].to_vec();
    let rest = SegStream {
      bytes: self.bytes[cut..].to_vec(),
      marks: self
        .marks
        .iter()
        .map(|(o, m)| (o.saturating_sub(cut), *m))
        .collect(),
    };
    (front, rest)
  }
  /// Collect the byte content (dropping markers) without consuming the stream.
  pub(crate) fn to_bytes(&self) -> Vec<u8> {
    self.bytes.clone()
  }
  pub(crate) fn into_bytes(self) -> Vec<u8> {
    self.bytes
  }
  pub(crate) fn cursor(&self) -> SegCursor<'_> {
    SegCursor::new(&self.bytes, &self.marks)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SegCursor<'a> {
  bytes: &'a [u8],
  marks: &'a [(usize, Marker)],
  bpos: usize, // next byte index
  mpos: usize, // next marker index
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
