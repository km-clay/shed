use std::ops::Range;

use bitflags::bitflags;

use crate::{
  eval::lex,
  expand::{
    stream::{ProcSubKind, SegCursor},
    var,
  },
  match_loop, sherr, try_var,
  util::{
    error::ShResult,
    strops::{ByteCursor, QuoteState, SliceCursor},
  },
};

use super::stream::{Marker, Quote, SegStream, Unit};

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

/// Convert internal quote/escape markers into glob-syntax for `glob::Pattern`.
pub(super) fn markers_to_glob_escapes(s: &SegStream) -> Vec<u8> {
  let mut out = vec![];
  let mut cursor = s.cursor();
  while let Some(unit) = cursor.next() {
    match unit {
      // A raw literal backslash (e.g. the `\` in `"a\*b"`, where `\` is literal
      // in double quotes) must be escaped so the matcher's `\`-escape doesn't
      // read it as escaping the following byte. Unquoted `*`/`?`/`[` stay raw so
      // they still glob.
      Unit::Byte(b'\\') => out.extend_from_slice(b"\\\\"),
      Unit::Byte(b) => out.push(b),
      Unit::Mark(m) => match m {
        Marker::Escape => {
          if let Some(next) = cursor.next_byte() {
            push_glob_literal(&mut out, next);
          }
        }
        Marker::Quote(q) => {
          while let Some(inner) = cursor.next() {
            match inner {
              Unit::Mark(m) => match m {
                Marker::Quote(inner_q) if inner_q == q => break,
                Marker::Escape => {
                  if let Some(next) = cursor.next_byte() {
                    push_glob_literal(&mut out, next);
                  }
                }
                _ => {}
              },
              Unit::Byte(b) => {
                push_glob_literal(&mut out, b);
              }
            }
          }
        }
        _ => {}
      },
    }
  }
  out
}

/// Push `c` to `out` as a literal glob character, backslash-escaping the glob
/// metacharacters (and `\` itself) so a quoted/escaped meta reaches the matcher
/// as a literal rather than being interpreted (or, for `\`, eating the next
/// char as an escape).
fn push_glob_literal(out: &mut Vec<u8>, c: u8) {
  if matches!(c, b'*' | b'?' | b'[' | b']' | b'\\') {
    out.push(b'\\');
    out.push(c);
  } else {
    out.push(c);
  }
}

