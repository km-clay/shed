//! `printf`-style formatting engine.
//!
//! Handles the `%[flags][width][.prec]` grammar, literal/escape handling, and field padding/justification.
//! A [`StrFmt`] implementor supplies the specifier *set*, which contains conversion letters and rendering logic.

use bitflags::bitflags;
use bstr::ByteSlice;

use crate::{
  match_loop, sherr,
  state::vars::VarStr,
  util::{self, error::ShResult, strops::ByteCursor},
};

use super::SliceCursor;

bitflags! {
  #[derive(Debug, Clone, Default, Copy, PartialEq, Eq)]
  pub(crate) struct FmtFlags: u8 {
    const LEFT  = 1 << 0;
    const ZERO  = 1 << 1;
    const PLUS  = 1 << 2;
    const SPACE = 1 << 3;
    const ALT   = 1 << 4;
  }
}

/// A width or precision
///
/// `Static`  -> literal count baked into the format string
/// `Dynamic` -> pulled from source at runtime
#[derive(Debug, Clone, Copy)]
pub(crate) enum Count {
  Static(usize),
  Dynamic,
}

/// The parsed `%[flags][width][.prec]` prefix, handed to `render`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FieldParams {
  flags: FmtFlags,
  width: Option<Count>,
  precision: Option<Count>,
}

impl FieldParams {
  pub(crate) fn flags(&self) -> FmtFlags {
    self.flags
  }

  pub(crate) fn width(&self) -> Option<&Count> {
    self.width.as_ref()
  }

