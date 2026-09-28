use std::io;

use crate::{
  opt, procio, sherr, signal,
  state::vars::VarStr,
  util::{
    self,
    error::{ShResult, ShResultExt},
    strops::ParseRadix,
  },
  varstr,
};

use super::opt::OptSpec;

#[derive(Debug, Clone, Copy)]
struct IntSpec {
  width: Option<u8>,
  signed: bool,
  little_endian: bool,
}

impl IntSpec {
  fn from_args(args: &super::BuiltinArgs) -> ShResult<Self> {
    if args.has_opt("type") && (args.has_opt("width") || args.has_opt("signed")) {
      return Err(sherr!(
        ParseErr,
        "cannot specify both --type and --width/--signed"
      ));
    }

    let little_endian = !args.has_opt("big-endian");

    let (signed, width) = if let Some(t) = args.opt_value("type") {
      Self::parse_type(&t.to_str_lossy())?
    } else {
      let width = args
        .opt_value("width")
        .map(|w| Self::parse_width(&w.to_str_lossy()))
        .transpose()?;
      let signed = args.has_opt("signed");
      (signed, width)
    };
    Ok(IntSpec {
      width,
      signed,
      little_endian,
    })
  }
  fn parse_width(s: &str) -> ShResult<u8> {
    match s {
      "8" => Ok(1),
      "16" => Ok(2),
      "32" => Ok(4),
      "64" => Ok(8),
      "128" => Ok(16),
      _ => Err(sherr!(ParseErr, "invalid integer width: {s}")),
    }
  }
  fn parse_type(s: &str) -> ShResult<(bool, Option<u8>)> {
    if s.len() <= 1 {
      return Err(sherr!(ParseErr, "invalid integer type: {s}"));
    }

    let signed = match s.chars().next().unwrap() {
      'i' => true,
      'u' => false,
      _ => return Err(sherr!(ParseErr, "invalid integer type: {s}")),
    };
    let width = Self::parse_width(&s[1..])?;

    Ok((signed, Some(width)))
  }

  fn decode(self, bytes: &[u8]) -> VarStr {
    debug_assert!(self.width.is_some());
    let width = u32::from(self.width.unwrap());

    let mut val: u128 = 0;
    if self.little_endian {
      for (i, &b) in bytes.iter().enumerate() {
        val |= u128::from(b) << (8 * i as u32);
      }
    } else {
      for &b in bytes {
        val = (val << 8) | u128::from(b);
      }
    }

    if !self.signed {
      return varstr!("{val}");
    }

    let bits = width * 8;
    let signed_val = if bits == 128 {
      val as i128
    } else if val & (1u128 << (bits - 1)) != 0 {
      val as i128 - (1i128 << bits)
    } else {
      val as i128
    };

    varstr!("{signed_val}")
  }

  fn encode(self, val: i128) -> Vec<u8> {
    debug_assert!(self.width.is_some());
    let width = self.width.unwrap();

    let u = val as u128;
    let mut out = Vec::with_capacity(width as usize);
    let bits = if self.little_endian {
      itertools::Either::Left(0..width)
    } else {
      itertools::Either::Right((0..width).rev())
    };

    for i in bits {
      out.push((u >> (8 * u32::from(i))) as u8);
    }

    out
  }
}

pub(super) struct ReadInt;
impl super::Builtin for ReadInt {
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("width" | b'w', 1),
      opt!("type" | b'T', 1),
      opt!("big-endian" | b'E'),
      opt!("signed" | b's'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut spec = IntSpec::from_args(&args).promote_err(args.cmd_span())?;

    let reader = procio::stdin_sink()?;
    let cap = spec.width.map_or(17, usize::from);
    let mut buf = vec![0u8; cap];
    let mut got = 0;

    while got < buf.len() {
      match reader.read(&mut buf[got..]) {
        Ok(0) => break,
        Ok(n) => got += n,
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
          signal::check_signals()?;
        }
        Err(e) => return Err(sherr!(ExecFail @ args.cmd_span(), "failed to read from stdin: {e}")),
      }
    }

    if let Some(w) = spec.width
      && got < usize::from(w)
    {
      return Err(sherr!(ExecFail @ args.cmd_span(), "expected {w} bytes, got {got}"));
    } else if spec.width.is_none() {
      if got == 0 {
        return Err(sherr!(ExecFail @ args.cmd_span(), "no input to read"));
      }
      if got > 16 {
        return Err(
          sherr!(ExecFail @ args.cmd_span(), "input too long to infer width: got {got} bytes, max 16"),
        );
      }
      spec.width = Some(got as u8);
    }

    procio::out_bytes(&spec.decode(&buf[..got]));
    util::with_status(0)
  }
}

pub(super) struct WriteInt;
impl super::Builtin for WriteInt {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("width" | b'w', 1),
      opt!("type" | b'T', 1),
      opt!("big-endian" | b'E'),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    if !args.has_opt("width") && !args.has_opt("type") {
      return Err(sherr!(ParseErr @ args.cmd_span(), "must specify --width or --type"));
    }
    let spec = IntSpec::from_args(&args).promote_err(args.cmd_span())?;

    let (int, span) = if let Some(input) = self.get_input_str(&mut args) {
      (input, args.cmd_span())
    } else {
      let Some((arg, span)) = args.arguments().next() else {
        return Err(sherr!(ParseErr @ args.cmd_span(), "missing integer argument"));
      };
      (arg.clone(), span)
    };

    let val: i128 = ParseRadix::parse_radix(int.to_str_lossy().trim())
      .ok_or_else(|| sherr!(ParseErr @ span, "invalid integer: {int}"))?;

    procio::out_bytes(&spec.encode(val));
    util::with_status(0)
  }
}
