use std::iter::Peekable;

use bstr::ByteSlice;

use crate::{
  errln,
  expand::escape,
  match_loop, procio, sherr,
  state::vars::VarStr,
  util::{
    self,
    error::ShResult,
    strops::{
      self, Base, ByteCursor, Case, Count, Field, FieldParams, FmtFlags, Formatter, ParseRadix,
      Sign, SliceCursor, StrFmt,
    },
  },
  varstr,
};

enum Conversion {
  SignedDecimal,
  UnsignedDecimal,
  UnsignedOctal,
  UnsignedHex(Case),
  FixedPointDecimal,
  Scientific(Case),
  ShortestFloat(Case),
  HumanSize,
  Char,
  Str,
  RepeatStr,
  AnsiC,
  ShellQuote,
  StrfTime(VarStr),
}

/// printf's value stream plus the soft-error accumulator. A bad number is
/// non-fatal (the `0` fallback is still rendered), so the error rides in the
/// source rather than aborting `render`.
struct PrintfArgs {
  args: Peekable<std::vec::IntoIter<Vec<u8>>>,
  errors: Vec<PrintfErr>,
}

struct PrintfFmt;

impl StrFmt for PrintfFmt {
  type Source = PrintfArgs;
  type Conv = Conversion;

  fn expand_literal(&self, literal: Vec<u8>) -> Vec<u8> {
    escape::expand_ansi_c(&literal)
  }

  fn take_count(&self, src: &mut Self::Source) -> ShResult<isize> {
    // Missing or non-numeric `*` args default to 0 (matches bash printf).
    let Some(arg) = src.args.next() else {
      return Ok(0);
    };
    Ok(VarStr::from(arg).parse::<isize>().unwrap_or(0))
  }

  fn parse_conv(&self, cur: &mut SliceCursor) -> ShResult<Conversion> {
    let Some(b) = cur.next_byte() else {
      return Err(sherr!(ParseErr, "invalid conversion specification"));
    };

    match b {
      b'd' | b'i' => Ok(Conversion::SignedDecimal),
      b'u' => Ok(Conversion::UnsignedDecimal),
      b'o' => Ok(Conversion::UnsignedOctal),
      b'x' => Ok(Conversion::UnsignedHex(Case::Lower)),
      b'X' => Ok(Conversion::UnsignedHex(Case::Upper)),
      b'f' => Ok(Conversion::FixedPointDecimal),
      b'e' => Ok(Conversion::Scientific(Case::Lower)),
      b'E' => Ok(Conversion::Scientific(Case::Upper)),
      b'g' => Ok(Conversion::ShortestFloat(Case::Lower)),
      b'G' => Ok(Conversion::ShortestFloat(Case::Upper)),
      b'c' => Ok(Conversion::Char),
      b's' => Ok(Conversion::Str),
      b'r' => Ok(Conversion::RepeatStr),
      b'b' => Ok(Conversion::AnsiC),
      b'q' => Ok(Conversion::ShellQuote),
      b'h' => Ok(Conversion::HumanSize),
      b'(' => {
        let mut strftime = util::scratch_buf();
        match_loop!(cur.next_byte() => b, {
          b'\\' => {
            let Some(escaped) = cur.next_byte() else {
              return Err(sherr!(ParseErr, "unterminated strftime format"))
            };
            strftime.push(escaped);
          }
          b')' => break,
          _ => strftime.push(b),
        });

        // The `T` after the closing paren is the actual conversion letter.
        match cur.next_byte() {
          Some(b'T') => {}
          Some(other) => {
            return Err(sherr!(
              ParseErr,
              "expected 'T' after strftime format, got '{}'",
              other as char,
            ));
          }
          None => {
            return Err(sherr!(
              ParseErr,
              "unterminated strftime conversion: expected 'T' after ')'",
            ));
          }
        }

        Ok(Conversion::StrfTime(strftime.as_slice().into()))
      }
      _ => Err(sherr!(ParseErr, "invalid conversion specification")),
    }
  }

