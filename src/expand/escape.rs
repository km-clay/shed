use std::fmt::Write;
use std::iter::Peekable;
use std::ops::Range;
use std::str::Chars;

use bitflags::bitflags;
use bstr::{ByteSlice, Bytes};
use smallvec::SmallVec;

use crate::{eval::lex, procio::RedirType, state::vars::VarStr, util};

use super::{QuoteState, ShResult, markers, match_loop, sherr, try_var, util::is_var_name_ch};

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  struct ExpandFlags: u8 {
    const TILDE    = 1 << 0;  // `~`, `~user`, `~uid`
    const SUBSHELL = 1 << 1;  // bare `(...)` as a substitution
    const VAR      = 1 << 2;  // `$var`, `${var}`, `$'...'`
    const CMDSUB   = 1 << 3;  // `$(...)`, backticks
    const ANSI_STR = 1 << 4;  // ANSI-C quoting (`$'...'`)
    const PROCSUB  = 1 << 5;  // `<(...)`, `>(...)`
    const QUOTE    = 1 << 6;  // single/double quote sub-machines
  }
}

impl ExpandFlags {
  const WORD: Self = Self::all();

  const HEREDOC: Self = Self::VAR.union(Self::CMDSUB);

  const PROMPT: Self = Self::VAR.union(Self::CMDSUB);

  /// Word expansion minus bare `(...)` subshell recognition. Used for the
  /// pattern and replacement operands of `${var#pat}`, `${var%pat}`,
  /// `${var/pat/rep}`, ... where a literal `(` must reach the glob matcher
  /// instead of being consumed as a subshell. `$(...)` and `$var` still
  /// expand (CMDSUB/VAR stay on).
  const PATTERN: Self = Self::WORD.difference(Self::SUBSHELL);
}

/// Strip ESCAPE markers from a string, leaving the characters they protect intact.
pub(super) fn strip_escape_markers(s: &mut VarStr) {
  let s_str = s.to_str_lossy();
  if !s_str.contains(markers::ESCAPE) {
    return;
  }

  *s = s_str.replace(markers::ESCAPE, "").into();
}

/// Strip ESCAPE markers from a string, leaving the characters they protect intact.
pub(super) fn strip_escape_markers_str(s: &mut String) {
  if !s.contains(markers::ESCAPE) {
    return;
  }

  *s = s.replace(markers::ESCAPE, "");
}

/// Convert internal quote/escape markers into glob-syntax for `glob::Pattern`.
pub(super) fn markers_to_glob_escapes(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut chars = s.chars();
  while let Some(c) = chars.next() {
    match c {
      markers::ESCAPE => {
        if let Some(next) = chars.next() {
          push_glob_literal(&mut out, next);
        }
      }
      markers::DUB_QUOTE | markers::SNG_QUOTE => {
        let closer = c;
        while let Some(inner) = chars.next() {
          if inner == closer {
            break;
          }
          if inner == markers::ESCAPE {
            if let Some(next) = chars.next() {
              push_glob_literal(&mut out, next);
            }
            continue;
          }
          push_glob_literal(&mut out, inner);
        }
      }
      _ => out.push(c),
    }
  }
  out
}

pub fn escape_glob(raw: &str, use_markers: bool) -> String {
  let esc_ch = if use_markers { markers::ESCAPE } else { '\\' };
  let mut out = String::new();
  let mut chars = raw.chars();
  match_loop!(chars.next() => ch, {
    _ if ch == esc_ch => {
      if let Some(nch) = chars.next() {
        out.push_str(&glob::Pattern::escape(&nch.to_string()));
      }
    }
    _ => out.push(ch),
  });

  out
}

/// Push `c` to `out` as a literal glob character, using a bracket expression
/// to escape glob metas since `glob::Pattern` doesn't recognize `\x` escapes.
fn push_glob_literal(out: &mut String, c: char) {
  if matches!(c, '*' | '?' | '[') {
    out.push('[');
    out.push(c);
    out.push(']');
  } else {
    out.push(c);
  }
}

