//! Human-readable parsing and formatting of durations, sizes, and file modes,
//! plus the natural-language [`TimeReader`].

use std::time::Duration;

use chrono::{
  DateTime, Datelike, Days, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc,
  Weekday,
};

use crate::{
  sherr,
  state::vars::VarStr,
  util::{Direction, error::ShResult},
};

use super::{ByteCursor, SliceCursor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalFormat {
  Iso,
  Eu,
  Us,
}
impl CalFormat {
  fn format(self, fields: [u32; 3]) -> Option<NaiveDate> {
    let (y, m, d) = match self {
      Self::Iso => (fields[0], fields[1], fields[2]),
      Self::Eu => (fields[2], fields[1], fields[0]),
      Self::Us => (fields[2], fields[0], fields[1]),
    };

    NaiveDate::from_ymd_opt(widen_year(y), m, d)
  }
}

/// Two-digit years follow the `strftime` convention
///
/// `00-68` are 2000s and `69-99` are 1900s.
fn widen_year(n: u32) -> i32 {
  match n {
    0..=68 => 2000 + n as i32,
    69..=99 => 1900 + n as i32,
    _ => n as i32,
  }
}

const CLOCK_FORMATS: [&str; 6] = [
  "%H:%M",       // 14:30
  "%H:%M:%S",    // 14:30:00
  "%I:%M%p",     // 02:30PM
  "%I:%M %p",    // 02:30 PM
  "%I:%M:%S%p",  // 02:30:00PM
  "%I:%M:%S %p", // 02:30:00 PM
];

const DATE_FORMATS: [&str; 1] = [
  "%Y-%m-%dT%H:%M:%S", // 2023-03-15T14:30:00
];

/// chrono's strftime rejects specifiers that glibc passes through untouched
/// (`%N`, `%Q`, ...), and its `Display` impl signals that by failing, which
/// `to_string()` turns into a panic. bash emits unknown specifiers literally,
/// so escape anything chrono can't parse into a `%%` sequence first.
fn escape_unknown_specs(format: &str) -> String {
  use chrono::format::{Item, StrftimeItems};

  const MODIFIERS: &[u8] = b"-_0^#:.";

  let bytes = format.as_bytes();
  let mut out = String::with_capacity(format.len());
  let mut i = 0;

  while i < bytes.len() {
    if bytes[i] != b'%' {
      let start = i;
      while i < bytes.len() && bytes[i] != b'%' {
        i += 1;
      }
      out.push_str(&format[start..i]);
      continue;
    }

    let start = i;
    i += 1;
    while i < bytes.len() && (MODIFIERS.contains(&bytes[i]) || bytes[i].is_ascii_digit()) {
      i += 1;
    }
    if i < bytes.len() {
      i += format[i..].chars().next().map_or(1, char::len_utf8);
    }

    let spec = &format[start..i];
    if StrftimeItems::new(spec).any(|item| matches!(item, Item::Error)) {
      out.push('%');
    }
    out.push_str(spec);
  }

  out
}

/// Parse a parenthesised strftime sub-format and its trailing `T`, as in
/// `printf`'s `%(%Y-%m-%d)T`. The cursor starts just past the `(`.
pub(crate) fn parse_paren_strftime(cur: &mut SliceCursor) -> ShResult<VarStr> {
  let mut fmt = Vec::new();
  loop {
    match cur.next_byte() {
      Some(b'\\') => {
        let Some(escaped) = cur.next_byte() else {
          return Err(sherr!(ParseErr, "unterminated strftime format"));
        };
        fmt.push(escaped);
      }
      Some(b')') => break,
      Some(b) => fmt.push(b),
      None => return Err(sherr!(ParseErr, "unterminated strftime format")),
    }
  }

  match cur.next_byte() {
    Some(b'T') => Ok(VarStr::from(fmt.as_slice())),
    Some(other) => Err(sherr!(
      ParseErr,
      "expected 'T' after strftime format, got '{}'",
      other as char
    )),
    None => Err(sherr!(
      ParseErr,
      "unterminated strftime conversion: expected 'T' after ')'"
    )),
  }
}

pub(crate) fn dur_delta(duration: Duration) -> TimeDelta {
  TimeDelta::from_std(duration).unwrap_or(TimeDelta::MAX)
}

/// Format `dt` with a user-supplied strftime string.
///
/// Tolerates specifiers chrono does not implement, and reports a genuine
/// formatting failure instead of panicking the way `to_string()` does.
pub(crate) fn strftime<Tz: TimeZone>(dt: &DateTime<Tz>, format: &str) -> ShResult<String>
where
  Tz::Offset: std::fmt::Display,
{
  use std::fmt::Write;

  let mut out = String::with_capacity(format.len());
  write!(out, "{}", dt.format(&escape_unknown_specs(format)))
    .map_err(|_| sherr!(ParseErr, "invalid strftime format '{format}'"))?;
  Ok(out)
}

pub(crate) fn format_time(delta: TimeDelta) -> Option<String> {
  const ETERNITY: u128 = f32::INFINITY as u128;
  let signed =
    i128::from(delta.num_seconds()) * 1_000_000 + i128::from(delta.subsec_nanos()) / 1_000;
  let negative = signed < 0;
  let mut micros = signed.unsigned_abs();
  let mut millis = 0;
  let mut seconds = 0;
  let mut minutes = 0;
  let mut hours = 0;
  let mut days = 0;
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
  // Divided out of the day count rather than chained, because 30 is not a
  // multiple of 7 and 365 is not a multiple of 30. These are the lengths
  // `TimeReader::parse_dur` gives `mo` and `y`, so the two round-trip.
  let mut years = days / 365;
  days %= 365;
  let months = days / 30;
  days %= 30;
  let weeks = days / 7;
  days %= 7;
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

  let joined = result.join(" ");
  if joined.is_empty() {
    None
  } else if negative {
    Some(format!("-{joined}"))
  } else {
    Some(joined)
  }
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
  Num(f64),
  Word(VarStr),
  Epoch(DateTime<Utc>),
  Clock(NaiveTime),
  Date(NaiveDate),
}

pub(crate) struct TimeReader<'a> {
  orig: &'a str,
  tks: Vec<TimeTk>,
  pos: usize,
  anchor: Option<DateTime<Utc>>,
  clock: Option<NaiveTime>,
  dir: Option<Direction>,
  offset: Option<i64>,
  pending: i64, // pending offset read
  upcoming: bool,
}