  fn render(
    &self,
    conv: &Conversion,
    field: &FieldParams,
    src: &mut Self::Source,
  ) -> ShResult<Field> {
    let flags = field.flags();
    let prec = prec_of(field);

    #[rustfmt::skip]
    let rendered = match conv {
      Conversion::SignedDecimal       => render_signed(src, flags, prec),
      Conversion::UnsignedDecimal     => render_unsigned(src, prec),
      Conversion::UnsignedOctal       => render_octal(src, flags, prec),
      Conversion::UnsignedHex(case)   => render_hex(src, flags, prec, *case),
      Conversion::FixedPointDecimal   => render_fixed(src, flags, prec),
      Conversion::Scientific(case)    => render_scientific(src, flags, prec, *case),
      Conversion::ShortestFloat(case) => render_shortest(src, flags, prec, *case),
      Conversion::HumanSize           => render_human(src),
      Conversion::Char                => render_char(src),
      Conversion::Str                 => render_str(src, prec),
      Conversion::RepeatStr           => render_repeat(src, field),
      Conversion::AnsiC               => render_ansi_c(src, prec),
      Conversion::ShellQuote          => render_shell_quote(src),
      Conversion::StrfTime(fmt)       => render_strftime(src, &fmt.to_str_lossy()),
    };

    Ok(rendered)
  }
}

/// Parse a printf numeric argument, returning the value and any soft error.
/// A *missing* argument (fewer args than conversions) yields `0` with no error;
/// a *present* argument that fails to parse yields `0` plus a
/// [`PrintfErr::BadNumber`], so the caller still substitutes `0` and continues
/// formatting while the run is flagged to exit non-zero.
fn parse_num_arg<T: ParseRadix + Default>(arg: Option<Vec<u8>>) -> (T, Option<PrintfErr>) {
  let Some(arg) = arg else {
    return (T::default(), None);
  };

  // posix thing: if a char leads with a quote, we parse it
  // as an integer, returning its actual byte value
  if let Some((&quote, rest)) = arg.split_first()
    && matches!(quote, b'\'' | b'"')
  {
    let byte = rest.first().copied().unwrap_or(0);
    let byte_s = varstr!("{byte}");
    return (
      ParseRadix::parse_radix(&byte_s.to_str_lossy()).unwrap_or_default(),
      None,
    );
  }

  match ParseRadix::parse_radix(&arg.to_str_lossy()) {
    Some(v) => (v, None),
    None => (
      T::default(),
      Some(PrintfErr::BadNumber(arg.to_str_lossy().into())),
    ),
  }
}

/// Parse a floating-point argument for the `%f`/`%e`/`%g` conversions. Floats
/// have no base prefixes and a leading `0` (`0.5`) is not an octal marker, so
/// this parses decimal directly instead of routing through the integer-literal
/// grammar. The POSIX leading-quote trick still applies.
fn parse_float_arg(arg: Option<Vec<u8>>) -> (f64, Option<PrintfErr>) {
  let Some(arg) = arg else {
    return (0.0, None);
  };

  if let Some((&quote, rest)) = arg.split_first()
    && matches!(quote, b'\'' | b'"')
  {
    let byte = rest.first().copied().unwrap_or(0);
    return (f64::from(byte), None);
  }

  match arg.to_str_lossy().trim().parse::<f64>() {
    Ok(v) => (v, None),
    Err(_) => (0.0, Some(PrintfErr::BadNumber(arg.to_str_lossy().into()))),
  }
}

/// Write the stderr diagnostic for each collected printf error. Returns whether
/// any were present, so the caller can set a non-zero exit status.
fn emit_printf_errors(errors: &[PrintfErr]) -> bool {
  for err in errors {
    match err {
      PrintfErr::BadNumber(arg) => errln!("printf: {arg}: invalid number"),
    }
  }
  !errors.is_empty()
}

/// Select the sign prefix for a numeric conversion from the value's sign and
/// the `+`/space flags.
fn sign_for(negative: bool, flags: FmtFlags) -> Option<Sign> {
  if negative {
    Some(Sign::Minus)
  } else if flags.contains(FmtFlags::PLUS) {
    Some(Sign::Plus)
  } else if flags.contains(FmtFlags::SPACE) {
    Some(Sign::Space)
  } else {
    None
  }
}

fn prec_of(field: &FieldParams) -> Option<usize> {
  match field.precision() {
    Some(Count::Static(p)) => Some(*p),
    _ => None,
  }
}

fn render_int(src: &mut PrintfArgs, flags: Option<FmtFlags>, prec: Option<usize>) -> Field {
  let (n, err): (i64, _) = parse_num_arg(src.args.next());
  src.errors.extend(err);
  let sign = flags.and_then(|f| sign_for(n.is_negative(), f));

  let mut digits = n.unsigned_abs().to_string();
  if let Some(p) = prec {
    digits = format!("{digits:0>p$}");
  }

  Field::numeric_padded(digits.into_bytes(), sign, None, prec.is_none())
}

fn render_signed(src: &mut PrintfArgs, flags: FmtFlags, prec: Option<usize>) -> Field {
  render_int(src, Some(flags), prec)
}

fn render_unsigned(src: &mut PrintfArgs, prec: Option<usize>) -> Field {
  render_int(src, None, prec)
}