/// Install internal marker characters for substitution, quoting, escape, etc.,
fn unescape_with(raw: &str, flags: ExpandFlags) -> String {
  if !raw.bytes().any(|b| {
    matches!(
      b,
      b'~' | b'\\' | b'(' | b'"' | b'\'' | b'`' | b'<' | b'>' | b'$'
    )
  }) {
    return raw.to_string();
  }

  let mut chars = raw.chars().peekable();
  let mut result = String::new();

  let (word_breaks, mut last_was_word_break, mut first_char) = if flags.contains(ExpandFlags::TILDE)
  {
    let wb = try_var!("COMP_WORDBREAKS").unwrap_or("\"'><=;|&(: ".into());
    let ifs = try_var!("IFS").unwrap_or(" \t\n".into());
    (format!("{wb}{ifs}"), false, true)
  } else {
    (String::new(), false, false)
  };

  // Depth inside a `${...}` parameter expansion. A bare `(` inside the body is
  // part of a pattern/operand (e.g. `${v%(x)}`, `${v/x/(y)}`), not a
  // subshell — keep it literal so it reaches the glob matcher. `$(...)`
  // cmdsubs still work (handled by the CMDSUB arm, which consumes its own
  // parens), and nested `${...}` increments this counter.
  let mut param_depth: u32 = 0;

  while let Some(ch) = chars.next() {
    match ch {
      // An existing ESCAPE marker (from a prior unescape pass,
      // e.g. the marker-encoded operand of a parameter expansion)
      // means the next char is literal
      markers::ESCAPE => {
        result.push(markers::ESCAPE);
        if let Some(next_ch) = chars.next() {
          result.push(next_ch);
        }
      }
      // An existing quote-region marker from a prior unescape pass.
      // Its contents were already processed, so copy the whole region
      // verbatim rather than re-scanning it
      markers::DUB_QUOTE | markers::SNG_QUOTE => {
        let closer = ch;
        result.push(ch);
        for inner in chars.by_ref() {
          result.push(inner);
          if inner == closer {
            break;
          }
        }
      }
      '~' if flags.contains(ExpandFlags::TILDE) && (last_was_word_break || first_char) => {
        result.push(markers::TILDE_SUB);
      }
      '\\' => {
        if let Some(next_ch) = chars.next() {
          result.push(markers::ESCAPE);
          result.push(next_ch);
        }
      }
      '"' if flags.contains(ExpandFlags::QUOTE) || param_depth > 0 => {
        read_dub_quote(&mut chars, &mut result);
      }
      '\'' if flags.contains(ExpandFlags::QUOTE) || param_depth > 0 => {
        read_sng_quote(&mut chars, &mut result);
      }
      '`' if flags.contains(ExpandFlags::CMDSUB) => read_backtick(&mut chars, &mut result, false),
      '<' if flags.contains(ExpandFlags::PROCSUB) && chars.peek() == Some(&'(') => {
        read_proc_sub_in(&mut chars, &mut result);
      }
      '>' if flags.contains(ExpandFlags::PROCSUB) && chars.peek() == Some(&'(') => {
        read_proc_sub_out(&mut chars, &mut result);
      }
      '$' if flags.contains(ExpandFlags::CMDSUB) && chars.peek() == Some(&'(') => {
        result.push(markers::VAR_SUB);
        chars.next();
        read_subsh(&mut chars, &mut result);
      }
      '$' if flags.contains(ExpandFlags::VAR) && chars.peek() == Some(&'\'') => {
        chars.next();
        result.push(markers::SNG_QUOTE);
        expand_dollar_quote(&mut chars, &mut result);
        result.push(markers::SNG_QUOTE);
      }
      '$' if flags.intersects(ExpandFlags::VAR.union(ExpandFlags::CMDSUB)) => {
        read_varsub(&mut chars, &mut result);
        // `${` opens a parameter expansion; track depth so bare `(` inside the
        // body stays literal.
        if chars.peek() == Some(&'{') {
          chars.next();
          result.push('{');
          param_depth = param_depth.saturating_add(1);
        }
      }
      // `}` closes the innermost `${...}` body.
      '}' if param_depth > 0 => {
        result.push('}');
        param_depth = param_depth.saturating_sub(1);
      }
      // Bare `(...)` as a substitution — only in word context, and only when
      // not inside a `${...}` (where a bare `(` is a literal pattern char).
      '(' if flags.contains(ExpandFlags::SUBSHELL) && param_depth == 0 => {
        read_subsh(&mut chars, &mut result);
      }
      _ => result.push(ch),
    }
    if flags.contains(ExpandFlags::TILDE) {
      last_was_word_break = word_breaks.contains(ch);
      first_char = false;
    }
  }

  result
}

/// Full word-context unescape: all substitutions, quote sub-machines, tildes,
/// process subs, escapes. Used by the main expansion pipeline.
pub fn unescape_str(raw: &str) -> String {
  unescape_with(raw, ExpandFlags::WORD)
}

/// Like `unescape_str` but for the pattern/replacement operand of a parameter
/// expansion (`${var#pat}`, `${var%pat}`, `${var/pat/rep}`, ...): a bare `(` is
/// a literal character, not a subshell. `$(...)`, `$var`, quotes, etc. still
/// expand as in word context.
pub(crate) fn unescape_pattern(raw: &str) -> String {
  unescape_with(raw, ExpandFlags::PATTERN)
}

