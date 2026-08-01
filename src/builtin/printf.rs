use std::{iter::Peekable, str::Chars};

use bitflags::bitflags;

use crate::{
  ShResult, match_loop, out, sherr,
  state::vars::VarStr,
  util::{self, with_status},
};

bitflags! {
  #[derive(Debug, Clone, Copy)]
  pub struct PrintFlags: u8 {
    const JUST_LEFT = 1;
    const SHOW_SIGN = 1 << 1;
    const SPACE_SIGN = 1 << 2;
    const ALT_FORM = 1 << 3;
    const ZERO_PAD = 1 << 4;
  }
}

enum Case {
  Lower,
  Upper,
}

enum Conversion {
  Percent,
  SignedDecimal,
  UnsignedDecimal,
  UnsignedOctal,
  UnsignedHex(Case),
  FixedPointDecimal,
  Scientific(Case),
  ShortestFloat(Case),
  Char,
  Str,
  RepeatStr,
  AnsiC,
  ShellQuote,
  StrfTime(VarStr),
}

#[derive(Debug, Clone, Copy)]
enum DynNum {
  Number(i32),
  Star,
}

pub struct PrintFormatter(Box<[Segment]>);

impl PrintFormatter {
  pub fn parse(fmt_str: &str) -> ShResult<Self> {
    let mut segments = vec![];
    let mut chars = fmt_str.chars().peekable();
    let mut literal = util::scratch_buf();

    match_loop!(chars.next() => ch, {
      '%' => {
        if !literal.is_empty() {
          let lit: String = std::mem::take(&mut literal).into();
          let expanded = util::expand_ansi_c(&lit);
          segments.push(Segment::Literal(expanded));
        }

        let spec = FmtSpec::parse(&mut chars);
        segments.push(Segment::Spec(spec?));
      }
      _ => literal.push(ch),
    });

    if !literal.is_empty() {
      let lit: String = std::mem::take(&mut literal).into();
      let expanded = util::expand_ansi_c(&lit);
      segments.push(Segment::Literal(expanded));
    }

    Ok(Self(segments.into_boxed_slice()))
  }

  pub fn apply_once<I: Iterator<Item = String>>(
    &self,
    args: &mut Peekable<I>,
  ) -> ShResult<Rendered> {
    let mut out = Rendered::new(String::new());
    for seg in &self.0 {
      match seg {
        Segment::Literal(s) => out.text.push_str(s),
        Segment::Spec(spec) => {
          let rendered = spec.apply(args)?;
          out.text.push_str(&rendered.text);
          out.merge_errors(rendered);
        }
      }
    }
    Ok(out)
  }

  pub fn has_specs(&self) -> bool {
    self.0.iter().any(|s| matches!(s, Segment::Spec(_)))
  }
}

enum Segment {
  Literal(String),
  Spec(FmtSpec),
}

pub struct FmtSpec {
  flags: PrintFlags,
  width: Option<DynNum>,
  precision: Option<DynNum>,
  conversion: Conversion,
}

/// Parse a printf numeric argument, returning the value and any soft error.
/// A *missing* argument (fewer args than conversions) yields `0` with no error;
/// a *present* argument that fails to parse yields `0` plus a
/// [`PrintfErr::BadNumber`], so the caller still substitutes `0` and continues
/// formatting while the run is flagged to exit non-zero.
fn parse_num_arg<T: std::str::FromStr + Default>(arg: Option<String>) -> (T, Option<PrintfErr>) {
  let Some(arg) = arg else {
    return (T::default(), None);
  };
  match arg.parse() {
    Ok(v) => (v, None),
    Err(_) => (T::default(), Some(PrintfErr::BadNumber(arg))),
  }
}

/// Write the stderr diagnostic for each collected printf error. Returns whether
/// any were present, so the caller can set a non-zero exit status.
fn emit_printf_errors(errors: &[PrintfErr]) -> bool {
  for err in errors {
    match err {
      PrintfErr::BadNumber(arg) => crate::errln!("printf: {arg}: invalid number"),
    }
  }
  !errors.is_empty()
}

impl FmtSpec {
  pub fn parse(chars: &mut Peekable<Chars>) -> ShResult<Self> {
    Ok(Self {
      flags: Self::parse_flags(chars)?,
      width: Self::parse_width(chars)?,
      precision: Self::parse_precision(chars)?,
      conversion: Self::parse_conversion(chars)?,
    })
  }

