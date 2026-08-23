mod alias;
mod arithmetic;
mod brace;
mod escape;
mod glob;
pub(super) mod markers;
mod param;
mod prompt;
pub(crate) mod stream;
pub(crate) mod subshell;
mod util;
mod var;

use std::{convert::Into, rc::Rc};

pub(super) use alias::{expand_alias_with_pos, expand_aliases, expand_keymap};
pub(super) use arithmetic::{expand_arithmetic, expand_arithmetic_wrapped};
pub(super) use escape::{
  escape_str, expand_ansi_c, shell_quote, shell_quote_bytes, shell_quote_fmt, unescape_heredoc,
  unescape_prompt, unescape_str, xtrace_quote,
};
pub(super) use glob::{GlobOpts, Pattern, expand_glob_with, replace_posix_classes};
pub(super) use prompt::expand_prompt;
pub(super) use util::expand_case_pattern;
pub(super) use var::{expand_raw, expand_raw_inner};

use crate::state::vars::{VarStr, VarStrSliceExt};

use super::{
  eval::{
    self,
    lex::{Tk, TkFlags, TkRule},
  },
  keys, match_loop, procio, sherr, shopt, state, status_msg, try_var, util as crate_util,
  util::{QuoteState, ShErr, ShResult, ShResultExt},
  var,
};

pub(crate) const PARAMETERS: [char; 8] = ['-', '@', '*', '#', '$', '?', '!', '0'];

impl Tk {
  /// Create a new expanded token
  pub fn expand(&self) -> ShResult<Self> {
    if let TkRule::Expanded { .. } = self.class {
      return Ok(self.clone());
    }
    if self.is_literal() {
      let raw = self.span.as_bytes().into();
      let class = TkRule::Expanded { exp: [raw].into() };
      return Ok(Self {
        class,
        ..self.clone()
      });
    }

    let flags = self.flags;
    let span = self.span.clone();
    let exp = Expander::new(self).expand().promote_err(span.clone())?;
    let class = TkRule::Expanded { exp: exp.into() };
    Ok(Self { class, span, flags })
  }
  pub fn expand_to_words(&self) -> ShResult<Rc<[VarStr]>> {
    if let TkRule::Expanded { exp } = &self.class {
      return Ok(exp.clone());
    }
    if self.is_literal() {
      return Ok([self.span.as_bytes().into()].into());
    }
    let span = self.span.clone();
    Expander::new(self)
      .expand()
      .map(Into::into)
      .promote_err(span)
  }
  pub fn expand_no_side_effects(&self) -> ShResult<Self> {
    if let TkRule::Expanded { .. } = self.class {
      return Ok(self.clone());
    }
    if self.is_literal() {
      let raw = self.span.as_bytes().into();
      let class = TkRule::Expanded { exp: [raw].into() };
      return Ok(Self {
        class,
        ..self.clone()
      });
    }

    let flags = self.flags;
    let span = self.span.clone();
    let exp: VarStr = Expander::new(self)
      .expand_no_side_effects()
      .promote_err(span.clone())?;

    let class = TkRule::Expanded { exp: [exp].into() };
    Ok(Self { class, span, flags })
  }
  pub fn expand_no_split(&self) -> ShResult<VarStr> {
    if let TkRule::Expanded { exp } = &self.class {
      return Ok(exp.join_with(" "));
    }
    if self.is_literal() {
      return Ok(self.span.as_bytes().into());
    }

    let span = self.span.clone();
    let exp = Expander::new(self)
      .no_glob()
      .no_split()
      .expand_no_split()
      .promote_err(span.clone())?;
    Ok(exp)
  }
  /// Perform word splitting
  pub fn get_words(&self) -> Rc<[VarStr]> {
    match &self.class {
      TkRule::Expanded { exp } => exp.clone(),
      _ => [self.as_bytes().into()].into(),
    }
  }

  pub fn get_first_word(&self) -> Option<VarStr> {
    self.get_words().iter().next().cloned()
  }
}

pub struct Expander {
  flags: TkFlags,
  noglob: bool,
  nosplit: bool,
  allow_side_effects: bool,
  raw: stream::SegStream,
}