/// Install internal marker characters for substitution, quoting, escape, etc.,
fn unescape_with(stream: SegStream, flags: ExpandFlags) -> SegStream {
  if !stream.has_meta() {
    return stream;
  }

  let mut cursor = stream.cursor();
  let mut out = SegStream::new();

  let (word_breaks, mut last_was_word_break, mut first_char) = if flags.contains(ExpandFlags::TILDE)
  {
    let wb = try_var!("COMP_WORDBREAKS").unwrap_or("\"'><=;|&(: ".into());
    let ifs = try_var!("IFS").unwrap_or(" \t\n".into());
    (Some([wb.as_bytes(), ifs.as_bytes()].concat()), false, true)
  } else {
    (None, false, false)
  };

  // Depth inside a `${...}` parameter expansion.
  let mut param_depth: u32 = 0;

  while let Some(unit) = cursor.next() {
    match unit {
      Unit::Mark(mark) => match mark {
        Marker::Escape => {
          out.push(unit);
          if let Some(n_unit) = cursor.next() {
            out.push(n_unit);
          }
        }
        Marker::Quote(quote) => {
          out.push(unit);
          while let Some(inner_unit) = cursor.next() {
            out.push(inner_unit);
            if let Unit::Mark(Marker::Quote(end)) = inner_unit
              && end == quote
            {
              break;
            }
          }
        }
        _ => { /* not handled */ }
      },
      Unit::Byte(byte) => match byte {
        b'~' if flags.contains(ExpandFlags::TILDE) && (last_was_word_break || first_char) => {
          out.push_marker(Marker::TildeSub);
        }
        b'\\' => {
          if let Some(esc_byte) = cursor.next_byte() {
            out.push_marker(Marker::Escape);
            out.push_byte(esc_byte);
          } else {
            out.push_byte(byte);
          }
        }
        b'"' if flags.contains(ExpandFlags::QUOTE) || param_depth > 0 => {
          read_dub_quote(&mut cursor, &mut out);
        }
        b'\'' if flags.contains(ExpandFlags::QUOTE) || param_depth > 0 => {
          read_sng_quote(&mut cursor, &mut out);
        }
        b'`' if flags.contains(ExpandFlags::CMDSUB) => {
          read_backtick(&mut cursor, &mut out, false);
        }
        dir @ (b'>' | b'<')
          if flags.contains(ExpandFlags::PROCSUB) && cursor.peek_byte() == Some(b'(') =>
        {
          let input = matches!(dir, b'<');
          read_proc_sub(&mut cursor, &mut out, input);
        }
        b'$' if flags.contains(ExpandFlags::CMDSUB) && cursor.peek_byte() == Some(b'(') => {
          out.push_marker(Marker::VarSub);
          cursor.next();
          read_subsh(&mut cursor, &mut out);
        }
        b'$' if flags.contains(ExpandFlags::VAR) && cursor.peek_byte() == Some(b'\'') => {
          cursor.next();
          out.push_marker(Marker::Quote(Quote::Single));
          expand_dollar_quote(&mut cursor, &mut out);
          out.push_marker(Marker::Quote(Quote::Single));
        }
        b'$' if flags.intersects(ExpandFlags::VAR.union(ExpandFlags::CMDSUB)) => {
          read_varsub(&mut cursor, &mut out);
          if cursor.peek_byte() == Some(b'{') {
            cursor.next();
            out.push_byte(b'{');
            param_depth = param_depth.saturating_add(1);
          }
        }
        b'}' if param_depth > 0 => {
          out.push_byte(b'}');
          param_depth = param_depth.saturating_sub(1);
        }
        b'(' if flags.contains(ExpandFlags::SUBSHELL) && param_depth == 0 => {
          read_subsh(&mut cursor, &mut out);
        }
        _ => out.push_byte(byte),
      },
    }
    if let Some(breaks) = &word_breaks
      && let Unit::Byte(b) = unit
      && flags.contains(ExpandFlags::TILDE)
    {
      last_was_word_break = breaks.contains(&b);
      first_char = false;
    }
  }

  out
}

/// Full word-context unescape: all substitutions, quote sub-machines, tildes,
/// process subs, escapes. Used by the main expansion pipeline.
pub(crate) fn unescape_str(raw: &[u8]) -> SegStream {
  unescape_with(SegStream::from_bytes(raw), ExpandFlags::WORD)
}

/// Like `unescape_str` but for the pattern/replacement operand of a parameter
/// expansion (`${var#pat}`, `${var%pat}`, `${var/pat/rep}`, ...): a bare `(` is
/// a literal character, not a subshell. `$(...)`, `$var`, quotes, etc. still
/// expand as in word context.
pub(crate) fn unescape_pattern(raw: SegStream) -> SegStream {
  unescape_with(raw, ExpandFlags::PATTERN)
}

/// Prompt-context unescape: $var, ${var}, $(cmd), backticks. No quote handling,
/// no tildes, no procsubs, no bare subshells. Used by `prompt.substitute`.
pub(crate) fn unescape_prompt(raw: &str) -> SegStream {
  unescape_with(SegStream::from_bytes(raw.as_bytes()), ExpandFlags::PROMPT)
}

fn read_varsub(stream: &mut SegCursor, out: &mut SegStream) -> bool {
  if stream
    .peek_byte()
    .is_none_or(|b| b != b'$' && b != b'(' && b != b'{' && !var::is_var_name_ch(b as char))
  {
    out.push_byte(b'$');
  } else {
    out.push_marker(Marker::VarSub);
    if stream.eat_byte(b'$') {
      out.push_byte(b'$');
      return false;
    }
  }
  true
}

