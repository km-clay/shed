use std::{iter::Peekable, str::Chars};

use crate::{
  expand::escape::{read_hex, read_octal},
  match_loop,
  parse::lex::{Span, Tk},
  sherr,
  util::error::ShResult,
};

/// Used to track whether the lexer is currently inside a quote, and if so, which type
#[derive(Default, Debug, PartialEq, Clone)]
pub enum QuoteState {
  #[default]
  Outside,
  Single,
  Double,
}

impl QuoteState {
  pub fn outside(&self) -> bool {
    matches!(self, QuoteState::Outside)
  }
  pub fn in_single(&self) -> bool {
    matches!(self, QuoteState::Single)
  }
  pub fn in_double(&self) -> bool {
    matches!(self, QuoteState::Double)
  }
  pub fn in_quote(&self) -> bool {
    !self.outside()
  }
  /// Toggles whether we are in a double quote. If self = QuoteState::Single or QuoteState::Backtick, this does nothing, since double quotes inside those quotes are just literal characters
  pub fn toggle_double(&mut self) {
    match self {
      QuoteState::Outside => *self = QuoteState::Double,
      QuoteState::Double => *self = QuoteState::Outside,
      _ => {}
    }
  }
  /// Toggles whether we are in a single quote. If self == QuoteState::Double or QuoteState::Backtick, this does nothing, since single quotes inside those quotes are just literal characters
  pub fn toggle_single(&mut self) {
    match self {
      QuoteState::Outside => *self = QuoteState::Single,
      QuoteState::Single => *self = QuoteState::Outside,
      _ => {}
    }
  }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct DelimState {
  quote: QuoteState,
  bracket_depth: usize,
  paren_depth: usize,
  brace_depth: usize,
}

impl DelimState {
  pub fn is_literal(&self) -> bool {
    self.quote.in_quote() || self.bracket_depth > 0 || self.paren_depth > 0 || self.brace_depth > 0
  }
}

/* - splitting functions
 * the splitting functions in std are fine, but don't cut it when quoting rules and escaping are involved
 * so we have to roll our own stuff. we can take a functional approach to to this that generalizes quite well
 */

pub fn split_all<F>(slice: &str, segment_fn: F) -> Vec<String>
where
  F: Fn(&str) -> Option<(usize, usize)>,
{
  split_all_with(slice, segment_fn, |start, end| {
    slice[start..end].to_string()
  })
}

pub fn split_case_pat(slice: &str) -> Vec<String> {
  split_all(slice, split_case_pattern_segment)
}

pub fn split_tk(tk: &Tk, pat: &str) -> Vec<Tk> {
  let slice = tk.as_str();
  let base = tk.span.range().start;
  split_all_with(
    slice,
    |s| split_at_unescaped(s, pat),
    |start, end| {
      Tk::new(
        tk.class.clone(),
        Span::new(base + start..base + end, tk.source()),
      )
    },
  )
}

pub fn split_tk_at(tk: &Tk, pat: &str) -> Option<(Tk, Tk)> {
  let slice = tk.as_str();
  let base = tk.span.range().start;
  split_at_unescaped(slice, pat).map(|(start, len)| {
    let left = Tk::new(tk.class.clone(), Span::new(base..base + start, tk.source()));
    let right = Tk::new(
      tk.class.clone(),
      Span::new(base + start + len..base + slice.len(), tk.source()),
    );
    (left, right)
  })
}

pub fn split_all_with<T, F, B>(slice: &str, segment_fn: F, mut build: B) -> Vec<T>
where
  F: Fn(&str) -> Option<(usize, usize)>,
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

pub fn split_case_pattern_segment(slice: &str) -> Option<(usize, usize)> {
  let pat = '|';
  let mut chars = slice.char_indices().peekable();
  let mut delim_state = DelimState::default();
  while let Some((i, ch)) = chars.next() {
    match ch {
      '\\' => {
        chars.next();
        continue;
      }
      '[' => delim_state.bracket_depth += 1,
      ']' if delim_state.bracket_depth > 0 => delim_state.bracket_depth -= 1,
      '\'' => delim_state.quote.toggle_single(),
      '"' => delim_state.quote.toggle_double(),
      _ if delim_state.is_literal() => continue,
      _ => {}
    }

    if slice[i..].starts_with(pat) {
      return Some((i, 1));
    }
  }

  None
}

/// Splits a string at the first occurrence of a pattern, but only if the pattern is not escaped by a backslash
/// and not in quotes. Returns None if the pattern is not found or only found escaped.
pub fn split_at_unescaped(slice: &str, pat: &str) -> Option<(usize, usize)> {
  let mut chars = slice.char_indices().peekable();
  let mut qt_state = QuoteState::default();

  while let Some((i, ch)) = chars.next() {
    match ch {
      '\\' => {
        chars.next();
        continue;
      }
      '\'' => qt_state.toggle_single(),
      '"' => qt_state.toggle_double(),
      _ if qt_state.in_quote() => continue,
      _ => {}
    }

    if slice[i..].starts_with(pat) {
      return Some((i, pat.len()));
    }
  }

  None
}

pub fn pos_is_escaped(slice: &str, pos: usize) -> bool {
  let bytes = slice.as_bytes();
  let mut escaped = false;
  let mut i = pos;
  while i > 0 && bytes[i - 1] == b'\\' {
    escaped = !escaped;
    i -= 1;
  }
  escaped
}

pub fn ends_with_unescaped(slice: &str, pat: &str) -> bool {
  slice.ends_with(pat) && !pos_is_escaped(slice, slice.len() - pat.len())
}

pub fn scan_parens(chars: &mut Peekable<Chars>, pos: &mut usize, depth: usize) -> bool {
  scan_delims('(', chars, pos, depth).unwrap()
}

pub fn scan_braces(chars: &mut Peekable<Chars>, pos: &mut usize, depth: usize) -> bool {
  scan_delims('{', chars, pos, depth).unwrap()
}

pub fn scan_brackets(chars: &mut Peekable<Chars>, pos: &mut usize, depth: usize) -> bool {
  scan_delims('[', chars, pos, depth).unwrap()
}

pub fn scan_angles(chars: &mut Peekable<Chars>, pos: &mut usize, depth: usize) -> bool {
  scan_delims('<', chars, pos, depth).unwrap()
}

pub fn scan_delims(
  opener: char,
  chars: &mut Peekable<Chars>,
  pos: &mut usize,
  mut depth: usize,
) -> ShResult<bool> {
  let closer = match opener {
    '(' => ')',
    '{' => '}',
    '[' => ']',
    '<' => '>',
    _ => {
      return Err(sherr!(
          ParseErr @ Span::new(*pos..*pos, "".into()),
          "Invalid opener '{opener}'",
      ));
    }
  };
  let mut qt = QuoteState::default();
  match_loop!(chars.next() => ch, {
    '\\' => {
      *pos += 1;
      if let Some(next_ch) = chars.next() {
        *pos += next_ch.len_utf8();
      }
    }
    '\'' => { *pos += 1; qt.toggle_single(); }
    '"' if !qt.in_single() => { *pos += 1; qt.toggle_double(); }
    _ if qt.in_quote() => *pos += ch.len_utf8(),
    _ if ch == opener => { *pos += 1; depth += 1; }
    _ if ch == closer => {
      *pos += 1;
      depth -= 1;
      if depth == 0 { break; }
    }
    _ => *pos += ch.len_utf8(),
  });
  Ok(depth == 0)
}

/// Expand standard ANSI-C escapes
pub fn expand_ansi_c(s: &str) -> String {
  let mut result = String::new();
  let mut chars = s.chars().peekable();

  while let Some(ch) = chars.next() {
    if ch != '\\' {
      result.push(ch);
      continue;
    }
    let Some(&next) = chars.peek() else {
      result.push(ch);
      break;
    };

    match next {
      'n' => {
        result.push('\n');
        chars.next();
      }
      't' => {
        result.push('\t');
        chars.next();
      }
      'r' => {
        result.push('\r');
        chars.next();
      }
      'a' => {
        result.push('\x07');
        chars.next();
      }
      'b' => {
        result.push('\x08');
        chars.next();
      }
      'e' => {
        result.push('\x1B');
        chars.next();
      }
      'E' => {
        result.push('\x1B');
        chars.next();
      }
      'v' => {
        result.push('\x0B');
        chars.next();
      }
      'x' => {
        chars.next();
        read_hex(&mut chars, &mut result);
      }
      '0' => {
        chars.next();
        read_octal(&mut chars, &mut result);
      }
      '\\' => {
        result.push('\\');
        chars.next();
      }
      _ => {
        result.push(ch);
      }
    }
  }

  result
}