impl Expander {
  pub fn new(raw: &Tk) -> Self {
    let tk_raw = raw.span.as_bytes();
    Self::from_raw(tk_raw, raw.flags)
  }
  pub fn from_raw(raw: &[u8], flags: TkFlags) -> Self {
    let raw = if raw.contains(&b'{') {
      brace::expand_braces_full(raw).join_with(" ")
    } else {
      VarStr::from(raw)
    };
    let unescaped = if flags.contains(TkFlags::IS_HEREDOC) {
      unescape_heredoc(&raw)
    } else {
      unescape_str(&raw)
    };
    Self::from_segs(unescaped, flags)
  }
  /// Like `from_raw` but the operand is a parameter-expansion pattern or
  /// replacement (`${var#pat}`, `${var%pat}`, `${var/pat/rep}`): a bare `(` is
  /// literal, not a subshell. The operand was carved out of an already-unescaped
  /// `${...}` body, so it arrives as a `SegStream` (markers preserved).
  pub fn from_raw_pattern(operand: stream::SegStream, flags: TkFlags) -> Self {
    Self::from_segs(escape::unescape_pattern(operand), flags)
  }
  /// Brace-free variant of `from_raw_pattern`.
  pub fn from_raw_no_brace_pattern(operand: stream::SegStream, flags: TkFlags) -> Self {
    Self::from_segs(escape::unescape_pattern(operand), flags)
  }
  fn from_segs(raw: stream::SegStream, flags: TkFlags) -> Self {
    Self {
      raw,
      noglob: false,
      nosplit: false,
      allow_side_effects: true,
      flags,
    }
  }
  pub fn no_glob(self) -> Self {
    Self {
      noglob: true,
      ..self
    }
  }
  pub fn no_split(self) -> Self {
    Self {
      nosplit: true,
      ..self
    }
  }
  pub fn expand(&mut self) -> ShResult<Vec<VarStr>> {
    let mark_split = !self.flags.contains(TkFlags::IS_HEREDOC) && !self.nosplit;
    self.expand_inner(mark_split)?;
    let words: Vec<stream::SegStream> = if mark_split {
      self.split_words()
    } else {
      vec![self.raw.clone()]
    };

    if self.noglob || shopt!(set.noglob) {
      return Ok(words.into_iter().map(|w| w.into_bytes().into()).collect());
    }

    let mut glob_words: Vec<VarStr> = Vec::with_capacity(words.len());

    for word in words {
      let pattern_bytes = escape::markers_to_glob_escapes(&word);
      let literal: VarStr = word.into_bytes().into();

      if !glob::might_be_glob(&pattern_bytes) {
        glob_words.push(literal);
        continue;
      }

      let expansions = glob::expand_glob(&pattern_bytes)
        .into_iter()
        .map(VarStr::from);

      glob_words.extend(expansions);
    }

    Ok(glob_words)
  }
  pub fn expand_no_side_effects(&mut self) -> ShResult<VarStr> {
    self.allow_side_effects = false;
    let raw = self.expand_inner(false)?;
    Ok(raw.into_bytes().into())
  }
  pub fn expand_no_split(&mut self) -> ShResult<VarStr> {
    let raw = self.expand_inner(false)?;
    Ok(raw.into_bytes().into())
  }
  pub fn expand_keep_quotes(&mut self) -> ShResult<VarStr> {
    let raw = self.expand_inner(false)?;
    let mut out: Vec<u8> = Vec::new();
    let mut cursor = raw.cursor();
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
  pub fn expand_for_glob(&mut self) -> ShResult<VarStr> {
    let raw = self.expand_inner(false)?;
    Ok(escape::markers_to_glob_escapes(&raw).into())
  }
  pub fn expand_inner(&mut self, mark_split: bool) -> ShResult<stream::SegStream> {
    let mut cursor = self.raw.cursor();
    self.raw = expand_raw_inner(&mut cursor, self.allow_side_effects, mark_split)?;

    Ok(self.raw.clone())
  }
  /// Perform POSIX word splitting.
  ///
  /// Resolves escapes and the special `$@`/`$*` cases, and performs IFS field
  /// splitting, but only inside `EXPAND_START`/`EXPAND_END` runs.
  pub fn split_words(&mut self) -> Vec<stream::SegStream> {
    use stream::{Marker, Quote, SegStream, StreamSeg, Unit};
    let mut words: Vec<SegStream> = vec![];
    let mut cursor = self.raw.cursor();
    let mut cur_word = SegStream::new();
    let mut was_quoted = false;
    let ifs = state::util::get_separators();
    // Delimiter-run tracking: whitespace and non-whitespace IFS chars combine
    // into one run that delimits a single field. A second non-WS IFS in the
    // same run emits an additional empty field (per POSIX step 5).
    let mut in_delim_run = false;
    let mut delim_has_non_ws = false;

    let mut expansion_depth = 0;

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
              words.push(SegStream::new());
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
    words.retain(|w| !matches!(w.stream(), [StreamSeg::Mark(Marker::NullExpand)]));
    for w in &mut words {
      if w
        .stream()
        .iter()
        .any(|s| matches!(s, StreamSeg::Mark(Marker::NullExpand)))
      {
        *w = w.without_marker(Marker::NullExpand);
      }
    }
    words
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::VecDeque;

  // These tests build an `Expander.raw` from a marker-char string; convert it
  // to the byte-native `SegStream` (marker chars → real markers).
  #[allow(dead_code)]
  mod markers {
    pub const DUB_QUOTE: char = '\u{fdd0}';
    pub const SNG_QUOTE: char = '\u{fdd1}';
    pub const NULL_EXPAND: char = '\u{fdd5}';
    pub const ARG_SEP: char = '\u{fdd6}';
    pub const SUBSH: char = '\u{fdd7}';
    pub const ESCAPE: char = '\u{fdd9}';
    pub const EXPAND_START: char = '\u{fde1}';
    pub const EXPAND_END: char = '\u{fde2}';
  }
  fn to_segstream(s: &str) -> stream::SegStream {
    use stream::{Marker, ProcSubKind, Quote};
    let mut seg = stream::SegStream::new();
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
        _ => None,
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
  fn render(seg: &stream::SegStream) -> String {
    use stream::{Marker, ProcSubKind, Quote, StreamSeg};
    let mut out = String::new();
    for s in seg.stream() {
      match s {
        StreamSeg::Bytes(b) => out.push_str(&String::from_utf8_lossy(b)),
        StreamSeg::Mark(m) => out.push(match m {
          Marker::Quote(Quote::Double) => '\u{fdd0}',
          Marker::Quote(Quote::Single) => '\u{fdd1}',
          Marker::TildeSub => '\u{fdd2}',
          Marker::ProcSub(ProcSubKind::In) => '\u{fdd3}',
          Marker::ProcSub(ProcSubKind::Out) => '\u{fdd4}',
          Marker::NullExpand => '\u{fdd5}',
          Marker::ArgSep => '\u{fdd6}',
          Marker::Subshell => '\u{fdd7}',
          Marker::VarSub => '\u{fdd8}',
          Marker::Escape => '\u{fdd9}',
          Marker::ExpandStart => '\u{fde1}',
          Marker::ExpandEnd => '\u{fde2}',
        }),
      }
    }
    out
  }

  use crate::state::{
    Shed,
    vars::{ArrIndex, VarFlags, VarKind, VarStr},
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
    let mut exp = Expander {
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["hello", "world", "foo"]);
  }

  #[test]
  fn word_split_custom_ifs() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::empty())).unwrap();

    let raw = format!("{}a:b:c{}", markers::EXPAND_START, markers::EXPAND_END);
    let mut exp = Expander {
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["a", "b", "c"]);
  }