fn read_subsh(stream: &mut SegCursor, out: &mut SegStream) {
  out.push_marker(Marker::Subshell);

  let mut peeker = *stream;
  let mut rest: Vec<u8> = vec![];
  while let Some(b) = peeker.next_byte() {
    rest.push(b);
  }

  if let Some(close) = lex::scan_cmd_sub_body(&rest) {
    out.push_bytes(&rest[..close]);
    out.push_marker(Marker::Subshell);
    for _ in 0..=close {
      stream.next();
    }
    return;
  }

  let mut paren_count = 1;
  let mut qt = QuoteState::default();
  match_loop!(stream.next() => unit, {
    Unit::Byte(b) => match b {
      b'\\' => {
        out.push_byte(b'\\');
        if let Some(next) = stream.next_byte() {
          out.push_byte(next);
        }
      }
      b'\'' if !qt.in_double() => {
        qt.toggle_single();
        out.push_byte(b'\'');
      }
      b'"' if !qt.in_single() => {
        qt.toggle_double();
        out.push_byte(b'"');
      }
      b if qt.in_quote() => out.push_byte(b),
      b'(' => {
        paren_count += 1;
        out.push_byte(b'(');
      }
      b')' => {
        paren_count -= 1;
        if paren_count == 0 {
          out.push_marker(Marker::Subshell);
          break;
        }
        out.push_byte(b')');
      }
      b => out.push_byte(b),
    }
    Unit::Mark(m) => out.push_marker(m),
  });
}

fn read_sng_quote(stream: &mut SegCursor, out: &mut SegStream) {
  out.push_marker(Marker::Quote(Quote::Single));
  match_loop!(stream.next_byte() => q_ch, {
    b'\'' => {
      out.push_marker(Marker::Quote(Quote::Single));
      break;
    }
    _ => out.push_byte(q_ch),
  });
}

fn read_dub_quote(stream: &mut SegCursor, out: &mut SegStream) {
  out.push_marker(Marker::Quote(Quote::Double));

  // the current depth of '${...}' expansions
  let mut param_depth: u32 = 0;

  match_loop!(stream.next() => q_unit, {
    Unit::Byte(byte) => match byte {
      b'\\' => {
        if let Some(next) = stream.next_byte() {
          match next {
            b'}' | b'/' | b'$' | b'`' if param_depth > 0 => {
              out.push_marker(Marker::Escape);
            }
            b'"' | b'\\' | b'`' | b'$' | b'!' => {
              // discard the backslash
            }
            _ => {
              out.push_byte(b'\\');
            }
          }
          out.push_byte(next);
        }
      }
      b'$' if stream.peek_byte() == Some(b'\'') => {
        if param_depth > 0 {
          stream.next();
          out.push_marker(Marker::Quote(Quote::Single));
          expand_dollar_quote(stream, out);
          out.push_marker(Marker::Quote(Quote::Single));
        } else {
          out.push_byte(b'$');
          stream.next();
          out.push_byte(b'\'');
        }
      }
      b'$' => {
        if read_varsub(stream, out) {
          if stream.eat_byte(b'{') {
            out.push_byte(b'{');
            param_depth = param_depth.saturating_add(1);
          } else if stream.eat_byte(b'(') {
            read_subsh(stream, out);
          }
        }
      }
      b'}' if param_depth > 0 => {
        out.push_byte(b'}');
        param_depth = param_depth.saturating_sub(1);
      }
      b'\'' if param_depth > 0 => {
        read_sng_quote(stream, out);
      }
      b'`' => {
        read_backtick(stream, out, true);
      }
      b'"' if param_depth > 0 => {
        read_dub_quote(stream, out);
      }
      b'"' => {
        out.push(Unit::Mark(Marker::Quote(Quote::Double)));
        break;
      }
      _ => out.push_byte(byte),
    }
    Unit::Mark(m) => out.push_marker(m),
  });
}

