pub(crate) mod alias;
pub(crate) mod arithmetic;
pub(crate) mod brace;
pub(crate) mod case;
pub(crate) mod escape;
pub(crate) mod glob;
pub(crate) mod markers;
pub(crate) mod param;
pub(crate) mod prompt;
pub(crate) mod stream;
pub(crate) mod subshell;
pub(crate) mod var;

use std::{
  convert::Into,
  ffi::OsStr,
  fmt::Display,
  hash::{Hash, Hasher},
  ops::{Deref, DerefMut},
  path::Path,
  sync::Arc,
};

use crate::{
  eval::lex::{Span, Tk, TkFlags, TkRule},
  expand::stream::{Marker, ProcSubKind},
  match_loop, sherr, shopt,
  state::{
    params,
    vars::{ShStr, ShStrSliceExt},
  },
  util::error::{ShResult, ShResultExt},
};

use stream::Quote;

/// The shell's main "word" representation.
///
/// Carries a [`ShStr`] (reference counted pointer to a byte array), and optionally a [`Span`].
/// The `Option<Span>` can be used to tag errors on the way up during propagation.
#[derive(Clone, Debug, Default)]
pub(crate) struct Word {
  content: ShStr,
  span   : Option<Span>,
}

impl PartialEq for Word {
  fn eq(&self, other: &Self) -> bool {
    self.content == other.content
  }
}

impl Eq for Word {}

impl Hash for Word {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.content.hash(state);
  }
}

impl Word {
  pub(crate) fn new(content: impl Into<ShStr>) -> Self {
    Self {
      content: content.into(),
      span   : None,
    }
  }
  pub(crate) fn unpack(self) -> (ShStr, Option<Span>) {
    (self.content, self.span)
  }
  pub(crate) fn with_span(mut self, span: Span) -> Self {
    if self.span.is_none() {
      self.clobber_span(span);
    }
    self
  }
  pub(crate) fn content(&self) -> ShStr {
    self.content.clone()
  }
  pub(crate) fn set_span(&mut self, span: Span) {
    if self.span.is_none() {
      self.clobber_span(span);
    }
  }
  pub(crate) fn clobber_span(&mut self, span: Span) {
    self.span = Some(span);
  }
  pub(crate) fn with_span_opt(self, span: Option<Span>) -> Self {
    if let Some(span) = span {
      self.with_span(span)
    } else {
      self
    }
  }
  pub(crate) fn span(&self) -> Option<Span> {
    self.span
  }
  #[track_caller]
  pub(crate) fn expect_span(&self) -> Span {
    self.span.expect("Word has no span")
  }
  pub(crate) fn absorb(mut self, other: Word) -> Self {
    let (other_c, other_s) = other.unpack();
    self.content.push_slice(other_c);
    if let Some(o_span) = other_s {
      if let Some(s_span) = self.span.as_mut() {
        s_span.merge_inplace(o_span);
      } else {
        self.span = Some(o_span);
      }
    }
    self
  }
}

impl Display for Word {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.content.fmt(f)
  }
}

impl Deref for Word {
  type Target = ShStr;

  fn deref(&self) -> &Self::Target {
    &self.content
  }
}

impl AsRef<Path> for Word {
  fn as_ref(&self) -> &Path {
    self.content.as_ref()
  }
}

impl AsRef<OsStr> for Word {
  fn as_ref(&self) -> &OsStr {
    self.content.as_ref()
  }
}

impl From<Span> for Word {
  fn from(value: Span) -> Self {
    Self::new(value.slice()).with_span(value)
  }
}

pub(crate) fn resolve_text(word: &Tk) -> Option<Word> {
  const UNSAFE: [Marker; 3] = [
    Marker::Subshell,
    Marker::ProcSub(ProcSubKind::In),
    Marker::ProcSub(ProcSubKind::Out),
  ];

  let seg = escape::unescape_str(&word.slice());
  if UNSAFE.iter().any(|m| seg.contains_marker(*m)) || seg.has_glob_meta() {
    return None;
  }

  word.expand_to_words_pure().ok()
}

impl From<Tk> for Word {
  fn from(tk: Tk) -> Self {
    Self::from(&tk)
  }
}
impl From<&Tk> for Word {
  fn from(tk: &Tk) -> Self {
    Word::new(tk.slice()).with_span(tk.span)
  }
}

