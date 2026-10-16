//! Numeric-literal parsing and display helpers.

use std::{
  fmt::{Display, LowerExp},
  ops::Deref,
  str::FromStr,
};

use crate::{
  expand::arithmetic::{Num, uNum},
  sherr,
  state::{params, vars::VarStr},
  util::error::{ShResult, ShResultExt},
  varstr,
};

use super::{ByteCursor, SliceCursor};

pub(crate) trait ParseRadix: Sized {
  fn from_radix(s: &str, radix: u32) -> Option<Self>;
  fn parse_radix(s: &str) -> Option<Self> {
    let (sign, rest) = match s.as_bytes().first() {
      Some(b'-') => ("-", &s[1..]),
      Some(b'+') => ("", &s[1..]),
      _          => ("", s),
    };
    let (radix, digits) =
      if let Some(h) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        (16, h)
      } else if let Some(b) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
        (2, b)
      } else if rest.len() > 1 && rest.starts_with('0') {
        (8, &rest[1..])
      } else {
        (10, rest)
      };
    (!digits.is_empty())
      .then(|| Self::from_radix(&format!("{sign}{digits}"), radix))
      .flatten()
  }
}
impl ParseRadix for i64 {
  fn from_radix(s: &str, radix: u32) -> Option<Self> {
    i64::from_str_radix(s, radix).ok()
  }
}
impl ParseRadix for i128 {
  fn from_radix(s: &str, radix: u32) -> Option<Self> {
    i128::from_str_radix(s, radix).ok()
  }
}
impl ParseRadix for u64 {
  fn from_radix(s: &str, radix: u32) -> Option<Self> {
    u64::from_str_radix(s, radix).ok()
  }
}
impl ParseRadix for u128 {
  fn from_radix(s: &str, radix: u32) -> Option<Self> {
    u128::from_str_radix(s, radix).ok()
  }
}

pub(crate) trait VarStrDisplay {
  fn to_var_str(&self) -> VarStr;
}

