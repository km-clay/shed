use std::{
  fmt::{Display, LowerExp},
  io::Write,
  ops::Deref,
  str::FromStr,
  sync::Arc,
};

use crate::{
  expand::arithmetic::{Num, uNum},
  opt,
  procio::{self, Sink, SinkIo},
  sherr,
  state::{params, vars::VarStr},
  util::{self, error::ShResultExt, strops},
  varstr,
};

use super::{BuiltinArgs, ReadLimit, ShResult, opt::OptSpec};

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

#[derive(Debug, Clone, Copy)]
struct FloatSpec {
  width: usize,
  exp_bits: u32,
  mant_bits: u32,
  little_endian: bool,
  decimal: bool,
  sep: char,
}

impl FloatSpec {
  fn from_args(args: &super::BuiltinArgs) -> ShResult<Self> {
    if args.has_opt("type") && args.has_opt("width") {
      let type_span  = args.opt_span("type").unwrap();
      let width_span = args.opt_span("width").unwrap();

      let t_slice    = type_span.slice();
      let w_slice    = width_span.slice();

      return Err(sherr!(
        ParseErr @ type_span,
        "cannot specify both {t_slice} and {w_slice}"
      ))
      .with_code(2);
    }

    let little_endian = !args.has_opt("big-endian");

    let width = args
      .opt_value("type")
      .map(|t| Self::parse_type(&t.to_str_lossy()))
      .or_else(|| {
        args
          .opt_value("width")
          .map(|w| Self::parse_width(&w.to_str_lossy()))
      })
      .transpose()?;

    let Some((width, exp_bits, mant_bits)) = width else {
      return Err(sherr!(ParseErr, "must specify either --type or --width")).with_code(2);
    };

    let sep = params::get_separator()
      .as_bytes()
      .first()
      .copied()
      .map(|b| b as char)
      .unwrap_or(' ');

    Ok(Self {
      width,
      exp_bits,
      mant_bits,
      little_endian,
      decimal: args.has_opt("decimal"),
      sep,
    })
  }
  fn parse_width(s: &str) -> ShResult<(usize, u32, u32)> {
    match s {
      "32" => Ok((4, 8, 23)),
      "64" => Ok((8, 11, 52)),
      _    => Err(sherr!(ParseErr, "invalid float width: {s}")).with_code(2),
    }
  }
  fn parse_type(s: &str) -> ShResult<(usize, u32, u32)> {
    match s {
      "bf16" => Ok((2, 8, 7)),
      "f16"  => Ok((2, 5, 10)),
      "f32"  => Ok((4, 8, 23)),
      "f64"  => Ok((8, 11, 52)),
      _      => Err(sherr!(ParseErr, "invalid float type: {s}")).with_code(2),
    }
  }