impl From<Vec<u8>> for Word {
  fn from(value: Vec<u8>) -> Self {
    Word::new(ShStr::from(value))
  }
}

impl From<&[u8]> for Word {
  fn from(value: &[u8]) -> Self {
    Word::new(ShStr::from(value))
  }
}

impl From<ShStr> for Word {
  fn from(value: ShStr) -> Self {
    Word::new(value)
  }
}

impl From<&str> for Word {
  fn from(value: &str) -> Self {
    Word::new(ShStr::from(value))
  }
}

/// A list of words. A word is a [`ShStr`] and a [`Span`] bundled together.
///
/// This type is used commonly in places that handle arg vectors and expansion
#[derive(Clone, Debug, Default)]
pub(crate) struct Words(Vec<Word>);

impl Words {
  pub(crate) fn new(words: Vec<Word>) -> Self {
    Self(words)
  }
  pub(crate) fn join_with(&self, sep: &str) -> ShStr {
    self.0.join_with(sep)
  }
  pub(crate) fn into_words(self) -> Vec<Word> {
    self.0
  }
  pub(crate) fn promote_words(&mut self, span: Span) {
    for word in &mut self.0 {
      word.set_span(span);
    }
  }
  pub(crate) fn with_span(mut self, span: Span) -> Self {
    self.promote_words(span);
    self
  }
  pub(crate) fn map_content(self, f: impl Fn(ShStr) -> ShStr) -> Self {
    self
      .0
      .into_iter()
      .map(|word| {
        let (content, span) = word.unpack();
        Word::new(f(content)).with_span_opt(span)
      })
      .collect()
  }

  pub(crate) fn as_one_word(&self) -> Word {
    let sep    = params::get_separator();
    let joined = self.join_with(&sep.to_str_lossy());
    Word::new(joined)
  }

  /// Whether the array is empty, or contains a single empty word
  ///
  /// Parameter expansion sees shell arrays as empty if they contain one empty word, or zero words at all
  pub(crate) fn empty_param(&self) -> bool {
    self.is_empty() || (self.len() == 1 && self[0].is_empty())
  }
}

impl IntoIterator for Words {
  type Item = Word;
  type IntoIter = std::vec::IntoIter<Word>;

  fn into_iter(self) -> Self::IntoIter {
    self.0.into_iter()
  }
}

impl FromIterator<ShStr> for Words {
  fn from_iter<T: IntoIterator<Item = ShStr>>(iter: T) -> Self {
    Self(iter.into_iter().map(Word::new).collect())
  }
}

impl FromIterator<Word> for Words {
  fn from_iter<T: IntoIterator<Item = Word>>(iter: T) -> Self {
    Self(iter.into_iter().collect())
  }
}

impl From<Words> for Arc<[Word]> {
  fn from(words: Words) -> Self {
    Arc::from(words.0)
  }
}

impl From<Tk> for Words {
  fn from(tk: Tk) -> Self {
    Self::from(&tk)
  }
}
impl From<&Tk> for Words {
  fn from(tk: &Tk) -> Self {
    Self(vec![Word::from(tk)])
  }
}

impl From<&[Word]> for Words {
  fn from(words: &[Word]) -> Self {
    Self(words.to_vec())
  }
}

impl From<ShStr> for Words {
  fn from(vs: ShStr) -> Self {
    Self(vec![Word::new(vs)])
  }
}

