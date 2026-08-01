use std::fmt::Write;
use std::iter::Peekable;
use std::ops::Range;
use std::str::Chars;

use bitflags::bitflags;

use crate::{state::vars::VarStr, util};

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
  if !s.contains(markers::ESCAPE) {
    return;
  }

  *s = s.replace(markers::ESCAPE, "").into();
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

const SPECIAL_CHARS: &str = "#$^*()=|{}[]`<>?~;& '\"";

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
      // An existing ESCAPE marker (from a prior unescape pass, e.g. the
      // marker-encoded operand of a parameter expansion) means the next
      // char is literal — preserve both without re-processing, so e.g.
      // `${v%\$(echo x)}` keeps `$(...)` literal instead of running it.
      markers::ESCAPE => {
        result.push(markers::ESCAPE);
        if let Some(next_ch) = chars.next() {
          result.push(next_ch);
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
      '"' if flags.contains(ExpandFlags::QUOTE) => read_dub_quote(&mut chars, &mut result),
      '\'' if flags.contains(ExpandFlags::QUOTE) => read_sng_quote(&mut chars, &mut result),
      '`' if flags.contains(ExpandFlags::CMDSUB) => read_backtick(&mut chars, &mut result),
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
        read_dollar_quote(&mut chars, &mut result);
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
  let mut paren_count = 1;
  match_loop!(chars.next() => subsh_ch, {
    '\\' => {
      result.push(subsh_ch);
      if let Some(next_ch) = chars.next() {
        result.push(next_ch);
      }
    }
    '\'' => {
      result.push(subsh_ch);
      match_loop!(chars.next() => q_ch, {
        '\\' => {
          result.push(q_ch);
          if let Some(next_ch) = chars.next() {
            result.push(next_ch);
          }
        }
        '\'' => {
          result.push(q_ch);
          break;
        }
        _ => result.push(q_ch),
      });
    }
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
  match_loop!(chars.next() => q_ch, {
    '\\' => {
      if let Some(next_ch) = chars.next() {
        match next_ch {
          '"' | '\\' | '`' | '$' | '!' => {
            // discard the backslash
          }
          _ => {
            result.push(q_ch);
          }
        }
        result.push(next_ch);
      }
    }
    '$' if chars.peek() == Some(&'\'') => {
      result.push(q_ch);
      let sng_quote = chars.next().unwrap();
      result.push(sng_quote);
    }
    '$' => {
      if read_varsub(chars, result) && chars.peek() == Some(&'(') {
        chars.next();
        read_subsh(chars, result);
      }
    }
    '`' => read_backtick(chars, result),
    '"' => {
      result.push(markers::DUB_QUOTE);
      break;
    }
    _ => result.push(q_ch),
  });
}

fn read_dollar_quote(chars: &mut Peekable<Chars>, result: &mut String) {
  match_loop!(chars.next() => q_ch, {
    '\'' => {
      break;
    }
    '\\' => {
      let Some(esc) = chars.next() else { continue };
      match esc {
        'n' => result.push('\n'),
        't' => result.push('\t'),
        'r' => result.push('\r'),
        '"' => result.push('"'),
        '\'' => result.push('\''),
        '\\' => result.push('\\'),
        'a' => result.push('\x07'),
        'b' => result.push('\x08'),
        'c' => read_stty_escape(chars, result),
        'e' | 'E' => result.push('\x1b'),
        'f' => result.push('\x0c'),
        'v' => result.push('\x0b'),
        'x' => read_hex(chars, result),
        _ if esc.is_ascii_digit() => read_octal(chars, result, Some(esc)),
        'o' => read_octal(chars, result, None),
        _ => {
          result.push('\\');
          result.push(esc);
        }
      }
    }
    _ => result.push(q_ch),
  });
}

pub fn read_stty_escape(chars: &mut Peekable<Chars>, result: &mut String) {
  let mut peeker = chars.clone();

  let Some(first) = peeker.next() else {
    result.push('\\');
    result.push('c');
    return;
  };

  let (target, consume_count) = if first == '\\' {
    let Some(second) = peeker.next() else {
      result.push('\\');
      result.push('c');
      return;
    };
    if second != '\\' {
      result.push('\\');
      result.push('c');
      return;
    }
    ('\\', 2)
  } else {
    (first, 1)
  };

  let upper = target.to_ascii_uppercase();
  if !matches!(upper, '@'..='_' | '?') {
    result.push('\\');
    result.push('c');
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
  result.push(code as char);
}

pub fn read_octal(chars: &mut Peekable<Chars>, result: &mut String, first: Option<char>) {
  let mut oct = util::scratch_buf();
  if let Some(first) = first {
    oct.push(first);
  }
  for _ in 0..3 {
    if let Some(o) = chars.peek() {
      if o.is_digit(8) {
        oct.push(*o);
        chars.next();
      } else {
        break;
      }
    } else {
      break;
    }
  }
  if let Ok(byte) = u8::from_str_radix(&oct, 8) {
    result.push(byte as char);
  } else {
    let _ = write!(result, "\\o{oct}");
  }
}

pub fn read_hex(chars: &mut Peekable<Chars>, result: &mut String) {
  let mut hex = util::scratch_buf();
  if let Some(h1) = chars.next() {
    hex.push(h1);
  } else {
    result.push_str("\\x");
    return;
  }
  if let Some(h2) = chars.next() {
    hex.push(h2);
  } else {
    let _ = write!(result, "\\x{hex}");
    return;
  }
  if let Ok(byte) = u8::from_str_radix(&hex, 16) {
    result.push(byte as char);
  } else {
    let _ = write!(result, "\\x{hex}");
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

fn read_backtick(chars: &mut Peekable<Chars>, result: &mut String) {
  result.push(markers::VAR_SUB);
  result.push(markers::SUBSH);
  match_loop!(chars.next() => bt_ch, {
    '\\' => {
      result.push(bt_ch);
      if let Some(next_ch) = chars.next() {
        result.push(next_ch);
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

pub fn shell_quote_fmt(s: &str, f: &mut impl std::fmt::Write) -> std::fmt::Result {
  // An empty string MUST be quoted, otherwise interpolating it into a command
  // line collapses into surrounding whitespace and the arg is silently dropped.
  if s.is_empty() {
    return write!(f, "''");
  }
  let has_control = s.chars().any(|c| c.is_ascii_control());
  let has_special = s.chars().any(|c| SPECIAL_CHARS.contains(c));

  if has_control {
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
  } else if has_special {
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

/// Escapes a string for displaying as a var value
pub fn shell_quote(s: &str) -> String {
  let mut result = String::new();
  shell_quote_fmt(s, &mut result).unwrap();
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