impl<'a> TimeReader<'a> {
  fn new(s: &'a str) -> Self {
    Self {
      orig: s,
      tks: vec![],
      pos: 0,
      anchor: None,
      clock: None,
      dir: None,
      offset: None,
      pending: 0,
      upcoming: false,
    }
  }
  pub(crate) fn interpret(s: &'a str) -> ShResult<DateTime<Utc>> {
    Self::new(s).parse()
  }

  pub(crate) fn interpret_upcoming(s: &'a str) -> ShResult<DateTime<Utc>> {
    // if a raw time is given like "5:30 pm", and it's 7 pm now,
    // this decides if we are talking about 5:30 pm tomorrow, or earlier today
    // in this case, we interpret it as "5:30 pm tomorrow"
    Self::new(s).upcoming().parse()
  }

  fn upcoming(self) -> Self {
    Self {
      upcoming: true,
      ..self
    }
  }

  fn next_tk(&mut self) -> Option<TimeTk> {
    let tk = self.tks.get(self.pos)?.clone();
    self.pos += 1;
    Some(tk)
  }

  fn peek_tk(&self) -> Option<&TimeTk> {
    self.tks.get(self.pos)
  }

  fn parse_epoch(s: &str) -> ShResult<DateTime<Utc>> {
    let bad = || sherr!(ParseErr, "invalid epoch timestamp '@{s}'");
    let (whole, frac) = s.split_once('.').map_or((s, None), |(a, b)| (a, Some(b)));

    let secs: i64 = whole.parse().map_err(|_| bad())?;
    let nanos: u32 = match frac {
      None => 0,
      Some(f) if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) => return Err(bad()),
      Some(f) => {
        let mut digits = f.as_bytes()[..f.len().min(9)].to_vec();
        digits.resize(9, b'0');
        String::from_utf8_lossy(&digits)
          .parse()
          .map_err(|_| bad())?
      }
    };

    // A fraction counts away from the epoch, so for a negative timestamp it
    // deepens the offset rather than easing it: `@-1.5` is 1.5s before 1970.
    let (secs, nanos) = if secs < 0 && nanos > 0 {
      (secs - 1, 1_000_000_000 - nanos)
    } else {
      (secs, nanos)
    };