  pub fn apply<I: Iterator<Item = String>>(&self, args: &mut Peekable<I>) -> ShResult<Rendered> {
    // Resolve dynamic width/precision. Negative width means left-justify
    // with abs(width); negative precision is treated as absent.
    let (flags, width) = self.resolve_width(args)?;
    let prec = self.resolve_precision(args)?;

    // Numeric conversions return a `Rendered` (they may report a bad number);
    // the rest produce plain text and are wrapped with `Rendered::new`.
    let out = match &self.conversion {
      Conversion::Percent => Rendered::new("%".into()),
      Conversion::SignedDecimal => Self::apply_signed_int(args, flags, width, prec)?,
      Conversion::UnsignedDecimal => Self::apply_unsigned_int(args, flags, width, prec)?,
      Conversion::UnsignedOctal => Self::apply_unsigned_octal(args, flags, width, prec)?,
      Conversion::UnsignedHex(case) => Self::apply_unsigned_hex(args, flags, width, prec, case)?,
      Conversion::FixedPointDecimal => Self::apply_fixed_float(args, flags, width, prec)?,
      Conversion::Scientific(case) => Self::apply_scientific(args, flags, width, prec, case)?,
      Conversion::ShortestFloat(case) => {
        Self::apply_shortest_float(args, flags, width, prec, case)?
      }
      Conversion::Char => Rendered::new(Self::apply_char(args, flags, width)?),
      Conversion::Str => Rendered::new(Self::apply_str(args, flags, width, prec)?),
      Conversion::RepeatStr => Rendered::new(Self::apply_repeat_str(args, width)?),
      Conversion::AnsiC => Rendered::new(Self::apply_ansi_c(args, flags, width, prec)?),
      Conversion::ShellQuote => Rendered::new(Self::apply_shell_quote(args, flags, width)?),
      Conversion::StrfTime(format) => {
        Rendered::new(Self::apply_strftime(args, flags, width, format.as_str())?)
      }
    };

    Ok(out)
  }

  fn resolve_width<I: Iterator<Item = String>>(
    &self,
    args: &mut Peekable<I>,
  ) -> ShResult<(PrintFlags, Option<usize>)> {
    let raw = match self.width {
      Some(DynNum::Star) => Some(Self::parse_int_arg(args)?),
      Some(DynNum::Number(n)) => Some(n),
      None => None,
    };
    match raw {
      Some(n) if n < 0 => Ok((
        self.flags | PrintFlags::JUST_LEFT,
        Some(n.unsigned_abs() as usize),
      )),
      Some(n) => Ok((self.flags, Some(n as usize))),
      None => Ok((self.flags, None)),
    }
  }

  fn resolve_precision<I: Iterator<Item = String>>(
    &self,
    args: &mut Peekable<I>,
  ) -> ShResult<Option<usize>> {
    let raw = match self.precision {
      Some(DynNum::Star) => Some(Self::parse_int_arg(args)?),
      Some(DynNum::Number(n)) => Some(n),
      None => None,
    };
    match raw {
      Some(n) if n < 0 => Ok(None),
      Some(n) => Ok(Some(n as usize)),
      None => Ok(None),
    }
  }