/// Prompt-context unescape: $var, ${var}, $(cmd), backticks. No quote handling,
/// no tildes, no procsubs, no bare subshells. Used by `prompt.substitute`.
pub fn unescape_prompt(raw: &str) -> String {
  unescape_with(raw, ExpandFlags::PROMPT)
}

fn read_varsub(chars: &mut Peekable<Chars>, result: &mut String) -> bool {
  if chars
    .peek()
    .is_none_or(|ch| *ch != '$' && *ch != '(' && *ch != '{' && !is_var_name_ch(*ch))
  {
    result.push('$');
  } else {
    result.push(markers::VAR_SUB);
    if chars.peek().is_some_and(|ch| *ch == '$') {
      chars.next();
      result.push('$');
      return false;
    }
  }
  true
}

fn read_subsh(chars: &mut Peekable<Chars>, result: &mut String) {
  result.push(markers::SUBSH);
  // `chars` sits just after `$(`. Delimit the body with the lexer's
  // case-aware subshell scanner
  let rest: String = chars.clone().collect();
  if let Some(close) = lex::scan_cmd_sub_body(&rest) {
    result.push_str(&rest[..close]);
    result.push(markers::SUBSH);
    for _ in 0..rest[..=close].chars().count() {
      chars.next();
    }
    return;
  }

  let mut paren_count = 1;
  let mut qt = QuoteState::default();
  match_loop!(chars.next() => ch, {
    '\\' => {
      result.push(ch);
      if let Some(next_ch) = chars.next() {
        result.push(next_ch);
      }
    }
    '\'' => {
      qt.toggle_single();
      result.push(ch);
    }
    '"' if !qt.in_single() => {
      qt.toggle_double();
      result.push(ch);
    }
    _ if qt.in_quote() => result.push(ch),
    '(' => {
      paren_count += 1;
      result.push(ch);
    }
    ')' => {
      paren_count -= 1;
      if paren_count == 0 {
        result.push(markers::SUBSH);
        break;
      }
      result.push(ch);
    }
    _ => result.push(ch),
  });
}

fn read_sng_quote(chars: &mut Peekable<Chars>, result: &mut String) {
  result.push(markers::SNG_QUOTE);
  match_loop!(chars.next() => q_ch, {
    '\'' => {
      result.push(markers::SNG_QUOTE);
      break;
    }
    _ => result.push(q_ch),
  });
}

fn read_dub_quote(chars: &mut Peekable<Chars>, result: &mut String) {
  result.push(markers::DUB_QUOTE);

  // the current depth of '${...}' expansions
  let mut param_depth: u32 = 0;

  match_loop!(chars.next() => q_ch, {
    '\\' => {
      if let Some(next_ch) = chars.next() {
        match next_ch {
          '"' | '\\' | '`' | '$' | '!' => {
            // discard the backslash
          }
          '}' | '/' if param_depth > 0 => {
            // `}` (the `${...}` closer) and `/` (the `${v/pat/rep}` separator)
            // are detected by char-driven scans downstream, so a backslash
            // escape on one inside a parameter expansion must be kept as an
            // ESCAPE marker rather than neutralized by de-marking.
            result.push(markers::ESCAPE);
          }
          _ => {
            result.push(q_ch);
          }
        }
        result.push(next_ch);
      }
    }
    '$' if chars.peek() == Some(&'\'') => {
      if param_depth > 0 {
        // Inside a `${...}`, `$'...'` ANSI-C quoting is active even within the
        // outer double quotes (matching bash). Consume the whole region so its
        // closing `'` isn't mistaken for a bare single-quote opener by the
        // `'` arm below.
        chars.next();
        result.push(markers::SNG_QUOTE);
        expand_dollar_quote(chars, result);
        result.push(markers::SNG_QUOTE);
      } else {
        result.push(q_ch);
        let sng_quote = chars.next().unwrap();
        result.push(sng_quote);
      }
    }
    '$' => {
      if read_varsub(chars, result) {
        if chars.peek() == Some(&'{') {
          chars.next();
          result.push('{');
          param_depth = param_depth.saturating_add(1);
        } else if chars.peek() == Some(&'(') {
          chars.next();
          read_subsh(chars, result);
        }
      }
    }
    '}' if param_depth > 0 => {
      result.push('}');
      param_depth = param_depth.saturating_sub(1);
    }
    '\'' if param_depth > 0 => read_sng_quote(chars, result),
    '`' => read_backtick(chars, result, true),
    '"' if param_depth > 0 => read_dub_quote(chars, result),
    '"' => {
      result.push(markers::DUB_QUOTE);
      break;
    }
    _ => result.push(q_ch),
  });
}

enum Quote {
  Single,
  Double,
}

enum ProcSubKind {
  In,
  Out,
}

enum StreamSeg {
  Bytes(SmallVec<[u8; 32]>),
  Subsh,
  VarSub,
  Escape,
  Reset,
  TildeSub,
  Quote(Quote),
  ProcSub(ProcSubKind),
  NullExpand,
  ArgSep,
  ExpandStart,
  ExpandEnd,
}

