use std::{fmt::Display, iter::Peekable, str::Chars};

use crate::{state::vars::VarStr, varstr};

use super::{
  error::ShResult,
  eval::lex::{Span, Tk},
  expand::markers,
  match_loop, sherr,
};

pub(crate) trait VarStrDisplay {
  fn to_var_str(&self) -> VarStr;
}

impl<T: Display + ?Sized> VarStrDisplay for T {
  fn to_var_str(&self) -> VarStr {
    varstr!("{self}")
  }
}

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
  /// Toggles whether we are in a double quote. If self = `QuoteState::Single` or `QuoteState::Backtick,` this does nothing, since double quotes inside those quotes are just literal characters
  pub fn toggle_double(&mut self) {
    match self {
      QuoteState::Outside => *self = QuoteState::Double,
      QuoteState::Double => *self = QuoteState::Outside,
      QuoteState::Single => {}
    }
  }
  /// Toggles whether we are in a single quote. If self == `QuoteState::Double` or `QuoteState::Backtick,` this does nothing, since single quotes inside those quotes are just literal characters
  pub fn toggle_single(&mut self) {
    match self {
      QuoteState::Outside => *self = QuoteState::Single,
      QuoteState::Single => *self = QuoteState::Outside,
      QuoteState::Double => {}
    }
  }
}

pub(crate) fn compile_glob(s: &str) -> Result<glob::Pattern, glob::PatternError> {
  let replaced = replace_posix_classes(s);
  match glob::Pattern::new(&replaced) {
    Ok(pattern) => Ok(pattern),
    // if we are here, we have an unclosed bracket in there
    // so we've gotta escape it
    Err(_) => glob::Pattern::new(&escape_stray_brackets(&replaced)),
  }
}

/// Escapes unclosed bracket globs
pub(crate) fn compile_glob_lenient(s: &str) -> glob::Pattern {
  compile_glob(s).unwrap_or_else(|_| {
    glob::Pattern::new(&glob::Pattern::escape(s)).expect("an escaped glob is always valid")
  })
}

/// Escape any `[` that does not open a valid bracket expression
fn escape_stray_brackets(s: &str) -> String {
  let chars: Vec<char> = s.chars().collect();
  let mut out = String::with_capacity(s.len());
  let mut i = 0;
  while i < chars.len() {
    match chars[i] {
      '\\' => {
        out.push('\\');
        if let Some(&next) = chars.get(i + 1) {
          out.push(next);
          i += 2;
        } else {
          i += 1;
        }
      }
      '[' => {
        if let Some(close) = valid_bracket_end(&chars, i) {
          out.extend(&chars[i..=close]);
          i = close + 1;
        } else {
          out.push_str("[[]");
          i += 1;
        }
      }
      c => {
        out.push(c);
        i += 1;
      }
    }
  }
  out
}

/// If a bracket expression opens at `open` (index of `[`), return the index of
/// its closing `]`. Follows fnmatch rules: an initial `!`/`^` negates, and a `]`
/// in the first position is a literal member rather than the terminator.
fn valid_bracket_end(chars: &[char], open: usize) -> Option<usize> {
  let mut j = open + 1;
  if matches!(chars.get(j), Some('!' | '^')) {
    j += 1;
  }
  if matches!(chars.get(j), Some(']')) {
    j += 1;
  }
  while j < chars.len() {
    match chars[j] {
      '\\' => j += 2,
      ']' => return Some(j),
      _ => j += 1,
    }
  }
  None
}

pub(crate) fn replace_posix_classes(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut chars = s.chars().peekable();
  let mut in_bracket = false;

  match_loop!(chars.next() => ch, {
    '\\' => {
      out.push(ch);
      if let Some(next_ch) = chars.next() {
        out.push(next_ch);
      }
    }
    '[' if !in_bracket => {
      in_bracket = true;
      out.push(ch);

      // convert '^' to '!'
      // glob crate uses '!' for negation in it's patterns
      if chars.peek() == Some(&'^') {
        chars.next();
        out.push('!');
      }
    }
    ']' if in_bracket => {
      in_bracket = false;
      out.push(ch);
    }
    '[' if in_bracket && chars.peek() == Some(&':') => {
      chars.next();
      let mut name = String::new();
      match_loop!(chars.peek() => &ch => ch, {
        ':' => {
          chars.next();
          break
        }
        _ => {
          name.push(ch);
          chars.next();
        }
      });

      if chars.peek() == Some(&']')
      && let Some(posix_chars) = posix_class_chars(&name) {
        chars.next();
        out.push_str(posix_chars);
      } else {
        out.push('[');
        out.push(':');
        out.push_str(&name);
      }

    }
    _ => out.push(ch),
  });

  out
}