pub(crate) fn expand_ansi_c(s: &[u8]) -> Vec<u8> {
  let input = SegStream::from_bytes(s);
  let mut out = SegStream::new();
  expand_ansi_c_stream(&mut input.cursor(), &mut out, None);
  out.into_bytes()
}

pub(crate) fn expand_dollar_quote(chars: &mut SegCursor, out: &mut SegStream) {
  expand_ansi_c_stream(chars, out, Some(b'\''));
}

pub(crate) fn expand_ansi_c_stream(
  stream: &mut SegCursor,
  out: &mut SegStream,
  terminator: Option<u8>,
) {
  match_loop!(stream.next() => unit, {
    Unit::Byte(c) if Some(c) == terminator => break,
    Unit::Byte(b'\\') => match stream.next_byte() {
      Some(b'x') => read_hex(stream, out),
      Some(b'o') => read_octal(stream, out, None),
      Some(esc) if esc.is_ascii_digit() => read_octal(stream, out, Some(esc)),
      Some(esc) => match esc {
        b'n' => out.push_byte(b'\n'),
        b't' => out.push_byte(b'\t'),
        b'r' => out.push_byte(b'\r'),
        b'"' => out.push_byte(b'"'),
        b'\'' => out.push_byte(b'\''),
        b'\\' => out.push_byte(b'\\'),
        b'a' => out.push_byte(b'\x07'),
        b'b' => out.push_byte(b'\x08'),
        b'c' => read_stty_escape(stream, out),
        b'e' | b'E' => out.push_byte(b'\x1b'),
        b'f' => out.push_byte(b'\x0c'),
        b'v' => out.push_byte(b'\x0b'),
        b'u' | b'U' => read_unicode(stream, out, esc),
        _ => {
          out.push_byte(b'\\');
          out.push_byte(esc);
        }
      },
      None => out.push_byte(b'\\'),
    }
    Unit::Byte(b) => out.push_byte(b),
    Unit::Mark(m) => out.push_marker(m),
  });
}

pub(crate) fn read_unicode(stream: &mut SegCursor, out: &mut SegStream, marker: u8) {
  let mut hex: Vec<u8> = vec![];
  let max = match marker {
    b'u' => 4,
    b'U' => 8,
    _ => unreachable!("read_unicode called with non-unicode marker"),
  };

  while hex.len() < max {
    match stream.peek_byte() {
      Some(h) if h.is_ascii_hexdigit() => {
        hex.push(h);
        stream.next();
      }
      _ => break,
    }
  }

  if let Some(ch) = std::str::from_utf8(&hex)
    .ok()
    .and_then(|s| u32::from_str_radix(s, 16).ok())
    .and_then(char::from_u32)
  {
    let mut buf = [0u8; 4];
    out.push_bytes(ch.encode_utf8(&mut buf).as_bytes());
  } else {
    // empty or invalid, just push it literally
    out.push_byte(b'\\');
    out.push_byte(marker);
    out.push_bytes(&hex);
  }
}

pub(crate) fn read_stty_escape(stream: &mut SegCursor, out: &mut SegStream) {
  let mut peeker = *stream;

  let Some(first) = peeker.next_byte() else {
    out.push_bytes(b"\\c");
    return;
  };

  let (target, consume_count) = if first == b'\\' {
    if let Some(b'\\') = peeker.next_byte() {
      (b'\\', 2)
    } else {
      out.push_bytes(b"\\c");
      return;
    }
  } else {
    (first, 1)
  };

  let upper = target.to_ascii_uppercase();
  if !matches!(upper, b'@'..=b'_' | b'?') {
    out.push_bytes(b"\\c");
    return;
  }

  for _ in 0..consume_count {
    stream.next();
  }

  // fun fact: all of the ascii control chars are exactly
  // the printable ascii chars with the high bit cleared.
  // so if we xor this char by 0x40, we automagically get our
  // control character
  out.push_byte(upper ^ 0x40);
}