impl<T: Display + ?Sized> VarStrDisplay for T {
  fn to_var_str(&self) -> VarStr {
    varstr!("{self}")
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntSpec {
  signed: bool,
  endian: Option<Endian>,
  width : Option<u8>,
}

impl IntSpec {
  pub(crate) fn new(signed: bool, endian: Endian, width: Option<u8>) -> Self {
    Self {
      signed,
      endian: Some(endian),
      width,
    }
  }

  pub(crate) fn check_range(self, val: Num) -> ShResult<()> {
    let width = self.width_bytes();
    let bits  = width * 8;
    if bits >= 128 {
      if !self.signed() && val < 0 {
        return Err(sherr!(ParseErr, "value {val} is negative but type is unsigned")).with_code(2);
      }
      return Ok(());
    }

    let (lo, hi): (Num, Num) = if self.signed() {
      (-(1 << (bits - 1)), (1 << (bits - 1)) - 1)
    } else {
      (0, (1 << bits) - 1)
    };

    if val < lo || val > hi {
      let signed = if self.signed() { "signed" } else { "unsigned" };
      return Err(sherr!(
        ParseErr,
        "value {val} does not fit a {width}-byte {signed} field (range: {lo}..={hi})",
      ))
      .with_code(2);
    }

    Ok(())
  }

  pub(crate) fn parse_int(ty: &VarStr) -> ShResult<Self> {
    let mut cursor = SliceCursor::new(ty);

    let signed = match cursor.next_byte() {
      Some(b'u') => false,
      Some(b'i') => true,
      Some(b)    => return Err(sherr!(ParseErr, "invalid integer type '{ty}' (unexpected '{b}')")),
      None       => return Err(sherr!(ParseErr, "invalid integer type '{ty}' (empty)")),
    };

    let (w_start, w_end) = cursor.bump_while_span(|b| b.is_ascii_digit());
    let width            = VarStr::from(&ty[w_start..w_end]);

    let width = match width.as_bytes() {
      b"8"   => 1,
      b"16"  => 2,
      b"32"  => 4,
      b"64"  => 8,
      b"128" => 16,
      _      => return Err(sherr!(ParseErr, "invalid integer width '{width}'")),
    };

    let endian = match cursor.next_byte() {
      None => None,
      Some(b'l') if cursor.bump_if_eq(b'e') => Some(Endian::Little),
      Some(b'b') if cursor.bump_if_eq(b'e') => Some(Endian::Big),
      Some(b) => return Err(sherr!(ParseErr, "invalid integer type '{ty}' (unexpected '{b}')")),
    };

    let None = cursor.next_byte() else {
      return Err(sherr!(ParseErr, "invalid integer type '{ty}' (trailing characters)"));
    };

    Ok(IntSpec {
      signed,
      endian,
      width: Some(width),
    })
  }
  pub(crate) fn parse_width(s: &str) -> ShResult<u8> {
    match s {
      "8"   => Ok(1),
      "16"  => Ok(2),
      "32"  => Ok(4),
      "64"  => Ok(8),
      "128" => Ok(16),
      _     => Err(sherr!(ParseErr, "invalid integer width: {s}")),
    }
  }
  pub(crate) fn decode_bits(self, bytes: &[u8]) -> u128 {
    let mut val: u128 = 0;
    if self.endian().little() {
      for (i, &b) in bytes.iter().enumerate() {
        val |= u128::from(b) << (8 * i as u32);
      }
    } else {
      for &b in bytes {
        val = (val << 8) | u128::from(b);
      }
    }

    val
  }

  pub(crate) fn decode(self, bytes: &[u8]) -> VarStr {
    debug_assert!(self.width.is_some());
    let width = u32::from(self.width.unwrap());

    let val   = self.decode_bits(bytes);

    if !self.signed {
      return varstr!("{val}");
    }

    let bits = width * 8;
    let signed_val = if bits == 128 {
      val as Num
    } else if val & (1 << (bits - 1)) != 0 {
      val as Num - (1 << bits)
    } else {
      val as Num
    };

    varstr!("{signed_val}")
  }

  pub(crate) fn encode(self, val: Num) -> Vec<u8> {
    debug_assert!(self.width.is_some());
    let     width = self.width.unwrap();

    let     uval  = val as u128;
    let mut out   = Vec::with_capacity(width as usize);
    let bits = if self.endian().little() {
      itertools::Either::Left(0..width)
    } else {
      itertools::Either::Right((0..width).rev())
    };

    for i in bits {
      out.push((uval >> (8 * u32::from(i))) as u8);
    }

    out
  }

  pub(crate) fn width(&self) -> Option<u8> {
    self.width
  }

  pub(crate) fn width_bytes(&self) -> usize {
    self.width.expect("width should be set") as usize
  }

  pub(crate) fn set_width(&mut self, width: u8) {
    self.width = Some(width);
  }

  pub(crate) fn endian(&self) -> Endian {
    self.endian.unwrap_or_default()
  }

  pub(crate) fn try_set_endian(&mut self, endian: Endian) {
    if self.endian.is_some() {
      return;
    }
    self.endian = Some(endian);
  }

  pub(crate) fn signed(&self) -> bool {
    self.signed
  }
}

struct Sign(bool);

impl Deref for Sign {
  type Target = bool;
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl FromStr for Sign {
  type Err = ();
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "0" => Ok(Self(false)),
      "1" => Ok(Self(true)),
      _   => Err(()),
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FloatSpec {
  width    : usize,
  exp_bits : u32,
  mant_bits: u32,
  endian   : Option<Endian>,
  decimal  : bool,
  sep      : char,
}

impl FloatSpec {
  pub(crate) fn new(
    width: usize,
    exp_bits: u32,
    mant_bits: u32,
    endian: Option<Endian>,
    decimal: bool,
    sep: char,
  ) -> Self {
    Self {
      width,
      exp_bits,
      mant_bits,
      endian,
      decimal,
      sep,
    }
  }

  pub(crate) fn from_width(
    width: VarStr,
    endian: Endian,
    decimal: bool,
    sep: char,
  ) -> ShResult<Self> {
    let (width, exp_bits, mant_bits) = Self::parse_width(&width)?;
    Ok(Self::new(
      width,
      exp_bits,
      mant_bits,
      Some(endian),
      decimal,
      sep,
    ))
  }

  pub(crate) fn parse_float(mut ty: VarStr) -> ShResult<Self> {
    let endian = if let Some(pref) = ty.strip_suffix(b"le") {
      ty = pref.into();
      Some(Endian::Little)
    } else if let Some(pref) = ty.strip_suffix(b"be") {
      ty = pref.into();
      Some(Endian::Big)
    } else {
      None
    };
    let (width, exp_bits, mant_bits) = Self::parse_type(&ty)?;
    let sep = params::get_separator()
      .as_bytes()
      .first()
      .copied()
      .map(|b| b as char)
      .unwrap_or(' ');

    Ok(Self::new(width, exp_bits, mant_bits, endian, false, sep))
  }

  pub(crate) fn set_decimal(&mut self, decimal: bool) {
    self.decimal = decimal;
  }

  pub(crate) fn parse_width(s: &VarStr) -> ShResult<(usize, u32, u32)> {
    match s.as_bytes() {
      b"32" => Ok((4, 8, 23)),
      b"64" => Ok((8, 11, 52)),
      _     => Err(sherr!(ParseErr, "invalid float width: {s}")).with_code(2),
    }
  }
  pub(crate) fn parse_type(s: &VarStr) -> ShResult<(usize, u32, u32)> {
    match s.as_bytes() {
      b"bf16" => Ok((2, 8, 7)),
      b"f16"  => Ok((2, 5, 10)),
      b"f32"  => Ok((4, 8, 23)),
      b"f64"  => Ok((8, 11, 52)),
      _       => Err(sherr!(ParseErr, "invalid float type: {s}")).with_code(2),
    }
  }

  pub(crate) fn format_decimal(self, bits: uNum) -> ShResult<VarStr> {
    fn shortest<T: Display + LowerExp>(v: T) -> VarStr {
      let plain = varstr!("{v}");
      let sci   = varstr!("{v:e}");
      if sci.len() < plain.len() { sci } else { plain }
    }
    match (self.exp_bits, self.mant_bits) {
      (8, 23)  => Ok(shortest(f32::from_bits(bits as u32))),
      (11, 52) => Ok(shortest(f64::from_bits(bits as u64))),
      _        => Err(sherr!(ParseErr, "--decimal supports only f32 and f64")).with_code(2),
    }
  }

  pub(crate) fn decode(self, bytes: &[u8]) -> ShResult<VarStr> {
    let mut bits: uNum = 0;
    if self.endian().little() {
      for (i, &b) in bytes.iter().enumerate() {
        bits |= uNum::from(b) << (8 * i as u32);
      }
    } else {
      for &b in bytes {
        bits = (bits << 8) | uNum::from(b);
      }
    }

    if self.decimal {
      return self.format_decimal(bits);
    }

    let bias      = (1 << (self.exp_bits - 1)) - 1;
    let exp_mask  = (1 << self.exp_bits) - 1;
    let mant_mask = (1 << self.mant_bits) - 1;
    let offset    = bias + Num::from(self.mant_bits);

    let sign      = (bits >> (self.width * 8 - 1)) & 1;
    let e_raw     = (bits >> self.mant_bits) & exp_mask;
    let m_raw     = bits & mant_mask;

    let (exp, mant): (Num, Num) = if e_raw == 0 {
      (1 - offset, m_raw as Num)
    } else if e_raw == exp_mask {
      (e_raw as Num - offset, m_raw as Num)
    } else {
      (e_raw as Num - offset, (1 << self.mant_bits) | m_raw as Num)
    };

    let sep = &self.sep;
    Ok(varstr!("{sign}{sep}{exp}{sep}{mant}"))
  }

  pub(crate) fn encode_triple(self, s: &[u8], e: &[u8], m: &[u8]) -> ShResult<Vec<u8>> {
    let sign = VarStr::from(s)
      .parse::<Sign>()
      .map_err(|v| sherr!(ParseErr, "invalid sign: '{v}'"))?;
    let exp = VarStr::from(e)
      .parse::<Num>()
      .map_err(|v| sherr!(ParseErr, "invalid exponent: '{v}'"))?;
    let mant = VarStr::from(m)
      .parse::<uNum>()
      .map_err(|v| sherr!(ParseErr, "invalid mantissa: '{v}'"))?;

    let bias      = (1 << (self.exp_bits - 1)) - 1;
    let exp_mask  = (1 << self.exp_bits) - 1;
    let mant_mask = (1 << self.mant_bits) - 1;
    let offset    = bias + Num::from(self.mant_bits);

    let shifted   = exp + offset;
    if !(0..=exp_mask as Num).contains(&shifted) {
      return Err(sherr!(ParseErr, "exponent out of range: {exp}")).with_code(2);
    }

    let implicit = 1 << self.mant_bits;

    if mant > (implicit | mant_mask) {
      return Err(sherr!(ParseErr, "mantissa out of range: {mant}")).with_code(2);
    }

    let (e_raw, m_raw) = if shifted == exp_mask as Num {
      if mant > mant_mask {
        return Err(sherr!(
          ParseErr,
          "mantissa out of range for infinity/NaN: {mant}"
        ))
        .with_code(2);
      }
      (exp_mask, mant)
    } else if mant >= implicit {
      if shifted < 1 {
        return Err(sherr!(
          ParseErr,
          "exponent too small for normalized mantissa: {exp}"
        ))
        .with_code(2);
      }
      (shifted as u128, mant - implicit)
    } else {
      if shifted != 1 {
        return Err(sherr!(
          ParseErr,
          "exponent too large for denormalized mantissa: {exp}"
        ))
        .with_code(2);
      }
      (0, mant)
    };

    let sign_bits = u128::from(*sign) << (self.width * 8 - 1);
    let exp_bits  = e_raw << self.mant_bits;
    let mant_bits = m_raw;

    let bits      = sign_bits | exp_bits | mant_bits;
    Ok(self.emit_bits(bits))
  }
  pub(crate) fn encode_decimal(self, decimal: &[u8]) -> ShResult<Vec<u8>> {
    let text = VarStr::from(decimal);
    let bits = match (self.exp_bits, self.mant_bits) {
      (8, 23) => u128::from(
        text
          .parse::<f32>()
          .map_err(|v| sherr!(ParseErr, "invalid f32: '{v}'"))?
          .to_bits(),
      ),
      (11, 52) => u128::from(
        text
          .parse::<f64>()
          .map_err(|v| sherr!(ParseErr, "invalid f64: '{v}'"))?
          .to_bits(),
      ),
      _ => return Err(sherr!(ParseErr, "--decimal supports only f32 and f64")).with_code(2),
    };
    Ok(self.emit_bits(bits))
  }

  pub(crate) fn emit_bits(self, bits: u128) -> Vec<u8> {
    let mut out = Vec::with_capacity(self.width);
    if self.endian().little() {
      for i in 0..self.width {
        out.push((bits >> (8 * i)) as u8);
      }
    } else {
      for i in (0..self.width).rev() {
        out.push((bits >> (8 * i)) as u8);
      }
    }

    out
  }

  pub(crate) fn sep(&self) -> char {
    self.sep
  }

  pub(crate) fn endian(&self) -> Endian {
    self.endian.unwrap_or_default()
  }

  pub(crate) fn try_set_endian(&mut self, endian: Endian) {
    if self.endian.is_some() {
      return;
    }
    self.endian = Some(endian);
  }

  pub(crate) fn decimal(&self) -> bool {
    self.decimal
  }

  pub(crate) fn width(&self) -> usize {
    self.width
  }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endian {
  #[default]
  Little,
  Big,
}

impl Endian {
  pub(crate) fn little(self) -> bool {
    self == Endian::Little
  }
}