fn render_octal(src: &mut PrintfArgs, flags: FmtFlags, prec: Option<usize>) -> Field {
  let (n, err): (u64, _) = parse_num_arg(src.args.next());
  src.errors.extend(err);

  let mut digits = format!("{n:o}");
  if let Some(p) = prec {
    digits = format!("{digits:0>p$}");
  }

  // # flag for %o: ensure at least one leading 0.
  let base = (flags.contains(FmtFlags::ALT) && !digits.starts_with('0')).then_some(Base::Octal);

  Field::numeric_padded(digits.into_bytes(), None, base, prec.is_none())
}

fn render_hex(src: &mut PrintfArgs, flags: FmtFlags, prec: Option<usize>, case: Case) -> Field {
  let (n, err): (u64, _) = parse_num_arg(src.args.next());
  src.errors.extend(err);

  let mut digits = match case {
    Case::Lower => format!("{n:x}"),
    Case::Upper => format!("{n:X}"),
  };
  if let Some(p) = prec {
    digits = format!("{digits:0>p$}");
  }

  // # flag for %x/%X: prepend 0x/0X for non-zero values.
  let base = (flags.contains(FmtFlags::ALT) && n != 0).then_some(Base::Hex(case));

  Field::numeric_padded(digits.into_bytes(), None, base, prec.is_none())
}

fn render_fixed(src: &mut PrintfArgs, flags: FmtFlags, prec: Option<usize>) -> Field {
  let (f, err) = parse_float_arg(src.args.next());
  src.errors.extend(err);
  let p = prec.unwrap_or(6);

  let body = format!("{f:.p$}");
  let abs = body.trim_start_matches('-').as_bytes().to_vec();
  let sign = sign_for(f.is_sign_negative() && f != 0.0, flags);

  // For floats, zero-padding applies independent of precision (precision
  // controls digits after the decimal point, not minimum total digits).
  Field::numeric_padded(abs, sign, None, true)
}

fn render_scientific(
  src: &mut PrintfArgs,
  flags: FmtFlags,
  prec: Option<usize>,
  case: Case,
) -> Field {
  let (f, err) = parse_float_arg(src.args.next());
  src.errors.extend(err);
  let p = prec.unwrap_or(6);

  let raw = match case {
    Case::Lower => format!("{f:.p$e}"),
    Case::Upper => format!("{f:.p$E}"),
  };
  let normalized = normalize_exponent(raw.as_bytes());
  let abs = normalized.trim_start_with(|c| c == '-').to_vec();
  let sign = sign_for(f.is_sign_negative() && f != 0.0, flags);

  Field::numeric_padded(abs, sign, None, true)
}

fn render_shortest(
  src: &mut PrintfArgs,
  flags: FmtFlags,
  prec: Option<usize>,
  case: Case,
) -> Field {
  let (f, err) = parse_float_arg(src.args.next());
  src.errors.extend(err);
  // %g: precision is number of significant digits (default 6, minimum 1).
  let p = prec.unwrap_or(6).max(1);

  // POSIX %g: use scientific when exponent < -4 or >= precision.
  let abs_val = f.abs();
  let exp = if abs_val == 0.0 {
    0i32
  } else {
    abs_val.log10().floor() as i32
  };
  let use_scientific = exp < -4 || exp >= p as i32;

  let body = if use_scientific {
    let mantissa_prec = p.saturating_sub(1);
    let raw = match case {
      Case::Lower => format!("{f:.mantissa_prec$e}"),
      Case::Upper => format!("{f:.mantissa_prec$E}"),
    };
    let normalized = normalize_exponent(raw.as_bytes());

    if flags.contains(FmtFlags::ALT) {
      normalized
    } else {
      strip_trailing_zeros(&normalized)
    }
  } else {
    let fp = (p as i32 - 1 - exp).max(0) as usize;
    let raw = format!("{f:.fp$}").into_bytes();
    if flags.contains(FmtFlags::ALT) {
      raw
    } else {
      strip_trailing_zeros(&raw)
    }
  };
  let abs = body.trim_start_with(|c| c == '-').to_vec();
  let sign = sign_for(f.is_sign_negative() && f != 0.0, flags);

  Field::numeric_padded(abs, sign, None, true)
}

fn render_human(src: &mut PrintfArgs) -> Field {
  let (n, err): (u64, _) = parse_num_arg(src.args.next());
  src.errors.extend(err);
  let mut s = String::new();
  strops::format_size(n, &mut s).ok();
  Field::numeric_padded(s.into_bytes(), None, None, true)
}