pub(crate) fn read_octal(stream: &mut SegCursor, out: &mut SegStream, first: Option<u8>) {
  let mut oct: Vec<u8> = vec![];
  if let Some(first) = first {
    oct.push(first);
  }
  for _ in 0..3 {
    match stream.peek_byte() {
      Some(o @ b'0'..=b'7') => {
        oct.push(o);
        stream.next();
      }
      _ => break,
    }
  }
  if let Some(byte) = std::str::from_utf8(&oct)
    .ok()
    .and_then(|s| u8::from_str_radix(s, 8).ok())
  {
    out.push_byte(byte);
  } else {
    out.push_bytes(b"\\o");
    out.push_bytes(&oct);
  }
}

pub(crate) fn read_hex(stream: &mut SegCursor, out: &mut SegStream) {
  let hex_val = |b: u8| (b as char).to_digit(16);
  let Some(d1) = stream.peek_byte().and_then(hex_val) else {
    out.push_bytes(b"\\x");
    return;
  };
  stream.next_byte();
  let mut value = d1 as u8;
  if let Some(d2) = stream.peek_byte().and_then(hex_val) {
    stream.next_byte();
    value = value * 16 + d2 as u8;
  }
  out.push_byte(value);
}

fn read_proc_sub(stream: &mut SegCursor, out: &mut SegStream, input: bool) {
  let kind = ProcSubKind::from(input);
  stream.next();
  let mut paren_count = 1;
  out.push_marker(Marker::ProcSub(kind));
  let mut qt = QuoteState::default();

  match_loop!(stream.next_byte() => subsh_ch, {
    b'\\' => {
      out.push_byte(subsh_ch);
      if let Some(next_ch) = stream.next_byte() {
        out.push_byte(next_ch);
      }
    }

    b'\'' if !qt.in_double() => {
      qt.toggle_single();
      out.push_byte(subsh_ch);
    }
    b'"' if !qt.in_single() => {
      qt.toggle_double();
      out.push_byte(subsh_ch);
    }
    byte if qt.in_quote() => out.push_byte(byte),

    b'$' if stream.peek_byte() == Some(b'\'') => {
      out.push_byte(subsh_ch);
    }
    b'(' => {
      out.push_byte(subsh_ch);
      paren_count += 1;
    }
    b')' => {
      paren_count -= 1;
      if paren_count <= 0 {
        out.push_marker(Marker::ProcSub(kind));
        break;
      }
      out.push_byte(subsh_ch);
    }
    _ => out.push_byte(subsh_ch),
  });
}

fn read_backtick(stream: &mut SegCursor, out: &mut SegStream, in_dquote: bool) {
  out.push_marker(Marker::VarSub);
  out.push_marker(Marker::Subshell);
  match_loop!(stream.next_byte() => bt_ch, {
    b'\\' => {
      // only push backslash for double quotes if we are already in double quotes
      // this is some weird posix corner case
      match stream.peek_byte() {
        Some(b'`' | b'$' | b'\\') => out.push_byte(stream.next_byte().unwrap()),
        Some(b'"') if in_dquote => out.push_byte(stream.next_byte().unwrap()),
        _ => out.push_byte(bt_ch),
      }
    }
    // fun fact: this one match arm allows us to parse backtick statements nested in regular command subs inside of other backtick statements.
    // Not even zsh's parser handles this case
    b'$' if stream.eat_byte(b'(') => {
      out.push_bytes(b"$(");
      let mut paren_count = 1;
      match_loop!(stream.next_byte() => subsh_ch, {
        b'\\' => {
          out.push_byte(subsh_ch);
          if let Some(next_ch) = stream.next_byte() {
            out.push_byte(next_ch);
          }
        }
        b'(' => {
          paren_count += 1;
          out.push_byte(b'(');
        }
        b')' => {
          paren_count -= 1;
          out.push_byte(b')');
          if paren_count == 0 {
            break;
          }
        }
        _ => out.push_byte(subsh_ch),
      });
    }
    b'`' => {
      out.push_marker(Marker::Subshell);
      break;
    }
    _ => out.push_byte(bt_ch),
  });
}

