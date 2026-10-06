//! Escape/quote-aware splitting and byte-cursor delimiter scanning.

use bstr::ByteSlice;

use crate::{
  eval::lex::{Span, Tk},
  match_loop,
};

use super::QuoteState;

/// POSIX field splitting. IFS-whitespace runs collapse into a single
/// delimiter and are stripped from both ends; non-whitespace IFS characters
/// are hard delimiters that can yield empty fields. When `max` is set,
/// splitting stops after `max - 1` fields and the untouched remainder (with
/// trailing IFS-whitespace trimmed) becomes the final field, mirroring
/// `read var1 var2 ...` where the last variable absorbs the rest of the line.
pub(crate) fn ifs_split(input: &[u8], ifs: &[u8], max: Option<usize>) -> Vec<Vec<u8>> {
  let is_ws = |b: u8| b.is_ascii_whitespace() && ifs.contains(&b);
  let is_hard = |b: u8| !b.is_ascii_whitespace() && ifs.contains(&b);

  let mut fields: Vec<Vec<u8>> = Vec::new();
  let mut cur: Vec<u8> = Vec::new();
  let mut bytes = input.iter().copied().enumerate().peekable();

  while bytes.peek().is_some_and(|&(_, c)| is_ws(c)) {
    bytes.next();
  }

  while let Some(&(i, c)) = bytes.peek() {
    if max.is_some_and(|max| fields.len() == max - 1) {
      let mut rest = input[i..].to_vec();
      while rest.last().is_some_and(|&b| is_ws(b)) {
        rest.pop();
      }
      fields.push(rest);
      return fields;
    }

    bytes.next();

    if is_ws(c) {
      while bytes.peek().is_some_and(|&(_, c)| is_ws(c)) {
        bytes.next();
      }
      if bytes.peek().is_some_and(|&(_, c)| is_hard(c)) {
        bytes.next();
        while bytes.peek().is_some_and(|&(_, c)| is_ws(c)) {
          bytes.next();
        }
      }
      // trailing whitespace must not produce an empty field
      if bytes.peek().is_some() {
        fields.push(std::mem::take(&mut cur));
      }
    } else if is_hard(c) {
      fields.push(std::mem::take(&mut cur));
      while bytes.peek().is_some_and(|&(_, c)| is_ws(c)) {
        bytes.next();
      }
    } else {
      cur.push(c);
    }
  }

  if !cur.is_empty() {
    fields.push(cur);
  }

  fields
}

/* - splitting functions
 * the splitting functions in std are fine, but don't cut it when quoting rules and escaping are involved
 * so we have to roll our own stuff. we can take a functional approach to to this that generalizes quite well
 */

pub(crate) fn split_tk(tk: &Tk, pat: &[u8]) -> Vec<Tk> {
  let slice = tk.slice(); // scary! make sure the tk's source input is still alive
  let base = tk.span.range().start;
  split_all_with(
    slice.as_bytes(),
    |s| split_at_unescaped(s, pat),
    |start, end| {
      let start = base + start;
      let end = base + end;
      Tk::new(tk.class.clone(), Span::new(start, end, tk.source()))
    },
  )
}

pub(crate) fn split_all_with<T, F, B>(slice: &[u8], segment_fn: F, mut build: B) -> Vec<T>
where
  F: Fn(&[u8]) -> Option<(usize, usize)>,
  B: FnMut(usize, usize) -> T,
{
  let mut cursor = 0;
  let mut splits = vec![];
  while let Some((len, skip)) = segment_fn(&slice[cursor..]) {
    splits.push(build(cursor, cursor + len));
    cursor += len + skip;
  }
  if let Some(remaining) = slice.get(cursor..) {
    splits.push(build(cursor, cursor + remaining.len()));
  }
  splits
}

/// Splits a byte slice at the first occurrence of a pattern, but only if the pattern is not escaped by a backslash
/// and not in quotes. Returns None if the pattern is not found or only found escaped.
pub(crate) fn split_at_unescaped(slice: &[u8], pat: &[u8]) -> Option<(usize, usize)> {
  split_at_any_unescaped(slice, &[pat])
}

pub(crate) fn split_at_any_unescaped(slice: &[u8], pats: &[&[u8]]) -> Option<(usize, usize)> {
  split_at_match(slice, |s| {
    pats.iter().find(|p| s.starts_with(p)).map(|p| p.len())
  })
}

pub(crate) struct ByteSet([bool; 256]);

impl ByteSet {
  pub(crate) fn new(bytes: &[u8]) -> Self {
    let mut set = [false; 256];
    for &b in bytes {
      set[b as usize] = true;
    }
    ByteSet(set)
  }