impl Deref for Words {
  type Target = Vec<Word>;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl DerefMut for Words {
  fn deref_mut(&mut self) -> &mut Vec<Word> {
    &mut self.0
  }
}

pub(crate) const PARAMETERS: [char; 8] = ['-', '@', '*', '#', '$', '?', '!', '0'];

impl Tk {
  /// Create a new expanded token
  pub(crate) fn expand(&self) -> ShResult<Self> {
    if let TkRule::Expanded { .. } = self.class {
      return Ok(self.clone());
    }

    let class = TkRule::Expanded {
      exp: self.expand_to_words()?.into(),
    };
    Ok(Self {
      class,
      span: self.span,
      flags: self.flags,
    })
  }
  pub(crate) fn expand_to_words(&self) -> ShResult<Words> {
    if let TkRule::Expanded { exp } = &self.class {
      return Ok(exp.iter().cloned().collect());
    }
    if self.is_literal() {
      return Ok(self.into());
    }
    let span = self.span;
    Expander::new(self)
      .expand()
      .map(|w| w.with_span(span))
      .promote_err(span)
  }
  pub(crate) fn expand_to_words_pure(&self) -> ShResult<Word> {
    if let TkRule::Expanded { exp } = &self.class {
      match exp.first() {
        Some(exp) => return Ok(exp.clone()),
        None      => return Err(sherr!(InternalErr @ self.span, "Expanded token has no words")),
      }
    }
    if self.is_literal() {
      return Ok(self.into());
    }

    let span = self.span;
    Expander::new(self)
      .expand_no_side_effects()
      .map(|v| Word::new(v).with_span(self.span))
      .promote_err(span)
  }
  pub(crate) fn expand_no_side_effects(&self) -> ShResult<Self> {
    if let TkRule::Expanded { .. } = self.class {
      return Ok(self.clone());
    }
    let class = TkRule::Expanded {
      exp: [self.expand_to_words_pure()?].into(),
    };
    Ok(Self {
      class,
      span: self.span,
      flags: self.flags,
    })
  }
  pub(crate) fn expand_no_split(&self) -> ShResult<ShStr> {
    if let TkRule::Expanded { exp } = &self.class {
      return Ok(exp.join_with(" "));
    }
    if self.is_literal() {
      return Ok(self.slice());
    }

    let span = self.span;
    let exp = Expander::new(self)
      .no_glob()
      .no_split()
      .expand_no_split()
      .promote_err(span)?;
    Ok(exp)
  }
  /// Perform word splitting
  pub(crate) fn get_words(&self) -> Arc<[Word]> {
    match &self.class {
      TkRule::Expanded { exp } => exp.clone(),
      _ => [Word::from(self)].into(),
    }
  }