fn render_char(src: &mut PrintfArgs) -> Field {
  let arg = src.args.next().unwrap_or_default();
  // POSIX %c: take first byte of the argument.
  Field::string(arg.get(..1).unwrap_or_default().to_vec())
}

fn render_str(src: &mut PrintfArgs, prec: Option<usize>) -> Field {
  let s = src.args.next().unwrap_or_default();
  let s = match prec {
    Some(p) => s.get(..p).unwrap_or(&s).to_vec(),
    None => s,
  };
  Field::string(s)
}

fn render_repeat(src: &mut PrintfArgs, field: &FieldParams) -> Field {
  // The width slot is the repeat count (`%*r` / `%5r`), not a field width, so
  // the field is emitted raw with no padding. A bare `%r` degrades to a single
  // copy.
  let count = match field.width() {
    Some(Count::Static(n)) => *n,
    _ => 1,
  };
  let s = src.args.next().unwrap_or_default();
  Field::raw(s.repeat(count))
}

fn render_ansi_c(src: &mut PrintfArgs, prec: Option<usize>) -> Field {
  let s = src.args.next().unwrap_or_default();
  let expanded = escape::expand_ansi_c(&s);
  let truncated = match prec {
    Some(p) => expanded.into_iter().take(p).collect(),
    None => expanded,
  };
  Field::string(truncated)
}

fn render_shell_quote(src: &mut PrintfArgs) -> Field {
  let s = src.args.next().unwrap_or_default();
  let quoted = escape::shell_quote_bytes(&s);
  Field::string(quoted)
}