  pub(crate) fn whitespace() -> Self {
    Self::new(b" \t\n\r")
  }

  pub(crate) fn contains(&self, byte: u8) -> bool {
    self.0[byte as usize]
  }
}

pub(crate) fn split_assignment_raw(arg: &[u8]) -> (&[u8], Option<&[u8]>) {
  let Some((e, l)) = split_at_unescaped(arg, b"=") else {
    return (arg, None);
  };
  (arg[..e].trim(), Some(&arg[e + l..]))
}

/// Which bytes, if any, the scanners treat as escape and quote characters.
///
/// [`QuotePolicy::SHELL`] is shed's own syntax, where a backslash escapes the
/// next byte and quotes open a region the delimiter cannot match inside.
/// [`QuotePolicy::LITERAL`] disables both, which is what splitting arbitrary
/// data wants -- an apostrophe in a CSV field should not open a quoted region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuotePolicy {
  pub(crate) esc: Option<u8>,
  pub(crate) sng_quote: Option<u8>,
  pub(crate) dub_quote: Option<u8>,
}

impl QuotePolicy {
  pub(crate) const SHELL: Self = Self {
    esc: Some(b'\\'),
    sng_quote: Some(b'\''),
    dub_quote: Some(b'"'),
  };

  pub(crate) const ESCAPE: Self = Self {
    esc: Some(b'\\'),
    sng_quote: None,
    dub_quote: None,
  };

  pub(crate) const LITERAL: Self = Self {
    esc: None,
    sng_quote: None,
    dub_quote: None,
  };
}

pub(crate) fn split_at_pat_with(
  slice: &[u8],
  pat: &[u8],
  policy: QuotePolicy,
) -> Option<(usize, usize)> {
  split_at_match_with(slice, |s| s.starts_with(pat).then_some(pat.len()), policy)
}

pub(crate) fn split_at_byteset_with(
  slice: &[u8],
  set: &ByteSet,
  policy: QuotePolicy,
) -> Option<(usize, usize)> {
  split_at_match_with(slice, |s| set.contains(s[0]).then_some(1), policy)
}

fn split_at_match(
  slice: &[u8],
  mut matches: impl FnMut(&[u8]) -> Option<usize>,
) -> Option<(usize, usize)> {
  split_at_match_with(slice, &mut matches, QuotePolicy::SHELL)
}

/// Split at the first position where `matches` reports a delimiter, skipping
/// anything `policy` marks as escaped or quoted.
pub(crate) fn split_at_match_with(
  slice: &[u8],
  mut matches: impl FnMut(&[u8]) -> Option<usize>,
  policy: QuotePolicy,
) -> Option<(usize, usize)> {
  let mut qt_state = QuoteState::default();
  let mut i = 0;

  while i < slice.len() {
    let b = slice[i];

    if policy.esc == Some(b) {
      i += 2;
      continue;
    }

    if policy.sng_quote == Some(b) {
      qt_state.toggle_single();
    } else if policy.dub_quote == Some(b) {
      qt_state.toggle_double();
    } else if qt_state.in_quote() {
      i += 1;
      continue;
    }

    if let Some(len) = matches(&slice[i..]) {
      return Some((i, len));
    }

    i += 1;
  }

  None
}

pub(crate) fn pos_is_escaped(slice: &[u8], pos: usize) -> bool {
  let mut escaped = false;
  let mut i = pos;
  while i > 0 && slice[i - 1] == b'\\' {
    escaped = !escaped;
    i -= 1;
  }
  escaped
}

pub(crate) fn ends_with_unescaped(slice: &[u8], pat: &[u8]) -> bool {
  slice.ends_with(pat) && !pos_is_escaped(slice, slice.len() - pat.len())
}

pub(crate) fn has_unescaped(slice: &[u8], pat: &[u8]) -> bool {
  split_at_unescaped(slice, pat).is_some()
}