  fn apply_signed_int<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    prec: Option<usize>,
  ) -> ShResult<Rendered> {
    let (n, err): (i64, _) = parse_num_arg(args.next());
    let abs = n.unsigned_abs();
    let sign = pick_sign(n.is_negative(), flags);

    let mut digits = abs.to_string();
    if let Some(p) = prec {
      while digits.chars().count() < p {
        digits.insert(0, '0');
      }
    }

    Ok(Rendered {
      text: pad_to_width(&digits, sign, flags, width, prec.is_none()),
      errors: err.into_iter().collect(),
    })
  }

  fn apply_unsigned_int<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    prec: Option<usize>,
  ) -> ShResult<Rendered> {
    let (n, err): (u64, _) = parse_num_arg(args.next());

    let mut digits = n.to_string();
    if let Some(p) = prec {
      while digits.chars().count() < p {
        digits.insert(0, '0');
      }
    }

    Ok(Rendered {
      text: pad_to_width(&digits, "", flags, width, prec.is_none()),
      errors: err.into_iter().collect(),
    })
  }

  fn apply_unsigned_octal<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    prec: Option<usize>,
  ) -> ShResult<Rendered> {
    let (n, err): (u64, _) = parse_num_arg(args.next());

    let mut digits = format!("{n:o}");
    if let Some(p) = prec {
      while digits.chars().count() < p {
        digits.insert(0, '0');
      }
    }

    // # flag for %o: ensure at least one leading 0.
    let prefix = if flags.contains(PrintFlags::ALT_FORM) && !digits.starts_with('0') {
      "0"
    } else {
      ""
    };

    Ok(Rendered {
      text: pad_to_width(&digits, prefix, flags, width, prec.is_none()),
      errors: err.into_iter().collect(),
    })
  }

  fn apply_unsigned_hex<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    prec: Option<usize>,
    case: &Case,
  ) -> ShResult<Rendered> {
    let (n, err): (u64, _) = parse_num_arg(args.next());

    let mut digits = match case {
      Case::Lower => format!("{n:x}"),
      Case::Upper => format!("{n:X}"),
    };
    if let Some(p) = prec {
      while digits.chars().count() < p {
        digits.insert(0, '0');
      }
    }

    // # flag for %x/%X: prepend 0x/0X for non-zero values.
    let prefix = if flags.contains(PrintFlags::ALT_FORM) && n != 0 {
      match case {
        Case::Lower => "0x",
        Case::Upper => "0X",
      }
    } else {
      ""
    };

    Ok(Rendered {
      text: pad_to_width(&digits, prefix, flags, width, prec.is_none()),
      errors: err.into_iter().collect(),
    })
  }

  fn apply_fixed_float<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    prec: Option<usize>,
  ) -> ShResult<Rendered> {
    let (f, err): (f64, _) = parse_num_arg(args.next());
    let p = prec.unwrap_or(6);

    let body = format!("{f:.p$}");
    let abs_body = body.trim_start_matches('-').to_string();
    let sign = pick_sign(f.is_sign_negative() && f != 0.0, flags);

    // For floats, ZERO_PAD applies independent of precision (precision
    // controls digits after decimal point, not minimum total digits).
    Ok(Rendered {
      text: pad_to_width(&abs_body, sign, flags, width, true),
      errors: err.into_iter().collect(),
    })
  }

  fn apply_scientific<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    prec: Option<usize>,
    case: &Case,
  ) -> ShResult<Rendered> {
    let (f, err): (f64, _) = parse_num_arg(args.next());
    let p = prec.unwrap_or(6);

    let raw = match case {
      Case::Lower => format!("{f:.p$e}"),
      Case::Upper => format!("{f:.p$E}"),
    };
    let normalized = normalize_exponent(&raw);
    let abs_body = normalized.trim_start_matches('-').to_string();
    let sign = pick_sign(f.is_sign_negative() && f != 0.0, flags);

    Ok(Rendered {
      text: pad_to_width(&abs_body, sign, flags, width, true),
      errors: err.into_iter().collect(),
    })
  }

  fn apply_shortest_float<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    prec: Option<usize>,
    case: &Case,
  ) -> ShResult<Rendered> {
    let (f, err): (f64, _) = parse_num_arg(args.next());
    // %g: precision is number of significant digits (default 6, minimum 1).
    let p = prec.unwrap_or(6).max(1);

    // POSIX %g: use scientific when exponent < -4 or >= precision.
    let abs = f.abs();
    let exp = if abs == 0.0 {
      0i32
    } else {
      abs.log10().floor() as i32
    };
    let use_scientific = exp < -4 || exp >= p as i32;

    let body = if use_scientific {
      let mantissa_prec = p.saturating_sub(1);
      let raw = match case {
        Case::Lower => format!("{f:.mantissa_prec$e}"),
        Case::Upper => format!("{f:.mantissa_prec$E}"),
      };
      let normalized = normalize_exponent(&raw);
      if flags.contains(PrintFlags::ALT_FORM) {
        normalized
      } else {
        strip_trailing_zeros(&normalized)
      }
    } else {
      let fp = (p as i32 - 1 - exp).max(0) as usize;
      let raw = format!("{f:.fp$}");
      if flags.contains(PrintFlags::ALT_FORM) {
        raw
      } else {
        strip_trailing_zeros(&raw)
      }
    };
    let abs_body = body.trim_start_matches('-').to_string();
    let sign = pick_sign(f.is_sign_negative() && f != 0.0, flags);

    Ok(Rendered {
      text: pad_to_width(&abs_body, sign, flags, width, true),
      errors: err.into_iter().collect(),
    })
  }

  fn apply_char<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
  ) -> ShResult<String> {
    let arg = args.next().unwrap_or_default();
    // POSIX %c: take first character of the argument.
    let c: String = arg.chars().take(1).collect();
    Ok(pad_to_width(&c, "", flags, width, false))
  }

  fn apply_str<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    prec: Option<usize>,
  ) -> ShResult<String> {
    let s = args.next().unwrap_or_default();
    let s = match prec {
      Some(p) => s.chars().take(p).collect::<String>(),
      None => s,
    };
    Ok(pad_to_width(&s, "", flags, width, false))
  }

  fn apply_repeat_str<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    width: Option<usize>,
  ) -> ShResult<String> {
    // The width slot is the repeat count (`%*r` / `%5r`), not a field width, so
    // there is no padding. A bare `%r` with no count degrades to a single copy.
    let s = args.next().unwrap_or_default();
    Ok(s.repeat(width.unwrap_or(1)))
  }

  fn apply_ansi_c<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    prec: Option<usize>,
  ) -> ShResult<String> {
    let s = args.next().unwrap_or_default();
    let expanded = util::expand_ansi_c(&s);
    let truncated = match prec {
      Some(p) => expanded.chars().take(p).collect::<String>(),
      None => expanded,
    };
    Ok(pad_to_width(&truncated, "", flags, width, false))
  }

  fn apply_shell_quote<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
  ) -> ShResult<String> {
    let s = args.next().unwrap_or_default();
    let quoted = crate::expand::shell_quote(&s);
    Ok(pad_to_width(&quoted, "", flags, width, false))
  }

  fn apply_strftime<I: Iterator<Item = String>>(
    args: &mut Peekable<I>,
    flags: PrintFlags,
    width: Option<usize>,
    format: &str,
  ) -> ShResult<String> {
    use crate::state::{Shed, meta::MetaTab};
    use chrono::{Local, TimeZone};
    let arg = args.next().unwrap_or_else(|| "-1".to_string());
    let secs: i64 = arg.parse().unwrap_or(-1);

    let dt = if secs == -1 {
      // Current time
      Local::now()
    } else if secs == -2 {
      // Shell start time: convert the monotonic Instant we recorded at
      // startup into a wall-clock time by subtracting its elapsed duration
      // from "now".
      let shell_start_instant = Shed::meta(MetaTab::shell_time);
      let elapsed = shell_start_instant.elapsed();
      let now = Local::now();
      chrono::Duration::from_std(elapsed)
        .ok()
        .and_then(|d| now.checked_sub_signed(d))
        .unwrap_or(now)
    } else if secs >= 0 {
      Local
        .timestamp_opt(secs, 0)
        .single()
        .unwrap_or_else(Local::now)
    } else {
      Local::now()
    };

    let formatted = dt.format(format).to_string();
    Ok(pad_to_width(&formatted, "", flags, width, false))
  }

  fn parse_int_arg<I: Iterator<Item = String>>(args: &mut Peekable<I>) -> ShResult<i32> {
    // Missing or non-numeric args default to 0 (matches bash printf).
    let Some(arg) = args.next() else {
      return Ok(0);
    };
    Ok(arg.parse::<i32>().unwrap_or(0))
  }

  fn parse_flags(chars: &mut Peekable<Chars>) -> ShResult<PrintFlags> {
    let mut flags = PrintFlags::empty();
    match_loop!(chars.peek() => &ch => ch, {
      '-' => { flags |= PrintFlags::JUST_LEFT; chars.next(); },
      '+' => { flags |= PrintFlags::SHOW_SIGN; chars.next(); },
      ' ' => { flags |= PrintFlags::SPACE_SIGN; chars.next(); },
      '#' => { flags |= PrintFlags::ALT_FORM; chars.next(); },
      '0' => { flags |= PrintFlags::ZERO_PAD; chars.next(); },
      _ => break,
    });
    Ok(flags)
  }
  fn parse_width(chars: &mut Peekable<Chars>) -> ShResult<Option<DynNum>> {
    match chars.peek() {
      Some('*') => {
        chars.next();
        Ok(Some(DynNum::Star))
      }
      Some('0'..='9') => {
        let width = Self::parse_uint(chars)?;
        Ok(Some(DynNum::Number(width)))
      }
      _ => Ok(None),
    }
  }
  fn parse_precision(chars: &mut Peekable<Chars>) -> ShResult<Option<DynNum>> {
    if chars.peek() != Some(&'.') {
      return Ok(None);
    }
    chars.next();
    match chars.peek() {
      Some('*') => {
        chars.next();
        Ok(Some(DynNum::Star))
      }
      Some('0'..='9') => {
        let precision = Self::parse_uint(chars)?;
        Ok(Some(DynNum::Number(precision)))
      }
      _ => Ok(Some(DynNum::Number(0))),
    }
  }
  fn parse_conversion(chars: &mut Peekable<Chars>) -> ShResult<Conversion> {
    let Some(char) = chars.next() else {
      return Err(sherr!(ParseErr, "invalid conversion specification"));
    };

    match char {
      '%' => Ok(Conversion::Percent),
      'd' | 'i' => Ok(Conversion::SignedDecimal),
      'u' => Ok(Conversion::UnsignedDecimal),
      'o' => Ok(Conversion::UnsignedOctal),
      'x' => Ok(Conversion::UnsignedHex(Case::Lower)),
      'X' => Ok(Conversion::UnsignedHex(Case::Upper)),
      'f' => Ok(Conversion::FixedPointDecimal),
      'e' => Ok(Conversion::Scientific(Case::Lower)),
      'E' => Ok(Conversion::Scientific(Case::Upper)),
      'g' => Ok(Conversion::ShortestFloat(Case::Lower)),
      'G' => Ok(Conversion::ShortestFloat(Case::Upper)),
      'c' => Ok(Conversion::Char),
      's' => Ok(Conversion::Str),
      'r' => Ok(Conversion::RepeatStr),
      'b' => Ok(Conversion::AnsiC),
      'q' => Ok(Conversion::ShellQuote),
      '(' => {
        let mut strftime = util::scratch_buf();
        match_loop!(chars.next() => ch, {
          '\\' => {
            let Some(escaped) = chars.next() else {
              return Err(sherr!(ParseErr, "unterminated strftime format"))
            };
            strftime.push(escaped);
          }
          ')' => break,
          _ => strftime.push(ch),
        });

        // The `T` after the closing paren is the actual conversion letter.
        // Without it, `%(...)T` was being parsed as `%(...)` followed by a
        // literal `T`, leaking the T into the output.
        match chars.next() {
          Some('T') => {}
          Some(other) => {
            return Err(sherr!(
              ParseErr,
              "expected 'T' after strftime format, got '{other}'",
            ));
          }
          None => {
            return Err(sherr!(
              ParseErr,
              "unterminated strftime conversion: expected 'T' after ')'",
            ));
          }
        }

        Ok(Conversion::StrfTime(strftime.into()))
      }
      _ => Err(sherr!(ParseErr, "invalid conversion specification")),
    }
  }

  fn parse_uint(chars: &mut Peekable<Chars>) -> ShResult<i32> {
    let mut width_str = util::scratch_buf();

    while let Some(c @ ('0'..='9')) = chars.peek() {
      width_str.push(*c);
      chars.next();
    }
    let width = width_str
      .parse::<usize>()
      .map_err(|_| sherr!(ParseErr, "invalid width"))?;

    Ok(width as i32)
  }
}

