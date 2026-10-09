use std::{
  io::{self, Write},
  sync::Arc,
};

use crate::{
  builtin::BuiltinArgs,
  opt,
  procio::{self, Sink, SinkIo},
  sherr, signal,
  state::{
    Shed, params,
    vars::{VarFlags, VarKind, VarStr},
  },
  util::{
    self,
    error::{ShResult, ShResultExt},
    strops::{self, ParseRadix},
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

enum IntRead {
  AtMost(usize),
  Exactly(usize),
}

impl Default for IntRead {
  fn default() -> Self {
    Self::Exactly(1)
  }
}

impl IntRead {
  fn at_most(arg: &VarStr) -> ShResult<Self> {
    let n = arg
      .to_str_lossy()
      .parse::<usize>()
      .map_err(|_| sherr!(ParseErr, "invalid integer for -n: {arg}"))?;
    if n == 0 {
      return Err(sherr!(
        ParseErr,
        "invalid integer for -n: {arg} (must be > 0)"
      ));
    }
    Ok(Self::AtMost(n))
  }
  fn exactly(arg: &VarStr) -> ShResult<Self> {
    let n = arg
      .to_str_lossy()
      .parse::<usize>()
      .map_err(|_| sherr!(ParseErr, "invalid integer for -N: {arg}"))?;
    if n == 0 {
      return Err(sherr!(
        ParseErr,
        "invalid integer for -N: {arg} (must be > 0)"
      ));
    }
    Ok(Self::Exactly(n))
  }

  fn wanted(&self) -> usize {
    match self {
      Self::AtMost(n) | Self::Exactly(n) => *n,
    }
  }
}

fn fill(reader: &Arc<dyn Sink>, buf: &mut [u8]) -> ShResult<usize> {
  let mut got = 0;
  while got < buf.len() {
    match reader.read(&mut buf[got..]) {
      Ok(0) => break,
      Ok(n) => got += n,
      Err(e) if e.kind() == io::ErrorKind::Interrupted => {
        signal::check_signals()?;
      }
      Err(e) => return Err(sherr!(ExecFail, "failed to read from stdin: {e}")),
    }
  }
  Ok(got)
}

fn emit(
  vals: impl Iterator<Item = VarStr>,
  writer: &mut SinkIo,
  args: &BuiltinArgs,
) -> ShResult<()> {
  if let Some(name) = args.opt_value("array") {
    Shed::vars_mut(|v| v.set_var(&name.to_str_lossy(), VarKind::arr(vals), VarFlags::empty()))
      .promote_err(args.cmd_span())?;
  } else {
    for (i, v) in vals.enumerate() {
      if i > 0 {
        writer.write_all(b"\n").ok();
      }
      writer.write_all(&v).ok();
    }
  }

  util::with_status(0)
}

pub(super) struct ReadInt;
#[rustfmt::skip]
impl super::Builtin for ReadInt {
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("at-most", b'n').argc(1),
      OptSpec::new_short("exactly", b'N').argc(1),
      OptSpec::new_short("array",   b'a').argc(1),
      opt!("width"      | b'w', 1),
      opt!("type"       | b'T', 1),
      opt!("big-endian" | b'E'),
      opt!("signed"     | b's'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.has_opt("at-most") && args.has_opt("exactly") {
      return Err(sherr!(ParseErr @ args.cmd_span(), "cannot specify both -n and -N")).with_code(2);
    }

    let spec = IntSpec::from_args(&args).promote_err(args.cmd_span())?;

    if spec.width.is_none() && (args.has_opt("at-most") || args.has_opt("exactly")) {
      return Err(sherr!(ParseErr @ args.cmd_span(), "cannot specify -n or -N without --width or --type")).with_code(2);
    }

    let reader = procio::stdin_sink()?;
    let mut writer = SinkIo(procio::stdout_sink()?);

    let limit = args.opt_value("at-most")
      .map(|v| IntRead::at_most(&v))
      .or_else(|| {
        args.opt_value("exactly")
          .map(|v| IntRead::exactly(&v))
      })
      .transpose()
      .promote_err(args.cmd_span())?
      .unwrap_or_default();

    match spec.width {
      Some(w) => Self::sized_read(
        usize::from(w),
        limit,
        &reader,
        &mut writer,
        spec,
        &args,
      ).promote_err(args.cmd_span()),

      None => Self::inferred_read(
        &reader,
        &mut writer,
        spec,
        &args,
      ).promote_err(args.cmd_span())
    }

  }
}

#[rustfmt::skip]
impl ReadInt {
  fn inferred_read(
    reader: &Arc<dyn Sink>,
    writer: &mut SinkIo,
    mut spec: IntSpec,
    args: &BuiltinArgs,
  ) -> ShResult<()> {
    let mut buf = vec![0u8; 17];
    let got = fill(reader, &mut buf)
      .promote_err(args.cmd_span())?;

    if got == 0 {
      return util::with_status(1)
    }
    if got > 16 {
      return Err(sherr!(ExecFail @ args.cmd_span(), "input too long to infer width: got {got} bytes, max 16"));
    }

    spec.width = Some(got as u8);
    let val = std::iter::once(spec.decode(&buf[..got]));

    emit(val, writer, args).promote_err(args.cmd_span())
  }
  fn sized_read(
    width: usize,
    limit: IntRead,
    reader: &Arc<dyn Sink>,
    writer: &mut SinkIo,
    spec: IntSpec,
    args: &BuiltinArgs,
  ) -> ShResult<()> {
    let want = limit.wanted();

    let mut buf = vec![0u8; width * want];
    let     got = fill(reader, &mut buf).promote_err(args.cmd_span())?;

    let whole = got / width;
    let rem   = got % width;

    if rem != 0 {
      return Err(sherr!(ExecFail, "trailing {rem} bytes do not form a complete integer"));
    }

    if let IntRead::Exactly(_) = limit && whole < want {
      return Err(sherr!(ExecFail, "expected {want} integers, got {whole}"));
    }

    if whole == 0 {
      return util::with_status(1)
    }

    let vals = buf[..got]
      .chunks_exact(width)
      .map(|c| spec.decode(c));

    emit(vals, writer, args).promote_err(args.cmd_span())
  }
}

pub(super) struct WriteInt;
#[rustfmt::skip]
impl super::Builtin for WriteInt {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("width"      | b'w', 1),
      opt!("type"       | b'T', 1),
      opt!("big-endian" | b'E'),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    if !args.has_opt("width") && !args.has_opt("type") {
      return Err(sherr!(ParseErr @ args.cmd_span(), "must specify --width or --type")).with_code(2);
    }
    let spec = IntSpec::from_args(&args).promote_err(args.cmd_span())?;
    let mut out = SinkIo(procio::stdout_sink()?);

    if let Some(input) = self.get_input(&mut args) {
      // ifs split the input, like read
      // ifs chars are probably not hex digits, so this should be fine
      let span = args.cmd_span();
      let ifs = params::get_separators();

      let fields = strops::ifs_split(&input, &ifs, None);

      if fields.is_empty() {
        return util::with_status(1);
      }

      for field in fields {
        Self::encode_into(&mut out, spec, &field.into()).promote_err(span)?;
      }
    } else {
      if args.no_arguments() {
        return util::with_status(1)
      };

      // arg case, used mainly to consume arrays created by readint
      for (arg, span) in args.arguments() {
        Self::encode_into(&mut out, spec, arg).promote_err(span)?;
      }
    };

    util::with_status(0)
  }
}

impl WriteInt {
  fn encode_into(out: &mut SinkIo, spec: IntSpec, raw: &VarStr) -> ShResult<()> {
    let text = raw.to_str_lossy();
    let val = <i128>::parse_radix(text.trim())
      .or_else(|| <u128>::parse_radix(text.trim()).map(|u| u as i128))
      .ok_or_else(|| sherr!(ParseErr, "invalid integer: {raw}"))?;

    out.write_all(&spec.encode(val)).ok();
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use crate::tests::testutil::{TestGuard, test_input};

  fn out_of(cmd: &str) -> String {
    let g = TestGuard::new();
    test_input(cmd).unwrap();
    g.read_output()
  }

  fn fails(cmd: &str) -> bool {
    let g = TestGuard::new();
    let _ = test_input(cmd);
    let out = g.read_output();
    out.contains("Error") || out.trim_end().ends_with("st=1") || out.trim_end().ends_with("st=2")
  }

  #[test]
  fn reads_a_sized_little_endian_value() {
    assert_eq!(out_of(r"printf '\x2c\x01' | readint -w 16"), "300");
  }

  #[test]
  fn reads_a_signed_value() {
    assert_eq!(out_of(r"printf '\xff\xff' | readint --type i16"), "-1");
  }

  #[test]
  fn reads_a_big_endian_value() {
    assert_eq!(out_of(r"printf '\x01\x2c' | readint -w 16 -E"), "300");
  }

  #[test]
  fn infers_width_from_the_input() {
    assert_eq!(out_of(r"printf '\x01\x02\x03' | readint"), "197121");
  }

  #[test]
  fn infers_width_through_a_thru_slice() {
    assert_eq!(
      out_of(r"printf '\x01\x02\x03\x04' | thru -T 3 | readint"),
      "197121"
    );
  }

  #[test]
  fn empty_input_is_quiet_failure() {
    assert_eq!(out_of(r"printf '' | readint; printf 'st=%s' $?"), "st=1");
  }

  #[test]
  fn inference_rejects_more_than_sixteen_bytes() {
    assert!(fails(r"head -c 17 /dev/zero | readint"));
  }

  #[test]
  fn a_count_reads_several_values_separated_by_newlines() {
    assert_eq!(
      out_of(r"printf '\x01\x00\x00\x00\x02\x00\x00\x00\x03\x00\x00\x00' | readint -T u32 -n 3"),
      "1\n2\n3"
    );
  }

  #[test]
  fn at_most_tolerates_a_short_stream() {
    assert_eq!(
      out_of(r"printf '\x01\x00\x00\x00\x02\x00\x00\x00' | readint -T u32 -n 5"),
      "1\n2"
    );
  }

  #[test]
  fn exactly_fails_on_a_short_stream() {
    assert!(fails(r"printf '\x01\x00\x00\x00' | readint -T u32 -N 2"));
  }

  #[test]
  fn a_partial_trailing_value_is_an_error() {
    assert!(fails(
      r"printf '\x01\x00\x00\x00\x02\x00' | readint -T u32 -n 2"
    ));
  }

  #[test]
  fn a_zero_count_is_rejected() {
    assert!(fails(r"printf '\x01\x00\x00\x00' | readint -T u32 -n 0"));
    assert!(fails(r"printf '\x01\x00\x00\x00' | readint -T u32 -N 0"));
  }

  #[test]
  fn both_count_flags_together_are_rejected() {
    assert!(fails(
      r"printf '\x01\x00\x00\x00' | readint -T u32 -n 2 -N 2"
    ));
  }

  #[test]
  fn a_count_without_a_width_is_rejected() {
    assert!(fails(r"printf '\x01\x02\x03' | readint -n 2"));
    assert!(fails(r"printf '\x01\x02\x03' | readint -N 2"));
  }

  #[test]
  fn an_array_receives_each_value() {
    assert_eq!(
      out_of(
        r"printf '\x01\x00\x00\x00\x02\x00\x00\x00' | readint -T u32 -n 2 -a arr; printf '%s,' ${arr[@]}"
      ),
      "1,2,"
    );
  }

  #[test]
  fn an_array_without_a_count_holds_one_value() {
    assert_eq!(
      out_of(r"printf '\x2c\x01' | readint -w 16 -a arr; printf '%s;%s' ${#arr[@]} ${arr[0]}"),
      "1;300"
    );
  }

  #[test]
  fn an_inferred_read_fills_an_array_too() {
    assert_eq!(
      out_of(r"printf '\x01\x02\x03' | readint -a arr; printf '%s;%s' ${#arr[@]} ${arr[0]}"),
      "1;197121"
    );
  }

  #[test]
  fn writeint_encodes_one_operand() {
    assert_eq!(
      out_of(r"writeint -w 16 300 >@a; printf '\x2c\x01' >@b; [[ $a == $b ]] && printf same"),
      "same"
    );
  }

  #[test]
  fn writeint_encodes_every_operand() {
    assert_eq!(
      out_of(
        r"writeint -T u16 1 2 >@a; printf '\x01\x00\x02\x00' >@b; [[ $a == $b ]] && printf same"
      ),
      "same"
    );
  }

  #[test]
  fn writeint_honors_big_endian() {
    assert_eq!(
      out_of(r"writeint -T u16 -E 300 >@a; printf '\x01\x2c' >@b; [[ $a == $b ]] && printf same"),
      "same"
    );
  }

  #[test]
  fn writeint_splits_stdin_on_ifs() {
    assert_eq!(
      out_of(
        r"printf '1\n2\n' | writeint -T u16 >@a; printf '\x01\x00\x02\x00' >@b; [[ $a == $b ]] && printf same"
      ),
      "same"
    );
    assert_eq!(
      out_of(
        r"echo '1 2' | writeint -T u16 >@a; printf '\x01\x00\x02\x00' >@b; [[ $a == $b ]] && printf same"
      ),
      "same"
    );
  }

  #[test]
  fn writeint_honors_a_custom_ifs() {
    assert_eq!(
      out_of(
        r"printf '1,2' | IFS=, writeint -T u16 >@a; printf '\x01\x00\x02\x00' >@b; [[ $a == $b ]] && printf same"
      ),
      "same"
    );
  }

  #[test]
  fn writeint_with_an_empty_stream_writes_nothing_and_fails() {
    assert_eq!(
      out_of(r"writeint -T u32 </dev/null >@v; printf 'len=%s st=%s' ${#v} $?"),
      "len=0 st=1"
    );
  }

  #[test]
  fn an_empty_batch_read_fails_so_a_while_loop_ends() {
    assert_eq!(
      out_of(r"printf '\x01\x00\x00\x00' | readint -T u32 -n 2 >@v; printf 'st=%s' $?"),
      "st=0"
    );
    assert_eq!(
      out_of(r"readint -T u32 -n 2 </dev/null; printf 'st=%s' $?"),
      "st=1"
    );
    assert_eq!(
      out_of(
        r"printf '\x01\x00\x02\x00\x03\x00\x04\x00' | { n=0; while readint -T u16 -n 2 -a f; do n=$(( n + 1 )); (( n >= 5 )) && break; done; printf 'iters=%s' $n; }"
      ),
      "iters=2"
    );
  }

  #[test]
  fn writeint_accepts_the_whole_unsigned_range() {
    assert_eq!(
      out_of(r"writeint -T u128 340282366920938463463374607431768211455 | readint -T u128"),
      "340282366920938463463374607431768211455"
    );
  }

  #[test]
  fn an_array_round_trips_through_writeint() {
    assert_eq!(
      out_of(
        r"printf '\x01\x00\x02\x00\x03\x00' >@src; printf '%s' $src | readint -T u16 -n 3 -a arr; writeint -T u16 ${arr[@]} >@back; [[ $src == $back ]] && printf same"
      ),
      "same"
    );
  }

  #[test]
  fn a_stream_round_trips_through_writeint() {
    assert_eq!(
      out_of(
        r"printf '\x01\x00\x02\x00\x03\x00' >@src; printf '%s' $src | readint -T u16 -n 3 | writeint -T u16 >@back; [[ $src == $back ]] && printf same"
      ),
      "same"
    );
  }
}