/// A forward, byte-at-a-time cursor over some source text.
///
/// Implemented by the lexer (advancing its own `cursor`) and by [`SliceCursor`]
/// for standalone scans over a plain byte slice (arithmetic, tests, etc). This
/// is what lets the delimiter scanners below crawl bytes without caring whether
/// they're driving the live lexer or a throwaway buffer.
pub(crate) trait ByteCursor {
  /// The byte at the current position, without advancing.
  fn peek_byte(&self) -> Option<u8>;
  /// Consume and return the byte at the current position, advancing by one.
  fn next_byte(&mut self) -> Option<u8>;
  /// The byte at the current position + `n`, without advancing.
  fn peek_nth(&self, n: usize) -> Option<u8>;
  /// Consume the byte at the current position, advancing by one. Equivalent to `next_byte()`, but doesn't return the byte.
  fn bump(&mut self) {
    self.next_byte();
  }
  /// Consume and return the byte at the current position if it satisfies the predicate `f`.
  /// Returns `None` if the byte does not satisfy `f` or if there is no byte to consume.
  fn next_byte_if(&mut self, f: impl FnOnce(u8) -> bool) -> Option<u8> {
    let b = self.peek_byte()?;
    if f(b) { self.next_byte() } else { None }
  }
  /// Consume the byte at the current position if it satisfies the predicate `f`.
  /// Returns `true` if a byte was consumed, `false` otherwise.
  /// A byte that does not satisfy `f` is not consumed.
  fn bump_if(&mut self, f: impl Fn(u8) -> bool) -> bool {
    let Some(b) = self.peek_byte() else {
      return false;
    };
    if f(b) {
      self.next_byte();
      true
    } else {
      false
    }
  }
  /// Consume the byte at the current position if it is equal to `b`.
  /// Returns `true` if a byte was consumed, `false` otherwise.
  /// A byte that does not equal `b` is not consumed.
  fn bump_if_eq(&mut self, b: u8) -> bool {
    self.bump_if(|x| x == b)
  }
  /// Consume bytes at the current position while they satisfy the predicate `f`.
  /// Stops when a byte does not satisfy `f` or when there are no more bytes to consume.
  /// A byte that does not satisfy `f` is not consumed.
  fn bump_while(&mut self, f: impl Fn(u8) -> bool) {
    while self.bump_if(&f) {}
  }
  /// Returns `true` if there are no more bytes to consume, `false` otherwise.
  fn is_empty(&self) -> bool {
    self.peek_byte().is_none()
  }
}

/// A [`ByteCursor`] over a borrowed byte slice, tracking its own position.
/// For callers that need to scan an in-memory buffer rather than the lexer.
pub(crate) struct SliceCursor<'a> {
  bytes: &'a [u8],
  pos: usize,
}

impl<'a> SliceCursor<'a> {
  pub(crate) fn new(bytes: &'a [u8]) -> Self {
    Self { bytes, pos: 0 }
  }
  /// Number of bytes consumed so far.
  pub(crate) fn pos(&self) -> usize {
    self.pos
  }

  pub(crate) fn into_slice(self) -> &'a [u8] {
    &self.bytes[self.pos..]
  }

  pub(crate) fn bump_while_span<F: Fn(u8) -> bool>(&mut self, f: F) -> (usize, usize) {
    let start = self.pos;
    self.bump_while(f);
    let end = self.pos;
    (start, end)
  }

  /// Attempt to run `f` on this cursor, rolling back the position if `f` returns `false`.
  pub(crate) fn attempt<F: FnOnce(&mut Self) -> bool>(&mut self, f: F) -> bool {
    let start = self.pos;
    let res = f(self);
    if !res {
      self.pos = start;
    }
    res
  }

  /// Attempt to run `f` on this cursor, rolling back the position if `f` returns `false`.
  pub(crate) fn attempt_get<T, F: FnOnce(&mut Self) -> Option<T>>(&mut self, f: F) -> Option<T> {
    let start = self.pos;
    let res = f(self);
    if res.is_none() {
      self.pos = start;
    }
    res
  }
}

impl ByteCursor for SliceCursor<'_> {
  fn peek_byte(&self) -> Option<u8> {
    self.bytes.get(self.pos).copied()
  }
  fn peek_nth(&self, n: usize) -> Option<u8> {
    self.bytes.get(self.pos + n).copied()
  }
  fn next_byte(&mut self) -> Option<u8> {
    let b = self.peek_byte()?;
    self.pos += 1;
    Some(b)
  }
}

/// Scan a balanced `(...)`, consuming through the closing paren. `depth` is the
/// nesting already entered — pass `1` when the opening `(` was just consumed.
/// Returns `true` if the group closed, `false` if input ran out first.
pub(crate) fn scan_parens<C: ByteCursor>(c: &mut C, depth: usize) -> bool {
  scan_delims(b'(', c, depth)
}

/// Scan a balanced `[...]`; see [`scan_parens`]. Does not recurse into
/// `$(...)`, so a literal `]` in a command substitution closes early.
pub(crate) fn scan_brackets<C: ByteCursor>(c: &mut C, depth: usize) -> bool {
  scan_delims(b'[', c, depth)
}