/// Pick the appropriate sign prefix character based on whether the value
/// is negative and which sign-flag is set.
fn pick_sign(negative: bool, flags: PrintFlags) -> &'static str {
  if negative {
    "-"
  } else if flags.contains(PrintFlags::SHOW_SIGN) {
    "+"
  } else if flags.contains(PrintFlags::SPACE_SIGN) {
    " "
  } else {
    ""
  }
}

/// Common padding logic shared by all conversion handlers.
///
/// `prefix` is sign/marker characters (`-`, `+`, ` `, `0x`, `0X`, `0`) that
/// stick to the body. With zero-padding the prefix stays attached to the
/// body and zeros are inserted between them; with space-padding the
/// spaces go outside the prefix.
///
/// `zero_pad_allowed` should be false for non-numeric conversions (string,
/// char, time, etc.) where the `0` flag has no semantic effect, and for
/// integer conversions with explicit precision (precision already supplies
/// the leading zeros).
fn pad_to_width(
  body: &str,
  prefix: &str,
  flags: PrintFlags,
  width: Option<usize>,
  zero_pad_allowed: bool,
) -> String {
  let total_chars = prefix.chars().count() + body.chars().count();
  let Some(w) = width else {
    return format!("{prefix}{body}");
  };
  if total_chars >= w {
    return format!("{prefix}{body}");
  }
  let pad = w - total_chars;

  if flags.contains(PrintFlags::JUST_LEFT) {
    format!("{prefix}{body}{}", " ".repeat(pad))
  } else if zero_pad_allowed && flags.contains(PrintFlags::ZERO_PAD) {
    format!("{prefix}{}{body}", "0".repeat(pad))
  } else {
    format!("{}{prefix}{body}", " ".repeat(pad))
  }
}