pub fn expand_ansi_c(s: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  expand_ansi_c_stream(&mut s.bytes().peekable(), &mut out, None);
  out
}

pub fn expand_dollar_quote(chars: &mut Peekable<Bytes>, out: &mut Vec<u8>) {
  expand_ansi_c_stream(chars, out, Some(b'\''));
}

pub fn expand_ansi_c_stream(
  chars: &mut Peekable<Bytes>,
  out: &mut Vec<u8>,
  terminator: Option<u8>,
) {
  let mut pending: Vec<u8> = Vec::new();
  macro_rules! flush {
    () => {
      if !pending.is_empty() {
        out.append(&mut pending);
      }
    };
  }

  match_loop!(chars.next() => q_ch, {
    c if Some(c) == terminator => break,
    b'\\' if let Some(esc) = chars.next() => {
      match esc {
        // byte-producing escapes: keep buffering so adjacent ones combine.
        b'x' => read_hex(chars, &mut pending),
        b'o' => read_octal(chars, &mut pending, None),
        _ if esc.is_ascii_digit() => read_octal(chars, &mut pending, Some(esc)),
        // everything else emits text. flush any pending byte run first.
        _ => {
          flush!();
          match esc {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'"' => out.push(b'"'),
            b'\'' => out.push(b'\''),
            b'\\' => out.push(b'\\'),
            b'a' => out.push(b'\x07'),
            b'b' => out.push(b'\x08'),
            b'c' => read_stty_escape(chars, out),
            b'e' | b'E' => out.push(b'\x1b'),
            b'f' => out.push(b'\x0c'),
            b'v' => out.push(b'\x0b'),
            b'u' | b'U' => read_unicode(chars, out, esc),
            _ => {
              out.push(b'\\');
              out.push(esc);
            }
          }
        }
      }
    }
    _ => {
      flush!();
      out.push(q_ch);
    }
  });
  flush!();
}

pub fn read_unicode(chars: &mut Peekable<Bytes>, result: &mut Vec<u8>, marker: u8) {
  let mut hex = vec![];
  let max = match marker {
    b'u' => 4,
    b'U' => 8,
    _ => unreachable!("read_unicode called with non-unicode marker"),
  };

  while hex.len() < max
    && chars
      .peek()
      .is_some_and(|c| (*c as char).is_ascii_hexdigit())
  {
    hex.push(chars.next().unwrap());
  }

  if let Some(ch) = u32::from_str_radix(&hex.to_str_lossy(), 16)
    .ok()
    .and_then(char::from_u32)
  {
    let mut buf = [0u8; 4];
    result.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
  } else {
    // empty or invalid, just push it literally
    result.push(b'\\');
    result.push(marker);
  }
}

pub fn read_stty_escape(chars: &mut Peekable<Bytes>, result: &mut Vec<u8>) {
  let mut peeker = chars.clone();

  let Some(first) = peeker.next() else {
    result.push(b'\\');
    result.push(b'c');
    return;
  };

  let (target, consume_count) = if first == b'\\' {
    let Some(second) = peeker.next() else {
      result.push(b'\\');
      result.push(b'c');
      return;
    };
    if second != b'\\' {
      result.push(b'\\');
      result.push(b'c');
      return;
    }
    (b'\\', 2)
  } else {
    (first, 1)
  };

  let upper = target.to_ascii_uppercase();
  if !matches!(upper, b'@'..=b'_' | b'?') {
    result.push(b'\\');
    result.push(b'c');
    return;
  }

  for _ in 0..consume_count {
    chars.next();
  }

  // fun fact: all of the ascii control chars are exactly
  // the printable ascii chars with the high bit cleared.
  // so if we xor this char by 0x40, we automagically get our
  // control character
  let code = (upper as u8) ^ 0x40;
  result.push(code);
}

pub fn read_octal(chars: &mut Peekable<Bytes>, result: &mut Vec<u8>, first: Option<u8>) {
  let mut oct = vec![];
  if let Some(first) = first {
    oct.push(first);
  }
  for _ in 0..3 {
    if let Some(o) = chars.peek() {
      if (*o as char).is_digit(8) {
        oct.push(*o);
        chars.next();
      } else {
        break;
      }
    } else {
      break;
    }
  }
  if let Ok(byte) = u8::from_str_radix(&oct.to_str_lossy(), 8) {
    result.push(byte);
  } else {
    result.extend_from_slice(b"\\o");
    result.extend_from_slice(oct.as_bytes());
  }
}