/// Scan a balanced `${...}`, following nested `${...}` / `$(...)`. See
/// [`scan_parens`] for the `depth` convention and return value.
pub(crate) fn scan_param_exp<C: ByteCursor>(c: &mut C, mut depth: usize) -> bool {
  let mut qt = QuoteState::default();
  match_loop!(c.next_byte() => b, {
    b'\\' if !qt.in_single() => c.bump(),
    b'\'' => qt.toggle_single(),
    b'"' if !qt.in_single() => qt.toggle_double(),
    _ if qt.in_quote() => {}
    b'$' if c.peek_byte() == Some(b'{') => {
      c.next_byte();
      depth += 1;
    }
    b'$' if c.peek_byte() == Some(b'(') => {
      c.next_byte();
      // Reuse the paren-matcher so an inner `$(... } ...)` doesn't trip the
      // param-expansion closer scan.
      if !scan_parens(c, 1) {
        return false;
      }
    }
    b'}' => {
      depth -= 1;
      if depth == 0 { break; }
    }
    _ => {}
  });
  depth == 0
}

fn scan_delims<C: ByteCursor>(opener: u8, c: &mut C, mut depth: usize) -> bool {
  let closer = match opener {
    b'(' => b')',
    b'{' => b'}',
    b'[' => b']',
    b'<' => b'>',
    // Only ever called with the literals above; a new opener is a caller bug.
    _ => unreachable!("scan_delims: invalid opener {opener:#x}"),
  };
  let mut qt = QuoteState::default();
  match_loop!(c.next_byte() => b, {
    b'\\' if !qt.in_single() => c.bump(),
    b'\'' => qt.toggle_single(),
    b'"' if !qt.in_single() => qt.toggle_double(),
    _ if qt.in_quote() => {}
    _ if b == opener => depth += 1,
    _ if b == closer => {
      depth -= 1;
      if depth == 0 { break; }
    }
    _ => {}
  });
  depth == 0
}

#[cfg(test)]
mod split_policy_tests {
  use super::{ByteSet, QuotePolicy, split_at_byteset_with, split_at_pat_with};
  use pretty_assertions::assert_eq;

  #[test]
  fn shell_policy_skips_a_quoted_delimiter() {
    assert_eq!(
      split_at_pat_with(b"'a,b',c", b",", QuotePolicy::SHELL),
      Some((5, 1))
    );
  }

  #[test]
  fn literal_policy_matches_inside_quotes() {
    assert_eq!(
      split_at_pat_with(b"'a,b',c", b",", QuotePolicy::LITERAL),
      Some((2, 1))
    );
  }

  #[test]
  fn shell_policy_skips_an_escaped_delimiter() {
    assert_eq!(
      split_at_pat_with(b"a\\,b,c", b",", QuotePolicy::SHELL),
      Some((4, 1))
    );
  }

  #[test]
  fn literal_policy_ignores_the_escape() {
    assert_eq!(
      split_at_pat_with(b"a\\,b,c", b",", QuotePolicy::LITERAL),
      Some((2, 1))
    );
  }

  #[test]
  fn escapes_only_honours_escape_but_not_quotes() {
    let policy = QuotePolicy::ESCAPE;

    assert_eq!(split_at_pat_with(b"a\\,b", b",", policy), None);
    assert_eq!(split_at_pat_with(b"'a,b'", b",", policy), Some((2, 1)));
  }

  #[test]
  fn byteset_respects_the_policy() {
    let set = ByteSet::whitespace();

    assert_eq!(
      split_at_byteset_with(b"'a b' c", &set, QuotePolicy::SHELL),
      Some((5, 1))
    );
    assert_eq!(
      split_at_byteset_with(b"'a b' c", &set, QuotePolicy::LITERAL),
      Some((2, 1))
    );
  }

  #[test]
  fn shell_default_skips_an_escaped_delimiter() {
    use super::split_at_unescaped;

    assert_eq!(split_at_unescaped(b"a\\=b=c", b"="), Some((4, 1)));
  }

  #[test]
  fn shell_default_skips_a_quoted_delimiter() {
    use super::split_at_unescaped;

    assert_eq!(split_at_unescaped(b"'a=b'=c", b"="), Some((5, 1)));
  }

  #[test]
  fn assignment_split_honours_an_escaped_equals() {
    use super::split_assignment_raw;

    let (name, value) = split_assignment_raw(b"a\\=b=c");

    assert_eq!(name, b"a\\=b");
    assert_eq!(value, Some(&b"c"[..]));
  }

  #[test]
  fn no_delimiter_is_none_under_every_policy() {
    for policy in [
      QuotePolicy::SHELL,
      QuotePolicy::LITERAL,
      QuotePolicy::ESCAPE,
    ] {
      assert_eq!(split_at_pat_with(b"abc", b",", policy), None);
    }
  }
}
