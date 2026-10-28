use std::{collections::VecDeque, io, sync::Arc};

use bstr::ByteSlice;
use itertools::Itertools;

use crate::{
  HashSet,
  eval::lex::Span,
  expand::Word,
  opt,
  procio::{self, ScratchGuard, Sink},
  sherr, shopt,
  state::{
    Shed,
    vars::{ShStr, VarFlags, VarKind},
  },
  util::{
    self,
    error::ShResultExt,
    strops::{self, FloatSpec, IntSpec},
  },
  varstr,
};

use super::{BuiltinArgs, ShResult, opt::OptSpec};

#[derive(Debug, Clone)]
enum SpecField {
  Named { name: ShStr, ty: SpecType },
  Positional(SpecType),
  Skip(SpecType),
}

impl SpecField {
  fn ty(&self) -> SpecType {
    match self {
      SpecField::Named { ty, .. } => *ty,
      SpecField::Positional(ty)   => *ty,
      SpecField::Skip(ty)         => *ty,
    }
  }
  fn parse_named<'a>(
    fields: impl Iterator<Item = &'a Word>,
  ) -> ShResult<Vec<(Self, Option<Span>)>> {
    let mut result = vec![];
    let mut seen   = HashSet::default();
    let mut idx    = 0;
    let mut fields = fields.peekable();

    while let Some(field) = fields.next() {
      let (field, span) = field.clone().unpack();

      if let Some(ty) = Self::try_skip(&field) {
        let ty = ty.option_promote(span)?;

        if ty == SpecType::ToEnd && fields.peek().is_some() {
          return Err(sherr!(
              ParseErr ? span,
              "the '{field}' specifier can only be used as the last field in a spec",
          ));
        }

        result.push((SpecField::Skip(ty), span));
        continue;
      }

      let (name, Some(val)) = strops::split_assignment_raw(&field) else {
        return Err(sherr!(ParseErr ? span, "invalid field spec '{field}' (missing '=')"));
      };
      let val = ShStr::from(val);
      let name = if name.is_empty() {
        varstr!("{idx}")
      } else {
        ShStr::from(name)
      };

      if !seen.insert(name.clone()) {
        return Err(sherr!(ParseErr ? span, "duplicate field name '{name}' in spec"));
      }

      let ty = SpecType::parse_type(&val).option_promote(span)?;

      if ty == SpecType::ToEnd && fields.peek().is_some() {
        return Err(sherr!(
            ParseErr ? span,
            "the '{val}' specifier can only be used as the last field in a spec",
        ));
      }

      idx += 1;
      result.push((SpecField::Named { name, ty }, span));
    }

    Ok(result)
  }

  fn try_skip(field: &ShStr) -> Option<ShResult<SpecType>> {
    field
      .strip_prefix(b"+")
      .map(|rest| SpecType::parse_type(&rest.into()))
  }

  fn parse_positional<'a>(
    fields: impl Iterator<Item = &'a Word>,
  ) -> ShResult<Vec<(Self, Option<Span>)>> {
    let mut result = vec![];
    let mut seen   = HashSet::default();
    let mut idx    = 0;
    let mut fields = fields.peekable();

    while let Some(field) = fields.next() {
      if let Some(ty) = Self::try_skip(field) {
        let ty = ty.option_promote(field.span())?;

        if ty == SpecType::ToEnd && fields.peek().is_some() {
          return Err(sherr!(
              ParseErr ? field.span(),
              "the '{field}' specifier can only be used as the last field in a spec",
          ));
        }

        result.push((SpecField::Skip(ty), field.span()));
        continue;
      }

      // allows for named fields in a positional spec
      // ignores the name and uses the value
      let (name, val) = strops::split_assignment_raw(field);
      let val         = val.map(ShStr::from);

      let ty = match val {
        Some(ref v) => {
          let name = if name.is_empty() {
            varstr!("{idx}")
          } else {
            ShStr::from(name)
          };

          if !seen.insert(name.clone()) {
            return Err(sherr!(ParseErr ? field.span(), "duplicate field name '{name}' in spec"));
          }
          SpecType::parse_type(v).option_promote(field.span())?
        }
        None => SpecType::parse_type(field).option_promote(field.span())?,
      };

      if ty == SpecType::ToEnd && fields.peek().is_some() {
        return Err(sherr!(
          ParseErr ? field.span(),
          "the '{}' specifier can only be used as the last field in a spec",
          val.unwrap_or(field.content())
        ));
      }

      idx += 1;
      result.push((SpecField::Positional(ty), field.span()));
    }

    Ok(result)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecType {
  Str(SpecStr),
  Int(IntSpec),
  Float(FloatSpec),
  Raw(SpecLen),
  ToEnd,
}

impl SpecType {
  fn parse_type(ty: &ShStr) -> ShResult<Self> {
    if let Some((start, len)) = strops::split_at_unescaped(ty, b"/") {
      let len_type = ShStr::from(&ty[..start]);
      let kind     = ShStr::from(&ty[start + len..]);

      match kind.as_bytes() {
        b"str" => {
          if let Ok(len) = len_type.parse::<usize>() {
            return Ok(SpecType::Str(SpecStr::Len(SpecLen::Static(len))));
          }
          let int_ty = IntSpec::parse_int(&len_type)?;

          return Ok(SpecType::Str(SpecStr::Len(SpecLen::Dynamic(int_ty))));
        }
        b"raw" => {
          if let Ok(len) = len_type.parse::<usize>() {
            return Ok(SpecType::Raw(SpecLen::Static(len)));
          }
          let int_ty = IntSpec::parse_int(&len_type)?;

          return Ok(SpecType::Raw(SpecLen::Dynamic(int_ty)));
        }
        _ => return Err(sherr!(ParseErr, "invalid type '{ty}'")),
      }
    }

    match ty.as_bytes().first() {
      Some(b'u' | b'i') => Ok(SpecType::Int(IntSpec::parse_int(ty)?)),
      Some(b'f' | b'b') => Ok(SpecType::Float(FloatSpec::parse_float(ty.clone())?)),

      _ => match ty.as_bytes() {
        b"str" => Ok(SpecType::Str(SpecStr::NullTerm)),
        b"*"   => Ok(SpecType::ToEnd),
        _ => match ty.parse::<usize>() {
          Ok(raw) => Ok(SpecType::Raw(SpecLen::Static(raw))),
          Err(e)  => Err(sherr!(ParseErr, "invalid field type '{e}'")),
        },
      },
    }
  }

  fn read_n(count: usize, reader: &dyn Sink, buffer: &mut Vec<u8>) -> ShResult<Option<usize>> {
    if count == 0 {
      return Ok(Some(0));
    }

    Self::check_limit(count)?;

    if buffer.len() < count {
      buffer.resize(count, 0);
    }

    match reader.read_all(&mut buffer[..count]) {
      Ok(0)              => Ok(None),
      Ok(n) if n < count => Err(sherr!(ParseErr, "expected {count} bytes, got {n}")),
      Ok(got)            => Ok(Some(got)),
      Err(e)             => Err(e),
    }
  }

  fn check_limit(count: usize) -> ShResult<()> {
    let max = *shopt!(core.max_read_limit) as usize;
    if count > max {
      let size = strops::human_size(max as u64);
      return Err(sherr!(
        ParseErr,
        "field length {count} exceeds core.max_read_limit value ({size})",
      ));
    }
    Ok(())
  }

  /// Grow `buffer` so there is room past `got`, capped at `core.max_read_limit`.
  fn grow_for(buffer: &mut Vec<u8>, got: usize, what: &str) -> ShResult<()> {
    let max = *shopt!(core.max_read_limit) as usize;
    if got >= max {
      let size = strops::human_size(max as u64);
      return Err(sherr!(ParseErr, "{what} exceeds core.max_read_limit value ({size})",));
    }
    let grow = buffer.len().saturating_mul(2).max(8192).min(max);
    buffer.resize(grow, 0);
    Ok(())
  }

  /// Returns `(consumed, data_len)` -- these differ when a length prefix is read.
  fn read_len(
    len: SpecLen,
    reader: &dyn Sink,
    buffer: &mut Vec<u8>,
  ) -> ShResult<Option<(usize, usize)>> {
    match len {
      SpecLen::Static(n)     => Ok(Self::read_n(n, reader, buffer)?.map(|got| (got, got))),
      SpecLen::Dynamic(spec) => Self::read_dynamic(spec, reader, buffer),
    }
  }

  fn read_dynamic(
    dyn_spec: IntSpec,
    reader: &dyn Sink,
    buffer: &mut Vec<u8>,
  ) -> ShResult<Option<(usize, usize)>> {
    let width = dyn_spec.width_bytes();
    let Some(_) = Self::read_n(width, reader, buffer)? else {
      return Ok(None);
    };

    let len = dyn_spec.decode_bits(&buffer[..width]) as usize;
    Self::check_limit(len)?;

    // The prefix was consumed, so EOF here is a truncated record, not a clean end.
    let Some(got) = Self::read_n(len, reader, buffer)? else {
      return Err(sherr!(ParseErr, "expected {len} bytes after length prefix, got 0"));
    };

    Ok(Some((width + got, got)))
  }

  /// Returns `(consumed, data_len)`, where `data_len` excludes the terminator.
  fn read_null_term(reader: &dyn Sink, buffer: &mut Vec<u8>) -> ShResult<Option<(usize, usize)>> {
    let     can_seek = reader.seek(io::SeekFrom::Current(0)).is_ok();
    let mut got      = 0;

    loop {
      if got == buffer.len() {
        Self::grow_for(buffer, got, "unterminated string")?;
      }

      // A pipe cannot be rewound, so it is walked one byte at a time rather
      // than consuming past the terminator.
      let limit = if can_seek { buffer.len() } else { got + 1 };
      let n     = reader.read(&mut buffer[got..limit])?;

      if n == 0 {
        if got == 0 {
          return Ok(None);
        }
        return Err(sherr!(ParseErr, "unterminated string: expected a NUL terminator"));
      }

      if let Some(pos) = procio::rewind_past(reader, &buffer[got..got + n], b'\0')? {
        let data = got + pos;
        return Ok(Some((data + 1, data)));
      }

      got += n;
    }
  }

  fn write_field(self, data: ShStr) -> ShResult<ShStr> {
    match self {
      SpecType::Str(SpecStr::NullTerm) => Self::write_null_term(&data),
      SpecType::Int(spec)              => Self::write_int(spec, &data),
      SpecType::Float(spec)            => Self::write_float(spec, &data),

      SpecType::Raw(len) | SpecType::Str(SpecStr::Len(len)) => Self::write_sized(len, &data),
      SpecType::ToEnd => Ok(data),
    }
  }

  fn exact_width(self) -> Option<usize> {
    match self {
      SpecType::Raw(SpecLen::Static(length)) => Some(length),
      _ => None,
    }
  }

  fn write_pad(self) -> ShResult<ShStr> {
    let zeros = match self {
      SpecType::Str(SpecStr::NullTerm) => 1,
      SpecType::Int(spec)              => spec.width_bytes(),
      SpecType::Float(spec)            => spec.width(),

      SpecType::Raw(SpecLen::Static(length))
      | SpecType::Str(SpecStr::Len(SpecLen::Static(length))) => length,

      SpecType::Raw(SpecLen::Dynamic(spec))
      | SpecType::Str(SpecStr::Len(SpecLen::Dynamic(spec))) => spec.width_bytes(),

      SpecType::ToEnd => {
        return Err(sherr!(ParseErr, "cannot write a pad for a field of type '*' (to-end)"));
      }
    };

    Ok(ShStr::from(vec![0u8; zeros]))
  }

  fn write_float(spec: FloatSpec, data: &ShStr) -> ShResult<ShStr> {
    if spec.decimal() {
      spec.encode_decimal(data.as_bytes()).map(ShStr::from)
    } else {
      let sep    = spec.sep();
      let fields = data.split_str(&sep.to_string()).collect_vec();
      let [s, e, m] = fields[..] else {
        return Err(sherr!(ParseErr, "expected 3 fields for float, got {}", fields.len()));
      };
      spec.encode_triple(s, e, m).map(ShStr::from)
    }
  }

  fn write_int(spec: IntSpec, data: &ShStr) -> ShResult<ShStr> {
    let val = data
      .parse::<i128>()
      .map_err(|e| sherr!(ParseErr, "expected an integer, got '{e}'"))?;

    spec.check_range(val)?;

    Ok(spec.encode(val).into())
  }

  fn write_null_term(data: &ShStr) -> ShResult<ShStr> {
    if let Some(pos) = data.find_byte(b'\0') {
      return Err(sherr!(ParseErr, "internal NUL byte in string at position {pos}"));
    };

    let mut out = data.as_bytes().to_vec();
    out.push(b'\0');

    Ok(out.into())
  }

  fn write_sized(len: SpecLen, data: &ShStr) -> ShResult<ShStr> {
    let limit = *shopt!(core.max_read_limit) as usize;
    match len {
      SpecLen::Static(n) => {
        if n > limit {
          let size = strops::human_size(limit as u64);
          return Err(sherr!(
            ParseErr,
            "field length {n} exceeds core.max_read_limit value ({size})",
          ));
        }
        let mut out = vec![0u8; n];
        let     cap = data.len().min(n);
        out[..cap].copy_from_slice(&data[..cap]);

        Ok(out.into())
      }
      SpecLen::Dynamic(spec) => {
        let n     = data.len();
        let width = spec.width_bytes();
        let max   = 1u128 << (width * 8);
        if n as u128 >= max {
          return Err(sherr!(
            ParseErr,
            "value of {n} bytes does not fit a {width}-byte length prefix"
          ));
        }

        let mut out = spec.encode(n as i128);
        out.extend_from_slice(data.as_bytes());

        Ok(out.into())
      }
    }
  }

  fn read_field(self, reader: &dyn Sink, buffer: &mut Vec<u8>) -> ShResult<(usize, Option<ShStr>)> {
    match self {
      SpecType::Raw(len)    => Self::read_raw(len, reader, buffer),
      SpecType::Int(spec)   => Self::read_int(spec, reader, buffer),
      SpecType::Str(spec)   => Self::read_str(spec, reader, buffer),
      SpecType::Float(spec) => Self::read_float(spec, reader, buffer),
      SpecType::ToEnd       => Self::read_to_end(reader, buffer),
    }
  }

  fn read_to_end(reader: &dyn Sink, buffer: &mut Vec<u8>) -> ShResult<(usize, Option<ShStr>)> {
    let mut got = 0;

    loop {
      if got == buffer.len() {
        Self::grow_for(buffer, got, "input")?;
      }

      let space = buffer.len() - got;
      let n     = reader.read_all(&mut buffer[got..])?;
      got += n;

      if n < space {
        break;
      }
    }

    Ok((got, Some(buffer[..got].into())))
  }

  fn read_str(
    spec: SpecStr,
    reader: &dyn Sink,
    buffer: &mut Vec<u8>,
  ) -> ShResult<(usize, Option<ShStr>)> {
    // A fixed-width field is NUL-padded, so it stops at the first NUL.
    // A length prefix is authoritative, so an embedded NUL is data.
    let (consumed, data, padded) = match spec {
      SpecStr::NullTerm => {
        let Some((consumed, data)) = Self::read_null_term(reader, buffer)? else {
          return Ok((0, None));
        };
        (consumed, data, false)
      }
      SpecStr::Len(len) => {
        let Some((consumed, data)) = Self::read_len(len, reader, buffer)? else {
          return Ok((0, None));
        };
        (consumed, data, matches!(len, SpecLen::Static(_)))
      }
    };

    let end = if padded {
      buffer[..data].find_byte(b'\0').unwrap_or(data)
    } else {
      data
    };

    Ok((consumed, Some(buffer[..end].into())))
  }

  fn read_raw(
    len: SpecLen,
    reader: &dyn Sink,
    buffer: &mut Vec<u8>,
  ) -> ShResult<(usize, Option<ShStr>)> {
    let Some((consumed, data)) = Self::read_len(len, reader, buffer)? else {
      return Ok((0, None));
    };

    Ok((consumed, Some(buffer[..data].into())))
  }

  fn read_int(
    spec: IntSpec,
    reader: &dyn Sink,
    buffer: &mut Vec<u8>,
  ) -> ShResult<(usize, Option<ShStr>)> {
    let width = spec.width_bytes();
    let Some(got) = Self::read_n(width, reader, buffer)? else {
      return Ok((0, None));
    };

    Ok((got, Some(spec.decode(&buffer[..width]))))
  }

  fn read_float(
    spec: FloatSpec,
    reader: &dyn Sink,
    buffer: &mut Vec<u8>,
  ) -> ShResult<(usize, Option<ShStr>)> {
    let width = spec.width();
    let Some(got) = Self::read_n(width, reader, buffer)? else {
      return Ok((0, None));
    };

    Ok((got, Some(spec.decode(&buffer[..width])?)))
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecStr {
  NullTerm,
  Len(SpecLen),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpecLen {
  Dynamic(IntSpec),
  Static(usize),
}

pub(super) struct Pack;
impl super::Builtin for Pack {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("exact" | b'E'),
      opt!("array" | b'a', 1),
      opt!("assoc" | b'A', 1),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    match (args.opt_value("array"), args.opt_value("assoc")) {
      (Some(array), None) => Self::pack(&args, array, false),
      (None, Some(assoc)) => Self::pack(&args, assoc, true),

      (None, None) => Err(sherr!(
          ParseErr @ args.span(),
          "must specify either --array or --assoc"
      ))
      .with_code(2),

      (Some(_), Some(_)) => {
        let ar_span : Span  = args.opt_span("array").unwrap();
        let as_span : Span  = args.opt_span("assoc").unwrap();
        let ar_slice: ShStr = ar_span.slice();
        let as_slice: ShStr = as_span.slice();

        Err(sherr!(
            ParseErr @ ar_span,
            "cannot specify both {ar_slice} and {as_slice}"
        ))
        .with_code(2)
      }
    }
  }
}

impl Pack {
  fn pack(args: &BuiltinArgs, arr_name: ShStr, associative: bool) -> ShResult<()> {
    let mut array: VecDeque<(Option<ShStr>, ShStr)> = if associative {
      Shed::vars(|v| v.try_get_assoc_items(&arr_name))
        .option_promote(args.opt_span("assoc"))
        .promote_err(args.cmd_span())?
        .into_iter()
        .map(|(k, v)| (Some(k), v))
        .collect()
    } else {
      Shed::vars(|v| v.try_get_arr_elems(&arr_name))
        .option_promote(args.opt_span("array"))
        .promote_err(args.cmd_span())?
        .into_iter()
        .map(|v| (None, v))
        .collect()
    };

    let mut scratch = procio::take_scratch();
    let     exact   = args.has_opt("exact");
    let mut written = 0;

    let field_spec = if associative {
      SpecField::parse_named(args.arguments())?
    } else {
      SpecField::parse_positional(args.arguments())?
    };

    for (field, span) in field_spec {
      let data = match &field {
        SpecField::Named { name, ty, .. } => {
          let Some(pos) = array.iter().position(|(k, _)| k.as_ref() == Some(name)) else {
            return Err(sherr!(
                ParseErr ? span,
                "field name '{name}' not found in {arr_name}"
            ));
          };

          let (_, value) = array.remove(pos).unwrap();

          let len        = value.len();
          let data       = ty.write_field(value).option_promote(span)?;

          if exact
            && let Some(width) = ty.exact_width()
            && width != len
          {
            return Err(sherr!(
                ParseErr ? span,
                "field length {width} does not match value length ({len})",
            ));
          }
          data
        }
        SpecField::Positional(ty) => match array.pop_front() {
          Some((_, value)) => {
            let len  = value.len();
            let data = ty.write_field(value).option_promote(span)?;

            if exact
              && let Some(width) = ty.exact_width()
              && width != len
            {
              return Err(sherr!(
                  ParseErr ? span,
                  "field length {width} does not match value length ({len})",
              ));
            }
            data
          }
          None => {
            return Err(sherr!(
                ParseErr @ args.cmd_span(),
                "not enough fields in {arr_name} to pack"
            ));
          }
        },
        SpecField::Skip(ty) => ty.write_pad().option_promote(span)?,
      };

      let buf     = scratch.buf_mut();

      let new_len = written + data.len();
      if buf.len() < new_len {
        buf.resize(new_len, 0);
      }

      buf[written..new_len].copy_from_slice(&data);
      written = new_len;
    }

    procio::out_bytes(&scratch[..written]);

    util::with_status(0)

    /*
      while let Some((arg, span)) = arguments.next() {
        let (arg, pad) = match arg.strip_prefix(b"+") {
          Some(rest) => (rest.into(), true),
          None       => (arg.clone(), false),
        };

        let (name, arg): (Option<ShStr>, ShStr) = match strops::split_assignment_raw(&arg) {
          (rhs, Some(lhs)) => {
            let name: ShStr = rhs.into();
            if !seen.insert(name.clone()) {
              return Err(sherr!(ParseErr @ span, "duplicate field name '{name}' in pack"));
            }
            (associative.then_some(name), lhs.into())
          }
          (rhs, None) => {
            if associative && !pad {
              return Err(sherr!(
                  ParseErr @ span,
                  "associative pack requires field names (e.g. 'field=5')"
              ));
            } else {
              (None, rhs.into())
            }
          }
        };

        let count = arg
          .parse::<usize>()
          .map_err(|v| sherr!(ParseErr @ span, "invalid field length: '{v}'"))?;

        if count > limit {
          let size = strops::human_size(limit as u64);
          return Err(sherr!(
             ParseErr @ span,
             "field length {count} exceeds core.max_read_limit value ({size})",
          ));
        }

        let buf = scratch.buf_mut();
        if buf.len() < written + count {
          buf.resize(written + count, 0);
        }

        if pad {
          let write_end = written + count;
          buf[written..write_end].fill(0);

          written = write_end;
          continue;
        }

        let value = match name {
          Some(name) => {
            let Some(pos) = array.iter().position(|(k, _)| k.as_ref() == Some(&name)) else {
              return Err(sherr!(
                  ParseErr @ args.cmd_span(),
                  "field name '{name}' not found in {arr_name}"
              ));
            };

            let (_, value) = array.remove(pos).unwrap();
            value
          }
          None => match array.pop_front() {
            Some((_, value)) => value,
            None => {
              return Err(sherr!(
                  ParseErr @ args.cmd_span(),
                  "not enough fields in {arr_name} to pack"
              ));
            }
          },
        };

        if exact && value.len() != count {
          return Err(sherr!(
              ParseErr @ args.cmd_span(),
              "field length {count} does not match value length ({})",
              value.len()
          ));
        }

        // memcpy time
        let cap      : usize = value.len().min(count);
        let write_end: usize = written + cap;
        let pad_end  : usize = written + count;

        buf[written..write_end].copy_from_slice(&value[..cap]);
        buf[write_end..pad_end].fill(0);

        written += count;
      }
    */
  }
}

pub(super) struct Unpack;
impl super::Builtin for Unpack {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("append" | b'C'),
      opt!("array" | b'a', 1),
      opt!("assoc" | b'A', 1),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    match (args.opt_value("array"), args.opt_value("assoc")) {
      (Some(array), None) => Self::unpack(&args, array, false, args.has_opt("append")),
      (None, Some(assoc)) => Self::unpack(&args, assoc, true, args.has_opt("append")),

      (None, None) => Err(sherr!(
          ParseErr @ args.span(),
          "must specify either --array or --assoc"
      ))
      .with_code(2),

      (Some(_), Some(_)) => {
        let ar_span : Span  = args.opt_span("array").unwrap();
        let as_span : Span  = args.opt_span("assoc").unwrap();
        let ar_slice: ShStr = ar_span.slice();
        let as_slice: ShStr = as_span.slice();

        Err(sherr!(
            ParseErr @ ar_span,
            "cannot specify both {ar_slice} and {as_slice}"
        ))
        .with_code(2)
      }
    }
  }
}

enum Field {
  Named {
    name: ShStr,
    data: ShStr,
    span: Option<Span>,
  },
  Positional {
    data: ShStr,
  },
}

impl Field {
  fn into_named_data(self) -> Option<(ShStr, ShStr, Option<Span>)> {
    match self {
      Field::Named { name, data, span } => Some((name, data, span)),
      _ => None,
    }
  }
  fn into_data(self) -> ShStr {
    match self {
      Field::Named { data, .. }      => data,
      Field::Positional { data, .. } => data,
    }
  }
}

impl Unpack {
  fn unpack(args: &BuiltinArgs, arr_name: ShStr, associative: bool, append: bool) -> ShResult<()> {
    let     reader  : Arc<dyn Sink> = procio::stdin_sink()?;
    let mut scratch : ScratchGuard  = procio::take_scratch();
    let     buffer  : &mut Vec<u8>  = scratch.buf_mut();
    let mut consumed: usize         = 0;

    let field_spec = if associative {
      SpecField::parse_named(args.arguments())?
    } else {
      SpecField::parse_positional(args.arguments())?
    };
    let mut fields: Vec<Field> = vec![];

    for (field, span) in field_spec {
      let (got, data) = field.ty().read_field(&*reader, buffer)?;

      let Some(data) = data else {
        if consumed == 0 {
          return util::with_status(1);
        } else {
          return Err(sherr!(
              ParseErr ? span,
              "truncated record: expected more fields"
          ));
        }
      };

      consumed += got;

      match field {
        SpecField::Named { name, .. } => {
          fields.push(Field::Named { name, data, span });
        }
        SpecField::Positional(_) => {
          fields.push(Field::Positional { data });
        }
        SpecField::Skip(_) => std::mem::drop(data),
      }
    }

    Shed::vars_mut(|v| {
      if associative {
        let named = fields
          .into_iter()
          .filter_map(Field::into_named_data)
          .collect_vec();

        if append {
          let assoc = v.get_assoc_mut(&arr_name).promote_err(args.cmd_span())?;

          if let Some((name, _, span)) = named
            .iter()
            .find(|(name, _, _)| assoc.iter().any(|(key, _)| key == name))
          {
            return Err(sherr!(
                ParseErr ? *span,
                "field name '{name}' already exists in {arr_name}"
            ));
          }

          assoc.extend(named.into_iter().map(|(key, val, _)| (key, val)));
        } else {
          v.set_var(
            &arr_name,
            VarKind::assoc_arr(named.into_iter().map(|(key, val, _)| (key, val))),
            VarFlags::empty(),
          )?;
        }

        Ok(())
      } else {
        let data = fields.into_iter().map(Field::into_data).collect_vec();

        if append {
          let arr = v.get_arr_mut(&arr_name).promote_err(args.cmd_span())?;

          arr.extend(data);
        } else {
          v.set_var(&arr_name, VarKind::arr(data), VarFlags::empty())?;
        }

        Ok(())
      }
    })
    .promote_err(args.cmd_span())?;

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

  const ELF_SPEC: &str =
    "SPEC=(magic=4 class=1 data=1 ver=1 osabi=1 +8 type=2 machine=2 version=4 entry=8)";
  const ELF_HDR: &str = r"printf '\x7f\x45\x4c\x46\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x03\x00\x3e\x00\x01\x00\x00\x00\xc0\x47\x3a\x00\x00\x00\x00\x00'";

  #[test]
  fn slices_positional_fields_by_width() {
    assert_eq!(
      out_of(r"printf 'abcdefgh' | unpack -a f 2 3 3; printf '%s,' ${f[@]}"),
      "ab,cde,fgh,"
    );
  }

  #[test]
  fn a_plus_prefix_discards_bytes() {
    assert_eq!(
      out_of(r"printf 'abcdefgh' | unpack -a f 2 +3 3; printf '%s,' ${f[@]}"),
      "ab,fgh,"
    );
  }

  #[test]
  fn positional_field_names_are_decorative() {
    assert_eq!(
      out_of(r"printf 'abcdefgh' | unpack -a f x=2 y=3 z=3; printf '%s,' ${f[@]}"),
      "ab,cde,fgh,"
    );
  }

  #[test]
  fn assoc_fields_land_under_their_names() {
    assert_eq!(
      out_of(
        r"printf 'abcdefgh' | unpack -A h x=2 y=3 z=3; printf '%s|%s|%s' ${h[x]} ${h[y]} ${h[z]}"
      ),
      "ab|cde|fgh"
    );
  }

  #[test]
  fn a_skip_creates_no_key() {
    assert_eq!(
      out_of(
        r"printf 'abcdefgh' | unpack -A h x=2 +3 z=3; printf '%s|%s|%s' ${h[x]} ${h[z]} ${#h[@]}"
      ),
      "ab|fgh|2"
    );
  }

  #[test]
  fn a_field_is_assembled_from_several_reads() {
    assert_eq!(
      out_of(r"{ printf 'ab'; printf 'cd'; } | unpack -a f 4; printf '%s' ${f[0]}"),
      "abcd"
    );
  }

  #[test]
  fn records_can_be_read_in_a_loop_until_exhausted() {
    assert_eq!(
      out_of(
        r"printf 'aabbccdd' | { while unpack -a f 2 2; do printf '%s%s,' ${f[0]} ${f[1]}; done; }"
      ),
      "aabb,ccdd,"
    );
  }

  #[test]
  fn a_leading_skip_still_reports_a_truncated_record() {
    assert!(fails(r"printf '12345678' | unpack -a f +8 x=4"));
  }

  #[test]
  fn a_zero_width_field_lands_wherever_it_sits() {
    assert_eq!(
      out_of(r"printf 'abcd' | unpack -a f x=0 y=4; printf '%s;%s' ${#f[@]} ${f[1]}"),
      "2;abcd"
    );
    assert_eq!(
      out_of(r"printf 'abcd' | unpack -a f y=4 x=0; printf '%s;%s' ${#f[@]} ${f[0]}"),
      "2;abcd"
    );
  }

  #[test]
  fn a_truncated_field_leaves_an_array_alone() {
    assert_eq!(
      out_of(r"f=(orig); { printf 'ab' | unpack -a f 4; } 2>/dev/null; printf '%s' ${f[0]}"),
      "orig"
    );
  }

  #[test]
  fn a_truncated_field_leaves_an_assoc_alone() {
    assert_eq!(
      out_of(
        r"declare -A h; h[k]=orig; { printf 'ab' | unpack -A h x=4; } 2>/dev/null; printf '%s' ${h[k]}"
      ),
      "orig"
    );
  }

  #[test]
  fn assoc_unpack_demands_names() {
    assert!(fails(r"printf 'abcdefgh' | unpack -A h 2 3 3"));
  }

  #[test]
  fn unpack_rejects_duplicate_names() {
    assert!(fails(r"printf 'abcdefgh' | unpack -A h x=2 x=3"));
    assert!(fails(r"printf 'abcdefgh' | unpack -a f x=2 x=3"));
  }

  #[test]
  fn append_adds_a_new_name_to_the_assoc() {
    assert_eq!(
      out_of(
        r"declare -A h; h[a]=1; printf 'bc' | unpack -A h -C b=2; printf '%s;%s;%s' ${h[a]} ${h[b]} ${#h[@]}"
      ),
      "1;bc;2"
    );
  }

  #[test]
  fn append_rejects_a_name_already_in_the_assoc() {
    assert!(fails(
      r"declare -A h; h[k]=orig; printf 'ab' | unpack -A h -C k=2"
    ));
    assert_eq!(
      out_of(
        r"declare -A h; h[k]=orig; { printf 'ab' | unpack -A h -C k=2; } 2>/dev/null; printf '%s;%s' ${h[k]} ${#h[@]}"
      ),
      "orig;1"
    );
  }

  #[test]
  fn unpack_needs_exactly_one_mode() {
    assert!(fails(r"printf 'abcdefgh' | unpack -a f -A h 2"));
    assert!(fails(r"printf 'abcdefgh' | unpack 2 3"));
  }

  #[test]
  fn unpack_rejects_a_malformed_width() {
    assert!(fails(r"printf 'abcdefgh' | unpack -a f +"));
    assert!(fails(r"printf 'abcdefgh' | unpack -a f x="));
  }

  #[test]
  fn unpack_rejects_a_field_past_the_read_limit() {
    assert!(fails(r"printf 'ab' | unpack -a f 2000000000"));
  }

  #[test]
  fn short_input_is_a_failure() {
    assert!(fails(r"printf 'ab' | unpack -a f 4"));
  }

  #[test]
  fn concatenates_positional_fields() {
    assert_eq!(out_of(r"arr=(ab cde fgh); pack -a arr 2 3 3"), "abcdefgh");
  }

  #[test]
  fn pack_field_names_are_decorative() {
    assert_eq!(
      out_of(r"arr=(ab cde fgh); pack -a arr x=2 y=3 z=3"),
      "abcdefgh"
    );
  }

  #[test]
  fn the_spec_drives_assoc_output_order() {
    assert_eq!(
      out_of(r"declare -A h; h[a]=ab; h[b]=cd; pack -A h b=2 a=2"),
      "cdab"
    );
  }

  #[test]
  fn a_plus_prefix_emits_zeros() {
    assert_eq!(
      out_of(r"arr=(ab cd); pack -a arr 2 +4 2 >@v; printf '%s' ${#v}"),
      "8"
    );
  }

  #[test]
  fn a_short_value_is_padded_without_shifting_the_layout() {
    assert_eq!(
      out_of(r"arr=(ab x cd); pack -a arr 2 4 2 >@v; printf '%s' ${#v}"),
      "8"
    );
  }

  #[test]
  fn a_long_value_is_truncated_to_its_width() {
    assert_eq!(out_of(r"arr=(abcdef); pack -a arr 2"), "ab");
  }

  #[test]
  fn only_the_packed_prefix_is_emitted() {
    assert_eq!(
      out_of(r"arr=(ab); pack -a arr 2 >@v; printf '%s' ${#v}"),
      "2"
    );
  }

  #[test]
  fn a_field_larger_than_the_scratch_survives() {
    assert_eq!(
      out_of(
        r#"b=a; while [ ${#b} -lt 300000 ]; do b="$b$b"; done; a=( "${b:0:300000}" ); pack -E -a a 300000 >@out; printf '%s' ${#out}"#
      ),
      "300000"
    );
  }

  #[test]
  fn exact_accepts_a_matching_width() {
    assert_eq!(
      out_of(r"arr=(ab); pack -E -a arr 2 >@v; printf '%s' ${#v}"),
      "2"
    );
  }

  #[test]
  fn exact_rejects_a_width_mismatch() {
    assert!(fails(r"arr=(ab); pack -E -a arr 4"));
  }

  #[test]
  fn pack_rejects_duplicate_names() {
    assert!(fails(r"declare -A h; h[foo]=abcd; pack -A h foo=4 foo=4"));
    assert!(fails(r"arr=(ab cd); pack -a arr x=2 x=2"));
  }

  #[test]
  fn pack_rejects_an_unknown_name() {
    assert!(fails(r"declare -A h; h[a]=ab; pack -A h zz=2"));
  }

  #[test]
  fn pack_rejects_too_few_fields() {
    assert!(fails(r"arr=(ab); pack -a arr 2 2"));
  }

  #[test]
  fn assoc_pack_demands_names() {
    assert!(fails(r"declare -A h; h[a]=ab; pack -A h 2"));
  }

  #[test]
  fn pack_needs_exactly_one_mode() {
    assert!(fails(r"arr=(ab); pack -a arr -A h 2"));
    assert!(fails(r"arr=(ab); pack 2"));
  }

  #[test]
  fn pack_rejects_a_field_past_the_limit() {
    assert!(fails(r"arr=(ab); pack -a arr 2000000000"));
  }

  #[test]
  fn a_shared_spec_round_trips_positionally() {
    assert_eq!(
      out_of(
        r#"SPEC=(x=2 y=3); printf 'abcde' >@src; printf '%s' "$src" | unpack -a f ${SPEC[@]}; pack -E -a f ${SPEC[@]} >@back; [[ "$src" == "$back" ]] && printf same"#
      ),
      "same"
    );
  }

  #[test]
  fn a_shared_spec_round_trips_through_an_assoc() {
    assert_eq!(
      out_of(
        r#"SPEC=(x=2 y=3); printf 'abcde' >@src; printf '%s' "$src" | unpack -A h ${SPEC[@]}; pack -E -A h ${SPEC[@]} >@back; [[ "$src" == "$back" ]] && printf same"#
      ),
      "same"
    );
  }

  #[test]
  fn an_elf_header_survives_the_round_trip() {
    let cmd = format!(
      "{ELF_SPEC}; {ELF_HDR} >@src; printf '%s' \"$src\" | unpack -A h ${{SPEC[@]}}; \
       pack -E -A h ${{SPEC[@]}} >@back; printf '%s:%s' ${{#back}} \
       \"$( [[ \"$src\" == \"$back\" ]] && printf same || printf diff )\""
    );
    assert_eq!(out_of(&cmd), "32:same");
  }

  #[test]
  fn an_unpacked_header_field_decodes() {
    let cmd = format!(
      "{ELF_SPEC}; {ELF_HDR} | unpack -A h ${{SPEC[@]}}; printf '%s' ${{h[machine]}} | readint -T u16"
    );
    assert_eq!(out_of(&cmd), "62");
  }

  /// Round-trips `hex` through `unpack` and back through `pack` with the same
  /// spec. Yields "same" when the bytes survive intact.
  fn round_trips(hex: &str, spec: &str) -> String {
    out_of(&format!(
      r#"printf '{hex}' >@src; printf '%s' "$src" | unpack -a f {spec}; pack -a f {spec} >@back; [[ "$src" == "$back" ]] && printf same || printf diff"#
    ))
  }

  #[test]
  fn typed_integer_fields_round_trip() {
    assert_eq!(round_trips(r"\x78\x56\x34\x12", "x=u32"), "same");
    assert_eq!(round_trips(r"\xff\xff", "x=i16"), "same");
    assert_eq!(round_trips(r"\x2a", "x=u8"), "same");
    assert_eq!(round_trips(r"\x12\x34\x56\x78", "x=u32be"), "same");
  }

  #[test]
  fn typed_float_fields_round_trip() {
    assert_eq!(round_trips(r"\x00\x00\x80\x3f", "x=f32"), "same");
    assert_eq!(
      round_trips(r"\x00\x00\x00\x00\x00\x00\xf0\x3f", "x=f64"),
      "same"
    );
  }

  #[test]
  fn string_fields_round_trip() {
    assert_eq!(round_trips(r"abc\x00", "x=str"), "same");
    assert_eq!(round_trips(r"abc\x00\x00\x00\x00\x00", "x=8/str"), "same");
    assert_eq!(round_trips(r"\x03\x00\x00\x00abc", "x=u32/str"), "same");
    assert_eq!(round_trips(r"\x03abc", "x=u8/str"), "same");
  }

  #[test]
  fn raw_and_to_end_fields_round_trip() {
    assert_eq!(round_trips(r"\x01\x02\x03\x04", "x=4/raw"), "same");
    assert_eq!(round_trips(r"\x02\xaa\xbb", "x=u8/raw"), "same");
    assert_eq!(round_trips("hello world", "x=*"), "same");
  }

  #[test]
  fn a_mixed_typed_spec_round_trips() {
    assert_eq!(
      round_trips(
        r"\x7fELF\x02\x00\x00name\x00\x00\x00\x00\x10\x00\x00\x00",
        "magic=4 class=u8 +2 nm=8/str sz=u32"
      ),
      "same"
    );
  }

  #[test]
  fn a_typed_spec_round_trips_through_an_assoc() {
    let cmd = r"SPEC=(magic=4 ver=u8 nm=8/str sz=u32); printf '\x7fELF\x02name\x00\x00\x00\x00\x10\x00\x00\x00' >@src; printf '%s' $src | unpack -A h ${SPEC[@]}; pack -A h ${SPEC[@]} >@back; printf '%s:%s:%s' ${h[ver]} ${h[sz]} $( [[ $src == $back ]] && printf same || printf diff )";
    assert_eq!(out_of(cmd), "2:16:same");
  }

  #[test]
  fn typed_records_can_be_read_in_a_loop() {
    assert_eq!(
      out_of(
        r"printf '\x01\x00\x00\x00\x02\x00\x00\x00\x03\x00\x00\x00' | { while unpack -a r n=u32; do printf '%s,' ${r[0]}; done; }"
      ),
      "1,2,3,"
    );
  }

  #[test]
  fn an_endian_suffix_selects_byte_order() {
    assert_eq!(
      out_of(r"printf '\x01\x00\x00\x00' | unpack -a f x=u32le; printf '%s' ${f[0]}"),
      "1"
    );
    assert_eq!(
      out_of(r"printf '\x01\x00\x00\x00' | unpack -a f x=u32be; printf '%s' ${f[0]}"),
      "16777216"
    );
  }

  #[test]
  fn pack_accepts_integers_at_the_width_boundary() {
    assert_eq!(
      out_of(r"a=(255); pack -a a x=u8 >@v; printf '%s' ${#v}"),
      "1"
    );
    assert_eq!(out_of(r"a=(0); pack -a a x=u8 >@v; printf '%s' ${#v}"), "1");
    assert_eq!(
      out_of(r"a=(127); pack -a a x=i8 >@v; printf '%s' ${#v}"),
      "1"
    );
    assert_eq!(
      out_of(r"a=(-128); pack -a a x=i8 >@v; printf '%s' ${#v}"),
      "1"
    );
  }

  #[test]
  fn pack_rejects_an_out_of_range_integer() {
    assert!(fails(r"a=(256); pack -a a x=u8"));
    assert!(fails(r"a=(-1); pack -a a x=u8"));
    assert!(fails(r"a=(128); pack -a a x=i8"));
    assert!(fails(r"a=(-129); pack -a a x=i8"));
    assert!(fails(r"a=(70000); pack -a a x=u16"));
  }

  #[test]
  fn pack_rejects_a_non_numeric_integer_field() {
    assert!(fails(r"a=(abc); pack -a a x=u32"));
  }

  #[test]
  fn a_nul_byte_cannot_go_in_a_nul_terminated_field() {
    assert!(fails("a=($'x\\x00y'); pack -a a x=str"));
  }

  #[test]
  fn a_typed_skip_emits_its_own_width_in_zeros() {
    assert_eq!(
      out_of(r"a=(ab); pack -a a 2 +u32 >@v; printf '%s' ${#v}"),
      "6"
    );
    assert_eq!(
      out_of(r"a=(ab); pack -a a 2 +str >@v; printf '%s' ${#v}"),
      "3"
    );
  }

  #[test]
  fn a_to_end_field_cannot_be_skipped() {
    assert!(fails(r"a=(ab); pack -a a 2 +*"));
  }

  #[test]
  fn a_length_prefix_rejects_a_value_it_cannot_measure() {
    assert!(fails(
      r"b=a; while [ ${#b} -lt 300 ]; do b=$b$b; done; a=( ${b:0:300} ); pack -a a x=u8/str"
    ));
    assert_eq!(
      out_of(
        r"b=a; while [ ${#b} -lt 300 ]; do b=$b$b; done; a=( ${b:0:300} ); pack -a a x=u16/str >@v; printf '%s' ${#v}"
      ),
      "302"
    );
  }

  #[test]
  fn a_nested_structure_round_trips() {
    let cmd = r#"printf 'BLOB\x02\x0a\x00\x01abc\x00\x02xy\x00\x00' >@src; unpack -A outer magic=4 count=u8 payload=u16/raw <@src; { unpack -A r1 id=u8 nm=4/str; unpack -A r2 id=u8 nm=4/str; } <@outer[payload]; pack -A r1 id=u8 nm=4/str >@i1; pack -A r2 id=u8 nm=4/str >@i2; outer[payload]="$i1$i2"; pack -A outer magic=4 count=u8 payload=u16/raw >@back; [[ "$src" == "$back" ]] && printf '%s:%s:same' "${r1[nm]}" "${r2[nm]}" || printf diff"#;
    assert_eq!(out_of(cmd), "abc:xy:same");
  }

  #[test]
  fn unpack_rejects_a_malformed_type() {
    assert!(fails(r"printf 'abcd' | unpack -a f x=u32JUNK"));
    assert!(fails(r"printf 'abcd' | unpack -a f x=u32l"));
    assert!(fails(r"printf 'abcd' | unpack -a f x=u7"));
    assert!(fails(r"printf 'abcd' | unpack -a f x=zz"));
  }
}