pub fn read_hex(chars: &mut Peekable<Bytes>, result: &mut Vec<u8>) {
  let mut hex = vec![];
  if let Some(h1) = chars.next() {
    hex.push(h1);
  } else {
    result.extend_from_slice(b"\\x");
    return;
  }
  if let Some(h2) = chars.next() {
    hex.push(h2);
  } else {
    result.extend_from_slice(b"\\x");
    result.extend_from_slice(hex.as_bytes());
    return;
  }
  if let Ok(byte) = u8::from_str_radix(&hex.to_str_lossy(), 16) {
    result.push(byte);
  } else {
    result.extend_from_slice(b"\\x");
    result.extend_from_slice(hex.as_bytes());
  }
}

fn read_proc_sub_in(chars: &mut Peekable<Chars>, result: &mut String) {
  read_proc_sub(chars, result, false);
}

fn read_proc_sub_out(chars: &mut Peekable<Chars>, result: &mut String) {
  read_proc_sub(chars, result, true);
}

fn read_proc_sub(chars: &mut Peekable<Chars>, result: &mut String, input: bool) {
  let marker = if input {
    markers::PROC_SUB_IN
  } else {
    markers::PROC_SUB_OUT
  };
  chars.next();
  let mut paren_count = 1;
  result.push(marker);
  match_loop!(chars.next() => subsh_ch, {
    '\\' => {
      result.push(subsh_ch);
      if let Some(next_ch) = chars.next() {
        result.push(next_ch);
      }
    }
    '$' if chars.peek() == Some(&'\'') => {
      result.push(subsh_ch);
    }
    '(' => {
      result.push(subsh_ch);
      paren_count += 1;
    }
    ')' => {
      paren_count -= 1;
      if paren_count <= 0 {
        result.push(marker);
        break;
      }
      result.push(subsh_ch);
    }
    _ => result.push(subsh_ch),
  });
}

fn read_backtick(chars: &mut Peekable<Chars>, result: &mut String, in_dquote: bool) {
  result.push(markers::VAR_SUB);
  result.push(markers::SUBSH);
  match_loop!(chars.next() => bt_ch, {
    '\\' => {
      // only push backslash for double quotes if we are already in double quotes
      // this is some weird posix corner case
      match chars.peek() {
        Some('`' | '$' | '\\') => result.push(chars.next().unwrap()),
        Some('"') if in_dquote => result.push(chars.next().unwrap()),
        _ => result.push(bt_ch),
      }
    }
    // fun fact: this one match arm allows us to parse backtick statements nested in regular command subs inside of other backtick statements.
    // Not even zsh's parser handles this case
    '$' if chars.peek() == Some(&'(') => {
      chars.next();
      result.push_str("$(");
      let mut paren_count = 1;
      match_loop!(chars.next() => subsh_ch, {
        '\\' => {
          result.push(subsh_ch);
          if let Some(next_ch) = chars.next() {
            result.push(next_ch);
          }
        }
        '(' => {
          paren_count += 1;
          result.push(subsh_ch);
        }
        ')' => {
          paren_count -= 1;
          result.push(subsh_ch);
          if paren_count == 0 {
            break;
          }
        }
        _ => result.push(subsh_ch),
      });
    }
    '`' => {
      result.push(markers::SUBSH);
      log::debug!("Finished reading backtick: {result}");
      break;
    }
    _ => result.push(bt_ch),
  });
}

/// Heredoc body: $var / ${var} / $(cmd) / backticks only. Quotes, tildes,
/// globs, process subs, and bare subshells all pass through as literal text.
pub fn unescape_heredoc(raw: &str) -> String {
  unescape_with(raw, ExpandFlags::HEREDOC)
}

pub fn escape_str(raw: &str, use_marker: bool) -> String {
  escape_str_bounded(raw, use_marker, None)
}

/// Opposite of `unescape_str`, escapes a string to be executed as literal text
/// Used for completion results, and glob filename matches.
///
/// if `use_marker` is true, it will check for `markers::ESCAPE` instead of a literal backslash.
/// if a bound (something like 0..5) is provided, the escaping logic will be limited to those bytes
/// this is mainly used for escaping the region of text that is changed during completion
pub fn escape_str_bounded(raw: &str, use_marker: bool, bound: Option<&Range<usize>>) -> String {
  let mut result = String::new();
  let mut chars = raw.char_indices();
  let esc_ch = if use_marker { markers::ESCAPE } else { '\\' };

  while let Some((i, ch)) = chars.next() {
    if let Some(bound) = &bound
      && !bound.contains(&i)
    {
      result.push(ch);
      continue;
    }

    match ch {
      '\'' | '"' | '\\' | '|' | '&' | ';' | '(' | ')' | '<' | '>' | '$' | '*' | '!' | '`' | '{'
      | '?' | '[' | '#' | ' ' | '\t' | '\n' => {
        result.push(esc_ch);
        result.push(ch);
      }
      '~' if result.is_empty() => {
        result.push(esc_ch);
        result.push(ch);
      }
      _ => {
        result.push(ch);
      }
    }
  }

  result
}

