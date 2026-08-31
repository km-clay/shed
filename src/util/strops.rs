use std::{collections::VecDeque, fmt::Display};

use bstr::ByteSlice;
use chrono::{DateTime, Datelike, Days, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

use crate::{
  eval::lex::{Span, Tk},
  match_loop, sherr,
  state::vars::VarStr,
  util::Direction,
  varstr,
};

use super::error::ShResult;

pub(crate) trait VarStrDisplay {
  fn to_var_str(&self) -> VarStr;
}

impl<T: Display + ?Sized> VarStrDisplay for T {
  fn to_var_str(&self) -> VarStr {
    varstr!("{self}")
  }
}

/// Used to track whether the lexer is currently inside a quote, and if so, which type
#[derive(Default, Copy, Debug, PartialEq, Clone)]
pub(crate) enum QuoteState {
  #[default]
  Outside,
  Single,
  Double,
}

impl QuoteState {
  pub(crate) fn outside(self) -> bool {
    matches!(self, QuoteState::Outside)
  }
  pub(crate) fn in_single(self) -> bool {
    matches!(self, QuoteState::Single)
  }
  pub(crate) fn in_double(self) -> bool {
    matches!(self, QuoteState::Double)
  }
  pub(crate) fn in_quote(self) -> bool {
    !self.outside()
  }
  /// Toggles whether we are in a double quote. If self = `QuoteState::Single` or `QuoteState::Backtick,` this does nothing, since double quotes inside those quotes are just literal characters
  pub(crate) fn toggle_double(&mut self) {
    match self {
      QuoteState::Outside => *self = QuoteState::Double,
      QuoteState::Double => *self = QuoteState::Outside,
      QuoteState::Single => {}
    }
  }
  /// Toggles whether we are in a single quote. If self == `QuoteState::Double` or `QuoteState::Backtick,` this does nothing, since single quotes inside those quotes are just literal characters
  pub(crate) fn toggle_single(&mut self) {
    match self {
      QuoteState::Outside => *self = QuoteState::Single,
      QuoteState::Single => *self = QuoteState::Outside,
      QuoteState::Double => {}
    }
  }
}

/* - splitting functions
 * the splitting functions in std are fine, but don't cut it when quoting rules and escaping are involved
 * so we have to roll our own stuff. we can take a functional approach to to this that generalizes quite well
 */

pub(crate) fn split_tk(tk: &Tk, pat: &[u8]) -> Vec<Tk> {
  let slice = tk.as_bytes();
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
  split_at_any_inner(slice, pats, b'\\', b'\'', b'"')
}

pub(crate) fn split_assignment_raw(arg: &[u8]) -> (&[u8], Option<&[u8]>) {
  let Some((e, l)) = split_at_unescaped(arg, b"=") else {
    return (arg, None);
  };
  (arg[..e].trim(), Some(&arg[e + l..]))
}

/// Split at the first of `pats` not escaped by `esc` and not inside a
/// `sng_quote`/`dub_quote` region. Shared by the backslash and marker
/// variants; only the escape/quote characters differ.
fn split_at_any_inner(
  slice: &[u8],
  pats: &[&[u8]],
  esc: u8,
  sng_quote: u8,
  dub_quote: u8,
) -> Option<(usize, usize)> {
  let mut qt_state = QuoteState::default();
  let mut i = 0;

  while i < slice.len() {
    let b = slice[i];
    match b {
      _ if b == esc => {
        i += 2;
        continue;
      }
      _ if b == sng_quote => qt_state.toggle_single(),
      _ if b == dub_quote => qt_state.toggle_double(),
      _ if qt_state.in_quote() => {
        i += 1;
        continue;
      }
      _ => {}
    }

    for pat in pats {
      if slice[i..].starts_with(pat) {
        return Some((i, pat.len()));
      }
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

/// Scan a balanced `${...}`, following nested `${...}` / `$(...)`. See
/// [`scan_parens`] for the `depth` convention and return value.
pub(crate) fn scan_param_exp<C: ByteCursor>(c: &mut C, mut depth: usize) -> bool {
  let mut qt = QuoteState::default();
  match_loop!(c.next_byte() => b, {
    b'\\' => { c.next_byte(); }
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
    b'\\' => { c.next_byte(); }
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
pub(crate) fn parse_size(s: &str) -> ShResult<u64> {
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

pub(crate) fn format_size(bytes: u64, buf: &mut impl std::fmt::Write) -> std::fmt::Result {
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

pub(crate) fn format_mode(mode: u32) -> String {
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

fn local_to_utc(ndt: NaiveDateTime) -> ShResult<DateTime<Utc>> {
  Local
    .from_local_datetime(&ndt)
    .earliest()
    .map(|dt| dt.with_timezone(&Utc))
    .ok_or_else(|| sherr!(ParseErr, "ambiguous local time: {ndt}"))
}

#[derive(Clone)]
enum TimeTk {
  Num(i64),
  Word(VarStr),
}

pub(crate) struct TimeReader<'a> {
  orig: &'a str,
  tks: Vec<TimeTk>,
  pos: usize,
  anchor: Option<DateTime<Utc>>,
  dir: Option<Direction>,
  offset: Option<i64>,
}

impl<'a> TimeReader<'a> {
  pub(crate) fn interpret(s: &'a str) -> ShResult<DateTime<Utc>> {
    Self {
      orig: s,
      tks: vec![],
      pos: 0,
      anchor: None,
      dir: None,
      offset: None,
    }
    .parse()
  }

  fn next_tk(&mut self) -> Option<TimeTk> {
    let tk = self.tks.get(self.pos)?.clone();
    self.pos += 1;
    Some(tk)
  }

  fn peek_tk(&self) -> Option<&TimeTk> {
    self.tks.get(self.pos)
  }

  pub(crate) fn parse(&mut self) -> ShResult<DateTime<Utc>> {
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S"] {
      if let Ok(time) = NaiveDateTime::parse_from_str(self.orig, fmt) {
        return local_to_utc(time);
      }
    }
    for sep in ["-", ".", "/"] {
      if let Ok(date) = NaiveDate::parse_from_str(self.orig, &format!("%Y{sep}%m{sep}%d")) {
        return local_to_utc(date.and_hms_opt(0, 0, 0).unwrap());
      }
    }
    for parser in [DateTime::parse_from_rfc2822, DateTime::parse_from_rfc3339] {
      if let Ok(time) = parser(self.orig) {
        return Ok(time.with_timezone(&Utc));
      }
    }

    self.tks = Self::tokenize(self.orig)?;
    while let Some(tk) = self.next_tk() {
      match tk {
        TimeTk::Num(n) => self.read_offset(n)?,
        TimeTk::Word(w) => self.read_word(&w)?,
      }
    }

    let anchor = self.anchor.unwrap_or_else(Utc::now);
    let Some(micros) = self.offset else {
      return Ok(anchor);
    };
    let delta = Duration::microseconds(micros);
    Ok(match self.dir.unwrap_or(Direction::Backward) {
      Direction::Backward => anchor - delta,
      Direction::Forward => anchor + delta,
    })
  }

  fn read_offset(&mut self, n: i64) -> ShResult<()> {
    let Some(TimeTk::Word(unit)) = self.next_tk() else {
      return Err(sherr!(ParseErr, "expected a unit after '{n}'"));
    };
    let Some(per) = Self::unit_micros(&unit) else {
      return Err(sherr!(ParseErr, "unknown unit '{unit}'"));
    };
    let add = n
      .checked_mul(per)
      .ok_or_else(|| sherr!(ParseErr, "time expression too large"))?;
    self.offset = Some(self.offset.unwrap_or(0).saturating_add(add));
    Ok(())
  }

  fn read_word(&mut self, word: &VarStr) -> ShResult<()> {
    if let Some(month) = Self::month_num(word) {
      return self.read_named_date(word, month);
    }
    if let Some(dir) = Self::direction(word) {
      self.dir = Some(dir);
    } else if let Some(anchor) = Self::keyword_anchor(word)? {
      self.anchor = Some(anchor);
    } else {
      return Err(sherr!(ParseErr, "unknown time expression '{word}'"));
    }
    Ok(())
  }

  fn read_named_date(&mut self, word: &VarStr, month: u32) -> ShResult<()> {
    let Some(TimeTk::Num(day)) = self.next_tk() else {
      return Err(sherr!(ParseErr, "expected a day after '{word}'"));
    };
    let year = match self.peek_tk() {
      Some(TimeTk::Num(y)) if *y >= 1000 => {
        let y = *y as i32;
        self.pos += 1;
        y
      }
      _ => Local::now().year(),
    };
    let date = NaiveDate::from_ymd_opt(year, month, day as u32)
      .ok_or_else(|| sherr!(ParseErr, "invalid date '{word} {day}'"))?;
    self.anchor = Some(local_to_utc(date.and_hms_opt(0, 0, 0).unwrap())?);
    Ok(())
  }

  fn keyword_anchor(word: &VarStr) -> ShResult<Option<DateTime<Utc>>> {
    let today = Local::now().date_naive();
    let midnight =
      |d: NaiveDate| -> ShResult<DateTime<Utc>> { local_to_utc(d.and_hms_opt(0, 0, 0).unwrap()) };
    Ok(match word.as_bytes() {
      b"now" => Some(Utc::now()),
      b"today" => Some(midnight(today)?),
      b"yesterday" => Some(midnight(today - Days::new(1))?),
      b"tomorrow" => Some(midnight(today + Days::new(1))?),
      _ => None,
    })
  }

  fn month_num(word: &VarStr) -> Option<u32> {
    Some(match word.as_bytes() {
      b"jan" | b"january" => 1,
      b"feb" | b"february" => 2,
      b"mar" | b"march" => 3,
      b"apr" | b"april" => 4,
      b"may" => 5,
      b"jun" | b"june" => 6,
      b"jul" | b"july" => 7,
      b"aug" | b"august" => 8,
      b"sep" | b"sept" | b"september" => 9,
      b"oct" | b"october" => 10,
      b"nov" | b"november" => 11,
      b"dec" | b"december" => 12,
      _ => return None,
    })
  }

  fn direction(word: &VarStr) -> Option<Direction> {
    match word.as_bytes() {
      b"after" | b"since" | b"from" => Some(Direction::Forward),
      b"ago" | b"before" | b"til" | b"until" => Some(Direction::Backward),
      _ => None,
    }
  }
  fn unit_micros(unit: &VarStr) -> Option<i64> {
    const MICROS: i64 = 1;
    const MILLIS: i64 = 1000 * MICROS;
    const SECOND: i64 = 1000 * MILLIS;
    const MINUTE: i64 = 60 * SECOND;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY; // approximate
    const YEAR: i64 = 365 * DAY; // approximate

    match unit.as_bytes() {
      b"us" | b"micro" | b"micros" | b"microsecond" | b"microseconds" => Some(MICROS),
      b"ms" | b"milli" | b"millis" | b"millisecond" | b"milliseconds" => Some(MILLIS),
      b"s" | b"sec" | b"secs" | b"second" | b"seconds" => Some(SECOND),
      b"m" | b"min" | b"mins" | b"minute" | b"minutes" => Some(MINUTE),
      b"h" | b"hr" | b"hrs" | b"hour" | b"hours" => Some(HOUR),
      b"d" | b"day" | b"days" => Some(DAY),
      b"w" | b"wk" | b"wks" | b"week" | b"weeks" => Some(WEEK),
      b"mo" | b"month" | b"months" => Some(MONTH),
      b"y" | b"yr" | b"yrs" | b"year" | b"years" => Some(YEAR),
      _ => None,
    }
  }
  fn tokenize(s: &str) -> ShResult<Vec<TimeTk>> {
    let mut cur = SliceCursor::new(s.as_bytes());
    let mut tks = vec![];

    loop {
      cur.bump_while(|c| c == b' ');
      match cur.peek_byte() {
        Some(c) if c.is_ascii_digit() => {
          let start = cur.pos();
          cur.bump_while(|c| c.is_ascii_digit());

          let n = s[start..cur.pos()]
            .parse()
            .map_err(|_| sherr!(ParseErr, "number too large in time expression"))?;
          tks.push(TimeTk::Num(n));
        }
        Some(c) if c.is_ascii_alphabetic() => {
          let start = cur.pos();
          cur.bump_while(|c| c.is_ascii_alphabetic());
          let word = s[start..cur.pos()].to_ascii_lowercase();
          tks.push(TimeTk::Word(word.as_str().into()));
        }
        Some(_) => cur.bump(),
        None => break,
      }
    }

    Ok(tks)
  }
  pub(crate) fn parse_dur(s: &str) -> ShResult<i64> {
    let mut tks = Self::tokenize(s)?.into_iter().peekable();
    let mut total: i64 = 0;
    let mut saw_any = false;

    while let Some(tk) = tks.next() {
      match tk {
        TimeTk::Num(n) => {
          let Some(TimeTk::Word(unit)) = tks.next() else {
            return Err(sherr!(ParseErr, "expected a unit after '{n}'"));
          };
          let Some(per) = Self::unit_micros(&unit) else {
            return Err(sherr!(ParseErr, "unknown unit '{unit}'"));
          };
          let add = n
            .checked_mul(per)
            .ok_or_else(|| sherr!(ParseErr, "duration too large"))?;
          total = total
            .checked_add(add)
            .ok_or_else(|| sherr!(ParseErr, "duration too large"))?;
          saw_any = true;
        }
        TimeTk::Word(w) => return Err(sherr!(ParseErr, "unexpected '{w}' in duration")),
      }
    }

    if !saw_any {
      return Err(sherr!(ParseErr, "invalid duration '{s}'"));
    }
    Ok(total)
  }
}

// this needs to be at least 2
pub(crate) const EDIT_WEIGHT: usize = 2;

pub(crate) fn levenshtein(left: &[u8], right: &[u8]) -> usize {
  /*
   * Levenshtein algorithm
   * https://en.wikipedia.org/wiki/Levenshtein_distance
   *
   * Given two strings, find the minimum number of edits required for one string to be turned into the other.
   * Useful for check typos, e.g. `gti` -> "Did you mean 'git'?"
   */
  let m = left.len();
  let n = right.len();

  // We are using the Damerau transposition checks, so we need
  // bookkeeping for three rows.
  let mut prev: Vec<usize> = (0..=n).map(|j| j * EDIT_WEIGHT).collect();
  let mut prev2: Vec<usize> = vec![0usize; n + 1]; // this is the row before prev, used for transposition
  let mut curr: Vec<usize> = vec![0usize; n + 1];

  // Since we are tracking three rows, we need an easy way to rotate them
  // as we iterate through the strings. VecDeque will do nicely for this
  let mut rows: VecDeque<&mut Vec<usize>> = [&mut prev2, &mut prev, &mut curr].into();

  // minimum of three values macro thing
  macro_rules! min3 {
    ($a:expr, $b:expr, $c:expr) => {
      ::std::cmp::min(::std::cmp::min($a, $b), $c)
    };
  }

  // Damerau-Levenshtein: check for transposition
  let check_transpose = |i: usize, j: usize| -> bool {
    i >= 2 && j >= 2 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1]
  };
  // transposition costs half as much as an edit
  // so that typos like 'gti' -> 'git' match only on 'git'
  let transpose_cost = EDIT_WEIGHT / 2;

  for i in 1..=m {
    rows[2][0] = i * EDIT_WEIGHT; // base case: first column is i deletions from
    // the empty prefix of the other string
    for j in 1..=n {
      rows[2][j] = if left[i - 1] == right[j - 1] {
        // both bytes match, free move
        rows[1][j - 1]
      } else {
        // Price each edit against its own predecessor. A substitution,
        // deletion, or insertion each cost EDIT_WEIGHT
        let sub = rows[1][j - 1] + EDIT_WEIGHT; // substitution
        let del = rows[1][j] + EDIT_WEIGHT; // deletion
        let ins = rows[2][j - 1] + EDIT_WEIGHT; // insertion
        let mut best = min3!(sub, del, ins);

        if check_transpose(i, j) {
          // transpositions cost half
          best = best.min(rows[0][j - 2] + transpose_cost);
        }

        best
      }
    }

    rows.rotate_left(1);
  }

  // return the bottom right value
  // thank you Vladimir Levenshtein
  rows[1][n]
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

#[cfg(test)]
mod levenshtein_tests {
  use super::{EDIT_WEIGHT, levenshtein};

  /// Convenience wrapper so the cases read as strings.
  fn lev(a: &str, b: &str) -> usize {
    levenshtein(a.as_bytes(), b.as_bytes())
  }

  // ─── identity & empty strings ────────────────────────────────────

  #[test]
  fn identical_is_zero() {
    assert_eq!(lev("cat", "cat"), 0);
    assert_eq!(lev("", ""), 0);
  }

  // Regression: the DP boundary must be *weighted*. Building an n-byte string
  // from the empty string is n insertions, each costing EDIT_WEIGHT — not a
  // raw 0,1,2,... count. (Previously `("" -> "abc")` returned 3 instead of 6.)
  #[test]
  fn empty_boundary_is_weighted() {
    assert_eq!(lev("", "abc"), 3 * EDIT_WEIGHT);
    assert_eq!(lev("abc", ""), 3 * EDIT_WEIGHT);
    assert_eq!(lev("", "a"), EDIT_WEIGHT);
  }

  // ─── single ordinary edits each cost EDIT_WEIGHT ─────────────────

  #[test]
  fn one_substitution() {
    assert_eq!(lev("a", "b"), EDIT_WEIGHT);
  }

  #[test]
  fn one_insertion() {
    assert_eq!(lev("a", "ab"), EDIT_WEIGHT);
  }

  #[test]
  fn one_deletion() {
    assert_eq!(lev("ab", "a"), EDIT_WEIGHT);
  }

  // Regression for the base-case bug: a leading deletion runs the optimal
  // alignment along the boundary, and must still cost a full edit. `cat -> at`
  // previously came back as 1 instead of EDIT_WEIGHT.
  #[test]
  fn boundary_hugging_indel_is_full_weight() {
    assert_eq!(lev("cat", "at"), EDIT_WEIGHT); // leading deletion
    assert_eq!(lev("cat", "cats"), EDIT_WEIGHT); // trailing insertion
  }

  // ─── transposition is the Damerau feature: half an edit ──────────

  #[test]
  fn adjacent_transposition_is_half() {
    let half = EDIT_WEIGHT / 2;
    assert_eq!(lev("ab", "ba"), half);
    assert_eq!(lev("teh", "the"), half);
    assert_eq!(lev("gti", "git"), half);
    assert_eq!(lev("grpe", "grep"), half);
    assert_eq!(lev("dokcer", "docker"), half);
  }

  // The whole point of weighting the transposition: `gti` should read as a
  // single swap of `git` (cheap), strictly beating the substitution `gtp` and
  // the deletion `gt` (both a full edit). This is what makes the typo suggester
  // surface `git` alone instead of tied three ways.
  #[test]
  fn transposition_beats_ordinary_edits() {
    assert!(lev("gti", "git") < lev("gti", "gtp"));
    assert!(lev("gti", "git") < lev("gti", "gt"));
    assert_eq!(lev("gti", "gtp"), EDIT_WEIGHT); // substitution
    assert_eq!(lev("gti", "gt"), EDIT_WEIGHT); // deletion
  }

  // Regression for the transpose-leak bug: `ab -> aba` and `eco -> echo` are
  // plain insertions that happen to satisfy the local transposition predicate,
  // but the cheap transposition price must NOT leak onto a move that wasn't
  // actually a transposition. Both previously returned 1 instead of EDIT_WEIGHT.
  #[test]
  fn insertion_masquerading_as_transposition_is_full_weight() {
    assert_eq!(lev("ab", "aba"), EDIT_WEIGHT);
    assert_eq!(lev("eco", "echo"), EDIT_WEIGHT);
  }

  // ─── multi-edit ──────────────────────────────────────────────────

  #[test]
  fn kitten_sitting_is_three_edits() {
    // k→s, e→i, and insert g: three ordinary edits, no transposition.
    assert_eq!(lev("kitten", "sitting"), 3 * EDIT_WEIGHT);
  }

  // ─── metric properties ───────────────────────────────────────────

  #[test]
  fn distance_is_symmetric() {
    for (a, b) in [
      ("kitten", "sitting"),
      ("dokcer", "docker"),
      ("cat", "at"),
      ("ab", "aba"),
      ("gti", "git"),
      ("", "abc"),
    ] {
      assert_eq!(lev(a, b), lev(b, a), "asymmetric on {a:?} / {b:?}");
    }
  }
}

#[cfg(test)]
mod time_reader_tests {
  use super::TimeReader;
  use chrono::{Datelike, Days, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Utc};

  /// The local wall-clock time the parsed instant represents (timezone-independent).
  fn wall(expr: &str) -> NaiveDateTime {
    TimeReader::interpret(expr)
      .unwrap()
      .with_timezone(&Local)
      .naive_local()
  }

  /// Assert a relative expression lands `expected` before now, allowing for the
  /// time that elapses between the parse and this check.
  fn assert_ago(expr: &str, expected: Duration) {
    let got = TimeReader::interpret(expr).unwrap();
    let off = (Utc::now() - got - expected).num_milliseconds().abs();
    assert!(off < 2000, "{expr}: {off}ms off from expected");
  }

  fn ymd_hms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d)
      .unwrap()
      .and_hms_opt(h, mi, s)
      .unwrap()
  }

  // ─── parse_dur: exact, deterministic ─────────────────────────────

  #[test]
  fn dur_single_units() {
    assert_eq!(TimeReader::parse_dur("5s").unwrap(), 5 * 1_000_000);
    assert_eq!(
      TimeReader::parse_dur("30 minutes").unwrap(),
      30 * 60 * 1_000_000
    );
    assert_eq!(
      TimeReader::parse_dur("2 hours").unwrap(),
      2 * 3600 * 1_000_000
    );
    assert_eq!(TimeReader::parse_dur("1 day").unwrap(), 86_400 * 1_000_000);
  }

  #[test]
  fn dur_multi_unit() {
    assert_eq!(TimeReader::parse_dur("1h30m").unwrap(), 90 * 60 * 1_000_000);
    assert_eq!(
      TimeReader::parse_dur("1 day 3 hours").unwrap(),
      (86_400 + 3 * 3600) * 1_000_000
    );
  }

  #[test]
  fn dur_rejects_non_durations() {
    assert!(TimeReader::parse_dur("5").is_err()); // no unit
    assert!(TimeReader::parse_dur("2 days ago").is_err()); // "ago" isn't a duration
    assert!(TimeReader::parse_dur("5 potatoes").is_err()); // unknown unit
    assert!(TimeReader::parse_dur("bananas").is_err());
    assert!(TimeReader::parse_dur("").is_err());
  }

  // ─── interpret: relative offsets (delta from now) ────────────────

  #[test]
  fn interp_relative() {
    assert_ago("2 days ago", Duration::days(2));
    assert_ago("10 minutes ago", Duration::minutes(10));
    assert_ago("1 hour ago", Duration::hours(1));
    assert_ago("30 seconds ago", Duration::seconds(30));
    assert_ago("1h30m ago", Duration::minutes(90));
    assert_ago("5 days", Duration::days(5)); // bare offset defaults to the past
  }

  #[test]
  fn interp_now() {
    let got = TimeReader::interpret("now").unwrap();
    assert!((Utc::now() - got).num_milliseconds().abs() < 2000);
  }

  // ─── interpret: calendar anchors (local wall-clock) ──────────────

  #[test]
  fn interp_day_keywords() {
    let today = Local::now().date_naive();
    assert_eq!(wall("today").date(), today);
    assert_eq!(
      wall("today").time(),
      NaiveTime::from_hms_opt(0, 0, 0).unwrap()
    );
    assert_eq!(wall("yesterday").date(), today - Days::new(1));
    assert_eq!(wall("tomorrow").date(), today + Days::new(1));
  }

  #[test]
  fn interp_absolute() {
    assert_eq!(wall("2024-01-01 12:00:00"), ymd_hms(2024, 1, 1, 12, 0, 0));
    assert_eq!(wall("2024-06-15"), ymd_hms(2024, 6, 15, 0, 0, 0));
  }

  #[test]
  fn interp_named_date() {
    assert_eq!(wall("may 5 2024"), ymd_hms(2024, 5, 5, 0, 0, 0));
    let wc = wall("may 5");
    assert_eq!((wc.month(), wc.day()), (5, 5));
    assert_eq!(wc.year(), Local::now().year());
  }

  #[test]
  fn interp_offset_from_anchor() {
    assert_eq!(
      wall("5 days after may 5 2024"),
      ymd_hms(2024, 5, 10, 0, 0, 0)
    );
    assert_eq!(
      wall("5 days before may 5 2024"),
      ymd_hms(2024, 4, 30, 0, 0, 0)
    );
  }

  #[test]
  fn interp_rejects_garbage() {
    assert!(TimeReader::interpret("bananas").is_err());
    assert!(TimeReader::interpret("5 potatoes").is_err());
  }
}