/// Convert Rust's exponent format (`1e2`, `1.5e-3`) to POSIX printf style
/// (`1e+02`, `1.5e-03`): sign always present, exponent zero-padded to at
/// least two digits.
fn normalize_exponent(s: &str) -> String {
  let Some(epos) = s.find(['e', 'E']) else {
    return s.to_string();
  };
  let (mantissa, exp_part) = s.split_at(epos);
  let exp_char = exp_part.chars().next().unwrap();
  let rest = &exp_part[exp_char.len_utf8()..];

  let (sign, digits) = match rest.chars().next() {
    Some('-') => ('-', &rest[1..]),
    Some('+') => ('+', &rest[1..]),
    _ => ('+', rest),
  };

  let padded = if digits.chars().count() < 2 {
    format!("0{digits}")
  } else {
    digits.to_string()
  };

  format!("{mantissa}{exp_char}{sign}{padded}")
}

fn strip_trailing_zeros(s: &str) -> String {
  if let Some(epos) = s.find(['e', 'E']) {
    let (mantissa, exp) = s.split_at(epos);
    let trimmed = if mantissa.contains('.') {
      mantissa.trim_end_matches('0').trim_end_matches('.')
    } else {
      mantissa
    };
    format!("{trimmed}{exp}")
  } else if s.contains('.') {
    s.trim_end_matches('0').trim_end_matches('.').to_string()
  } else {
    s.to_string()
  }
}

pub(super) enum PrintfErr {
  BadNumber(String),
}

pub(super) struct Rendered {
  text: String,
  errors: Vec<PrintfErr>,
}

impl Rendered {
  pub fn new(text: String) -> Self {
    Self {
      text,
      errors: vec![],
    }
  }

  pub fn merge_errors(&mut self, other: Self) {
    self.errors.extend(other.errors);
  }
}

pub(super) struct Printf;
impl super::Builtin for Printf {
  fn execute(&self, args: super::BuiltinArgs) -> crate::ShResult<()> {
    let mut arg_vec = args.argv.into_iter().map(|(s, _)| s.to_string());
    let format_str = arg_vec
      .next()
      .ok_or_else(|| sherr!(ExecFail, "printf: missing format string"))?;
    let formatter = PrintFormatter::parse(&format_str)?;
    let remaining: Vec<String> = arg_vec.collect();
    let mut values = remaining.into_iter().peekable();

    // Set when any present numeric argument fails to convert; printf still emits
    // the `0` fallback and continues, but exits non-zero (POSIX).
    let mut had_error = false;

    if formatter.has_specs() {
      // Recycle the format string until args are exhausted. If a full cycle
      // consumes no arguments (e.g. the only spec is `%%`), stop instead of
      // looping forever.
      loop {
        let before = values.len();
        let rendered = formatter.apply_once(&mut values)?;
        out!("{}", rendered.text);
        had_error |= emit_printf_errors(&rendered.errors);
        if values.peek().is_none() || values.len() == before {
          break;
        }
      }
    } else {
      // No specs: emit format once, ignore extra args.
      let rendered = formatter.apply_once(&mut values)?;
      out!("{}", rendered.text);
      had_error |= emit_printf_errors(&rendered.errors);
    }

    with_status(i32::from(had_error))
  }
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  // ===================== invalid-number handling =====================