  fn format_decimal(self, bits: uNum) -> ShResult<VarStr> {
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

  fn decode(self, bytes: &[u8]) -> ShResult<VarStr> {
    let mut bits: uNum = 0;
    if self.little_endian {
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

    let bias      = (1i128 << (self.exp_bits - 1)) - 1;
    let exp_mask  = (1u128 << self.exp_bits) - 1;
    let mant_mask = (1u128 << self.mant_bits) - 1;
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

  fn encode_triple(self, s: &[u8], e: &[u8], m: &[u8]) -> ShResult<Vec<u8>> {
    let sign = VarStr::from(s)
      .parse::<Sign>()
      .map_err(|v| sherr!(ParseErr, "invalid sign: '{v}'"))?;
    let exp = VarStr::from(e)
      .parse::<Num>()
      .map_err(|v| sherr!(ParseErr, "invalid exponent: '{v}'"))?;
    let mant = VarStr::from(m)
      .parse::<uNum>()
      .map_err(|v| sherr!(ParseErr, "invalid mantissa: '{v}'"))?;

    let bias      = (1i128 << (self.exp_bits - 1)) - 1;
    let exp_mask  = (1u128 << self.exp_bits) - 1;
    let mant_mask = (1u128 << self.mant_bits) - 1;
    let offset    = bias + Num::from(self.mant_bits);

    let shifted   = exp + offset;
    if !(0..=exp_mask as Num).contains(&shifted) {
      return Err(sherr!(ParseErr, "exponent out of range: {exp}")).with_code(2);
    }

    let implicit = 1u128 << self.mant_bits;

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
  fn encode_decimal(self, decimal: &[u8]) -> ShResult<Vec<u8>> {
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

  fn emit_bits(self, bits: u128) -> Vec<u8> {
    let mut out = Vec::with_capacity(self.width);
    if self.little_endian {
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
}

pub(crate) struct ReadFloat;
impl super::Builtin for ReadFloat {
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("at-most", b'n').argc(1),
      OptSpec::new_short("exactly", b'N').argc(1),
      opt!("array" | b'a', 1),
      opt!("width" | b'w', 1),
      opt!("type" | b'T', 1),
      opt!("big-endian" | b'E'),
      opt!("decimal" | b'd'),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let (at_most, exactly) = (args.opt_value("at-most"), args.opt_value("exactly"));
    let limit = match (at_most, exactly) {
      (Some(v), None) => ReadLimit::at_most(&v).promote_err(args.cmd_span())?,
      (None, Some(v)) => ReadLimit::exactly(&v).promote_err(args.cmd_span())?,
      (None, None)    => ReadLimit::default(),
      (Some(_), Some(_)) => {
        return Err(sherr!(ParseErr @ args.cmd_span(), "cannot specify both -n and -N"))
          .with_code(2);
      }
    };

    let     reader: Arc<dyn Sink> = procio::stdin_sink()?;
    let mut writer: SinkIo        = SinkIo(procio::stdout_sink()?);

    let     spec  : FloatSpec     = FloatSpec::from_args(&args).promote_err(args.cmd_span())?;
    let     width : usize         = spec.width;
    let     want  : usize         = limit.wanted();

    let mut buf   : Vec<u8>       = vec![0u8; want * width];
    let     got   : usize         = reader.read_all(&mut buf).promote_err(args.cmd_span())?;
    let     whole : usize         = got / width;
    let     rem   : usize         = got % width;

    if rem != 0 {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "incomplete float read: got {got} bytes, expected multiple of {width}"),
      );
    }

    if let ReadLimit::Exactly(_) = limit
      && whole < want
    {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "incomplete float read: got {whole} floats, expected {want}"),
      );
    }

    if whole == 0 {
      return util::with_status(1);
    }

    let vals = buf[..got]
      .chunks_exact(width)
      .map(|chunk| spec.decode(chunk))
      .collect::<ShResult<Vec<_>>>()
      .promote_err(args.cmd_span())?
      .into_iter();

    super::emit(vals, &mut writer, &args).promote_err(args.cmd_span())
  }
}

pub(crate) struct WriteFloat;
impl super::Builtin for WriteFloat {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("type" | b'T', 1),
      opt!("width" | b'w', 1),
      opt!("decimal" | b'd'),
      opt!("big-endian" | b'E'),
    ]
  }
  fn execute(&self, mut args: BuiltinArgs) -> ShResult<()> {
    if !args.has_opt("width") && !args.has_opt("type") {
      return Err(sherr!(ParseErr @ args.cmd_span(), "must specify --width or --type"))
        .with_code(2);
    }

    let     spec                 = FloatSpec::from_args(&args).promote_err(args.cmd_span())?;
    let mut writer               = SinkIo(procio::stdout_sink()?);

    let     ifs                  = params::get_separators();
    let mut fields: Vec<Vec<u8>> = vec![];

    if let Some(input) = self.get_input(&mut args) {
      fields = strops::ifs_split(&input, &ifs, None);
    } else {
      for (arg, _) in args.arguments() {
        let split = strops::ifs_split(arg, &ifs, None);
        fields.extend(split);
      }
    }

    if fields.is_empty() {
      return util::with_status(1);
    }

    let per = if spec.decimal { 1 } else { 3 };
    if !fields.len().is_multiple_of(per) {
      return Err(sherr!(ParseErr @ args.cmd_span(), "expected a multiple of {per} fields per float, got {}", fields.len())).with_code(2);
    }

    for chunk in fields.chunks_exact(per) {
      let out = if spec.decimal {
        spec
          .encode_decimal(&chunk[0])
          .promote_err(args.cmd_span())?
      } else {
        spec
          .encode_triple(&chunk[0], &chunk[1], &chunk[2])
          .promote_err(args.cmd_span())?
      };

      writer.write_all(&out).ok();
    }

    util::with_status(0)
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
    let g   = TestGuard::new();
    let _   = test_input(cmd);
    let out = g.read_output();
    out.contains("Error") || out.trim_end().ends_with("st=1") || out.trim_end().ends_with("st=2")
  }