fn posix_class_chars(name: &str) -> Option<&'static str> {
  match name {
    "alnum" => Some("a-zA-Z0-9"),
    "alpha" => Some("a-zA-Z"),
    "blank" => Some(" \t"),
    "cntrl" => Some("\x00-\x1F\x7F"),
    "digit" => Some("0-9"),
    "graph" => Some("!-~"),
    "lower" => Some("a-z"),
    "print" => Some(" -~"),
    "punct" => Some("!-/:-@\\[-`{-~"),
    "space" => Some(" \t\r\n\x0b\x0c"),
    "upper" => Some("A-Z"),
    "xdigit" => Some("A-Fa-f0-9"),
    _ => None,
  }
}

/* - splitting functions
 * the splitting functions in std are fine, but don't cut it when quoting rules and escaping are involved
 * so we have to roll our own stuff. we can take a functional approach to to this that generalizes quite well
 */

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

/// Splits a string at the first occurrence of a pattern, but only if the pattern is not escaped by a backslash
/// and not in quotes. Returns None if the pattern is not found or only found escaped.
pub fn split_at_unescaped(slice: &str, pat: &str) -> Option<(usize, usize)> {
  split_at_any_unescaped(slice, &[pat])
}

pub fn split_at_any_unescaped(slice: &str, pats: &[&str]) -> Option<(usize, usize)> {
  split_at_any_inner(slice, pats, '\\', '\'', '"')
}

/// Marker-aware counterpart of [`split_at_unescaped`], for strings that have
/// already been through expansion unescaping. There the escape is
/// `markers::ESCAPE` and quotes are the `SNG_QUOTE`/`DUB_QUOTE` markers rather
/// than literal `\`, `'`, `"`. A string only ever uses one scheme (raw or
/// marker-encoded), never a mix, so this is a separate variant rather than a
/// combined one.
pub fn split_at_unescaped_markers(slice: &str, pat: &str) -> Option<(usize, usize)> {
  split_at_any_unescaped_markers(slice, &[pat])
}

pub fn split_at_any_unescaped_markers(slice: &str, pats: &[&str]) -> Option<(usize, usize)> {
  split_at_any_inner(
    slice,
    pats,
    markers::ESCAPE,
    markers::SNG_QUOTE,
    markers::DUB_QUOTE,
  )
}