  #[test]
  fn printf_invalid_number_exits_nonzero() {
    let _g = TestGuard::new();
    test_input("printf '%d' abc").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn printf_invalid_number_still_prints_fallback() {
    // A bad number is a soft error: the width-formatted `0` is still emitted
    // (stdout), alongside the diagnostic (stderr; the test harness merges them).
    let g = TestGuard::new();
    test_input("printf '[%5d]' abc").unwrap();
    let out = g.read_output();
    assert!(
      out.starts_with("[    0]"),
      "fallback output missing: {out:?}"
    );
    assert!(
      out.contains("printf: abc: invalid number"),
      "diagnostic missing: {out:?}"
    );
  }

  #[test]
  fn printf_missing_number_arg_is_silent_success() {
    // Fewer args than conversions: the missing one is `0` with no diagnostic
    // and a zero exit status (bash), distinct from a present-but-invalid arg.
    let _g = TestGuard::new();
    test_input("printf '%d %d' 5").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn printf_valid_numbers_exit_zero() {
    let _g = TestGuard::new();
    test_input("printf '%d %.2f %x %g' 42 3.14 255 0.5").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Basic conversions =====================

  #[test]
  fn printf_string() {
    let guard = TestGuard::new();
    test_input(r"printf '%s' hello").unwrap();
    assert_eq!(guard.read_output(), "hello");
  }

  #[test]
  fn printf_repeat_literal_count() {
    let guard = TestGuard::new();
    test_input(r"printf '%5r' x").unwrap();
    assert_eq!(guard.read_output(), "xxxxx");
  }

  #[test]
  fn printf_repeat_dynamic_count() {
    let guard = TestGuard::new();
    test_input(r"printf '%*r' 3 ab").unwrap();
    assert_eq!(guard.read_output(), "ababab");
  }

  #[test]
  fn printf_repeat_multibyte() {
    let guard = TestGuard::new();
    test_input(r"printf '%4r' '─'").unwrap();
    assert_eq!(guard.read_output(), "────");
  }

  #[test]
  fn printf_repeat_zero_count_is_empty() {
    // Count 0 must come via the dynamic form; a literal leading `0` is the
    // zero-pad flag, not a count.
    let guard = TestGuard::new();
    test_input(r"printf '%*r' 0 x").unwrap();
    assert_eq!(guard.read_output(), "");
  }

  #[test]
  fn printf_repeat_bare_is_single_copy() {
    let guard = TestGuard::new();
    test_input(r"printf '%r' hi").unwrap();
    assert_eq!(guard.read_output(), "hi");
  }

  #[test]
  fn printf_repeat_recycles_format() {
    let guard = TestGuard::new();
    test_input(r"printf '%*r' 3 a 2 b").unwrap();
    assert_eq!(guard.read_output(), "aaabb");
  }

  #[test]
  fn printf_repeat_in_separator_pattern() {
    // The qtable use case: build a separator inline, no fork.
    let guard = TestGuard::new();
    test_input(r"printf '╭%*r╮' 3 '─'").unwrap();
    assert_eq!(guard.read_output(), "╭───╮");
  }

  #[test]
  fn printf_signed_decimal() {
    let guard = TestGuard::new();
    test_input(r"printf '%d' 42").unwrap();
    assert_eq!(guard.read_output(), "42");
  }

  #[test]
  fn printf_signed_decimal_negative() {
    let guard = TestGuard::new();
    test_input(r"printf '%d' -42").unwrap();
    assert_eq!(guard.read_output(), "-42");
  }

  #[test]
  fn printf_i_alias() {
    let guard = TestGuard::new();
    test_input(r"printf '%i' 42").unwrap();
    assert_eq!(guard.read_output(), "42");
  }

  #[test]
  fn printf_unsigned_decimal() {
    let guard = TestGuard::new();
    test_input(r"printf '%u' 42").unwrap();
    assert_eq!(guard.read_output(), "42");
  }

  #[test]
  fn printf_octal() {
    let guard = TestGuard::new();
    test_input(r"printf '%o' 8").unwrap();
    assert_eq!(guard.read_output(), "10");
  }

  #[test]
  fn printf_hex_lower() {
    let guard = TestGuard::new();
    test_input(r"printf '%x' 255").unwrap();
    assert_eq!(guard.read_output(), "ff");
  }

  #[test]
  fn printf_hex_upper() {
    let guard = TestGuard::new();
    test_input(r"printf '%X' 255").unwrap();
    assert_eq!(guard.read_output(), "FF");
  }

  #[test]
  fn printf_fixed_float_default_precision() {
    let guard = TestGuard::new();
    test_input(r"printf '%f' 3.14").unwrap();
    assert_eq!(guard.read_output(), "3.140000");
  }

  #[test]
  fn printf_scientific_lower() {
    let guard = TestGuard::new();
    test_input(r"printf '%e' 1234.5").unwrap();
    assert_eq!(guard.read_output(), "1.234500e+03");
  }

  #[test]
  fn printf_scientific_upper() {
    let guard = TestGuard::new();
    test_input(r"printf '%E' 1234.5").unwrap();
    assert_eq!(guard.read_output(), "1.234500E+03");
  }

  #[test]
  fn printf_scientific_negative_exponent() {
    let guard = TestGuard::new();
    test_input(r"printf '%e' 0.001").unwrap();
    assert_eq!(guard.read_output(), "1.000000e-03");
  }

  #[test]
  fn printf_char_takes_first() {
    let guard = TestGuard::new();
    test_input(r"printf '%c' hello").unwrap();
    assert_eq!(guard.read_output(), "h");
  }

  #[test]
  fn printf_literal_percent() {
    let guard = TestGuard::new();
    test_input(r"printf '%%'").unwrap();
    assert_eq!(guard.read_output(), "%");
  }

  // ===================== Format string escapes =====================

  #[test]
  fn printf_newline_escape() {
    let guard = TestGuard::new();
    test_input(r"printf 'a\nb'").unwrap();
    assert_eq!(guard.read_output(), "a\nb");
  }

  #[test]
  fn printf_tab_escape() {
    let guard = TestGuard::new();
    test_input(r"printf 'a\tb'").unwrap();
    assert_eq!(guard.read_output(), "a\tb");
  }

  #[test]
  fn printf_backslash_escape() {
    let guard = TestGuard::new();
    test_input(r"printf 'a\\b'").unwrap();
    assert_eq!(guard.read_output(), "a\\b");
  }

  // ===================== Width =====================

  #[test]
  fn printf_width_right_justify_default() {
    let guard = TestGuard::new();
    test_input(r"printf '[%5d]' 42").unwrap();
    assert_eq!(guard.read_output(), "[   42]");
  }

  #[test]
  fn printf_width_left_justify_flag() {
    let guard = TestGuard::new();
    test_input(r"printf '[%-5d]' 42").unwrap();
    assert_eq!(guard.read_output(), "[42   ]");
  }

  #[test]
  fn printf_width_zero_pad() {
    let guard = TestGuard::new();
    test_input(r"printf '[%05d]' 42").unwrap();
    assert_eq!(guard.read_output(), "[00042]");
  }

  #[test]
  fn printf_width_string_right_pad() {
    let guard = TestGuard::new();
    test_input(r"printf '[%10s]' hi").unwrap();
    assert_eq!(guard.read_output(), "[        hi]");
  }

  #[test]
  fn printf_width_string_left_just() {
    let guard = TestGuard::new();
    test_input(r"printf '[%-10s]' hi").unwrap();
    assert_eq!(guard.read_output(), "[hi        ]");
  }

  #[test]
  fn printf_width_dynamic_star() {
    let guard = TestGuard::new();
    test_input(r"printf '[%*d]' 8 42").unwrap();
    assert_eq!(guard.read_output(), "[      42]");
  }

  #[test]
  fn printf_width_less_than_content_no_truncate() {
    let guard = TestGuard::new();
    test_input(r"printf '[%2d]' 12345").unwrap();
    assert_eq!(guard.read_output(), "[12345]");
  }

  // ===================== Precision =====================

  #[test]
  fn printf_precision_float() {
    let guard = TestGuard::new();
    test_input(r"printf '%.2f' 3.14159").unwrap();
    assert_eq!(guard.read_output(), "3.14");
  }

  #[test]
  fn printf_precision_zero_float() {
    let guard = TestGuard::new();
    test_input(r"printf '%.0f' 3.7").unwrap();
    assert_eq!(guard.read_output(), "4");
  }

  #[test]
  fn printf_precision_string_truncate() {
    let guard = TestGuard::new();
    test_input(r"printf '%.3s' hello").unwrap();
    assert_eq!(guard.read_output(), "hel");
  }

  #[test]
  fn printf_precision_int_min_digits() {
    let guard = TestGuard::new();
    test_input(r"printf '%.5d' 42").unwrap();
    assert_eq!(guard.read_output(), "00042");
  }

  #[test]
  fn printf_precision_dynamic_star() {
    let guard = TestGuard::new();
    test_input(r"printf '%.*f' 3 3.14159").unwrap();
    assert_eq!(guard.read_output(), "3.142");
  }

  #[test]
  fn printf_width_and_precision_combined() {
    let guard = TestGuard::new();
    test_input(r"printf '[%10.3f]' 3.14159").unwrap();
    assert_eq!(guard.read_output(), "[     3.142]");
  }

  // ===================== Flags =====================

  #[test]
  fn printf_show_sign_positive() {
    let guard = TestGuard::new();
    test_input(r"printf '%+d' 42").unwrap();
    assert_eq!(guard.read_output(), "+42");
  }

  #[test]
  fn printf_show_sign_negative_still_minus() {
    let guard = TestGuard::new();
    test_input(r"printf '%+d' -42").unwrap();
    assert_eq!(guard.read_output(), "-42");
  }

  #[test]
  fn printf_space_sign_positive() {
    let guard = TestGuard::new();
    test_input(r"printf '% d' 42").unwrap();
    assert_eq!(guard.read_output(), " 42");
  }

  #[test]
  fn printf_alt_form_hex_nonzero() {
    let guard = TestGuard::new();
    test_input(r"printf '%#x' 255").unwrap();
    assert_eq!(guard.read_output(), "0xff");
  }

  #[test]
  fn printf_alt_form_hex_zero_no_prefix() {
    // # flag on 0 should NOT add 0x prefix per POSIX
    let guard = TestGuard::new();
    test_input(r"printf '%#x' 0").unwrap();
    assert_eq!(guard.read_output(), "0");
  }

  #[test]
  fn printf_alt_form_octal_ensures_leading_zero() {
    let guard = TestGuard::new();
    test_input(r"printf '%#o' 8").unwrap();
    assert_eq!(guard.read_output(), "010");
  }

  #[test]
  fn printf_zero_pad_overrides_default_when_no_just_left() {
    let guard = TestGuard::new();
    test_input(r"printf '[%+06d]' 42").unwrap();
    assert_eq!(guard.read_output(), "[+00042]");
  }

  // ===================== Argument recycling =====================

  #[test]
  fn printf_recycle_format() {
    let guard = TestGuard::new();
    test_input(r"printf '%s:%d ' alice 1 bob 2 carol 3").unwrap();
    assert_eq!(guard.read_output(), "alice:1 bob:2 carol:3 ");
  }

  #[test]
  fn printf_no_specs_ignores_extras() {
    let guard = TestGuard::new();
    test_input(r"printf 'hello' ignored extras").unwrap();
    assert_eq!(guard.read_output(), "hello");
  }

  #[test]
  fn printf_missing_int_arg_defaults_to_zero() {
    let guard = TestGuard::new();
    test_input(r"printf '%d-%d-%d' 1").unwrap();
    assert_eq!(guard.read_output(), "1-0-0");
  }

  #[test]
  fn printf_missing_string_arg_defaults_to_empty() {
    let guard = TestGuard::new();
    test_input(r"printf '[%s][%s]' hi").unwrap();
    assert_eq!(guard.read_output(), "[hi][]");
  }

  // ===================== Bash extensions =====================

  #[test]
  fn printf_ansi_c_b_interprets_escapes() {
    let guard = TestGuard::new();
    test_input(r"printf '%b' 'a\tb'").unwrap();
    assert_eq!(guard.read_output(), "a\tb");
  }

  #[test]
  fn printf_shell_quote_plain() {
    let guard = TestGuard::new();
    test_input(r"printf '%q' hello").unwrap();
    assert_eq!(guard.read_output(), "hello");
  }

  #[test]
  fn printf_shell_quote_with_whitespace() {
    let guard = TestGuard::new();
    test_input(r"printf '%q' 'hello world'").unwrap();
    assert_eq!(guard.read_output(), "'hello world'");
  }

  #[test]
  fn printf_strftime_consumes_trailing_t() {
    // Regression: parser used to leave the trailing 'T' in the format,
    // leaking it into the literal portion of the output.
    let guard = TestGuard::new();
    test_input(r"printf '%(%Y)T'").unwrap();
    let out = guard.read_output();
    assert!(
      !out.ends_with('T'),
      "trailing T leaked into output: {out:?}"
    );
    assert_eq!(
      out.chars().count(),
      4,
      "expected a 4-digit year, got {out:?}"
    );
  }

  #[test]
  fn printf_strftime_explicit_epoch_zero() {
    // Year of epoch=0 is 1969 or 1970 depending on local timezone.
    let guard = TestGuard::new();
    test_input(r"printf '%(%Y)T' 0").unwrap();
    let out = guard.read_output();
    assert!(
      out == "1969" || out == "1970",
      "expected 1969 or 1970, got {out:?}"
    );
  }

  // ===================== Multi-spec format strings =====================

  #[test]
  fn printf_multi_string_specs() {
    let guard = TestGuard::new();
    test_input(r"printf '%s and %s' alice bob").unwrap();
    assert_eq!(guard.read_output(), "alice and bob");
  }

  #[test]
  fn printf_mixed_spec_types() {
    let guard = TestGuard::new();
    test_input(r"printf '%s is %d' alice 30").unwrap();
    assert_eq!(guard.read_output(), "alice is 30");
  }

  #[test]
  fn printf_literals_around_specs() {
    let guard = TestGuard::new();
    test_input(r"printf '<<%s>>' middle").unwrap();
    assert_eq!(guard.read_output(), "<<middle>>");
  }

  // ===================== Edge cases =====================

  #[test]
  fn printf_empty_format() {
    let guard = TestGuard::new();
    test_input(r"printf ''").unwrap();
    assert_eq!(guard.read_output(), "");
  }

  #[test]
  fn printf_format_with_no_specs_or_args() {
    let guard = TestGuard::new();
    test_input(r"printf 'just text'").unwrap();
    assert_eq!(guard.read_output(), "just text");
  }

  #[test]
  fn printf_status_zero() {
    let _g = TestGuard::new();
    test_input(r"printf '%s' hello").unwrap();
    assert_eq!(crate::state::Shed::get_status(), 0);
  }
}