  pub(crate) fn get_first_word(&self) -> Option<Word> {
    self.get_words().iter().next().cloned()
  }
}

pub(crate) struct Expander {
  span: Option<Span>,
  flags: TkFlags,
  noglob: bool,
  nosplit: bool,
  allow_side_effects: bool,
  raw: stream::SegStream,
}

impl Expander {
  pub(crate) fn new(raw: &Tk) -> Self {
    let     tk_raw = raw.slice();
    let mut new    = Self::from_raw(&tk_raw, raw.flags);
    new.span = Some(raw.span);
    new
  }
  pub(crate) fn from_raw(raw: &[u8], flags: TkFlags) -> Self {
    let raw = if raw.contains(&b'{') {
      brace::expand_braces_full(None, raw).join_with(" ")
    } else {
      ShStr::from(raw)
    };
    let unescaped = if flags.contains(TkFlags::IS_HEREDOC) {
      escape::unescape_heredoc(&raw)
    } else {
      escape::unescape_str(&raw)
    };
    Self::from_segs(unescaped, flags)
  }
  /// Like `from_raw` but the operand is a parameter-expansion pattern or
  /// replacement (`${var#pat}`, `${var%pat}`, `${var/pat/rep}`): a bare `(` is
  /// literal, not a subshell. The operand was carved out of an already-unescaped
  /// `${...}` body, so it arrives as a `SegStream` (markers preserved).
  pub(crate) fn from_raw_pattern(operand: stream::SegStream, flags: TkFlags) -> Self {
    Self::from_segs(escape::unescape_pattern(operand), flags)
  }
  /// Brace-free variant of `from_raw_pattern`.
  pub(crate) fn from_raw_no_brace_pattern(operand: stream::SegStream, flags: TkFlags) -> Self {
    Self::from_segs(escape::unescape_pattern(operand), flags)
  }
  fn from_segs(raw: stream::SegStream, flags: TkFlags) -> Self {
    Self {
      span: None,
      raw,
      noglob: false,
      nosplit: false,
      allow_side_effects: true,
      flags,
    }
  }
  pub(crate) fn no_glob(self) -> Self {
    Self {
      noglob: true,
      ..self
    }
  }
  pub(crate) fn no_split(self) -> Self {
    Self {
      nosplit: true,
      ..self
    }
  }
  pub(crate) fn expand(self) -> ShResult<Words> {
    let noglob = self.noglob || shopt!(set.noglob);
    if let Some(b) = self.raw.as_plain_bytes() {
      // single literal byte string, so no splitting is needed
      if noglob || !glob::might_be_glob(b) {
        return Ok(Words::new(vec![b.into()])); // no globs either, just return it
      }
      let exp = glob::expand_glob(b).into_iter().map(ShStr::from).collect();
      return Ok(exp);
    }

    let mark_split = !self.flags.contains(TkFlags::IS_HEREDOC) && !self.nosplit;
    let raw        = self.expand_inner(mark_split)?;

    let words: Vec<stream::SegStream> = if mark_split {
      Self::split_words(&raw)
    } else {
      vec![raw]
    };

    if noglob {
      return Ok(
        words
          .into_iter()
          .map(|w| Word::from(w.as_bytes()))
          .collect(),
      );
    }

    let mut glob_words = Vec::with_capacity(words.len());

    for word in words {
      if !word.has_glob_meta() {
        // no glob characters, just push the word
        glob_words.push(word.as_bytes().into());
        continue;
      }

      let pattern_bytes  = escape::markers_to_glob_escapes(&word);
      let literal: ShStr = word.as_bytes().into();

      if !glob::might_be_glob(&pattern_bytes) {
        glob_words.push(literal.into());
        continue;
      }

      let expansions = glob::expand_glob(&pattern_bytes)
        .into_iter()
        .map(Word::from);

      glob_words.extend(expansions);
    }

    Ok(Words::new(glob_words))
  }
  pub(crate) fn expand_no_side_effects(mut self) -> ShResult<ShStr> {
    self.allow_side_effects = false;
    let raw = self.expand_inner(false)?;
    Ok(raw.as_bytes().into())
  }
  pub(crate) fn expand_no_split(self) -> ShResult<ShStr> {
    let raw = self.expand_inner(false)?;
    Ok(raw.as_bytes().into())
  }
  pub(crate) fn expand_keep_quotes(self) -> ShResult<ShStr> {
    let     raw          = self.expand_inner(false)?;
    let mut out: Vec<u8> = Vec::new();
    let mut cursor       = raw.cursor();
    while let Some(unit) = cursor.next() {
      match unit {
        stream::Unit::Byte(b) => out.push(b),
        stream::Unit::Mark(stream::Marker::Quote(stream::Quote::Double)) => out.push(b'"'),
        stream::Unit::Mark(stream::Marker::Quote(stream::Quote::Single)) => out.push(b'\''),
        stream::Unit::Mark(_) => {}
      }
    }
    Ok(out.into())
  }
  pub(crate) fn expand_for_glob(self) -> ShResult<ShStr> {
    let raw = self.expand_inner(false)?;
    Ok(escape::markers_to_glob_escapes(&raw).into())
  }
  pub(crate) fn expand_inner(self, mark_split: bool) -> ShResult<stream::SegStream> {
    let mut cursor = self.raw.cursor();
    let raw = var::expand_raw_inner(self.span, &mut cursor, self.allow_side_effects, mark_split)?;

    Ok(raw)
  }
  /// Perform POSIX word splitting.
  ///
  /// Resolves escapes and the special `$@`/`$*` cases, and performs IFS field
  /// splitting, but only inside `EXPAND_START`/`EXPAND_END` runs.
  pub(crate) fn split_words(raw: &stream::SegStream) -> Vec<stream::SegStream> {
    use stream::{Marker, SegStream, Unit};
    let mut words: Vec<SegStream> = vec![];
    let mut cursor                = raw.cursor();
    let mut cur_word              = SegStream::default();
    let mut was_quoted            = false;
    let     ifs                   = params::get_separators();
    // Delimiter-run tracking: whitespace and non-whitespace IFS chars combine
    // into one run that delimits a single field. A second non-WS IFS in the
    // same run emits an additional empty field (per POSIX step 5).
    let mut in_delim_run          = false;
    let mut delim_has_non_ws      = false;

    let mut expansion_depth       = 0;

    'outer: while let Some(unit) = cursor.next() {
      match unit {
        Unit::Mark(Marker::ExpandStart) => expansion_depth += 1,
        Unit::Mark(Marker::ExpandEnd) => {
          if expansion_depth > 0 {
            expansion_depth -= 1;
          }
        }
        Unit::Mark(Marker::Escape) => {
          in_delim_run = false;
          delim_has_non_ws = false;
          if let Some(next_unit) = cursor.next() {
            // Preserve the ESCAPE marker so glob expansion (running after
            // split_words) treats backslash-escaped meta chars as literal.
            // expand() will strip remaining ESCAPE markers after globbing.
            cur_word.push_marker(Marker::Escape);
            cur_word.push(next_unit);
          }
        }
        Unit::Mark(Marker::Quote(_) | Marker::Subshell) => {
          in_delim_run = false;
          delim_has_non_ws = false;
          match_loop!(cursor.next() => q_unit, {
            Unit::Mark(Marker::ArgSep) if unit == Unit::Mark(Marker::Quote(Quote::Double)) => {
              words.push(std::mem::take(&mut cur_word));
            }
            _ if q_unit == unit => {
              was_quoted = true;
              continue 'outer; // Isn't rust cool
            }
            Unit::Byte(b) => {
              // Quote-region content: glob meta chars inside quotes must
              // remain literal at glob time. Prepend ESCAPE so escape_glob
              // converts them to glob-literal form.
              if matches!(b, b'*' | b'?' | b'[' | b']') {
                cur_word.push_marker(Marker::Escape);
              }
              cur_word.push_byte(b);
            }
            Unit::Mark(m) => cur_word.push_marker(m),
          });
        }
        _ if unit == Unit::Mark(Marker::ArgSep)
          || matches!(unit, Unit::Byte(b) if expansion_depth > 0 && ifs.contains(&b)) =>
        {
          let is_ws =
            unit == Unit::Mark(Marker::ArgSep) || matches!(unit, Unit::Byte(b' ' | b'\t' | b'\n'));
          if !in_delim_run {
            // Just exited a field (or saw leading IFS). Decide whether to emit.
            if is_ws {
              if !cur_word.is_empty() || was_quoted {
                words.push(std::mem::take(&mut cur_word));
                was_quoted = false;
              }
            } else {
              // Non-WS IFS always emits (preserves leading/middle empty fields).
              words.push(std::mem::take(&mut cur_word));
              was_quoted = false;
              delim_has_non_ws = true;
            }
            in_delim_run = true;
          } else if !is_ws {
            // Already in a delimiter run and we hit another non-WS IFS char.
            if delim_has_non_ws {
              // Second non-WS in this run -> emit an empty field.
              words.push(SegStream::default());
            } else {
              // First non-WS adjacent to WS in the run -> just absorb into the run.
              delim_has_non_ws = true;
            }
          }
          // else: WS within an existing delim run -> absorb
        }
        Unit::Byte(b) => {
          in_delim_run = false;
          delim_has_non_ws = false;
          cur_word.push_byte(b);
        }
        Unit::Mark(m) => {
          in_delim_run = false;
          delim_has_non_ws = false;
          cur_word.push_marker(m);
        }
      }
    }
    if words.is_empty() && (cur_word.is_empty() && !was_quoted) {
      return words;
    } else if !cur_word.is_empty() || was_quoted {
      words.push(cur_word);
    }