pub fn unescape_math(raw: &str) -> ShResult<String> {
  let mut chars = raw.chars().peekable();
  let mut result = String::new();
  let mut qt_state = QuoteState::default();

  match_loop!(chars.next() => ch, {
    '\\' => {
      if (!qt_state.in_single() || chars.peek().is_some_and(|&c| c == '\''))
      && let Some(next_ch) = chars.next() {
        result.push(next_ch);
      }
    }
    '"' => qt_state.toggle_double(),
    '\'' => qt_state.toggle_single(),
    _ if qt_state.in_single() => result.push(ch),
    '$' => {
      result.push(markers::VAR_SUB);
      if chars.peek() == Some(&'(') {
        result.push(markers::SUBSH);
        chars.next();
        let mut paren_count = 1;
        match_loop!(chars.next() => subsh_ch, {
          '\\' => {
            result.push(subsh_ch);
            if let Some(next_ch) = chars.next() {
              result.push(next_ch);
            }
          }
          '$' if chars.peek() != Some(&'(') => result.push(markers::VAR_SUB),
          '(' => {
            paren_count += 1;
            result.push(subsh_ch);
          }
          ')' => {
            paren_count -= 1;
            if paren_count == 0 {
              result.push(markers::SUBSH);
              break;
            }
            result.push(subsh_ch);
          }
          _ => result.push(subsh_ch),
        });
      }
    }
    _ if qt_state.in_double() => { result.push(ch); }
    _ => result.push(ch),
  });

  if !qt_state.outside() {
    return Err(sherr!(ParseErr, "Unmatched quote in arithmetic expression",));
  }

  Ok(result)
}

fn quote_fmt(
  s: &str,
  f: &mut impl std::fmt::Write,
  special_chars: &str,
  escape_ws_controls: bool,
) -> std::fmt::Result {
  // An empty string MUST be quoted, otherwise interpolating it into a command
  // line collapses into surrounding whitespace and the arg is silently dropped.
  if s.is_empty() {
    return write!(f, "''");
  }

  let has_hard_control = s
    .chars()
    .any(|c| c.is_ascii_control() && c != '\n' && c != '\t');
  let has_ws_control = s.chars().any(|c| c == '\n' || c == '\t');
  let has_special = s.chars().any(|c| special_chars.contains(c));

  if has_hard_control || (has_ws_control && escape_ws_controls) {
    // $'...' ANSI-C quoting: backslashes and all special chars must be escaped
    write!(f, "$'")?;
    for ch in s.chars() {
      match ch {
        '\\' => write!(f, "\\\\")?,
        '\'' => write!(f, "\\'")?,
        '\n' => write!(f, "\\n")?,
        '\r' => write!(f, "\\r")?,
        '\t' => write!(f, "\\t")?,
        '\x07' => write!(f, "\\a")?,
        '\x08' => write!(f, "\\b")?,
        '\x0B' => write!(f, "\\v")?,
        '\x0C' => write!(f, "\\f")?,
        c if c.is_ascii_control() => {
          let _ = write!(f, "\\x{:02x}", c as u8);
        }
        c => write!(f, "{c}")?,
      }
    }
    write!(f, "'")
  } else if has_special || has_ws_control {
    write!(f, "'")?;
    for ch in s.chars() {
      if ch == '\'' {
        write!(f, "'\\''")?;
      } else {
        write!(f, "{ch}")?;
      }
    }
    write!(f, "'")
  } else {
    write!(f, "{s}")
  }
}