  pub(crate) fn precision(&self) -> Option<&Count> {
    self.precision.as_ref()
  }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NumPrefix {
  Sign(Sign),
  Base(Base),
}

impl NumPrefix {
  pub(crate) fn new(sign: Option<Sign>, base: Option<Base>) -> Option<Self> {
    sign.map(Self::Sign).or_else(|| base.map(Self::Base))
  }
  pub(crate) fn marker(self) -> &'static [u8] {
    match self {
      NumPrefix::Sign(Sign::Plus) => b"+",
      NumPrefix::Sign(Sign::Minus) => b"-",
      NumPrefix::Sign(Sign::Space) => b" ",
      NumPrefix::Base(Base::Hex(Case::Upper)) => b"0X",
      NumPrefix::Base(Base::Hex(Case::Lower)) => b"0x",
      NumPrefix::Base(Base::Octal) => b"0",
    }
  }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Case {
  Upper,
  Lower,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Base {
  Hex(Case),
  Octal,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Sign {
  Plus,
  Minus,
  Space,
}

impl From<Sign> for u8 {
  fn from(value: Sign) -> Self {
    match value {
      Sign::Plus => b'+',
      Sign::Minus => b'-',
      Sign::Space => b' ',
    }
  }
}

pub(crate) enum FieldKind {
  Numeric {
    prefix: Option<NumPrefix>,
    zero_pad: bool,
  },
  String,
  Styled, // uses display width for padding, instead of char count
  Raw,    // no padding applied, used for literal segments
}

/// One rendered directive, before width padding
pub(crate) struct Field {
  body: Vec<u8>,
  kind: FieldKind,
}

impl Field {
  pub(crate) fn body(&self) -> &[u8] {
    &self.body
  }

  pub(crate) fn kind(&self) -> &FieldKind {
    &self.kind
  }
}

impl Field {
  pub(crate) fn string(body: Vec<u8>) -> Self {
    Self {
      body,
      kind: FieldKind::String,
    }
  }
  pub(crate) fn styled(body: Vec<u8>) -> Self {
    Self {
      body,
      kind: FieldKind::Styled,
    }
  }
  pub(crate) fn raw(body: Vec<u8>) -> Self {
    Self {
      body,
      kind: FieldKind::Raw,
    }
  }
  pub(crate) fn numeric(body: Vec<u8>, sign: Option<Sign>, base: Option<Base>) -> Self {
    Self::numeric_padded(body, sign, base, true)
  }
  pub(crate) fn numeric_padded(
    body: Vec<u8>,
    sign: Option<Sign>,
    base: Option<Base>,
    zero_pad: bool,
  ) -> Self {
    Self {
      body,
      kind: FieldKind::Numeric {
        prefix: NumPrefix::new(sign, base),
        zero_pad,
      },
    }
  }
}

/// A trait for a set of conversion specifiers and their rendering logic.
///
/// Based on the formatting logic of `printf` - implementors define a `Source` that provides
/// values, and a `Conv` that defines a set of conversion letters.
///
/// The engine handles parsing the format string and applying width/precision/padding.
pub(crate) trait StrFmt {
  /// Where values come from. Arg iterator for `printf`, `&FileInfo` for `stat -c`, etc.
  type Source;
  /// The parsed conversion token (letter(s) after the field parameters)
  type Conv;

  /// Consume the conversion letter(s) at `cur` into a `Conv`.
  ///
  /// Owns this step so multi-char specifiers work (`%Hd` for stat, `%(fmt)T` for `printf`).
  fn parse_conv(&self, cur: &mut SliceCursor) -> ShResult<Self::Conv>;

  /// Render one directive against the source.
  ///
  /// Applies precision itself; the engine applies width afterward.
  fn render(
    &self,
    conv: &Self::Conv,
    field: &FieldParams,
    src: &mut Self::Source,
  ) -> ShResult<Field>;

  /// A hook for operating on literal runs before pushing as a segment
  ///
  /// This is used by `printf` to expand ANSI-C escapes in the format string, for instance.
  /// Returns the literal untouched by default.
  fn expand_literal(&self, literal: Vec<u8>) -> Vec<u8> {
    literal
  }

  fn take_count(&self, _src: &mut Self::Source) -> ShResult<isize> {
    Err(sherr!(InternalErr, "dynamic width not supported here"))
  }
}

enum Segment<C> {
  Literal(Vec<u8>),
  Spec(FieldParams, C),
}

/// `printf`-style formatting engine.
///
/// Made generic so that many builtins can re-use the same internal formatting logic
/// Requires an implementor of [`StrFmt`] and a format string.
/// [`Formatter::render()`] requires a source of values to format, which is also defined by the [`StrFmt`] implementor.
pub(crate) struct Formatter<'s, S: StrFmt> {
  set: &'s S,
  segments: Box<[Segment<S::Conv>]>,
}

impl<'s, S: StrFmt> Formatter<'s, S> {
  pub(crate) fn parse(set: &'s S, fmt: &[u8]) -> ShResult<Self> {
    let mut cur = SliceCursor::new(fmt);
    let mut segments: Vec<Segment<S::Conv>> = Vec::new();
    let mut literal = vec![];

    let push_lit = |l: &mut Vec<u8>, s: &mut Vec<Segment<S::Conv>>| {
      if !l.is_empty() {
        let lit = set.expand_literal(std::mem::take(l));
        s.push(Segment::Literal(lit));
      }
    };

    match_loop!(cur.next_byte() => byte, {
      b'%' if cur.peek_byte().is_none_or(|b| b == b'%') => {
        literal.push(byte);
        cur.bump_if_eq(b'%');
      }
      b'%' => {
        push_lit(&mut literal, &mut segments);
        let fields = Self::parse_fields(&mut cur)?;
        let conv = set.parse_conv(&mut cur)?;
        segments.push(Segment::Spec(fields, conv));
      }
      _ => literal.push(byte),
    });

    push_lit(&mut literal, &mut segments);

    Ok(Self {
      set,
      segments: segments.into_boxed_slice(),
    })
  }

  fn parse_fields(cur: &mut SliceCursor) -> ShResult<FieldParams> {
    Ok(FieldParams {
      flags: Self::parse_flags(cur)?,
      width: Self::parse_width(cur)?,
      precision: Self::parse_prec(cur)?,
    })
  }
  fn parse_prec(cur: &mut SliceCursor) -> ShResult<Option<Count>> {
    if !cur.bump_if_eq(b'.') {
      return Ok(None);
    }
    Ok(Some(Self::parse_width(cur)?.unwrap_or(Count::Static(0))))
  }
  fn parse_width(cur: &mut SliceCursor) -> ShResult<Option<Count>> {
    match cur.peek_byte() {
      Some(b'*') => {
        cur.bump();
        Ok(Some(Count::Dynamic))
      }
      Some(b'0'..=b'9') => {
        let width = Self::parse_uint(cur)?;
        Ok(Some(Count::Static(width)))
      }
      _ => Ok(None),
    }
  }

  fn parse_uint(cur: &mut SliceCursor) -> ShResult<usize> {
    let mut digits = util::scratch_buf();
    while let Some(b) = cur.next_byte_if(|b| b.is_ascii_digit()) {
      digits.push(b);
    }

    let width = VarStr::from(digits)
      .parse::<usize>()
      .ok_or_else(|| sherr!(ParseErr, "invalid width"))?;

    Ok(width)
  }

  fn parse_flags(cur: &mut SliceCursor) -> ShResult<FmtFlags> {
    let mut flags = FmtFlags::empty();

    match_loop!(cur.peek_byte() => b, {
      b'-' => { flags |= FmtFlags::LEFT; cur.bump();  }
      b'+' => { flags |= FmtFlags::PLUS; cur.bump();  }
      b' ' => { flags |= FmtFlags::SPACE; cur.bump(); }
      b'#' => { flags |= FmtFlags::ALT; cur.bump();   }
      b'0' => { flags |= FmtFlags::ZERO; cur.bump();  }
      _ => break
    });

    Ok(flags)
  }

  pub(crate) fn has_specs(&self) -> bool {
    self
      .segments
      .iter()
      .any(|s| matches!(s, Segment::Spec(_, _)))
  }

  /// Render the format string against a source of values, producing a byte vector.
  pub(crate) fn render(&self, src: &mut S::Source, out: &mut Vec<u8>) -> ShResult<()> {
    for seg in &self.segments {
      match seg {
        Segment::Literal(b) => out.extend_from_slice(b),
        Segment::Spec(field, conv) => {
          let field = self.resolve_counts(field, src)?;
          let rendered = self.set.render(conv, &field, src)?;
          pad_field(&rendered, &field, out);
        }
      }
    }

    Ok(())
  }

  fn resolve_counts(&self, field: &FieldParams, src: &mut S::Source) -> ShResult<FieldParams> {
    const MAX_FIELD: usize = u16::MAX as usize;

    let mut flags = field.flags();

    let width = match field.width() {
      Some(Count::Static(w)) => Some(Count::Static((*w).min(MAX_FIELD))),
      Some(Count::Dynamic) => {
        let n = self.set.take_count(src)?;
        if n < 0 {
          flags |= FmtFlags::LEFT;
        }
        Some(Count::Static(n.unsigned_abs().min(MAX_FIELD)))
      }
      None => None,
    };

    let prec = match field.precision() {
      Some(Count::Static(p)) => Some(Count::Static((*p).min(MAX_FIELD))),
      Some(Count::Dynamic) => {
        let n = self.set.take_count(src)?;
        (n >= 0).then_some(Count::Static((n as usize).min(MAX_FIELD)))
      }
      None => None,
    };

    Ok(FieldParams {
      flags,
      width,
      precision: prec,
    })
  }
}

/// Pad a rendered [`Field`] to `params`' width and append it to `out`.
fn pad_field(field: &Field, params: &FieldParams, out: &mut Vec<u8>) {
  let body = field.body();
  if let FieldKind::Raw = field.kind() {
    out.extend_from_slice(body);
    return;
  }
  let (sign, zero_ok): (Option<&[u8]>, bool) = match field.kind() {
    FieldKind::String | FieldKind::Styled | FieldKind::Raw => (None, false),
    FieldKind::Numeric { prefix, zero_pad } => (prefix.map(NumPrefix::marker), *zero_pad),
  };

  let Some(Count::Static(width)) = params.width().copied() else {
    if let Some(sign) = sign {
      out.extend_from_slice(sign);
    }
    out.extend_from_slice(body);
    return;
  };

  let measured = match field.kind() {
    FieldKind::Styled => util::ui::calc_str_width(&body.to_str_lossy()),
    _ => body.chars().count(),
  };
  let total = sign.map_or(0, <[u8]>::len) + measured;
  if total >= width {
    if let Some(sign) = sign {
      out.extend_from_slice(sign);
    }
    out.extend_from_slice(body);
    return;
  }
  let pad = width - total;
  let flags = params.flags();

  if flags.contains(FmtFlags::LEFT) {
    if let Some(sign) = sign {
      out.extend_from_slice(sign);
    }
    out.extend_from_slice(body);
    out.resize(out.len() + pad, b' ');
  } else if zero_ok && flags.contains(FmtFlags::ZERO) {
    if let Some(sign) = sign {
      out.extend_from_slice(sign);
    }
    out.resize(out.len() + pad, b'0');
    out.extend_from_slice(body);
  } else {
    out.resize(out.len() + pad, b' ');
    if let Some(sign) = sign {
      out.extend_from_slice(sign);
    }
    out.extend_from_slice(body);
  }
}