    // Drop a lone NULL_EXPAND word (`"$@"`/`"$*"` with no positional args) and
    // strip the marker from any surviving fields.
    words.retain(|w| w.sole_marker() != Some(Marker::NullExpand));
    for w in &mut words {
      if w.contains_marker(Marker::NullExpand) {
        *w = w.without_marker(Marker::NullExpand);
      }
    }
    words
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{expand::stream::SegStream, set_var};
  use std::collections::VecDeque;

  // These tests build an `Expander.raw` from a marker-char string; convert it
  // to the byte-native `SegStream` (marker chars → real markers).
  #[allow(dead_code)]
  mod markers {
    pub(super) const DUB_QUOTE: char = '\u{fdd0}';
    pub(super) const SNG_QUOTE: char = '\u{fdd1}';
    pub(super) const NULL_EXPAND: char = '\u{fdd5}';
    pub(super) const ARG_SEP: char = '\u{fdd6}';
    pub(super) const SUBSH: char = '\u{fdd7}';
    pub(super) const ESCAPE: char = '\u{fdd9}';
    pub(super) const EXPAND_START: char = '\u{fde1}';
    pub(super) const EXPAND_END: char = '\u{fde2}';
  }
  fn to_segstream(s: &str) -> SegStream {
    use stream::{Marker, ProcSubKind, Quote};
    let mut seg = SegStream::default();
    for ch in s.chars() {
      let marker = match ch {
        '\u{fdd0}' => Some(Marker::Quote(Quote::Double)),
        '\u{fdd1}' => Some(Marker::Quote(Quote::Single)),
        '\u{fdd2}' => Some(Marker::TildeSub),
        '\u{fdd3}' => Some(Marker::ProcSub(ProcSubKind::In)),
        '\u{fdd4}' => Some(Marker::ProcSub(ProcSubKind::Out)),
        '\u{fdd5}' => Some(Marker::NullExpand),
        '\u{fdd6}' => Some(Marker::ArgSep),
        '\u{fdd7}' => Some(Marker::Subshell),
        '\u{fdd8}' => Some(Marker::VarSub),
        '\u{fdd9}' => Some(Marker::Escape),
        '\u{fde1}' => Some(Marker::ExpandStart),
        '\u{fde2}' => Some(Marker::ExpandEnd),
        _          => None,
      };
      if let Some(m) = marker {
        seg.push_marker(m);
      } else {
        let mut buf = [0u8; 4];
        seg.push_bytes(ch.encode_utf8(&mut buf).as_bytes());
      }
    }
    seg
  }
  fn render(seg: &SegStream) -> String {
    use stream::{Marker, ProcSubKind, Quote, StreamSeg};
    let mut out = String::new();
    for s in &seg.stream() {
      match s {
        StreamSeg::Bytes(b) => out.push_str(&String::from_utf8_lossy(b)),
        StreamSeg::Mark(m) => out.push(match m {
          Marker::Quote(Quote::Double) => '\u{fdd0}',
          Marker::Quote(Quote::Single) => '\u{fdd1}',
          Marker::TildeSub             => '\u{fdd2}',
          Marker::ProcSub(ProcSubKind::In)  => '\u{fdd3}',
          Marker::ProcSub(ProcSubKind::Out) => '\u{fdd4}',
          Marker::NullExpand  => '\u{fdd5}',
          Marker::ArgSep      => '\u{fdd6}',
          Marker::Subshell    => '\u{fdd7}',
          Marker::VarSub      => '\u{fdd8}',
          Marker::Escape      => '\u{fdd9}',
          Marker::ExpandStart => '\u{fde1}',
          Marker::ExpandEnd   => '\u{fde2}',
        }),
      }
    }
    out
  }

