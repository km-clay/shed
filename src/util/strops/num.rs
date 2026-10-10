//! Numeric-literal parsing and display helpers.

use std::fmt::Display;

use crate::{state::vars::VarStr, varstr};

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