  #[test]
  fn word_split_empty_ifs() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(VarStr::default()), VarFlags::empty()))
      .unwrap();

    // Even as expansion output, an empty IFS suppresses all field splitting.
    let raw = format!(
      "{}hello world{}",
      markers::EXPAND_START,
      markers::EXPAND_END
    );
    let mut exp = Expander {
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["hello world"]);
  }

  #[test]
  fn word_split_quoted_no_split() {
    let _guard = TestGuard::new();

    let raw = format!("{}hello world{}", markers::DUB_QUOTE, markers::DUB_QUOTE);
    let mut exp = Expander {
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["hello world"]);
  }

  // ===================== Escaped Word Splitting =====================

  #[test]
  fn word_split_escaped_space() {
    let _guard = TestGuard::new();

    let raw = format!("hello{}world", render(&unescape_str(b"\\ ")));
    let mut exp = Expander {
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: true,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.expand().unwrap();
    assert_eq!(words, vec!["hello world"]);
  }

  #[test]
  fn word_split_escaped_tab() {
    let _guard = TestGuard::new();

    let raw = format!("hello{}world", render(&unescape_str(b"\\\t")));
    let mut exp = Expander {
      allow_side_effects: true,
      raw: to_segstream(&raw),
      noglob: true,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.expand().unwrap();
    assert_eq!(words, vec!["hello\tworld"]);
  }

  #[test]
  fn word_split_escaped_custom_ifs() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::empty())).unwrap();

    let raw = format!("a{}b:c", render(&unescape_str(b"\\:")));
    let mut exp = Expander {
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
    assert_eq!(words, vec!["a:b:c"]);
  }

  // ===================== Array Indexing (TestGuard) =====================

  #[test]
  fn array_index_first() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "arr",
        VarKind::arr(["a", "b", "c"].map(VarStr::from)),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let val = Shed::vars(|v| v.index_var("arr", &ArrIndex::Literal(0))).unwrap();
    assert_eq!(val, "a");
  }

  #[test]
  fn array_index_second() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "arr",
        VarKind::arr(["x", "y", "z"].map(VarStr::from)),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let val = Shed::vars(|v| v.index_var("arr", &ArrIndex::Literal(1))).unwrap();
    assert_eq!(val, "y");
  }

  #[test]
  fn array_all_elems() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "arr",
        VarKind::arr(["a", "b", "c"].map(VarStr::from)),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let elems = Shed::vars(|v| v.try_get_arr_elems("arr")).unwrap();
    assert_eq!(elems, vec!["a", "b", "c"]);
  }

  #[test]
  fn array_elem_count() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "arr",
        VarKind::arr(["a", "b", "c"].map(VarStr::from)),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let elems = Shed::vars(|v| v.try_get_arr_elems("arr")).unwrap();
    assert_eq!(elems.len(), 3);
  }

  // ===================== Direct Input Tests (TestGuard) =====================

  #[test]
  fn index_simple() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "arr",
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
        "arr",
        VarKind::Arr(VecDeque::from(["foo".into(), "bar".into(), "biz".into()])),
        VarFlags::empty(),
      )
    })
    .unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "i",
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