  use crate::state::{
    Shed,
    vars::{ArrIndex, ShStr, VarFlags, VarKind},
  };
  use crate::tests::testutil::{TestGuard, test_input};

  // ===================== Word Splitting (TestGuard) =====================

  #[test]
  fn word_split_default_ifs() {
    let _guard = TestGuard::new();

    let raw = format!(
      "{}hello world\tfoo{}",
      markers::EXPAND_START,
      markers::EXPAND_END
    );
    let exp = Expander {
      span: None,
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = Expander::split_words(&exp.raw);
    assert_eq!(words.clone(), vec!["hello", "world", "foo"]);
  }

  #[test]
  fn word_split_custom_ifs() {
    let _guard = TestGuard::new();
    set_var!(b"IFS", VarKind::Str(":".into())).unwrap();

    let raw = format!("{}a:b:c{}", markers::EXPAND_START, markers::EXPAND_END);
    let exp = Expander {
      span: None,
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = Expander::split_words(&exp.raw);
    assert_eq!(words.clone(), vec!["a", "b", "c"]);
  }

  #[test]
  fn word_split_empty_ifs() {
    let _guard = TestGuard::new();
    set_var!(b"IFS", VarKind::Str(ShStr::default())).unwrap();

    // Even as expansion output, an empty IFS suppresses all field splitting.
    let raw = format!(
      "{}hello world{}",
      markers::EXPAND_START,
      markers::EXPAND_END
    );
    let exp = Expander {
      span: None,
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = Expander::split_words(&exp.raw);
    assert_eq!(words.clone(), vec!["hello world"]);
  }

  #[test]
  fn word_split_quoted_no_split() {
    let _guard = TestGuard::new();

    let raw    = format!("{}hello world{}", markers::DUB_QUOTE, markers::DUB_QUOTE);
    let exp = Expander {
      span: None,
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = Expander::split_words(&exp.raw);
    assert_eq!(words.clone(), vec!["hello world"]);
  }

  // ===================== Escaped Word Splitting =====================

  #[test]
  fn word_split_escaped_space() {
    let _guard = TestGuard::new();

    let raw    = format!("hello{}world", render(&escape::unescape_str(b"\\ ")));
    let exp = Expander {
      span: None,
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: true,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.expand().unwrap();
    assert_eq!(words.to_vec(), vec!["hello world".into()]);
  }

  #[test]
  fn word_split_escaped_tab() {
    let _guard = TestGuard::new();

    let raw    = format!("hello{}world", render(&escape::unescape_str(b"\\\t")));
    let exp = Expander {
      span: None,
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: true,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.expand().unwrap();
    assert_eq!(words.to_vec(), vec!["hello\tworld".into()]);
  }

  #[test]
  fn word_split_escaped_custom_ifs() {
    let _guard = TestGuard::new();
    set_var!(b"IFS", VarKind::Str(":".into())).unwrap();

    let raw = format!("a{}b:c", render(&escape::unescape_str(b"\\:")));
    let exp = Expander {
      span: None,
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: true,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    // A literal word with no expansion is never field-split, so neither the
    // escaped `\:` nor the bare `:` splits — both are literal colons and the
    // word stays whole (matches bash: `IFS=:; echo a\:b:c` -> `a:b:c`).
    let words = exp.expand().unwrap();
    assert_eq!(words.to_vec(), vec!["a:b:c".into()]);
  }

  // ===================== Array Indexing (TestGuard) =====================

  #[test]
  fn array_index_first() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        b"arr",
        VarKind::arr(["a", "b", "c"].map(ShStr::from)),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let val = Shed::vars(|v| v.index_var(b"arr", &ArrIndex::Literal(0))).unwrap();
    assert_eq!(val, "a");
  }

  #[test]
  fn array_index_second() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        b"arr",
        VarKind::arr(["x", "y", "z"].map(ShStr::from)),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let val = Shed::vars(|v| v.index_var(b"arr", &ArrIndex::Literal(1))).unwrap();
    assert_eq!(val, "y");
  }

  #[test]
  fn array_all_elems() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        b"arr",
        VarKind::arr(["a", "b", "c"].map(ShStr::from)),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let elems = Shed::vars(|v| v.try_get_arr_elems(b"arr")).unwrap();
    assert_eq!(elems, vec!["a", "b", "c"]);
  }

  #[test]
  fn array_elem_count() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        b"arr",
        VarKind::arr(["a", "b", "c"].map(ShStr::from)),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let elems = Shed::vars(|v| v.try_get_arr_elems(b"arr")).unwrap();
    assert_eq!(elems.len(), 3);
  }

  // ===================== Direct Input Tests (TestGuard) =====================

  #[test]
  fn index_simple() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        b"arr",
        VarKind::Arr(VecDeque::from(["foo".into(), "bar".into(), "biz".into()])),
        VarFlags::empty(),
      )
    })
    .unwrap();

    test_input("echo $arr").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "foo bar biz\n");
  }

  #[test]
  fn index_cursed() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        b"arr",
        VarKind::Arr(VecDeque::from(["foo".into(), "bar".into(), "biz".into()])),
        VarFlags::empty(),
      )
    })
    .unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        b"i",
        VarKind::Arr(VecDeque::from(["0".into(), "1".into(), "2".into()])),
        VarFlags::empty(),
      )
    })
    .unwrap();

    test_input("echo $echo ${var:-${arr[$(($(echo ${i[@]:1:1}) + 1))]}}").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "biz\n");
  }
}