  #[test]
  fn decodes_a_normal_value() {
    assert_eq!(
      out_of(r"printf '\x00\x00\x80\x3f' | readfloat -T f32"),
      "0 -23 8388608"
    );
    assert_eq!(
      out_of(r"printf '\x00\x00\x40\x3f' | readfloat -T f32"),
      "0 -24 12582912"
    );
  }

  #[test]
  fn the_sign_bit_is_a_separate_field() {
    assert_eq!(
      out_of(r"printf '\x00\x00\x00\xbf' | readfloat -T f32"),
      "1 -24 8388608"
    );
  }

  #[test]
  fn both_zeros_decode_distinctly() {
    assert_eq!(
      out_of(r"printf '\x00\x00\x00\x00' | readfloat -T f32"),
      "0 -149 0"
    );
    assert_eq!(
      out_of(r"printf '\x00\x00\x00\x80' | readfloat -T f32"),
      "1 -149 0"
    );
  }

  /// A subnormal has no implicit bit and takes its exponent from `1 - bias -
  /// mant_bits`, not from the stored zero. The smallest normal shares that
  /// exponent, so the implicit bit is what tells them apart.
  #[test]
  fn subnormals_share_the_smallest_exponent_and_differ_by_the_implicit_bit() {
    assert_eq!(
      out_of(r"printf '\x00\x00\x80\x00' | readfloat -T f32"),
      "0 -149 8388608"
    );
    assert_eq!(
      out_of(r"printf '\x01\x00\x00\x00' | readfloat -T f32"),
      "0 -149 1"
    );
  }

  #[test]
  fn the_largest_finite_value_decodes() {
    assert_eq!(
      out_of(r"printf '\xff\xff\x7f\x7f' | readfloat -T f32"),
      "0 104 16777215"
    );
  }

  /// Infinity and NaN land one past the finite maximum, so they need no
  /// sentinel of their own; a zero mantissa separates infinity from NaN.
  #[test]
  fn infinity_and_nan_sit_past_the_finite_exponent_range() {
    assert_eq!(
      out_of(r"printf '\x00\x00\x80\x7f' | readfloat -T f32"),
      "0 105 0"
    );
    assert_eq!(
      out_of(r"printf '\x00\x00\xc0\x7f' | readfloat -T f32"),
      "0 105 4194304"
    );
  }

  #[test]
  fn big_endian_reverses_the_byte_order() {
    assert_eq!(
      out_of(r"printf '\x3f\x80\x00\x00' | readfloat -T f32 -E"),
      "0 -23 8388608"
    );
  }

  #[test]
  fn f64_uses_its_own_field_widths() {
    assert_eq!(
      out_of(r"printf '\x00\x00\x00\x00\x00\x00\xf0\x3f' | readfloat -T f64"),
      "0 -52 4503599627370496"
    );
  }

  /// f16 and bf16 are both two bytes with different layouts, so the byte width
  /// alone cannot identify the format.
  #[test]
  fn f16_and_bf16_decode_the_same_bytes_differently() {
    assert_eq!(
      out_of(r"printf '\x80\x3f' | readfloat -T f16"),
      "0 -10 1920"
    );
    assert_eq!(out_of(r"printf '\x80\x3f' | readfloat -T bf16"), "0 -7 128");
  }