pub fn xtrace_quote_fmt<W: std::fmt::Write>(s: &str, f: &mut W) -> std::fmt::Result {
  quote_fmt(s, f, r#" !*?$;|&<>(){}[]`'"\"#, false)
}

pub fn shell_quote_fmt<W: std::fmt::Write>(s: &str, f: &mut W) -> std::fmt::Result {
  quote_fmt(s, f, r#"\\!#$^*()=|{}[]`<>?~;& "'"#, true)
}

/// Quotes a string such that it can be round-tripped as shell syntax
pub fn shell_quote(s: &str) -> String {
  quote(s, shell_quote_fmt)
}

/// Quotes an xtrace argument
pub fn xtrace_quote(s: &str) -> String {
  quote(s, xtrace_quote_fmt)
}

/// Takes a generic quoting function and applies it to the given string
fn quote<S: AsRef<str>, F: Fn(&str, &mut String) -> std::fmt::Result>(s: S, f: F) -> String {
  let s_str = s.as_ref();
  let mut result = String::new();
  f(s_str, &mut result).unwrap();
  result
}

#[cfg(test)]
mod tests {
  use super::*;

  // ===================== unescape_str =====================

  #[test]
  fn unescape_backslash() {
    let result = unescape_str("hello\\nworld");
    let expected = format!("hello{}nworld", markers::ESCAPE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_tilde_at_start() {
    let result = unescape_str("~/foo");
    assert!(result.starts_with(markers::TILDE_SUB));
    assert!(result.ends_with("/foo"));
  }

  #[test]
  fn unescape_tilde_not_at_start() {
    let result = unescape_str("a~b");
    assert!(!result.contains(markers::TILDE_SUB));
    assert!(result.contains('~'));
  }

  #[test]
  fn unescape_dollar_becomes_var_sub() {
    let result = unescape_str("$foo");
    assert!(result.starts_with(markers::VAR_SUB));
    assert!(result.ends_with("foo"));
  }

  #[test]
  fn unescape_single_quotes() {
    let result = unescape_str("'hello'");
    let expected = format!("{}hello{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_double_quotes() {
    let result = unescape_str("\"hello\"");
    let expected = format!("{}hello{}", markers::DUB_QUOTE, markers::DUB_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_newline() {
    let result = unescape_str("$'\\n'");
    let expected = format!("{}\n{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_tab() {
    let result = unescape_str("$'\\t'");
    let expected = format!("{}\t{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_escape() {
    let result = unescape_str("$'\\e'");
    let expected = format!("{}\x1b{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_hex() {
    let result = unescape_str("$'\\x41'");
    let expected = format!("{}A{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_backslash() {
    let result = unescape_str("$'\\\\'");
    let expected = format!("{}\\{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  // ===================== shell_quote =====================

  #[test]
  fn display_simple_value_unquoted() {
    assert_eq!(shell_quote("hello"), "hello");
  }

  #[test]
  fn display_value_with_spaces_single_quoted() {
    assert_eq!(shell_quote("hello world"), "'hello world'");
  }

  #[test]
  fn display_backslash_no_escaping_in_single_quote_context() {
    // backslash not before ' - should not be doubled
    assert_eq!(shell_quote("\\@prompt "), "'\\@prompt '");
  }

  #[test]
  fn display_backslash_passthrough_inside_squotes() {
    assert_eq!(shell_quote("bar\\' biz"), "'bar\\'\\'' biz'");
  }

  #[test]
  fn display_single_quote_uses_posix_idiom() {
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
  }

  #[test]
  fn display_control_char_uses_ansi_c_quoting() {
    assert_eq!(shell_quote("foo\nbar"), "$'foo\\nbar'");
  }

  #[test]
  fn display_backslash_escaped_in_ansi_c_context() {
    assert_eq!(shell_quote("foo\\\nbar"), "$'foo\\\\\\nbar'");
  }

  #[test]
  fn display_tab_uses_ansi_c_quoting() {
    assert_eq!(shell_quote("foo\tbar"), "$'foo\\tbar'");
  }

  #[test]
  fn display_special_chars_single_quoted() {
    assert_eq!(shell_quote("$VAR"), "'$VAR'");
    assert_eq!(shell_quote("foo|bar"), "'foo|bar'");
    assert_eq!(shell_quote("foo&bar"), "'foo&bar'");
  }

  #[test]
  fn display_empty_string() {
    // Empty must be quoted so it survives whitespace collapsing when
    // interpolated into a command line.
    assert_eq!(shell_quote(""), "''");
  }
}

#[cfg(test)]
#[expect(non_snake_case)] // names preserve uppercase vs lowercase E
mod expand_ansi_c_tests {
  use super::expand_ansi_c;
  // ─── identity passthrough ─────────────────────────────────────────

  #[test]
  fn plain_text_unchanged() {
    assert_eq!(expand_ansi_c("hello world"), "hello world");
  }

  #[test]
  fn empty_string() {
    assert_eq!(expand_ansi_c(""), "");
  }

  // ─── named single-char escapes ───────────────────────────────────

  #[test]
  fn backslash_n_is_newline() {
    assert_eq!(expand_ansi_c("a\\nb"), "a\nb");
  }

  #[test]
  fn backslash_t_is_tab() {
    assert_eq!(expand_ansi_c("a\\tb"), "a\tb");
  }

  #[test]
  fn backslash_r_is_carriage_return() {
    assert_eq!(expand_ansi_c("a\\rb"), "a\rb");
  }

  #[test]
  fn backslash_a_is_bel() {
    assert_eq!(expand_ansi_c("\\a"), "\x07");
  }

  #[test]
  fn backslash_b_is_backspace() {
    assert_eq!(expand_ansi_c("\\b"), "\x08");
  }

  #[test]
  fn backslash_lower_e_is_escape() {
    assert_eq!(expand_ansi_c("\\e"), "\x1b");
  }

  #[test]
  fn backslash_upper_E_is_escape() {
    assert_eq!(expand_ansi_c("\\E"), "\x1b");
  }

  #[test]
  fn backslash_f_is_form_feed() {
    assert_eq!(expand_ansi_c("\\f"), "\x0c");
  }

  #[test]
  fn backslash_v_is_vertical_tab() {
    assert_eq!(expand_ansi_c("\\v"), "\x0b");
  }

  // ─── escaped quote and backslash ─────────────────────────────────

  #[test]
  fn backslash_single_quote_is_single_quote() {
    assert_eq!(expand_ansi_c("\\'"), "'");
  }

  #[test]
  fn backslash_backslash_is_single_backslash() {
    assert_eq!(expand_ansi_c("\\\\"), "\\");
  }

  // ─── \xNN — hex byte ─────────────────────────────────────────────

  #[test]
  fn hex_two_digits_decodes_byte() {
    assert_eq!(expand_ansi_c("\\x41"), "A");
  }

  #[test]
  fn hex_uppercase_digits() {
    assert_eq!(expand_ansi_c("\\xFF"), "\u{ff}");
  }

  #[test]
  fn hex_with_trailing_text() {
    // \x41 = 'A', then literal "BC"
    assert_eq!(expand_ansi_c("\\x41BC"), "ABC");
  }

  // ─── \oNNN — octal byte (with leading 'o') ──────────────────────

  #[test]
  fn octal_with_o_prefix() {
    // 'A' = octal 101
    assert_eq!(expand_ansi_c("\\o101"), "A");
  }

  // ─── \<digit>... — octal byte (no 'o' prefix) ────────────────────

  #[test]
  fn octal_digit_only() {
    assert_eq!(expand_ansi_c("\\101"), "A");
  }

  #[test]
  fn octal_short_form() {
    // \0 → null byte
    assert_eq!(expand_ansi_c("\\0"), "\0");
  }

  // ─── multibyte UTF-8 from consecutive byte escapes (#146) ────────
  //
  // Adjacent `\ooo`/`\xHH` bytes must reassemble into one character rather than
  // each byte being widened to its own code point.

  #[test]
  fn octal_bytes_reassemble_utf8() {
    // \342\234\224 = e2 9c 94 = U+2714 ✔
    assert_eq!(expand_ansi_c("\\342\\234\\224"), "\u{2714}");
    assert_eq!(expand_ansi_c("\\342\\234\\224").as_bytes(), b"\xe2\x9c\x94");
  }

  #[test]
  fn hex_bytes_reassemble_utf8() {
    assert_eq!(expand_ansi_c("\\xe2\\x9c\\x94"), "\u{2714}");
  }

  #[test]
  fn byte_escapes_flush_around_literal_text() {
    // 'A' + ✔ + 'B': byte run flushes at the literal boundaries.
    assert_eq!(expand_ansi_c("A\\342\\234\\224B"), "A\u{2714}B");
  }

  // ─── \c<char> — stty-style control char ──────────────────────────

  #[test]
  fn control_a() {
    assert_eq!(expand_ansi_c("\\cA"), "\x01"); // Ctrl+A
  }

  #[test]
  fn control_g_is_bel() {
    assert_eq!(expand_ansi_c("\\cG"), "\x07"); // Ctrl+G = BEL
  }

  #[test]
  fn control_lowercase_normalized_to_upper() {
    // \ca and \cA both produce Ctrl+A.
    assert_eq!(expand_ansi_c("\\ca"), "\x01");
  }

  #[test]
  fn control_question_mark_is_del() {
    assert_eq!(expand_ansi_c("\\c?"), "\x7f"); // DEL
  }

  #[test]
  fn control_invalid_target_preserves_literal() {
    // '0' is outside @..._ and isn't '?', so the escape isn't valid.
    // read_stty_escape pushes back "\\c" and leaves the '0' for the
    // outer loop to handle as a normal char.
    assert_eq!(expand_ansi_c("\\c0"), "\\c0");
  }

  // ─── unrecognized escape — preserves backslash ───────────────────

  #[test]
  fn unknown_escape_preserves_backslash() {
    assert_eq!(expand_ansi_c("\\z"), "\\z");
  }

  // ─── edge cases ──────────────────────────────────────────────────

  #[test]
  fn trailing_backslash_with_no_followup_kept() {
    // Bare `\` at end of string is kept as-is.
    assert_eq!(expand_ansi_c("foo\\"), "foo\\");
  }

  #[test]
  fn multiple_escapes_in_sequence() {
    assert_eq!(expand_ansi_c("\\t\\n\\r"), "\t\n\r");
  }

  #[test]
  fn mixed_escapes_and_literals() {
    assert_eq!(expand_ansi_c("line1\\nline2\\tcol2"), "line1\nline2\tcol2");
  }
}