    DateTime::from_timestamp(secs, nanos)
      .ok_or_else(|| sherr!(ParseErr, "epoch timestamp '@{s}' is out of range"))
  }

  pub(crate) fn parse(&mut self) -> ShResult<DateTime<Utc>> {
    if let Some(epoch) = self.orig.trim().strip_prefix('@') {
      return Self::parse_epoch(epoch);
    }

    for fmt in DATE_FORMATS {
      if let Ok(time) = NaiveDateTime::parse_from_str(self.orig, fmt) {
        return local_to_utc(time);
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
        TimeTk::Epoch(dt) => self.anchor = Some(dt),
        TimeTk::Clock(time) => self.clock = Some(time),
        // a calendar date names a day, which is what an anchor is
        TimeTk::Date(d) => self.anchor = Some(local_to_utc(d.and_hms_opt(0, 0, 0).unwrap())?),
      }
    }

    if self.pending != 0 {
      let dir = self.dir.unwrap_or(Direction::Backward);
      self.commit(dir);
    }

    let base = match (self.anchor, self.clock) {
      (Some(a), Some(t)) => {
        let date = a.with_timezone(&Local).date_naive();
        local_to_utc(date.and_time(t))?
      }
      (None, Some(t)) => {
        let mut when = Local::now().date_naive().and_time(t);
        if self.upcoming && when < Local::now().naive_local() {
          when += chrono::Duration::days(1);
        }
        local_to_utc(when)?
      }
      (Some(a), None) => a,
      (None, None) => Utc::now(),
    };
    let Some(micros) = self.offset else {
      return Ok(base);
    };
    Ok(base + chrono::Duration::microseconds(micros))
  }

  fn read_offset(&mut self, n: f64) -> ShResult<()> {
    let Some(TimeTk::Word(unit)) = self.next_tk() else {
      return Err(sherr!(ParseErr, "expected a unit after '{n}'"));
    };
    let Some(per) = Self::unit_micros(&unit) else {
      return Err(sherr!(ParseErr, "unknown unit '{unit}'"));
    };
    let scaled = Self::scale_f64(n, per)?;

    self.pending = self.pending.saturating_add(scaled);
    Ok(())
  }

  /// Fold the unsigned offset read so far into the running total.
  fn commit(&mut self, dir: Direction) {
    let signed = match dir {
      Direction::Forward => self.pending,
      Direction::Backward => self.pending.saturating_neg(),
    };
    self.offset = Some(self.offset.unwrap_or(0).saturating_add(signed));
    self.pending = 0;
  }

  fn read_word(&mut self, word: &VarStr) -> ShResult<()> {
    if let Some(month) = Self::month_num(word) {
      return self.read_named_date(word, month);
    }
    if let Some(dir) = Self::direction(word) {
      self.commit(dir);
      self.dir = Some(dir);
    } else if let Some(anchor) = Self::keyword_anchor(word, self.upcoming)? {
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
      Some(TimeTk::Num(y)) if *y >= 1000.0 => {
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

  #[rustfmt::skip]
  fn keyword_anchor(word: &VarStr, upcoming: bool) -> ShResult<Option<DateTime<Utc>>> {
    let today = Local::now().date_naive();
    let midnight = |d: NaiveDate| -> ShResult<DateTime<Utc>> {
      local_to_utc(d.and_hms_opt(0, 0, 0).unwrap())
    };
    if let Some(wd) = word.parse::<Weekday>() {
      let delta = i64::from(wd.num_days_from_monday())
                - i64::from(today.weekday().num_days_from_monday());
      let mut when = midnight(today + TimeDelta::days(delta))?;
      if upcoming && when <= Utc::now() {
        when = midnight(today + TimeDelta::days(delta + 7))?;
      }
      return Ok(Some(when));
    }

    Ok(match word.as_bytes() {
      b"now" => Some(Utc::now()),
      b"today" => Some(midnight(today)?),
      b"yesterday" => Some(midnight(today - Days::new(1))?),
      b"tomorrow" => Some(midnight(today + Days::new(1))?),
      _ => None,
    })
  }

  #[rustfmt::skip]
  fn month_num(word: &VarStr) -> Option<u32> {
    Some(match word.as_bytes() {
      b"jan" | b"january"  => 1,
      b"feb" | b"february" => 2,
      b"mar" | b"march"    => 3,
      b"apr" | b"april"    => 4,
      b"may"               => 5,
      b"jun" | b"june"     => 6,
      b"jul" | b"july"     => 7,
      b"aug" | b"august"   => 8,
      b"oct" | b"october"  => 10,
      b"nov" | b"november" => 11,
      b"dec" | b"december" => 12,
      b"sep"
      | b"sept"
      | b"september"       => 9,
      _                    => return None,
    })
  }

  fn direction(word: &VarStr) -> Option<Direction> {
    match word.as_bytes() {
      b"after" | b"since" | b"from" => Some(Direction::Forward),
      b"ago" | b"before" | b"til" | b"until" => Some(Direction::Backward),
      _ => None,
    }
  }
  #[rustfmt::skip]
  fn unit_micros(unit: &VarStr) -> Option<i64> {
    const MICROS: i64 = 1;
    const MILLIS: i64 = 1000 * MICROS;
    const SECOND: i64 = 1000 * MILLIS;
    const MINUTE: i64 = 60   * SECOND;
    const HOUR  : i64 = 60   * MINUTE;
    const DAY   : i64 = 24   * HOUR;
    const WEEK  : i64 = 7    * DAY;
    const MONTH : i64 = 30   * DAY; // approximate
    const YEAR  : i64 = 365  * DAY; // approximate

    match unit.as_bytes() {
      b"us" | b"micro" | b"micros" | b"microsecond" | b"microseconds" => Some(MICROS),
      b"ms" | b"milli" | b"millis" | b"millisecond" | b"milliseconds" => Some(MILLIS),
      b"s"  | b"sec"   | b"secs"   | b"second"      | b"seconds"      => Some(SECOND),
      b"m"  | b"min"   | b"mins"   | b"minute"      | b"minutes"      => Some(MINUTE),
      b"h"  | b"hr"    | b"hrs"    | b"hour"        | b"hours"        => Some(HOUR),
      b"d"  | b"day"   | b"days"                                      => Some(DAY),
      b"w"  | b"wk"    | b"wks"    | b"week"        | b"weeks"        => Some(WEEK),
      b"mo" | b"month" | b"months"                                    => Some(MONTH),
      b"y"  | b"yr"    | b"yrs"    | b"year"        | b"years"        => Some(YEAR),
      _                                                               => None,
    }
  }

  /// A calendar date written with separators: `2026-10-13`, `10/13/26`,
  /// `13.10.26`. Returns `None` when the text is not one, so the caller falls
  /// back to reading a plain number.
  ///
  /// Field order differs by locale, so it is settled in three steps: a
  /// four-digit field can only be a year; failing that the separator says
  /// which convention is meant, `-` ISO, `.` European, `/` American; and an
  /// ordering that names an impossible date is discarded, which is what
  /// rescues `13/10/26` for writers who put the day first.
  fn scan_calendar_date(s: &str, cur: &mut SliceCursor) -> Option<NaiveDate> {
    fn digits(s: &str, cur: &mut SliceCursor) -> Option<(u32, usize)> {
      let (start, end) = cur.bump_while_span(|b| b.is_ascii_digit());
      if start == end {
        return None;
      }
      s[start..end].parse().ok().map(|n| (n, end - start))
    }

    let (first, first_len) = digits(s, cur)?;
    let sep = cur
      .peek_byte()
      .filter(|b| matches!(b, b'-' | b'.' | b'/'))?;
    cur.bump();

    let (second, _) = digits(s, cur)?;
    if !cur.bump_if_eq(sep) {
      return None; // the two separators must agree
    }
    let (third, third_len) = digits(s, cur)?;

    // candidate (year, month, day) formats, best guess first
    let orders: &[CalFormat] = if first_len == 4 {
      &[CalFormat::Iso]
    } else if third_len == 4 {
      match sep {
        b'.' => &[CalFormat::Eu],
        _ => &[CalFormat::Us],
      }
    } else {
      match sep {
        b'.' => &[CalFormat::Eu, CalFormat::Us],
        b'/' => &[CalFormat::Us, CalFormat::Eu],
        _ => &[CalFormat::Iso, CalFormat::Us, CalFormat::Eu],
      }
    };

    let fields = [first, second, third];
    orders.iter().find_map(|order| order.format(fields))
  }

  fn scan_clock_time(s: &str, cur: &mut SliceCursor) -> Option<NaiveTime> {
    let start = cur.pos();
    if !cur.bump_if(|b| b.is_ascii_digit()) {
      return None;
    }
    cur.bump_while(|b| b.is_ascii_digit());

    let mut groups = 0;
    while groups < 2 {
      let res = cur.attempt(|cur| {
        if !cur.bump_if_eq(b':') {
          return false;
        }

        if !cur.bump_if(|b| b.is_ascii_digit()) {
          return false;
        }
        cur.bump_while(|b| b.is_ascii_digit());

        true
      });

      if res {
        groups += 1;
      } else {
        break;
      }
    }

    let digit_end = cur.pos();

    let suffix = cur.attempt(|cur| {
      cur.bump_while(|b| b.is_ascii_whitespace());
      let (w_start, w_end) = cur.bump_while_span(|b| b.is_ascii_alphabetic());
      s.get(w_start..w_end)
        .is_some_and(|w| w.eq_ignore_ascii_case("am") || w.eq_ignore_ascii_case("pm"))
    });

    if groups == 0 && !suffix {
      return None;
    }

    let end = cur.pos();

    let text = if groups == 0 {
      format!("{}:00{}", &s[start..digit_end], &s[digit_end..end])
    } else {
      s[start..end].to_string()
    };

    CLOCK_FORMATS
      .iter()
      .find_map(|f| NaiveTime::parse_from_str(&text, f).ok())
  }
  fn tokenize(s: &str) -> ShResult<Vec<TimeTk>> {
    let mut cur = SliceCursor::new(s.as_bytes());
    let mut tks = vec![];

    loop {
      cur.bump_while(|c| c.is_ascii_whitespace());
      match cur.peek_byte() {
        Some(c) if c.is_ascii_digit() => {
          let start = cur.pos();
          if let Some(time) = cur.attempt_get(|cur| Self::scan_clock_time(s, cur)) {
            tks.push(TimeTk::Clock(time));
          } else if let Some(date) = cur.attempt_get(|cur| Self::scan_calendar_date(s, cur)) {
            tks.push(TimeTk::Date(date));
          } else {
            cur.bump_while(|c| c.is_ascii_digit());
            if cur.bump_if_eq(b'.') {
              cur.bump_while(|c| c.is_ascii_digit());
            }

            let n = s[start..cur.pos()]
              .parse()
              .map_err(|_| sherr!(ParseErr, "number too large in time expression"))?;
            tks.push(TimeTk::Num(n));
          }
        }
        Some(b'@') => {
          cur.bump();
          let (start, end) = cur.bump_while_span(|c| c.is_ascii_digit() || c == b'-' || c == b'.');
          let epoch_secs = Self::parse_epoch(&s[start..end])?;
          let tk = TimeTk::Epoch(epoch_secs);
          tks.push(tk);
        }
        Some(c) if c.is_ascii_alphabetic() => {
          let (start, end) = cur.bump_while_span(|c| c.is_ascii_alphabetic());
          let word = s[start..end].to_ascii_lowercase();
          tks.push(TimeTk::Word(word.as_str().into()));
        }
        Some(_) => cur.bump(),
        None => break,
      }
    }

    Ok(tks)
  }
  fn scale_f64(n: f64, per: i64) -> ShResult<i64> {
    let scaled = n * per as f64;
    if !scaled.is_finite() || scaled.abs() >= i64::MAX as f64 {
      return Err(sherr!(ParseErr, "time expression too large"));
    }
    Ok(scaled.round() as i64)
  }
  /// `A to B` where both sides name an instant. Matched as a whole word --
  /// `october`, `today` and `tomorrow` all contain `to`.
  fn split_span(s: &str) -> Option<(String, String)> {
    let words: Vec<&str> = s.split_whitespace().collect();
    let i = words.iter().position(|w| *w == "to")?;
    if i == 0 || i + 1 == words.len() {
      return None;
    }
    Some((words[..i].join(" "), words[i + 1..].join(" ")))
  }

  /// Parse a duration like "1m 30s" or something
  ///
  /// Returns the duration as microseconds if it succeeds
  pub(crate) fn parse_dur(s: &str) -> ShResult<i64> {
    if let Some((lhs, mut rhs)) = Self::split_span(s) {
      let mut instants = vec![TimeReader::interpret(&lhs)?];
      while let Some((sub_lhs, sub_rhs)) = Self::split_span(&rhs) {
        instants.push(TimeReader::interpret(&sub_lhs)?);
        rhs = sub_rhs;
      }
      instants.push(TimeReader::interpret(&rhs)?);

      let total: TimeDelta = instants.windows(2).map(|w| w[1] - w[0]).sum();

      return total
        .num_microseconds()
        .ok_or_else(|| sherr!(ParseErr, "span in '{s}' is too large"));
    }

    let mut tks = Self::tokenize(s)?.into_iter().peekable();
    let mut total: i64 = 0;
    let mut saw_any = false;

    while let Some(tk) = tks.next() {
      match tk {
        TimeTk::Num(n) => {
          let Some(TimeTk::Word(unit)) = tks.next() else {
            return Err(
              sherr!(ParseErr, "expected a unit after '{n}'")
                .with_note("e.g. '10s', '100ms', '1m 30s', etc".into()),
            );
          };
          let Some(per) = Self::unit_micros(&unit) else {
            return Err(sherr!(ParseErr, "unknown unit '{unit}'"));
          };
          let scaled = Self::scale_f64(n, per)?;
          total = total
            .checked_add(scaled)
            .ok_or_else(|| sherr!(ParseErr, "duration too large"))?;
          saw_any = true;
        }
        TimeTk::Word(w) => return Err(sherr!(ParseErr, "unexpected '{w}' in duration")),
        TimeTk::Clock(_) => return Err(sherr!(ParseErr, "a clock time is not a duration")),
        TimeTk::Epoch(_) => return Err(sherr!(ParseErr, "a timestamp is not a duration")),
        TimeTk::Date(_) => return Err(sherr!(ParseErr, "a date is not a duration")),
      }
    }

    if !saw_any {
      return Err(sherr!(ParseErr, "invalid duration '{s}'"));
    }
    Ok(total)
  }
}

#[cfg(test)]
mod format_time_tests {
  use chrono::TimeDelta;

  /// Tests read better in `Duration`; the function takes a signed delta.
  fn format_time(d: std::time::Duration) -> String {
    super::format_time(TimeDelta::from_std(d).unwrap()).unwrap_or_default()
  }
  use std::time::Duration;

  // ─── single-unit base cases ──────────────────────────────────────

  #[test]
  fn negative_delta_is_signed() {
    assert_eq!(
      super::format_time(TimeDelta::seconds(-90)).unwrap_or_default(),
      "-1m 30s"
    );
    assert_eq!(
      super::format_time(TimeDelta::days(-1)).unwrap_or_default(),
      "-1 day"
    );
  }

  #[test]
  fn zero_is_unsigned_either_way() {
    assert_eq!(
      super::format_time(TimeDelta::zero()).unwrap_or_default(),
      ""
    );
    assert_eq!(
      super::format_time(TimeDelta::microseconds(-0)).unwrap_or_default(),
      ""
    );
  }

  #[test]
  fn units_round_trip_with_parse_dur() {
    // `mo` and `y` must mean the same length in both directions, or
    // `chrono fmt -d 1y` humanises back as something other than "1 year".
    for unit in ["1 week", "1mo", "1y", "3mo", "2y"] {
      let micros = super::TimeReader::parse_dur(unit).unwrap();
      let back = super::format_time(TimeDelta::microseconds(micros)).unwrap_or_default();
      let expect = match unit {
        "1 week" => "1 week",
        "1mo" => "1 month",
        "1y" => "1 year",
        "3mo" => "3 months",
        "2y" => "2 years",
        _ => unreachable!(),
      };
      assert_eq!(back, expect, "{unit} did not round-trip");
    }
  }

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
    assert_eq!(format_time(Duration::from_hours(720)), "1 month");
  }

  #[test]
  fn one_year() {
    // ... and a year as 12 months.
    assert_eq!(format_time(Duration::from_hours(8760)), "1 year");
  }

  #[test]
  fn one_decade() {
    assert_eq!(format_time(Duration::from_hours(87_600)), "1 decade");
  }

  #[test]
  fn one_century() {
    assert_eq!(format_time(Duration::from_hours(876_000)), "1 century");
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
    assert_eq!(format_time(Duration::from_hours(1_752_000)), "2 centuries");
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
    let dur = Duration::from_hours(9480);
    assert_eq!(format_time(dur), "1 year 1 month");
  }

  #[test]
  fn singular_millennium_is_singular() {
    let dur = Duration::from_hours(8_760_000);
    assert!(
      format_time(dur).contains("1 millennium"),
      "got {:?}",
      format_time(dur)
    );
  }

  #[test]
  fn plural_millennia_is_plural() {
    let dur = Duration::from_hours(17_520_000);
    assert!(
      format_time(dur).contains("2 millennia"),
      "got {:?}",
      format_time(dur)
    );
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

  fn ymd(y: i32, mo: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, mo, d).unwrap()
  }

  fn ymd_hms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d)
      .unwrap()
      .and_hms_opt(h, mi, s)
      .unwrap()
  }

  // ─── calendar dates ──────────────────────────────────────────────

  #[test]
  fn date_four_digit_field_is_the_year() {
    // Unambiguous regardless of separator.
    for src in ["2026-10-13", "2026/10/13", "2026.10.13"] {
      assert_eq!(wall(src).date(), ymd(2026, 10, 13), "{src}");
    }
    // ... and a trailing four-digit year keeps the separator's convention
    assert_eq!(wall("10/13/2026").date(), ymd(2026, 10, 13));
    assert_eq!(wall("13.10.2026").date(), ymd(2026, 10, 13));
  }

  #[test]
  fn date_separator_picks_the_convention() {
    // The same day written each way. `/` is American, `.` European.
    assert_eq!(wall("10/12/26").date(), ymd(2026, 10, 12));
    assert_eq!(wall("12.10.26").date(), ymd(2026, 10, 12));
  }

  #[test]
  fn date_impossible_month_forces_the_other_order() {
    // 13 cannot be a month, so these are day-first whatever the separator.
    assert_eq!(wall("13/10/26").date(), ymd(2026, 10, 13));
    assert_eq!(wall("10-13-26").date(), ymd(2026, 10, 13));
    // the old separator loop read this as the year 13
    assert_eq!(wall("13.10.26").date(), ymd(2026, 10, 13));
  }

  #[test]
  fn date_two_digit_years_follow_strftime() {
    assert_eq!(wall("1/1/68").date(), ymd(2068, 1, 1));
    assert_eq!(wall("1/1/69").date(), ymd(1969, 1, 1));
  }

  #[test]
  fn date_composes_with_a_clock_in_either_order() {
    let expect = ymd(2026, 10, 13).and_hms_opt(17, 0, 0).unwrap();
    assert_eq!(wall("10/13/26 5:00pm"), expect);
    assert_eq!(wall("5:00pm 10/13/26"), expect);
    assert_eq!(wall("2026-10-13 09:00").date(), ymd(2026, 10, 13));
  }

  #[test]
  fn date_scanner_leaves_other_things_alone() {
    for src in ["1h30m", "5 minutes", "17:00", "2 hours ago"] {
      let tks = TimeReader::tokenize(src).unwrap();
      assert!(
        !tks.iter().any(|t| matches!(t, super::TimeTk::Date(_))),
        "{src} should contain no date token"
      );
    }
  }

  #[test]
  fn date_rejects_mismatched_separators() {
    assert!(TimeReader::interpret("2026-10/13").is_err());
  }

  // ─── tokenize: clock times ───────────────────────────────────────

  #[test]
  fn tokenize_clock_does_not_swallow_the_next_word() {
    // The meridiem probe reads ahead; when what follows is not am/pm the
    // cursor must go back so the word survives as its own token.
    let tks = TimeReader::tokenize("17:00 tomorrow").unwrap();
    assert_eq!(tks.len(), 2, "expected a clock and a word");
    assert!(matches!(tks[0], super::TimeTk::Clock(_)));
    match &tks[1] {
      super::TimeTk::Word(w) => assert_eq!(w.to_str_lossy(), "tomorrow"),
      _ => panic!("second token should be the word"),
    }
  }

  #[test]
  fn tokenize_clock_reads_every_spelling() {
    use chrono::Timelike;
    for (src, hour, min) in [
      ("17:00", 17, 0),
      ("17:00:30", 17, 0),
      ("5:00pm", 17, 0),
      ("5:00 pm", 17, 0),
      ("5:00PM", 17, 0),
      ("5:00:00pm", 17, 0),
      ("5pm", 17, 0),
      ("5 pm", 17, 0),
      ("5PM", 17, 0),
      ("9:30am", 9, 30),
    ] {
      let tks = TimeReader::tokenize(src).unwrap();
      match tks.first() {
        Some(super::TimeTk::Clock(t)) => {
          assert_eq!((t.hour(), t.minute()), (hour, min), "{src}");
        }
        other => panic!("{src} did not tokenize as a clock: {}", other.is_some()),
      }
    }
  }

  #[test]
  fn tokenize_leaves_plain_numbers_alone() {
    // A bare run of digits is a count, and a colon that leads nowhere is not
    // a time -- both must fall through rather than erroring.
    for src in ["5 minutes", "5", "1h30m", "2 hours ago"] {
      let tks = TimeReader::tokenize(src).unwrap();
      assert!(
        !tks.iter().any(|t| matches!(t, super::TimeTk::Clock(_))),
        "{src} should contain no clock token"
      );
    }
  }

  // ─── interpret: epoch timestamps ─────────────────────────────────

  #[test]
  fn interp_epoch() {
    let at = |s: i64, n: u32| chrono::DateTime::from_timestamp(s, n).unwrap();
    assert_eq!(TimeReader::interpret("@0").unwrap(), at(0, 0));
    assert_eq!(
      TimeReader::interpret("@1000000000").unwrap(),
      at(1_000_000_000, 0)
    );
    assert_eq!(TimeReader::interpret("@-1").unwrap(), at(-1, 0));
  }

  #[test]
  fn interp_epoch_fractional() {
    let at = |s: i64, n: u32| chrono::DateTime::from_timestamp(s, n).unwrap();
    assert_eq!(TimeReader::interpret("@1.5").unwrap(), at(1, 500_000_000));
    assert_eq!(
      TimeReader::interpret("@1.123456789").unwrap(),
      at(1, 123_456_789)
    );
    // a fraction counts away from the epoch, so -1.5 is earlier than -1
    assert_eq!(TimeReader::interpret("@-1.5").unwrap(), at(-2, 500_000_000));
  }

  #[test]
  fn interp_epoch_is_an_anchor() {
    // it composes with offsets like any other anchor
    let at = |s: i64| chrono::DateTime::from_timestamp(s, 0).unwrap();
    assert_eq!(
      TimeReader::interpret("2 hours after @0").unwrap(),
      at(7_200)
    );
    assert_eq!(
      TimeReader::interpret("1 hour before @7200").unwrap(),
      at(3_600)
    );
  }

  #[test]
  fn interp_epoch_rejects_garbage() {
    for bad in ["@", "@abc", "@1.x", "@99999999999999999999"] {
      assert!(
        TimeReader::interpret(bad).is_err(),
        "{bad} should not parse"
      );
    }
  }

  #[test]
  fn dur_rejects_a_timestamp() {
    assert!(TimeReader::parse_dur("@100").is_err());
  }

  // ─── parse_dur: spans between two instants ───────────────────────

  #[test]
  fn dur_span_between_instants() {
    let hour = 3_600 * 1_000_000;
    assert_eq!(TimeReader::parse_dur("9:00am to 5:00pm").unwrap(), 8 * hour);
    assert_eq!(
      TimeReader::parse_dur("october 5 2024 to october 10 2024").unwrap(),
      5 * 24 * hour
    );
  }

  #[test]
  fn dur_span_is_signed() {
    let hour = 3_600 * 1_000_000;
    assert_eq!(
      TimeReader::parse_dur("5:00pm to 9:00am").unwrap(),
      -8 * hour
    );
  }

  #[test]
  fn dur_span_does_not_split_inside_words() {
    // `october`, `today` and `tomorrow` all contain "to".
    assert!(TimeReader::parse_dur("tomorrow").is_err());
    assert!(TimeReader::interpret("tomorrow").is_ok());
    assert!(TimeReader::interpret("october 5 2024").is_ok());
    // a bare or edge-positioned `to` is not a span
    assert!(TimeReader::parse_dur("to").is_err());
    assert!(TimeReader::parse_dur("5 minutes to").is_err());
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
  fn interp_mixed_directions() {
    // A direction word signs only the offset that preceded it, so the two
    // halves of a mixed expression cancel instead of summing under one sign.
    assert_ago("5 days before 7 days from now", Duration::days(-2));
    assert_ago("5 days from now 3 days ago", Duration::days(-2));
    assert_ago("1 hour from 30 minutes ago", Duration::minutes(-30));
    assert_ago("3 days ago from now", Duration::days(3));
  }

  #[test]
  fn interp_chained_same_direction() {
    // Several segments agreeing on a direction still sum.
    assert_ago("5 days from 7 days from now", Duration::days(-12));
    assert_ago("2 hours ago 30 minutes ago", Duration::minutes(150));
  }

  #[test]
  fn interp_trailing_offset_takes_last_direction() {
    // An offset with no direction word after it follows the last one seen,
    // falling back to the past when there was none.
    assert_ago("from now 5 days", Duration::days(-5));
    assert_ago("5 days", Duration::days(5));
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