  #[test]
  fn a_count_reads_several_floats() {
    assert_eq!(
      out_of(r"printf '\x00\x00\x80\x3f\x00\x00\x40\x3f' | readfloat -T f32 -n 2"),
      "0 -23 8388608\n0 -24 12582912"
    );
  }

  #[test]
  fn an_array_holds_one_record_per_float() {
    assert_eq!(
      out_of(
        r#"printf '\x00\x00\x80\x3f\x00\x00\x40\x3f' | readfloat -T f32 -n 2 -a f; printf '[%s]' "${f[@]}""#
      ),
      "[0 -23 8388608][0 -24 12582912]"
    );
  }

  #[test]
  fn an_array_record_resplits_into_three_fields() {
    assert_eq!(
      out_of(
        r"printf '\x00\x00\x40\x3f' | readfloat -T f32 -a f; parts=( ${f[0]} ); printf '%s/%s/%s' ${parts[0]} ${parts[1]} ${parts[2]}"
      ),
      "0/-24/12582912"
    );
  }

  #[test]
  fn an_empty_batch_fails_so_a_while_loop_ends() {
    assert_eq!(
      out_of(
        r"printf '\x00\x00\x80\x3f\x00\x00\x40\x3f' | { n=0; while readfloat -T f32 -n 1 -a f; do n=$(( n + 1 )); (( n >= 5 )) && break; done; printf 'iters=%s' $n; }"
      ),
      "iters=2"
    );
  }

  #[test]
  fn readfloat_rejects_bad_usage() {
    assert!(fails(r"printf '\x00\x00\x80\x3f' | readfloat"));
    assert!(fails(r"printf '\x00\x00\x80\x3f' | readfloat -T f32 -w 32"));
    assert!(fails(r"printf '\x00\x00\x80\x3f' | readfloat -T f17"));
    assert!(fails(r"printf '\x00\x00\x80\x3f' | readfloat -w 16"));
    assert!(fails(
      r"printf '\x00\x00\x80\x3f' | readfloat -T f32 -n 1 -N 1"
    ));
    assert!(fails(r"printf '\x00\x00\x80\x3f' | readfloat -T f32 -n 0"));
    assert!(fails(r"printf '\x01\x02\x03' | readfloat -T f32"));
    assert!(fails(r"printf '\x00\x00\x80\x3f' | readfloat -T f32 -N 2"));
  }

  #[test]
  fn decimal_mode_prints_one_field_per_float() {
    assert_eq!(
      out_of(r"printf '\x00\x00\x40\x3f' | readfloat -T f32 -d"),
      "0.75"
    );
    assert_eq!(
      out_of(r"printf '\x00\x00\x00\x80' | readfloat -T f32 -d"),
      "-0"
    );
    assert_eq!(
      out_of(r"printf '\x00\x00\x80\x7f' | readfloat -T f32 -d"),
      "inf"
    );
  }

  /// `--decimal` needs a Rust float type to reinterpret the bits into, so it is
  /// limited to the two widths that have one.
  #[test]
  fn decimal_mode_is_f32_and_f64_only() {
    assert!(fails(r"printf '\x00\x3c' | readfloat -T f16 -d"));
    assert!(fails(r"printf '\x80\x3f' | readfloat -T bf16 -d"));
    assert!(fails(r"writefloat -T f16 -d 1.0"));
    assert!(fails(r"writefloat -T bf16 -d 1.0"));
  }

  #[test]
  fn writefloat_encodes_a_triple() {
    assert_eq!(
      out_of(
        r"writefloat -T f32 0 -24 12582912 >@v; printf '\x00\x00\x40\x3f' >@w; [[ $v == $w ]] && printf same"
      ),
      "same"
    );
  }