/// Split at the first of `pats` not escaped by `esc` and not inside a
/// `sng_quote`/`dub_quote` region. Shared by the backslash and marker
/// variants; only the escape/quote characters differ.
fn split_at_any_inner(
  slice: &str,
  pats: &[&str],
  esc: char,
  sng_quote: char,
  dub_quote: char,
) -> Option<(usize, usize)> {
  let mut chars = slice.char_indices().peekable();
  let mut qt_state = QuoteState::default();

  while let Some((i, ch)) = chars.next() {
    match ch {
      _ if ch == esc => {
        chars.next();
        continue;
      }
      _ if ch == sng_quote => qt_state.toggle_single(),
      _ if ch == dub_quote => qt_state.toggle_double(),
      _ if qt_state.in_quote() => continue,
      _ => {}
    }

    for pat in pats {
      if slice[i..].starts_with(pat) {
        return Some((i, pat.len()));
      }
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

pub fn starts_with_unescaped(slice: &str, pat: &str) -> bool {
  slice.starts_with(pat) && !pos_is_escaped(slice, 0)
}

pub fn count_unescaped(slice: &str, pat: &str) -> usize {
  let mut count = 0;
  let mut start = 0;
  while let Some((pos, skip)) = split_at_unescaped(&slice[start..], pat) {
    count += 1;
    start += pos + skip;
  }
  count
}

pub fn has_unescaped(slice: &str, pat: &str) -> bool {
  split_at_unescaped(slice, pat).is_some()
}

pub fn has_any_unescaped(slice: &str, pats: &[&str]) -> bool {
  split_at_any_unescaped(slice, pats).is_some()
}

pub fn scan_parens(chars: &mut Peekable<Chars>, pos: &mut usize, depth: usize) -> bool {
  scan_delims('(', chars, pos, depth).unwrap()
}

pub fn scan_param_exp(chars: &mut Peekable<Chars>, pos: &mut usize, mut depth: usize) -> bool {
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
    '$' if chars.peek() == Some(&'{') => {
      chars.next();
      *pos += 2;
      depth += 1;
    }
    '$' if chars.peek() == Some(&'(') => {
      chars.next();
      *pos += 2;
      // Reuse the paren-matcher so an inner `$(... } ...)` doesn't trip the
      // param-expansion closer scan.
      if !scan_parens(chars, pos, 1) {
        return false;
      }
    }
    '}' => {
      *pos += 1;
      depth -= 1;
      if depth == 0 { break; }
    }
    _ => *pos += ch.len_utf8(),
  });
  depth == 0
}

fn scan_delims(
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

#[expect(clippy::too_many_lines)]
pub(crate) fn format_time(dur: std::time::Duration) -> String {
  const ETERNITY: u128 = f32::INFINITY as u128;
  let mut micros = dur.as_micros();
  let mut millis = 0;
  let mut seconds = 0;
  let mut minutes = 0;
  let mut hours = 0;
  let mut days = 0;
  let mut weeks = 0;
  let mut months = 0;
  let mut years = 0;
  let mut decades = 0;
  let mut centuries = 0;
  let mut millennia = 0;
  let mut epochs = 0;
  let mut aeons = 0;
  let mut eternities = 0; // just in case, you know?

  if micros >= 1000 {
    millis = micros / 1000;
    micros %= 1000;
  }
  if millis >= 1000 {
    seconds = millis / 1000;
    millis %= 1000;
  }
  if seconds >= 60 {
    minutes = seconds / 60;
    seconds %= 60;
  }
  if minutes >= 60 {
    hours = minutes / 60;
    minutes %= 60;
  }
  if hours >= 24 {
    days = hours / 24;
    hours %= 24;
  }
  if days >= 7 {
    weeks = days / 7;
    days %= 7;
  }
  if weeks >= 4 {
    months = weeks / 4;
    weeks %= 4;
  }
  if months >= 12 {
    years = months / 12;
    months %= 12;
  }
  if years >= 10 {
    decades = years / 10;
    years %= 10;
  }
  if decades >= 10 {
    centuries = decades / 10;
    decades %= 10;
  }
  if centuries >= 10 {
    millennia = centuries / 10;
    centuries %= 10;
  }
  if millennia >= 1000 {
    epochs = millennia / 1000;
    millennia %= 1000;
  }
  if epochs >= 1000 {
    aeons = epochs / 1000;
    epochs %= 1000;
  }
  if aeons == ETERNITY {
    eternities = aeons / ETERNITY;
    aeons %= ETERNITY;
  }

  // Format the result
  let mut result = Vec::new();
  if eternities > 0 {
    let mut string = format!("{eternities} eternit");
    if eternities > 1 {
      string.push_str("ies");
    } else {
      string.push('y');
    }
    result.push(string);
  }
  if aeons > 0 {
    let mut string = format!("{aeons} aeon");
    if aeons > 1 {
      string.push('s');
    }
    result.push(string);
  }
  if epochs > 0 {
    let mut string = format!("{epochs} epoch");
    if epochs > 1 {
      string.push('s');
    }
    result.push(string);
  }
  if millennia > 0 {
    let mut string = format!("{millennia} millenni");
    if millennia > 1 {
      string.push('a');
    } else {
      string.push_str("um");
    }
    result.push(string);
  }
  if centuries > 0 {
    let mut string = format!("{centuries} centur");
    if centuries > 1 {
      string.push_str("ies");
    } else {
      string.push('y');
    }
    result.push(string);
  }
  if decades > 0 {
    let mut string = format!("{decades} decade");
    if decades > 1 {
      string.push('s');
    }
    result.push(string);
  }
  if years > 0 {
    let mut string = format!("{years} year");
    if years > 1 {
      string.push('s');
    }
    result.push(string);
  }
  if months > 0 {
    let mut string = format!("{months} month");
    if months > 1 {
      string.push('s');
    }
    result.push(string);
  }
  if weeks > 0 {
    let mut string = format!("{weeks} week");
    if weeks > 1 {
      string.push('s');
    }
    result.push(string);
  }
  if days > 0 {
    let mut string = format!("{days} day");
    if days > 1 {
      string.push('s');
    }
    result.push(string);
  }
  if hours > 0 {
    let string = format!("{hours}h");
    result.push(string);
  }
  if minutes > 0 {
    let string = format!("{minutes}m");
    result.push(string);
  }
  if seconds > 0 {
    let string = format!("{seconds}s");
    result.push(string);
  }
  if result.is_empty() && millis > 0 {
    let string = format!("{millis}ms");
    result.push(string);
  }
  if result.is_empty() && micros > 0 {
    let string = format!("{micros}µs");
    result.push(string);
  }

  result.join(" ")
}

/// Parse human-readable size strings into raw byte number
pub fn parse_size(s: &str) -> ShResult<u64> {
  let s = s.trim().to_lowercase();

  let units: [(&str, f64); 19] = [
    ("eib", (1u64 << 60) as f64), // 2^60 bytes (binary exabyte)
    ("pib", (1u64 << 50) as f64), // 2^50 bytes (binary petabyte)
    ("tib", (1u64 << 40) as f64), // 2^40 bytes (binary terabyte)
    ("gib", (1u64 << 30) as f64), // 2^30 bytes (binary gigabyte)
    ("mib", (1u64 << 20) as f64), // 2^20 bytes (binary megabyte)
    ("kib", (1u64 << 10) as f64), // 2^10 bytes (binary kilobyte)
    ("eb", 10u64.pow(18) as f64), // 10^18 bytes (decimal exabyte)
    ("pb", 10u64.pow(15) as f64), // 10^15 bytes (decimal petabyte)
    ("tb", 10u64.pow(12) as f64), // 10^12 bytes (decimal terabyte)
    ("gb", 10u64.pow(9) as f64),  // 10^9 bytes (decimal gigabyte)
    ("mb", 10u64.pow(6) as f64),  // 10^6 bytes (decimal megabyte)
    ("kb", 10u64.pow(3) as f64),  // 10^3 bytes (decimal kilobyte)
    ("e", 10u64.pow(18) as f64),  // allow omission of the 'b'
    ("p", 10u64.pow(15) as f64),
    ("t", 10u64.pow(12) as f64),
    ("g", 10u64.pow(9) as f64),
    ("m", 10u64.pow(6) as f64),
    ("k", 10u64.pow(3) as f64),
    ("b", 1.0), // bytes
  ];

  for (unit, multiplier) in &units {
    if s.ends_with(unit) {
      let num_str = s.trim_end_matches(unit).trim();

      match num_str.parse::<f64>() {
        Ok(n) if n < 0.0 => {
          return Err(sherr!(
            ParseErr,
            "Size number cannot be negative: {num_str}",
          ));
        }
        Ok(n) => {
          let bytes = n * multiplier;
          if bytes > u64::MAX as f64 {
            return Err(sherr!(ParseErr, "Size number too large: {num_str}{unit}",));
          }
          return Ok(bytes.round() as u64);
        }
        Err(_) => return Err(sherr!(ParseErr, "Invalid size number: {num_str}",)),
      }
    }
  }

  // If no unit suffix found, interpret as raw sector count
  match s.parse::<i64>() {
    Err(_) => Err(sherr!(ParseErr, "Invalid size number: {s}",)),
    Ok(n) if n < 0 => Err(sherr!(ParseErr, "Size number cannot be negative: {s}",)),
    Ok(n) => Ok(n as u64),
  }
}

pub fn format_size(bytes: u64, buf: &mut impl std::fmt::Write) -> std::fmt::Result {
  const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
  let mut size = bytes as f64;
  let mut unit = 0;
  while size >= 1024.0 && unit < UNITS.len() - 1 {
    size /= 1024.0;
    unit += 1;
  }
  if unit == 0 {
    write!(buf, "{} {}", size as u64, UNITS[unit])
  } else {
    write!(buf, "{:.1} {}", size, UNITS[unit])
  }
}

pub fn format_mode(mode: u32) -> String {
  let mut out = String::new();
  let mut check_bit = |bit: u32, ch: char| {
    if mode & bit != 0 {
      out.push(ch);
    } else {
      out.push('-');
    }
  };
  check_bit(0o400, 'r');
  check_bit(0o200, 'w');
  check_bit(0o100, 'x');
  check_bit(0o040, 'r');
  check_bit(0o020, 'w');
  check_bit(0o010, 'x');
  check_bit(0o004, 'r');
  check_bit(0o002, 'w');
  check_bit(0o001, 'x');

  out
}

#[cfg(test)]
mod format_time_tests {
  use super::format_time;
  use std::time::Duration;

  // ─── single-unit base cases ──────────────────────────────────────

  #[test]
  fn zero_duration_is_empty_string() {
    assert_eq!(format_time(Duration::ZERO), "");
  }

  #[test]
  fn sub_millisecond_uses_microseconds() {
    assert_eq!(format_time(Duration::from_micros(500)), "500µs");
  }

  #[test]
  fn sub_second_uses_milliseconds() {
    assert_eq!(format_time(Duration::from_millis(250)), "250ms");
  }

  #[test]
  fn exact_second_uses_s_suffix() {
    assert_eq!(format_time(Duration::from_secs(1)), "1s");
  }

  #[test]
  fn one_minute() {
    assert_eq!(format_time(Duration::from_mins(1)), "1m");
  }

  #[test]
  fn one_hour() {
    assert_eq!(format_time(Duration::from_hours(1)), "1h");
  }

  #[test]
  fn one_day_uses_day_word() {
    assert_eq!(format_time(Duration::from_hours(24)), "1 day");
  }

  #[test]
  fn one_week() {
    assert_eq!(format_time(Duration::from_hours(168)), "1 week");
  }

  #[test]
  fn one_month() {
    // shed defines a month as 4 weeks (28 days).
    assert_eq!(format_time(Duration::from_hours(672)), "1 month");
  }

  #[test]
  fn one_year() {
    // ... and a year as 12 months.
    assert_eq!(format_time(Duration::from_hours(8064)), "1 year");
  }

  #[test]
  fn one_decade() {
    assert_eq!(format_time(Duration::from_hours(80640)), "1 decade");
  }

  #[test]
  fn one_century() {
    assert_eq!(format_time(Duration::from_hours(806_400)), "1 century");
  }

  // ─── singular vs plural ──────────────────────────────────────────

  #[test]
  fn plural_days() {
    assert_eq!(format_time(Duration::from_hours(48)), "2 days");
  }

  #[test]
  fn plural_weeks() {
    assert_eq!(format_time(Duration::from_hours(336)), "2 weeks");
  }

  #[test]
  fn plural_centuries() {
    assert_eq!(format_time(Duration::from_hours(1_612_800)), "2 centuries");
  }

  // ─── combined output ─────────────────────────────────────────────

  #[test]
  fn combined_h_m_s() {
    // 1h 2m 3s = 3600 + 120 + 3 = 3723s
    assert_eq!(format_time(Duration::from_secs(3723)), "1h 2m 3s");
  }

  #[test]
  fn combined_day_and_hour() {
    // 1 day 5h = 86400 + 18000 = 104400s
    assert_eq!(format_time(Duration::from_hours(29)), "1 day 5h");
  }

  #[test]
  fn combined_week_and_day() {
    // 1 week 3 days = 7*86400 + 3*86400 = 10*86400
    assert_eq!(format_time(Duration::from_hours(240)), "1 week 3 days");
  }

  // ─── sub-unit suppression ────────────────────────────────────────

  #[test]
  fn ms_suppressed_when_seconds_present() {
    // 1500ms = 1s + 500ms; only "1s" appears (ms only shows when
    // nothing else does).
    assert_eq!(format_time(Duration::from_millis(1500)), "1s");
  }

  #[test]
  fn micros_suppressed_when_millis_present() {
    // 1500µs = 1ms + 500µs; only "1ms" appears.
    assert_eq!(format_time(Duration::from_micros(1500)), "1ms");
  }

  // ─── regression tests for previously-buggy paths ────────────────

  #[test]
  fn thirteen_months_carries_one_month_not_thirteen() {
    // Regression: `months %= 12;` was previously `weeks %= 12;`, which
    // left `months` un-modulo'd and produced "1 year 13 months" instead.
    let dur = Duration::from_hours(8736);
    assert_eq!(format_time(dur), "1 year 1 month");
  }

  #[test]
  fn singular_millennium_is_singular() {
    let dur = Duration::from_hours(8_064_000);
    assert!(
      format_time(dur).contains("1 millennium"),
      "got {:?}",
      format_time(dur)
    );
  }

  #[test]
  fn plural_millennia_is_plural() {
    let dur = Duration::from_hours(16_128_000);
    assert!(
      format_time(dur).contains("2 millennia"),
      "got {:?}",
      format_time(dur)
    );
  }
}