/// Heredoc body: $var / ${var} / $(cmd) / backticks only. Quotes, tildes,
/// globs, process subs, and bare subshells all pass through as literal text.
pub(crate) fn unescape_heredoc(raw: &[u8]) -> SegStream {
  unescape_with(SegStream::from_bytes(raw), ExpandFlags::HEREDOC)
}

pub(crate) fn escape_str(raw: &str) -> String {
  escape_str_bounded(raw, None)
}

/// Opposite of `unescape_str`, escapes a string to be executed as literal text
/// Used for completion results, and glob filename matches.
///
/// if `use_marker` is true, it will check for `markers::ESCAPE` instead of a literal backslash.
/// if a bound (something like 0..5) is provided, the escaping logic will be limited to those bytes
/// this is mainly used for escaping the region of text that is changed during completion
pub(crate) fn escape_str_bounded(raw: &str, bound: Option<&Range<usize>>) -> String {
  let mut result = String::new();
  let mut chars = raw.char_indices();

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
        result.push('\\');
        result.push(ch);
      }
      '~' if result.is_empty() => {
        result.push('\\');
        result.push(ch);
      }
      _ => {
        result.push(ch);
      }
    }
  }

  result
}

pub(crate) fn unescape_math(raw: &[u8]) -> ShResult<SegStream> {
  let mut cur = SliceCursor::new(raw);
  let mut out = SegStream::new();
  let mut qt_state = QuoteState::default();

  match_loop!(cur.next_byte() => ch, {
    b'\\' => {
      if (!qt_state.in_single() || cur.peek_byte() == Some(b'\''))
      && let Some(next_ch) = cur.next_byte() {
        out.push_byte(next_ch);
      }
    }
    b'"' => qt_state.toggle_double(),
    b'\'' => qt_state.toggle_single(),
    _ if qt_state.in_single() => out.push_byte(ch),
    b'$' => {
      out.push_marker(Marker::VarSub);
      if cur.peek_byte() == Some(b'(') {
        out.push_marker(Marker::Subshell);
        cur.next_byte();
        let mut paren_count = 1;
        match_loop!(cur.next_byte() => subsh_ch, {
          b'\\' => {
            out.push_byte(subsh_ch);
            if let Some(next_ch) = cur.next_byte() {
              out.push_byte(next_ch);
            }
          }
          b'(' => {
            paren_count += 1;
            out.push_byte(subsh_ch);
          }
          b')' => {
            paren_count -= 1;
            if paren_count == 0 {
              out.push_marker(Marker::Subshell);
              break;
            }
            out.push_byte(subsh_ch);
          }
          _ => out.push_byte(subsh_ch),
        });
      }
    }
    _ if qt_state.in_double() => { out.push_byte(ch); }
    _ => out.push_byte(ch),
  });

  if !qt_state.outside() {
    return Err(sherr!(ParseErr, "Unmatched quote in arithmetic expression",));
  }

  Ok(out)
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

pub(crate) fn xtrace_quote_fmt<W: std::fmt::Write>(s: &str, f: &mut W) -> std::fmt::Result {
  quote_fmt(s, f, r#" !*?$;|&<>(){}[]`'"\"#, false)
}

pub(crate) fn shell_quote_fmt<W: std::fmt::Write>(s: &str, f: &mut W) -> std::fmt::Result {
  quote_fmt(s, f, r#"\\!#$^*()=|{}[]`<>?~;& "'"#, true)
}

/// Quotes a string such that it can be round-tripped as shell syntax
pub(crate) fn shell_quote(s: &str) -> String {
  quote(s, shell_quote_fmt)
}

/// Byte-native shell quoting: like [`shell_quote`], but preserves arbitrary
/// non-UTF-8 bytes by rendering the value in `$'...'` ANSI-C form, so
/// `declare`/`set`/`alias` output round-trips raw bytes instead of laundering
/// them into `U+FFFD`. Valid UTF-8 values take the ordinary `shell_quote` path.
pub(crate) fn shell_quote_bytes(bytes: &[u8]) -> Vec<u8> {
  if let Ok(s) = std::str::from_utf8(bytes) {
    shell_quote(s).into_bytes()
  } else {
    let mut out = Vec::with_capacity(bytes.len() + 4);
    out.extend_from_slice(b"$'");
    for chunk in bytes.utf8_chunks() {
      for ch in chunk.valid().chars() {
        push_ansi_c_escaped(&mut out, ch);
      }
      for &b in chunk.invalid() {
        out.extend_from_slice(format!("\\x{b:02x}").as_bytes());
      }
    }
    out.push(b'\'');
    out
  }
}

/// Append `ch` to `out` in the `$'...'` ANSI-C escaping convention (matching the
/// `$'...'` branch of [`quote_fmt`]).
fn push_ansi_c_escaped(out: &mut Vec<u8>, ch: char) {
  match ch {
    '\\' => out.extend_from_slice(b"\\\\"),
    '\'' => out.extend_from_slice(b"\\'"),
    '\n' => out.extend_from_slice(b"\\n"),
    '\r' => out.extend_from_slice(b"\\r"),
    '\t' => out.extend_from_slice(b"\\t"),
    '\x07' => out.extend_from_slice(b"\\a"),
    '\x08' => out.extend_from_slice(b"\\b"),
    '\x0B' => out.extend_from_slice(b"\\v"),
    '\x0C' => out.extend_from_slice(b"\\f"),
    c if c.is_ascii_control() => out.extend_from_slice(format!("\\x{:02x}", c as u8).as_bytes()),
    c => {
      let mut buf = [0u8; 4];
      out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
  }
}

/// Quotes an xtrace argument
pub(crate) fn xtrace_quote(s: &str) -> String {
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

  // `unescape_str` is SegStream-native now; these tests assert marker
  // placement, so render the stream back to a marker-char string (markers as
  // their sentinel chars) for the string-shaped assertions below.
  fn unescape_str(s: &str) -> String {
    use crate::expand::stream::StreamSeg;
    let mut out = String::new();
    for seg in super::unescape_str(s.as_bytes()).stream() {
      match seg {
        StreamSeg::Bytes(b) => out.push_str(&String::from_utf8_lossy(b)),
        StreamSeg::Mark(m) => out.push(marker_char(*m)),
      }
    }
    out
  }
  fn marker_char(m: crate::expand::stream::Marker) -> char {
    use crate::expand::stream::{Marker, ProcSubKind, Quote};
    match m {
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
    }
  }
  #[allow(dead_code)]
  mod markers {
    pub(super) const ESCAPE: char = '\u{fdd9}';
    pub(super) const SNG_QUOTE: char = '\u{fdd1}';
    pub(super) const DUB_QUOTE: char = '\u{fdd0}';
    pub(super) const VAR_SUB: char = '\u{fdd8}';
    pub(super) const TILDE_SUB: char = '\u{fdd2}';
  }

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
  // `expand_ansi_c` is byte-native now; wrap it as str→str for these ASCII
  // assertions (byte-specific cases call `super::expand_ansi_c` directly).
  fn expand_ansi_c(s: &str) -> String {
    String::from_utf8_lossy(&super::expand_ansi_c(s.as_bytes())).into_owned()
  }
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
    // `\xFF` is the raw byte 0xFF (byte-native), not the widened `U+00FF`.
    assert_eq!(super::expand_ansi_c(b"\\xFF"), [0xFF]);
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