  #[test]
  fn writefloat_groups_fields_the_same_way_however_they_arrive() {
    let want = "len=8";
    assert_eq!(
      out_of(r"writefloat -T f32 '0 -24 12582912' '1 -24 8388608' >@v; printf 'len=%s' ${#v}"),
      want
    );
    assert_eq!(
      out_of(r"writefloat -T f32 0 -24 12582912 1 -24 8388608 >@v; printf 'len=%s' ${#v}"),
      want
    );
    assert_eq!(
      out_of(
        r"f=('0 -24 12582912' '1 -24 8388608'); writefloat -T f32 ${f[@]} >@v; printf 'len=%s' ${#v}"
      ),
      want
    );
    assert_eq!(
      out_of(
        r#"f=('0 -24 12582912' '1 -24 8388608'); writefloat -T f32 "${f[@]}" >@v; printf 'len=%s' ${#v}"#
      ),
      want
    );
    assert_eq!(
      out_of(
        r"printf '0 -24 12582912 1 -24 8388608' | writefloat -T f32 >@v; printf 'len=%s' ${#v}"
      ),
      want
    );
  }

  #[test]
  fn writefloat_rejects_an_inconsistent_triple() {
    assert!(fails(r"writefloat -T f32 2 -24 12582912"));
    assert!(fails(r"writefloat -T f32 -1 -24 12582912"));
    assert!(fails(r"writefloat -T f32 x -24 12582912"));
    assert!(fails(r"writefloat -T f32 0 -200 8388608"));
    assert!(fails(r"writefloat -T f32 0 50 5"));
    assert!(fails(r"writefloat -T f32 0 -24 z"));
    assert!(fails(r"writefloat -T f32 0 -24 -5"));
  }

  /// For infinity and NaN the mantissa must still fit the mantissa field; one
  /// bit past it collides with the exponent and the payload is lost.
  #[test]
  fn writefloat_bounds_the_nan_payload() {
    assert_eq!(
      out_of(r"writefloat -T f32 0 105 8388607 | readfloat -T f32"),
      "0 105 8388607"
    );
    assert!(fails(r"writefloat -T f32 0 105 8388608"));
  }

  #[test]
  fn writefloat_needs_a_whole_number_of_records() {
    assert!(fails(r"writefloat -T f32 0 -24"));
    assert!(fails(r"writefloat -T f32 0 -24 12582912 0"));
  }

  #[test]
  fn writefloat_with_no_input_writes_nothing() {
    assert_eq!(
      out_of(r"writefloat -T f32 </dev/null >@v; printf 'len=%s st=%s' ${#v} $?"),
      "len=0 st=1"
    );
  }

  #[test]
  fn writefloat_encodes_decimals() {
    assert_eq!(
      out_of(r"writefloat -T f32 -d 0.75 | readfloat -T f32"),
      "0 -24 12582912"
    );
    assert_eq!(
      out_of(r"writefloat -T f32 -d 1.5 2.5 3.5 >@v; printf 'len=%s' ${#v}"),
      "len=12"
    );
    assert!(fails(r"writefloat -T f32 -d notanumber"));
  }

  #[test]
  fn a_triple_round_trips_through_writefloat() {
    for (ty, n) in [("f32", 2), ("f64", 1), ("f16", 4), ("bf16", 4)] {
      assert_eq!(
        out_of(&format!(
          r#"printf '\x00\x00\x40\x3f\x00\x00\x80\x3f' >@src; printf '%s' $src | readfloat -T {ty} -n {n} -a f; writefloat -T {ty} "${{f[@]}}" >@back; [[ $src == $back ]] && printf same"#
        )),
        "same",
        "round trip failed for {ty}"
      );
    }
  }

  #[test]
  fn a_stream_round_trips_through_writefloat() {
    assert_eq!(
      out_of(
        r"printf '\x00\x00\x40\x3f\x00\x00\x80\x3f' >@src; printf '%s' $src | readfloat -T f32 -n 2 | writefloat -T f32 >@back; [[ $src == $back ]] && printf same"
      ),
      "same"
    );
  }
}