fn render_strftime(src: &mut PrintfArgs, format: &str) -> Field {
  use crate::state::{Shed, meta::MetaTab};
  use chrono::{Local, TimeZone};
  let arg = src.args.next().unwrap_or_else(|| b"-1".to_vec());
  let secs: i64 = VarStr::from(arg).parse().unwrap_or(-1);

  let dt = if secs == -1 {
    // Current time
    Local::now()
  } else if secs == -2 {
    // Shell start time: convert the monotonic Instant we recorded at startup
    // into a wall-clock time by subtracting its elapsed duration from "now".
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

  Field::string(dt.format(format).to_string().into_bytes())
}

/// Convert Rust's exponent format (`1e2`, `1.5e-3`) to POSIX printf style
/// (`1e+02`, `1.5e-03`): sign always present, exponent zero-padded to at
/// least two digits.
fn normalize_exponent(s: &[u8]) -> Vec<u8> {
  let Some(epos) = s.find_byteset(b"eE") else {
    return s.to_vec();
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
    [b"0", digits.as_bytes()].concat()
  } else {
    digits.as_bytes().to_vec()
  };

  [
    mantissa.as_bytes(),
    &[exp_char as u8],
    &[sign as u8],
    &padded,
  ]
  .concat()
}

fn strip_trailing_zeros(s: &[u8]) -> Vec<u8> {
  if let Some(epos) = s.find_byteset(b"eE") {
    let (mantissa, exp) = s.split_at(epos);

    let trimmed = if mantissa.contains(&b'.') {
      // strip fractional zeros first (stops at the dot), then the dangling dot.
      mantissa
        .trim_end_with(|c| c == '0')
        .trim_end_with(|c| c == '.')
    } else {
      mantissa
    };

    [trimmed, exp].concat()
  } else if s.contains(&b'.') {
    s.trim_end_with(|c| c == '0')
      .trim_end_with(|c| c == '.')
      .to_vec()
  } else {
    s.to_vec()
  }
}

#[derive(Debug, Clone)]
pub(super) enum PrintfErr {
  BadNumber(String),
}

pub(super) struct Printf;
impl super::Builtin for Printf {
  fn no_help(&self) -> bool {
    true
  }
  fn double_dash_operand(&self) -> bool {
    // Keep `--` as an operand; `execute` strips only a single *leading* `--`
    // (options end there), so a `--` at or after the format stays literal data.
    true
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let (arg_vec, _) = args.take_argv();

    let mut arg_iter = arg_vec.into_iter();
    let mut first = arg_iter
      .next()
      .ok_or_else(|| sherr!(ExecFail, "printf: missing format string"))?;
    // A single leading `--` ends option processing (POSIX utility syntax).
    if first.0 == "--" {
      first = arg_iter
        .next()
        .ok_or_else(|| sherr!(ExecFail, "printf: missing format string"))?;
    }

    let (format_str, _) = first;
    let formatter = Formatter::parse(&PrintfFmt, format_str.as_bytes())?;
    let remaining: Vec<Vec<u8>> = arg_iter.map(|(s, _)| s.as_bytes().to_vec()).collect();

    let mut src = PrintfArgs {
      args: remaining.into_iter().peekable(),
      errors: vec![],
    };

    // Set when any present numeric argument fails to convert; printf still emits
    // the `0` fallback and continues, but exits non-zero (POSIX).
    let mut had_error = false;
    let mut out = vec![];

    if formatter.has_specs() {
      // Recycle the format string until args are exhausted. If a full cycle
      // consumes no arguments (e.g. the only spec is `%%`), stop instead of
      // looping forever.
      loop {
        let before = src.args.len();
        out.clear();
        src.errors.clear();
        formatter.render(&mut src, &mut out)?;
        procio::out_bytes(&out);
        had_error |= emit_printf_errors(&src.errors);
        if src.args.peek().is_none() || src.args.len() == before {
          break;
        }
      }
    } else {
      // No specs: emit format once, ignore extra args.
      formatter.render(&mut src, &mut out)?;
      procio::out_bytes(&out);
      had_error |= emit_printf_errors(&src.errors);
    }

    util::with_status(i32::from(had_error))
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
  fn printf_char_constant_yields_code() {
    let guard = TestGuard::new();
    test_input(r#"printf '%d' "'A""#).unwrap();
    assert_eq!(guard.read_output(), "65");
  }

  #[test]
  fn printf_char_constant_bare_quote_is_zero() {
    let guard = TestGuard::new();
    test_input(r#"printf '%d' "'""#).unwrap();
    assert_eq!(guard.read_output(), "0");
  }

  #[test]
  fn printf_char_constant_is_byte_value() {
    // 'é' is 0xC3 0xA9 in UTF-8; byte-native semantics take the first byte.
    let guard = TestGuard::new();
    test_input(r#"printf '%d' "'é""#).unwrap();
    assert_eq!(guard.read_output(), "195");
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
  fn printf_g_keeps_integer_zeros() {
    // Regression (ultrareview bug_002): strip_trailing_zeros walked past the
    // decimal point and ate integer-part zeros (`100.0` -> `1`).
    let guard = TestGuard::new();
    test_input(r"printf '%g %g %.4g %.5g' 100.0 10.0 10.0 200.0").unwrap();
    assert_eq!(guard.read_output(), "100 10 10 200");
  }

  #[test]
  fn printf_g_still_strips_fractional_zeros() {
    let guard = TestGuard::new();
    test_input(r"printf '%g' 1.5000").unwrap();
    assert_eq!(guard.read_output(), "1.5");
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

  // ===================== Unicode escapes (\u / \U) =====================

  // `\uHHHH`: up to 4 hex digits -> a BMP scalar, UTF-8 encoded. Bytes pinned
  // to bash's output for U+F130 (`printf '' | xxd` -> ef 84 b0).
  #[test]
  fn printf_unicode_u_bmp() {
    let guard = TestGuard::new();
    test_input(r"printf ''").unwrap();
    assert_eq!(guard.read_output().as_bytes(), b"\xef\x84\xb0");
  }

  // `\UHHHHHHHH`: up to 8 hex digits -> reaches past the BMP. U+F036C is a
  // supplementary PUA-A scalar; bash emits the 4-byte f3 b0 8d ac.
  #[test]
  fn printf_unicode_big_u_supplementary() {
    let guard = TestGuard::new();
    test_input(r"printf '\U000f036c'").unwrap();
    assert_eq!(guard.read_output().as_bytes(), b"\xf3\xb0\x8d\xac");
  }

  // Reading is variable-length and stops at the first non-hex digit, so each
  // `\u48` consumes only two digits (U+0048 = 'H', U+0049 = 'I').
  #[test]
  fn printf_unicode_variable_length() {
    let guard = TestGuard::new();
    test_input(r"printf '\u48\u49'").unwrap();
    assert_eq!(guard.read_output(), "HI");
  }

  // ...and a following literal survives: `x` -> U+F130 then a bare 'x'.
  #[test]
  fn printf_unicode_stops_before_literal() {
    let guard = TestGuard::new();
    test_input(r"printf 'x'").unwrap();
    assert_eq!(guard.read_output().as_bytes(), b"\xef\x84\xb0x");
  }

  // A surrogate isn't a valid Rust `char`, so shed leaves the escape literal
  // (bash instead emits raw invalid-UTF-8 surrogate bytes). This documents the
  // one deliberate divergence.
  #[test]
  fn printf_unicode_surrogate_left_literal() {
    let guard = TestGuard::new();
    test_input(r"printf '\uD800'").unwrap();
    assert_eq!(guard.read_output(), r"\uD800");
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
