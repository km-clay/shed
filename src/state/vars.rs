//! Shell variables and their values
//!
//! Data structures/functions used for storing and interacting with shell variables:
//!
//! * [`Var`]: the main variable type, which contains a [`VarKind`] and [`VarFlags`],
//! * [`VarKind`]: the type of the variable (string, integer, array, associative array, magic, or unset), and its value
//! * [`VarFlags`]: the attributes of the variable (exported, local, readonly, integer)

use std::{
  borrow::{Borrow, Cow},
  collections::VecDeque,
  ffi::{CString, OsStr, OsString},
  fmt::{self, Debug, Display},
  ops::{Deref, RangeBounds},
  os::unix::ffi::{OsStrExt, OsStringExt},
  path::{Path, PathBuf},
  rc::Rc,
  str::FromStr,
  sync::Arc,
  time::{Duration, Instant},
};

use bitflags::bitflags;
use bstr::ByteSlice;
use hipstr::HipByt;
use itertools::Itertools;
use nix::{
  NixPath,
  errno::Errno,
  sys::stat,
  unistd::{self, Pid, Uid, User},
};
use rusqlite::{ToSql, types::FromSql};
use smallvec::SmallVec;
use smol_str::{SmolStr, SmolStrBuilder};

use crate::{
  HashMap,
  eval::{
    lex::{LexFlags, LexStream, Tk, TkFlags, TkRule},
    parse::ast::Ast,
  },
  expand::{
    Expander, Word, arithmetic, escape,
    stream::{Marker, Quote, SegStream, Unit},
    var,
  },
  match_loop, procio,
  readline::Candidate,
  sherr,
  state::{self, Shed, params, paths},
  util::{
    self,
    error::{LabelBuilder, ShResult},
    random,
    strops::{ByteCursor, FromBytes, ParseBytes, QuoteState, SliceCursor},
  },
  varstr,
};

use super::{meta::MetaTab, scopes::ScopeStack, terminal::Terminal};

const OPERATORS: &[u8] = b"!#%-+^,=/?";

/// A variable/alias value that can be rendered as raw bytes for reuse as a
/// shell assignment. Unlike `Display`, this preserves arbitrary non-UTF-8
/// bytes instead of laundering them into `U+FFFD`.
pub(crate) trait ValueBytes {
  fn value_bytes(&self) -> Vec<u8>;
}

impl<T: ValueBytes + ?Sized> ValueBytes for &T {
  fn value_bytes(&self) -> Vec<u8> {
    (**self).value_bytes()
  }
}

impl ValueBytes for ShStr {
  fn value_bytes(&self) -> Vec<u8> {
    self.as_bytes().to_vec()
  }
}

impl ValueBytes for Var {
  fn value_bytes(&self) -> Vec<u8> {
    self.kind.value_bytes()
  }
}

impl ValueBytes for VarKind {
  fn value_bytes(&self) -> Vec<u8> {
    match self {
      VarKind::Str(s) => s.as_bytes().to_vec(),
      VarKind::Int(i) => i.to_string().into_bytes(),
      VarKind::Arr(items) => {
        let mut out       = Vec::new();
        let mut item_iter = items.iter().peekable();
        while let Some(item) = item_iter.next() {
          out.extend_from_slice(item.as_bytes());
          if item_iter.peek().is_some() {
            out.push(b' ');
          }
        }
        out
      }
      VarKind::AssocArr(items) => {
        let mut out       = Vec::new();
        let mut item_iter = items.iter().peekable();
        while let Some((k, v)) = item_iter.next() {
          out.extend_from_slice(&escape::shell_quote(k));
          out.push(b'=');
          out.extend_from_slice(&escape::shell_quote(v));

          if item_iter.peek().is_some() {
            out.push(b' ');
          }
        }
        out
      }
      VarKind::Magic(func) => func().unwrap_or_default().as_bytes().to_vec(),
      VarKind::Unset       => Vec::new(),
    }
  }
}

/// Join lines with `\n` (no trailing newline), byte-native.
fn join_lines(mut lines: Vec<Vec<u8>>) -> Vec<u8> {
  lines.sort();
  let mut out  = Vec::new();
  let mut iter = lines.into_iter().peekable();
  while let Some(line) = iter.next() {
    out.extend_from_slice(&line);
    if iter.peek().is_some() {
      out.push(b'\n');
    }
  }
  out
}

pub(crate) fn serialize_var(name: &[u8], var: &Var) -> Option<ShStr> {
  let Var { flags, kind } = var;

  if let VarKind::Magic(_) = kind {
    return None;
  }

  let mut letters: Vec<u8> = Vec::new();
  match kind {
    VarKind::Arr(_)      => letters.push(b'a'),
    VarKind::AssocArr(_) => letters.push(b'A'),
    _ => {}
  }
  if flags.contains(VarFlags::INTEGER) {
    letters.push(b'i');
  }
  if flags.contains(VarFlags::READONLY) {
    letters.push(b'r');
  }
  if flags.contains(VarFlags::EXPORT) {
    letters.push(b'x');
  }

  let mut out: Vec<u8> = b"declare ".to_vec();
  if letters.is_empty() {
    out.extend_from_slice(b"--");
  } else {
    out.push(b'-');
    out.extend_from_slice(&letters);
  }
  out.push(b' ');
  out.extend_from_slice(name.as_bytes());

  match kind {
    VarKind::Unset => {}
    VarKind::Arr(items) => {
      out.extend_from_slice(b"=(");
      let mut it = items.iter().peekable();

      while let Some(item) = it.next() {
        out.extend_from_slice(&escape::shell_quote(item));
        if it.peek().is_some() {
          out.push(b' ');
        }
      }

      out.push(b')');
    }
    VarKind::AssocArr(items) => {
      out.extend_from_slice(b"=(");
      let mut it = items.iter().peekable();

      while let Some((k, v)) = it.next() {
        out.push(b'[');
        out.extend_from_slice(&escape::shell_quote(k));
        out.extend_from_slice(b"]=");
        out.extend_from_slice(&escape::shell_quote(v));
        if it.peek().is_some() {
          out.push(b' ');
        }
      }

      out.push(b')');
    }
    VarKind::Str(_) | VarKind::Int(_) => {
      out.push(b'=');
      out.extend_from_slice(&escape::shell_quote(&kind.value_bytes()));
    }
    VarKind::Magic(_) => unreachable!(),
  }

  Some(out.into())
}

pub(crate) fn serialize_vars(vars: &ScopeStack) -> Vec<u8> {
  let lines = vars
    .flatten_vars()
    .into_iter()
    .filter_map(|(k, v)| serialize_var(&k, &v))
    .map(|line| line.as_bytes().to_vec())
    .collect();
  join_lines(lines)
}

/// Display key/value pairs as '{key}={value}', one per line, sorted.
///
/// The 'value' is escaped in such a way that the whole line can be reused as a shell assignment
pub(crate) fn display_as_vars(
  vars: impl Iterator<Item = (impl AsRef<[u8]>, impl ValueBytes)>,
) -> Vec<u8> {
  join_lines(vars.map(|(k, v)| display_as_var(k, v)).collect())
}

pub(crate) fn display_as_var(name: impl AsRef<[u8]>, value: impl ValueBytes) -> Vec<u8> {
  let     name  = name.as_ref();
  let     value = escape::shell_quote(&value.value_bytes());
  let mut out   = Vec::with_capacity(name.len() + value.len() + 1);
  out.extend_from_slice(name);
  out.push(b'=');
  out.extend_from_slice(&value);
  out
}

fn display_vars_internal(vars: &ScopeStack, filter: Option<VarFlags>, polarity: bool) -> Vec<u8> {
  let vars = vars.flatten_vars().into_iter();

  if let Some(flags) = filter {
    display_as_vars(vars.filter(|(_, v)| v.flags().contains(flags) && polarity))
  } else {
    display_as_vars(vars)
  }
}

/// The readonly variables (`readonly` / `readonly -p`), each as a reusable
/// `readonly NAME=value` line.
pub(crate) fn display_readonly(vars: &ScopeStack) -> Vec<u8> {
  let lines = vars
    .flatten_vars()
    .into_iter()
    .filter(|(_, v)| v.flags().contains(VarFlags::READONLY))
    .map(|(k, v)| {
      let mut line = b"readonly ".to_vec();
      line.extend_from_slice(&display_as_var(k, v));
      line
    })
    .collect();
  join_lines(lines)
}

/// The exported variables (`export` / `export -p`), each as a reusable
/// `export NAME=value` line. Reads the variable table (filtered by the export
/// attribute) rather than the live process environment, so variables exported
/// during this session are included, not just inherited ones.
pub(crate) fn display_exported(vars: &ScopeStack) -> Vec<u8> {
  let lines = vars
    .flatten_vars()
    .into_iter()
    .filter(|(_, v)| v.flags().contains(VarFlags::EXPORT))
    .map(|(k, v)| {
      let mut line = b"export ".to_vec();
      line.extend_from_slice(&display_as_var(k, v));
      line
    })
    .collect();
  join_lines(lines)
}

pub(crate) fn display_local(vars: &ScopeStack) -> Vec<u8> {
  display_vars_internal(vars, None, true)
}

pub(crate) fn display_global(vars: &ScopeStack) -> Vec<u8> {
  display_vars_internal(vars, Some(VarFlags::LOCAL), false)
}

#[derive(Hash, Eq, PartialEq, Debug, Clone, Copy)]
pub(crate) enum ShellParam {
  // Global
  ShPid,
  LastJob,
  ShellName,

  // Local
  Pos(usize),
  AllArgs,
  AllArgsStr,
  ArgCount,
}

impl ShellParam {
  pub(crate) fn is_param(value: &[u8]) -> bool {
    let Some(first) = value.first() else {
      return false;
    };
    matches!(first, b'$' | b'!' | b'0' | b'@' | b'*' | b'#') || first.is_ascii_digit()
  }

  pub(crate) fn is_global(&self) -> bool {
    matches!(self, Self::ShPid | Self::LastJob | Self::ShellName)
  }

  pub(crate) fn from_char(c: char) -> Option<Self> {
    match c {
      '$' => Some(Self::ShPid),
      '!' => Some(Self::LastJob),
      '0' => Some(Self::ShellName),
      '@' => Some(Self::AllArgs),
      '*' => Some(Self::AllArgsStr),
      '#' => Some(Self::ArgCount),
      _   => None,
    }
  }
}

impl Display for ShellParam {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::ShPid      => write!(f, "$"),
      Self::LastJob    => write!(f, "!"),
      Self::ShellName  => write!(f, "0"),
      Self::Pos(n)     => write!(f, "{n}"),
      Self::AllArgs    => write!(f, "@"),
      Self::AllArgsStr => write!(f, "*"),
      Self::ArgCount   => write!(f, "#"),
    }
  }
}

impl FromBytes for ShellParam {
  type Error = ();

  fn from_bytes<B: AsRef<[u8]>>(bytes: B) -> Result<Self, Self::Error>
  where
    Self: Sized,
  {
    let bytes = bytes.as_ref();
    match bytes {
      b"$" => Ok(Self::ShPid),
      b"!" => Ok(Self::LastJob),
      b"0" => Ok(Self::ShellName),
      b"@" => Ok(Self::AllArgs),
      b"*" => Ok(Self::AllArgsStr),
      b"#" => Ok(Self::ArgCount),
      _    => ShStr::from(bytes).parse_bytes::<usize>().map(Self::Pos),
    }
  }
}

bitflags! {
  #[derive(Clone, Default, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
  pub struct VarFlags: u32 {
    const EXPORT = 1 << 0;
    const LOCAL = 1 << 1;
    const READONLY = 1 << 2;
    const INTEGER = 1 << 3;
    const SHELL = 1 << 4;
  }
}

pub(crate) const SHELL_MANAGED_VARS: &[&[u8]] = &[
  b"PWD", b"OLDPWD", b"SHLVL", b"PPID", b"LINENO", b"OPTIND", b"UMASK", b"_",
];

#[derive(Clone, Debug)]
pub(crate) enum ArrIndex {
  Literal(usize),
  FromBack(usize),
  ArgCount,
  AllJoined,
  AllSplit,
  Key(ShStr),

  /// Unresolved index, parsed depending on whether we are targeting an
  /// indexed array or an associative array
  Raw(ShStr),
}

impl ArrIndex {
  /// Parse an array index expression.
  ///
  /// the `allow_side_effects` parameter controls whether or not mutating parameter
  /// expansions and command substitutions will be evaluated.
  pub(crate) fn parse(s: &str, allow_side_effects: bool) -> ShResult<Self> {
    let input    = SegStream::from(s);
    let expanded = var::expand_raw_inner(None, &mut input.cursor(), allow_side_effects, false)?;
    let s        = ShStr::from(expanded.as_bytes());

    match s.as_bytes() {
      b"@" => Ok(Self::AllSplit),
      b"*" => Ok(Self::AllJoined),
      b"#" => Ok(Self::ArgCount),
      _ if s.starts_with(b"-")
        && !s[1..].is_empty()
        && s[1..].chars().all(|c| c.is_ascii_digit()) =>
      {
        let idx = ShStr::from(&s[1..]);
        let idx = idx.parse_bytes::<usize>().unwrap();
        Ok(Self::FromBack(idx))
      }
      _ if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => {
        let idx = s.parse_bytes::<usize>().unwrap();
        Ok(Self::Literal(idx))
      }
      // Anything else — variable references, arithmetic expressions,
      // string keys — gets deferred to `resolve_for`. Whether it's
      // arithmetic-evaluated or used as a literal key depends on
      // whether the target is indexed or associative.
      _ => Ok(Self::Raw(s)),
    }
  }
}

impl ArrIndex {
  pub(crate) fn resolve_for(self, tag: VarKindTag) -> ShResult<Self> {
    match self {
      Self::Raw(s) => match tag {
        VarKindTag::Unset => {
          Err(sherr!(ParseErr, "Cannot resolve array index '{s}': variable is unset"))
        }
        VarKindTag::AssocArr => Ok(Self::Key(s)),
        VarKindTag::Arr | VarKindTag::Str | VarKindTag::Int | VarKindTag::Magic => {
          let evaluated = arithmetic::expand_arithmetic(None, s.as_bytes())?;
          let n: usize = evaluated
            .to_str_lossy()
            .parse()
            .map_err(|_| sherr!(ParseErr, "Invalid array index '{s}': not a number"))?;
          Ok(Self::Literal(n))
        }
      },
      Self::Literal(n) if matches!(tag, VarKindTag::AssocArr) => Ok(Self::Key(varstr!("{n}"))),
      _ => Ok(self),
    }
  }
}

/// A parsed variable name, optionally with an array index and slice.
/// Index expansion happens at construction time, so it's safe
/// to use inside `Shed::vars`/`write_vars` closures without
/// causing re-entrant borrows.
#[derive(Clone, Debug)]
pub(crate) struct VarName {
  name       : ShStr,
  index      : Option<ArrIndex>,
  slice_start: Option<usize>,
  slice_len  : Option<usize>,
}

impl VarName {
  fn plain(name: ShStr) -> Self {
    Self {
      name,
      index: None,
      slice_start: None,
      slice_len: None,
    }
  }
  fn slice_component(part: &SegStream) -> Option<usize> {
    var::expand_raw(None, &mut part.cursor())
      .map_or_else(|_| part.content(), |sg| sg.content())
      .parse_bytes::<usize>()
      .ok()
  }
  pub(crate) fn parse_stream(
    stream: &SegStream,
    allow_side_effects: bool,
  ) -> ShResult<(Self, usize)> {
    let mut cursor    = stream.cursor();
    let mut qt_state  = QuoteState::default();
    let mut name      = SegStream::default();
    let mut index     = SegStream::default();
    let mut depth     = 0;
    let mut saw_quote = false;
    let mut closed    = false;

    let push_to = |depth: usize, name: &mut SegStream, index: &mut SegStream, u: Unit| {
      if depth == 0 {
        name.push(u);
      } else {
        index.push(u);
      }
    };

    // handling "${?}, ${!}, ${-}" as special cases, since these are shell paramaters
    // also accounts for indirect expansion with "${!var}"
    if let Some(b @ (b'?' | b'!' | b'-')) = cursor.peek_byte() {
      let next_is_name = cursor
        .peek_nth(1)
        .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_');

      if next_is_name {
        return Ok((Self::plain(ShStr::default()), 0));
      }

      cursor.bump();
      return Ok((Self::plain(varstr!("{}", b as char)), 1));
    }

    while let Some(unit) = cursor.peek() {
      match unit {
        Unit::Mark(Marker::Escape) => {
          cursor.bump();
          push_to(depth, &mut name, &mut index, unit);
          if let Some(next) = cursor.next() {
            push_to(depth, &mut name, &mut index, next);
          }
          continue;
        }

        Unit::Mark(Marker::Quote(Quote::Single)) | Unit::Byte(b'\'') => {
          if qt_state.toggle_single()
            && let Unit::Byte(_) = unit
          {
            saw_quote = true;
          }
          push_to(depth, &mut name, &mut index, unit);
        }
        Unit::Mark(Marker::Quote(Quote::Double)) | Unit::Byte(b'"') => {
          if qt_state.toggle_double()
            && let Unit::Byte(_) = unit
          {
            saw_quote = true;
          }
          push_to(depth, &mut name, &mut index, unit);
        }

        Unit::Byte(b'[') if qt_state.outside() => {
          depth += 1;
          if depth > 1 {
            index.push(unit);
          }
        }
        Unit::Byte(b']') if qt_state.outside() => {
          depth = depth.saturating_sub(1);
          if depth == 0 {
            closed = true;
            cursor.bump();
            break;
          }

          index.push(unit);
        }

        Unit::Byte(b)
          if depth == 0
            && qt_state.outside()
            && !name.is_empty()
            && (b == b':' || OPERATORS.contains(&b)) =>
        {
          break;
        }

        _ => push_to(depth, &mut name, &mut index, unit),
      }

      cursor.bump();
    }

    if !closed {
      return Ok((Self::plain(name.content()), cursor.bpos()));
    }

    // expand array indexes
    // ones with quotes need special handling
    let index = if saw_quote {
      params::expand_arr_index(index.as_bytes(), allow_side_effects)?
    } else {
      let mut idx_cursor = index.cursor();
      let     expanded   = var::expand_raw_inner(None, &mut idx_cursor, allow_side_effects, false)?;
      ArrIndex::parse(&expanded.content().to_str_lossy(), allow_side_effects)?
    };

    let mut slice_start      = None;
    let mut slice_len        = None;
    let     next_is_colon    = cursor.peek_byte() == Some(b':');
    let     is_all_index     = matches!(index, ArrIndex::AllSplit | ArrIndex::AllJoined);
    let     operator_follows = matches!(cursor.peek_nth(1), Some(b) if OPERATORS.contains(&b));
    let     slice_follows    = next_is_colon && !operator_follows;
    let     read_slices      = is_all_index && slice_follows;

    if read_slices {
      cursor.bump();
      qt_state.reset();

      let mut skip_part   = SegStream::default();
      let mut take_part   = SegStream::default();
      let mut seen_colon  = false;
      let mut brace_depth = 0usize;
      let mut paren_depth = 0usize;

      while let Some(unit) = cursor.peek() {
        match unit {
          Unit::Mark(Marker::Quote(q)) => {
            qt_state.toggle_from(q);
          }
          Unit::Byte(b @ (b'\'' | b'"')) => {
            qt_state.toggle_from(b);
          }

          _ if qt_state.in_quote() => {}

          Unit::Byte(b'{') => brace_depth += 1,
          Unit::Byte(b'}') => brace_depth = brace_depth.saturating_sub(1),
          Unit::Byte(b'(') => paren_depth += 1,
          Unit::Byte(b')') => paren_depth = paren_depth.saturating_sub(1),

          Unit::Byte(b':') if brace_depth == 0 && paren_depth == 0 => {
            if seen_colon {
              break;
            }
            seen_colon = true;
            cursor.bump();
            continue;
          }

          Unit::Byte(b) if brace_depth == 0 && paren_depth == 0 && OPERATORS.contains(&b) => {
            break;
          }

          _ => {}
        }

        if seen_colon {
          take_part.push(unit);
        } else {
          skip_part.push(unit);
        }

        cursor.bump();
      }

      slice_start = Self::slice_component(&skip_part);
      if seen_colon {
        slice_len = Self::slice_component(&take_part);
      }
    }

    Ok((
      Self {
        name: name.content(),
        index: Some(index),
        slice_start,
        slice_len,
      },
      cursor.bpos(),
    ))
  }
  pub(crate) fn parse(raw: &str, allow_side_effects: bool) -> ShResult<Self> {
    Self::parse_stream(&SegStream::from(raw), allow_side_effects).map(|(varname, _)| varname)
  }

  pub(crate) fn name(&self) -> ShStr {
    self.name.clone()
  }
  pub(crate) fn index(&self) -> Option<&ArrIndex> {
    self.index.as_ref()
  }
  /// Replace the parsed index. Used to substitute a pre-resolved index
  /// (e.g. one whose `expand_arithmetic` was performed outside a borrow
  /// to avoid forking under a held `RefCell` guard).
  pub(crate) fn set_index(&mut self, idx: ArrIndex) {
    self.index = Some(idx);
  }
  pub(crate) fn slice_start(&self) -> Option<usize> {
    self.slice_start
  }
  pub(crate) fn slice_len(&self) -> Option<usize> {
    self.slice_len
  }

  pub(crate) fn is_all_split(&self) -> bool {
    matches!(self.index(), Some(ArrIndex::AllSplit)) || self.is_argv_split()
  }
  pub(crate) fn is_all_joined(&self) -> bool {
    matches!(self.index(), Some(ArrIndex::AllJoined)) || self.is_argv_joined()
  }
  pub(crate) fn is_argv_split(&self) -> bool {
    self.index.is_none() && *self.name() == *b"@"
  }
  pub(crate) fn is_argv_joined(&self) -> bool {
    self.index.is_none() && *self.name() == *b"*"
  }
}

#[derive(Clone)]
pub(crate) struct MagicVar(Arc<dyn Fn() -> Option<ShStr> + Send + Sync>);

impl<F: Fn() -> Option<ShStr> + 'static + Send + Sync> From<F> for MagicVar {
  fn from(value: F) -> Self {
    Self(Arc::new(value))
  }
}

impl Deref for MagicVar {
  type Target = dyn Fn() -> Option<ShStr>;
  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl Debug for MagicVar {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "<magic var>")
  }
}

pub(crate) trait ShStrSliceExt {
  fn join_with<P: AsRef<[u8]>>(&self, sep: P) -> ShStr;
}

impl ShStrSliceExt for [ShStr] {
  fn join_with<P: AsRef<[u8]>>(&self, sep: P) -> ShStr {
    let     sep  = sep.as_ref();

    let mut out  = vec![];
    let mut iter = self.iter();
    if let Some(first) = iter.next() {
      out.extend_from_slice(first.as_bytes());
      for v in iter {
        out.extend_from_slice(sep.as_bytes());
        out.extend_from_slice(v.as_bytes());
      }
    }
    ShStr::from(out)
  }
}

impl ShStrSliceExt for [Word] {
  fn join_with<P: AsRef<[u8]>>(&self, sep: P) -> ShStr {
    let     sep  = sep.as_ref();

    let mut out  = vec![];
    let mut iter = self.iter();
    if let Some(first) = iter.next() {
      out.extend_from_slice(first.as_bytes());
      for v in iter {
        out.extend_from_slice(sep.as_bytes());
        out.extend_from_slice(v.as_bytes());
      }
    }
    ShStr::from(out)
  }
}

impl ShStrSliceExt for [&ShStr] {
  fn join_with<P: AsRef<[u8]>>(&self, sep: P) -> ShStr {
    let     sep  = sep.as_ref();

    let mut out  = vec![];
    let mut iter = self.iter();
    if let Some(first) = iter.next() {
      out.extend_from_slice(first.as_bytes());
      for v in iter {
        out.extend_from_slice(sep.as_bytes());
        out.extend_from_slice(v.as_bytes());
      }
    }
    ShStr::from(out)
  }
}

/// shed's internal string type. Basically just a very fancy `&[u8]`. Used mainly when you need
/// ownership of a byte slice that doesn't necessarily allocate like `Vec<u8>` would.
///
/// `ShStr` is immutable, unlike [`String`].
///
/// It is a wrapper around [`hipstr::HipByt`], which is itself a shared-ownership byte sequence.
/// It does not have the same UTF-8 guarantees as [`String`] and [`str`], which is necessary to preserve
/// exact byte structure instead of mangling with functions like `to_string_lossy()`.
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Default, Hash)]
pub(crate) struct ShStr(HipByt<'static>);

impl ShStr {
  pub(crate) fn as_bytes(&self) -> &[u8] {
    &self.0
  }

  pub(crate) fn into_bytes(self) -> Vec<u8> {
    self.0.to_vec()
  }

  pub(crate) fn with_capacity(capacity: usize) -> Self {
    Self(HipByt::with_capacity(capacity))
  }

  pub(crate) fn contains_slice(&self, needle: &[u8]) -> bool {
    self.0.windows(needle.len()).any(|window| window == needle)
  }

  /// Borrowed UTF-8 view, or `None` if the bytes aren't valid UTF-8.
  pub(crate) fn to_str(&self) -> Option<&str> {
    std::str::from_utf8(&self.0).ok()
  }

  /// Lossy UTF-8 view (invalid bytes -> `U+FFFD`).
  pub(crate) fn to_str_lossy(&self) -> Cow<'_, str> {
    String::from_utf8_lossy(&self.0)
  }

  /// Wrapper method for [`FromStr::from_str`]
  ///
  /// Fails if the `ShStr` is invalid UTF-8, or the parse fails.
  /// On failure, returns a clone of the original `ShStr` for error reporting.
  pub(crate) fn parse<T: FromStr>(&self) -> Result<T, ShStr> {
    self
      .to_str()
      .ok_or_else(|| self.clone())?
      .parse::<T>()
      .map_err(|_| self.clone())
  }

  pub(crate) fn to_cstring(&self) -> Option<CString> {
    CString::new(self.as_bytes()).ok()
  }

  pub(crate) fn to_cstring_lossy(&self) -> CString {
    let mut bytes = self.as_bytes().to_vec();
    bytes.retain(|&b| b != 0);

    unsafe { CString::from_vec_unchecked(bytes) }
  }

  pub(crate) fn try_slice(&self, range: impl RangeBounds<usize>) -> Option<Self> {
    self.0.try_slice(range).ok().map(Self)
  }

  pub(crate) fn truncate(&mut self, len: usize) {
    self.0.truncate(len);
  }

  pub(crate) fn push_slice(&mut self, other: impl AsRef<[u8]>) {
    self.0.push_slice(other.as_ref());
  }

  pub(crate) fn push(&mut self, byte: u8) {
    self.0.push(byte);
  }

  pub(crate) fn from_slice(other: impl AsRef<[u8]>) -> Self {
    Self(HipByt::from(other.as_ref()))
  }

  pub(crate) fn from_slices(others: impl Iterator<Item = impl AsRef<[u8]>>) -> Self {
    let mut out = Self::default();
    for other in others {
      out.push_slice(other);
    }
    out
  }

  pub(crate) fn chain(mut self, other: impl AsRef<[u8]>) -> Self {
    self.push_slice(other);
    self
  }

  pub(crate) fn to_ascii_uppercase(&self) -> Self {
    Self(self.0.to_ascii_uppercase())
  }

  pub(crate) fn to_ascii_lowercase(&self) -> Self {
    Self(self.0.to_ascii_lowercase())
  }
}

impl Debug for ShStr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "ShStr({:?})", self.to_str_lossy())
  }
}

impl Deref for ShStr {
  type Target = [u8];
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl ToSql for ShStr {
  fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
    // Bind valid UTF-8 as TEXT so callers reading columns back as `String`
    // keep working; fall back to BLOB only when the bytes aren't UTF-8.
    match self.to_str() {
      Some(s) => Ok(rusqlite::types::ToSqlOutput::from(s)),
      None    => Ok(rusqlite::types::ToSqlOutput::from(self.as_bytes())),
    }
  }
}

impl FromSql for ShStr {
  fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
    match value {
      // `ToSql` binds values as BLOBs (ShStr may hold non-UTF-8 bytes), but
      // accept TEXT too so rows written by older, UTF-8-only builds still read.
      rusqlite::types::ValueRef::Text(s) | rusqlite::types::ValueRef::Blob(s) => {
        Ok(Self(HipByt::from(s)))
      }
      _ => Err(rusqlite::types::FromSqlError::InvalidType),
    }
  }
}

impl NixPath for ShStr {
  fn is_empty(&self) -> bool {
    self.0.is_empty()
  }

  fn len(&self) -> usize {
    self.0.len()
  }

  fn with_nix_path<T, F>(&self, f: F) -> nix::Result<T>
  where
    F: FnOnce(&std::ffi::CStr) -> T,
  {
    match self.to_cstring() {
      Some(cstr) => Ok(f(&cstr)),
      None       => Err(Errno::EINVAL),
    }
  }
}

impl Borrow<[u8]> for ShStr {
  fn borrow(&self) -> &[u8] {
    self.as_bytes()
  }
}

impl From<SmallVec<[u8; 24]>> for ShStr {
  fn from(value: SmallVec<[u8; 24]>) -> Self {
    Self(HipByt::from(value.as_slice()))
  }
}

impl From<char> for ShStr {
  fn from(value: char) -> Self {
    let mut buf = [0u8; 4];
    let     s   = value.encode_utf8(&mut buf);
    Self(HipByt::from(s.as_bytes()))
  }
}

impl From<OsString> for ShStr {
  fn from(value: OsString) -> Self {
    Self(HipByt::from(value.as_bytes()))
  }
}

impl From<ShStr> for PathBuf {
  fn from(value: ShStr) -> Self {
    PathBuf::from(OsStr::from_bytes(value.as_bytes()))
  }
}

impl From<PathBuf> for ShStr {
  fn from(value: PathBuf) -> Self {
    Self(HipByt::from(value.as_os_str().as_bytes()))
  }
}

impl From<&Path> for ShStr {
  fn from(value: &Path) -> Self {
    Self(HipByt::from(value.as_os_str().as_bytes()))
  }
}

impl From<Vec<u8>> for ShStr {
  fn from(value: Vec<u8>) -> Self {
    Self(HipByt::from(value))
  }
}

impl From<&[u8]> for ShStr {
  fn from(value: &[u8]) -> Self {
    Self(HipByt::from(value))
  }
}

impl FromIterator<char> for ShStr {
  fn from_iter<T: IntoIterator<Item = char>>(iter: T) -> Self {
    let mut builder = SmolStrBuilder::new();
    for ch in iter {
      builder.push(ch);
    }
    Self(HipByt::from(builder.finish().as_bytes()))
  }
}

impl From<Var> for ShStr {
  fn from(value: Var) -> Self {
    match value.into_kind() {
      VarKind::Str(vstr) => vstr,
      VarKind::Int(n)    => varstr!("{n}"),
      other              => ShStr::from(other.to_string()),
    }
  }
}

impl From<&Var> for ShStr {
  fn from(value: &Var) -> Self {
    let is_scalar = matches!(value.kind(), VarKind::Str(_) | VarKind::Int(_));
    if is_scalar {
      let Var { kind, .. } = value;
      match kind {
        VarKind::Str(var_str) => var_str.clone(),
        VarKind::Int(n)       => (*n).into(),
        _ => unreachable!(),
      }
    } else {
      value.to_string().into()
    }
  }
}

impl From<ShStr> for Vec<u8> {
  fn from(value: ShStr) -> Self {
    value.as_bytes().to_vec()
  }
}

macro_rules! impl_varstr_from {
  ($($t:ty),*) => {
    $(impl From<$t> for ShStr {
      fn from(value: $t) -> ShStr {
        $crate::varstr!("{value}")
      }
    })*
  };
}

impl_varstr_from!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl AsRef<std::ffi::OsStr> for ShStr {
  fn as_ref(&self) -> &std::ffi::OsStr {
    std::ffi::OsStr::from_bytes(self.as_bytes())
  }
}

impl AsRef<Path> for ShStr {
  fn as_ref(&self) -> &Path {
    Path::new(OsStr::from_bytes(self.as_bytes()))
  }
}

impl AsRef<[u8]> for ShStr {
  fn as_ref(&self) -> &[u8] {
    self.as_bytes()
  }
}

impl From<ShStr> for Rc<str> {
  fn from(value: ShStr) -> Self {
    Rc::from(value.to_str_lossy().as_ref())
  }
}

impl PartialEq<str> for ShStr {
  fn eq(&self, other: &str) -> bool {
    self.0 == other.as_bytes()
  }
}

impl PartialEq<&str> for ShStr {
  fn eq(&self, other: &&str) -> bool {
    self.0 == other.as_bytes()
  }
}

impl PartialEq<String> for ShStr {
  fn eq(&self, other: &String) -> bool {
    self.0 == other.as_bytes()
  }
}

impl Display for ShStr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    Display::fmt(&self.0.to_str_lossy(), f)
  }
}

impl From<&mut str> for ShStr {
  fn from(s: &mut str) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}

impl From<&str> for ShStr {
  fn from(s: &str) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}
impl From<String> for ShStr {
  fn from(s: String) -> Self {
    // Owning: reuse the String's allocation instead of copying.
    Self(HipByt::from(s.into_bytes()))
  }
}
impl From<&String> for ShStr {
  fn from(s: &String) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}
impl From<SmolStr> for ShStr {
  fn from(s: SmolStr) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}
impl From<Cow<'_, str>> for ShStr {
  fn from(s: Cow<'_, str>) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}

#[derive(Clone, Debug)]
pub(crate) enum VarKind {
  Str(ShStr),
  Int(i32),
  Arr(VecDeque<ShStr>),
  AssocArr(Vec<(ShStr, ShStr)>),

  /// A "magic" variable. Lazily evaluated on access by calling the wrapped function, which can return `None`
  /// It wraps an `Arc<dyn Fn() -> Option<String>>`
  ///
  /// You can put call parens on the wrapped value directly to obtain the value of the variable.
  /// These aren't currently exposed in the user-facing syntax.
  Magic(MagicVar),

  /// A variable that has been declared but not assigned a value.
  /// This is distinct from a variable that has never been declared, which is represented by the absence of a `Var` entry in the variable table.
  /// Created by non-assigning declare calls, like 'declare var'
  Unset,
}

impl Default for VarKind {
  fn default() -> Self {
    Self::Str(ShStr::default())
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VarKindTag {
  Str,
  Int,
  Arr,
  AssocArr,
  Magic,
  Unset,
}

impl VarKind {
  pub(crate) fn tag(&self) -> VarKindTag {
    match self {
      Self::Str(_)      => VarKindTag::Str,
      Self::Int(_)      => VarKindTag::Int,
      Self::Arr(_)      => VarKindTag::Arr,
      Self::AssocArr(_) => VarKindTag::AssocArr,
      Self::Magic(_)    => VarKindTag::Magic,
      Self::Unset       => VarKindTag::Unset,
    }
  }
}

impl VarKind {
  pub(crate) fn arr_from_tk(tk: &Tk) -> ShResult<Self> {
    let raw = tk.slice();
    Self::arr_from_raw(&raw)
  }

  pub(crate) fn arr_from_raw(raw: &[u8]) -> ShResult<Self> {
    if !raw.starts_with(b"(") || !raw.ends_with(b")") {
      return Err(sherr!(ParseErr, "Invalid array syntax: {}", raw.to_str_lossy(),));
    }
    let raw    = &raw[1..raw.len() - 1];

    let handle = state::register_source(raw);
    let tokens = LexStream::new(&handle, LexFlags::empty())
      .filter(|tk| {
        !tk
          .as_ref()
          .is_ok_and(|tk| matches!(tk.class, TkRule::Sep | TkRule::Comment))
      })
      .map(|tk| tk.and_then(|tk| tk.expand()).map(|tk| tk.get_words()))
      .try_fold(Vec::new(), |mut acc, wrds| {
        match wrds {
          Ok(wrds) => acc.extend(wrds.iter().map(|w| w.content())),
          Err(e)   => return Err(e),
        }
        Ok(acc)
      })?;

    Ok(Self::Arr(tokens.into()))
  }

  pub(crate) fn parse(raw: &[u8]) -> Self {
    Self::arr_from_raw(raw).unwrap_or_else(|_| Self::Str(raw.into()))
  }

  pub(crate) fn string(raw: ShStr) -> Self {
    Self::Str(raw)
  }

  pub(crate) fn arr<I: IntoIterator<Item = ShStr>>(iter: I) -> Self {
    let vec: VecDeque<ShStr> = iter.into_iter().collect();
    Self::Arr(vec)
  }

  pub(crate) fn assoc_arr<I: IntoIterator<Item = (ShStr, ShStr)>>(iter: I) -> Self {
    let pairs = iter.into_iter().collect();
    Self::AssocArr(pairs)
  }

  pub(crate) fn assoc_arr_from_raw(raw: &[u8]) -> ShResult<Self> {
    if !raw.starts_with(b"(") || !raw.ends_with(b")") {
      return Err(sherr!(ParseErr, "Invalid associative array syntax: {}", raw.to_str_lossy(),));
    }
    let     body  = &raw[1..raw.len() - 1];
    let mut pairs = Vec::new();
    let mut bytes = SliceCursor::new(body);

    loop {
      // Skip whitespace
      bytes.bump_while(|b| b.is_ascii_whitespace());

      if bytes.is_empty() {
        break;
      }

      // Expect '[' to open the key.
      if bytes.next_byte() != Some(b'[') {
        return Err(sherr!(
          ParseErr,
          "Invalid associative array element: expected '[' to start key",
        ));
      }

      // Read until the matching ']'.
      let mut key   = util::scratch_buf();
      let mut depth = 1usize;
      loop {
        let Some(b) = bytes.next_byte() else {
          return Err(sherr!(ParseErr, "Unclosed '[' in associative array key",));
        };
        match b {
          b'[' => {
            depth += 1;
            key.push(b);
          }
          b']' => {
            depth -= 1;
            if depth == 0 {
              break;
            }
            key.push(b);
          }
          _ => key.push(b),
        }
      }

      let expanded_key = Expander::from_raw(&key, TkFlags::empty()).expand_no_split()?;

      // Expect '=' immediately after ']'.
      if !bytes.bump_if_eq(b'=') {
        return Err(sherr!(ParseErr, "Expected '=' after ']' in associative array element",));
      }

      // Read the value up to top-level whitespace. Quote chars are kept (and
      // tracked only to tell when whitespace is inside them) so the expander
      // still handles the original quoting, e.g. $'...' ANSI-C strings.
      let mut val      = util::scratch_buf();
      let mut qt_state = QuoteState::default();
      match_loop!(bytes.peek_byte() => c, {
        b'\\' if !qt_state.in_single() => {
          bytes.bump();
          val.push(c);
          if let Some(next) = bytes.next_byte() {
            val.push(next);
          }
        }
        b'"' => {
          bytes.bump();
          qt_state.toggle_double();
          val.push(c);
        }
        b'\'' => {
          bytes.bump();
          qt_state.toggle_single();
          val.push(c);
        }
        _ if c.is_ascii_whitespace() && qt_state.outside() => break,
        _ => {
          bytes.bump();
          val.push(c);
        }
      });

      // Expand the value
      let expanded_val = Expander::from_raw(&val, TkFlags::empty()).expand_no_split()?;

      pairs.push((expanded_key, expanded_val));
    }

    Ok(Self::AssocArr(pairs))
  }
}

impl Display for VarKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      VarKind::Str(s) => write!(f, "{s}"),
      VarKind::Int(i) => write!(f, "{i}"),
      VarKind::Arr(items) => {
        let mut item_iter = items.iter().peekable();
        while let Some(item) = item_iter.next() {
          write!(f, "{item}")?;
          if item_iter.peek().is_some() {
            write!(f, " ")?;
          }
        }
        Ok(())
      }
      VarKind::AssocArr(items) => {
        let mut item_iter = items.iter().peekable();
        while let Some(item) = item_iter.next() {
          let (k, v) = item;
          let key    = escape::shell_quote(k);
          let val    = escape::shell_quote(v);
          write!(f, "{key}={val}")?;
          if item_iter.peek().is_some() {
            write!(f, " ")?;
          }
        }
        Ok(())
      }
      VarKind::Magic(func) => write!(f, "{}", func().unwrap_or_default()),
      VarKind::Unset       => Ok(()),
    }
  }
}

#[derive(Clone, Debug)]
pub(crate) struct Var {
  flags: VarFlags,
  kind : VarKind,
}

impl Default for Var {
  fn default() -> Self {
    Self {
      flags: VarFlags::default(),
      kind : VarKind::Str(ShStr::default()),
    }
  }
}

impl Var {
  pub(crate) fn env_var(val: &str) -> Self {
    Self {
      flags: VarFlags::EXPORT,
      kind : VarKind::Str(val.into()),
    }
  }
  pub(crate) fn new(kind: VarKind, flags: VarFlags) -> Self {
    Self { flags, kind }
  }
  pub(crate) fn kind(&self) -> &VarKind {
    &self.kind
  }
  pub(crate) fn kind_mut(&mut self) -> &mut VarKind {
    &mut self.kind
  }
  pub(crate) fn into_kind(self) -> VarKind {
    self.kind
  }
  pub(crate) fn mark_for_export(&mut self) {
    self.flags.set(VarFlags::EXPORT, true);
  }
  pub(crate) fn flags(&self) -> VarFlags {
    self.flags
  }
  pub(crate) fn flags_mut(&mut self) -> &mut VarFlags {
    &mut self.flags
  }
}

impl Display for Var {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    Display::fmt(&self.kind, f)
  }
}

impl From<Vec<String>> for Var {
  fn from(value: Vec<String>) -> Self {
    let arr = value.into_iter().map(ShStr::from).collect();
    Self::new(VarKind::Arr(arr), VarFlags::empty())
  }
}

impl From<Vec<Candidate>> for Var {
  fn from(value: Vec<Candidate>) -> Self {
    let as_strs = value
      .into_iter()
      .map(|c| c.content().into())
      .collect::<Vec<_>>();
    Self::new(VarKind::Arr(as_strs.into()), VarFlags::empty())
  }
}

impl From<&[String]> for Var {
  fn from(value: &[String]) -> Self {
    let mut new = VecDeque::new();
    new.extend(value.iter().map(ShStr::from));
    Self::new(VarKind::Arr(new), VarFlags::empty())
  }
}

impl From<ShStr> for Var {
  fn from(value: ShStr) -> Self {
    Self::new(VarKind::Str(value), VarFlags::empty())
  }
}

impl From<String> for Var {
  fn from(value: String) -> Self {
    Self::new(VarKind::Str(value.into()), VarFlags::empty())
  }
}

impl From<OsString> for Var {
  fn from(value: OsString) -> Self {
    Self::new(VarKind::Str(value.into()), VarFlags::empty())
  }
}

impl From<&str> for Var {
  fn from(value: &str) -> Self {
    Self::new(VarKind::Str(value.into()), VarFlags::empty())
  }
}

impl From<&String> for Var {
  fn from(value: &String) -> Self {
    Self::new(VarKind::Str(value.into()), VarFlags::empty())
  }
}

macro_rules! impl_var_from {
  ($($t:ty),*) => {
    $(impl From<$t> for Var {
      fn from(value: $t) -> Self {
        Self::new(VarKind::Str(value.to_string().into()), VarFlags::empty())
      }
    })*
  };
}

impl_var_from!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, bool);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub(crate) enum ScopeKind {
  Ceiling,
  Function,
  #[default]
  Block,
}

#[derive(Clone, Debug)]
pub(crate) struct DeferredAst {
  pub ast: Ast,
  pub ctx: LabelBuilder,
}

impl Deref for DeferredAst {
  type Target = Ast;
  fn deref(&self) -> &Self::Target {
    &self.ast
  }
}

#[derive(Default, Clone, Debug)]
pub(crate) struct VarTab {
  vars   : HashMap<ShStr, Var>,
  params : HashMap<ShellParam, ShStr>,
  sh_argv: VecDeque<ShStr>, /* Using a VecDeque makes the implementation of `shift` straightforward */

  kind: ScopeKind,
  deferred_cmds: Vec<DeferredAst>,
}

impl VarTab {
  pub(crate) fn bare() -> Self {
    Self {
      vars: HashMap::default(),
      params: HashMap::default(),
      sh_argv: VecDeque::new(),
      kind: ScopeKind::Block,
      deferred_cmds: Vec::new(),
    }
  }
  pub(crate) fn new() -> Self {
    let vars   = Self::init_sh_vars();
    let params = Self::init_params();
    let mut var_tab = Self {
      vars,
      params,
      sh_argv: VecDeque::new(),
      kind: ScopeKind::Block,
      deferred_cmds: Vec::new(),
    };
    var_tab.init_sh_argv();
    var_tab.init_magic_vars();
    var_tab
  }
  pub(crate) fn set_kind(&mut self, scope_kind: ScopeKind) {
    self.kind = scope_kind;
  }
  pub(crate) fn is_ceiling(&self) -> bool {
    matches!(self.kind, ScopeKind::Ceiling)
  }
  pub(crate) fn is_function(&self) -> bool {
    matches!(self.kind, ScopeKind::Function)
  }
  fn init_params() -> HashMap<ShellParam, ShStr> {
    let mut params = HashMap::default();
    params.insert(ShellParam::ArgCount, "0".into()); // Number of positional parameters
    params.insert(ShellParam::ShPid, Pid::this().to_string().into()); // PID of the shell
    params.insert(ShellParam::LastJob, ShStr::default()); // PID of the last background job (if any)
    params
  }
  fn init_sh_vars() -> HashMap<ShStr, Var> {
    let mut vars = HashMap::default();
    vars.insert("COMP_WORDBREAKS".into(), " \t\n\"'@><=;|&(:".into());
    vars.insert("OPTIND".into(), "1".into());
    let env_vars = Self::init_env();
    vars.extend(env_vars);
    for (name, var) in &mut vars {
      if SHELL_MANAGED_VARS.contains(&name.as_bytes()) {
        var.flags |= VarFlags::SHELL;
      }
    }
    vars
  }
  fn init_env() -> Vec<(ShStr, Var)> {
    let pathbuf_to_string =
      |pb: Result<PathBuf, std::io::Error>| -> ShStr { pb.unwrap_or_default().into() };

    // real fd, not the sink table: this runs during Shed construction, before
    // the fd table exists — stdin_is_tty() -> Shed::sinks would re-enter init.
    let term = if unistd::isatty(procio::stdin_fileno()).unwrap_or(false) {
      std::env::var("TERM").unwrap_or_else(|_| "linux".to_string())
    } else {
      "xterm-256color".to_string()
    };

    let home_fallback;
    let username_fallback;
    let uid;
    static CURRENT_USER: std::sync::OnceLock<Option<User>> = std::sync::OnceLock::new();
    let cached_user = CURRENT_USER
      .get_or_init(|| User::from_uid(Uid::current()).ok().flatten())
      .clone();
    if let Some(user) = cached_user {
      home_fallback = user.dir;
      username_fallback = user.name;
      uid = user.uid;
    } else {
      home_fallback = PathBuf::new();
      username_fallback = "unknown".into();
      uid = 0.into();
    }
    let home_fallback = pathbuf_to_string(Ok(home_fallback));
    let hostname = unistd::gethostname()
      .map(|hname| hname.to_string_lossy().to_string())
      .unwrap_or_default();

    // Inherit OS env. Subsequent steps either insert-if-missing (user-set
    // vars like HOME/USER) or are unconditionally overridden for
    // shed-controlled vars like UID/PPID/PWD.
    let mut env: HashMap<ShStr, ShStr> = std::env::vars_os()
      .map(|(k, v)| (k.as_bytes().into(), v.as_bytes().into()))
      .collect();

    // Insert-if-missing: prefer the inherited value, fall back otherwise.
    env
      .entry("HOME".into())
      .or_insert_with(|| home_fallback.clone());
    env
      .entry("USER".into())
      .or_insert_with(|| username_fallback.clone().into());
    if let Some(path) = params::get_default_path() {
      env.entry("PATH".into()).or_insert_with(|| path.into());
    }

    let resolved_home = env[b"HOME".as_slice()].clone();
    let resolved_user = env[b"USER".as_slice()].clone();

    let mut state_dir = env.get(b"XDG_STATE_HOME".as_slice()).map_or_else(
      || PathBuf::from(format!("{resolved_home}/.local/state")),
      |x| PathBuf::from(x.clone()),
    );
    state_dir.push("shed");
    let shed_db = state_dir.join("shed_hist.db");

    env.entry("TMPDIR".into()).or_insert_with(|| "/tmp".into());
    env.entry("TERM".into()).or_insert_with(|| term.into());
    env
      .entry("LANG".into())
      .or_insert_with(|| "en_US.UTF-8".into());
    env.entry("LOGNAME".into()).or_insert(resolved_user);
    env
      .entry("SHELL".into())
      .or_insert_with(|| pathbuf_to_string(std::env::current_exe()));
    env
      .entry("SHED_HISTDB".into())
      .or_insert_with(|| shed_db.display().to_string().into());

    // SHED_HPATH: prepend install_dir if it isn't already present.
    if let Some(install_dir) = super::builtin::HELP_PAGE_INSTALL_DIR {
      let new_hpath = match env.get(b"SHED_HPATH".as_slice()) {
        Some(hpath) if !paths::split_path_list(hpath).any(|p| p.as_os_str() == install_dir) => {
          Some(format!("{install_dir}:{}", hpath.to_str_lossy()))
        }
        None => Some(install_dir.to_string()),
        _    => None,
      };
      if let Some(hpath) = new_hpath {
        env.insert("SHED_HPATH".into(), hpath.into());
      }
    }

    // Unconditional overrides: shed-controlled values that should not
    // honor an inherited (potentially spoofed) env entry.
    env.insert(
      "PWD".into(),
      paths::path_to_varstr(&std::env::current_dir().unwrap_or_default()),
    );
    env.insert("IFS".into(), " \t\n".into());
    env.insert("UID".into(), uid.to_string().into());
    env.insert("PPID".into(), unistd::getppid().to_string().into());
    env.insert("HOST".into(), hostname.into());

    let mut vars = env
      .into_iter()
      .map(|(k, v)| (k, Var::new(VarKind::Str(v), VarFlags::EXPORT)))
      .collect_vec();

    let     orig      = stat::umask(stat::Mode::empty());
    let     umask     = stat::umask(orig);
    let mut umask_var = Var::env_var(&format!("{umask:04o}"));
    let underline = Var::new(
      VarKind::Str(ShStr::from(
        std::env::args_os().next().unwrap_or_default().into_vec(),
      )),
      VarFlags::EXPORT,
    );
    umask_var.flags |= VarFlags::READONLY;
    vars.push(("UMASK".into(), umask_var));
    vars.push(("_".into(), underline));

    vars
  }
  pub(crate) fn init_magic_vars(&mut self) {
    let magic_vars = [
      ("?".into(), get_status_str.into()),
      ("SECONDS".into(), get_seconds.into()),
      ("EPOCHREALTIME".into(), get_epoch_realtime.into()),
      ("EPOCHSECONDS".into(), get_epoch_seconds.into()),
      ("RANDOM".into(), get_random.into()),
      ("LINES".into(), get_lines.into()),
      ("COLUMNS".into(), get_columns.into()),
      ("-".into(), get_set_flags.into()),
    ];

    for (name, func) in magic_vars {
      self
        .vars
        .insert(name, Var::new(VarKind::Magic(func), VarFlags::READONLY));
    }
  }
  pub(crate) fn init_sh_argv(&mut self) {
    self.extend_args(std::env::args_os().map(|a| ShStr::from(a.into_vec())));
  }
  pub(crate) fn defer_cmd(&mut self, ast: Ast, ctx: LabelBuilder) {
    self.deferred_cmds.push(DeferredAst { ast, ctx });
  }
  pub(crate) fn take_deferred_cmds(&mut self) -> Vec<DeferredAst> {
    std::mem::take(&mut self.deferred_cmds)
  }
  pub(crate) fn has_deferred_cmds(&self) -> bool {
    !self.deferred_cmds.is_empty()
  }
  pub(crate) fn sh_argv(&self) -> &VecDeque<ShStr> {
    &self.sh_argv
  }
  pub(crate) fn sh_argv_mut(&mut self) -> &mut VecDeque<ShStr> {
    &mut self.sh_argv
  }
  pub(crate) fn clear_pos_args(&mut self) {
    self.sh_argv.truncate(1);
  }
  pub(crate) fn extend_args(&mut self, args: impl IntoIterator<Item = ShStr>) {
    self.sh_argv.extend(args);
  }
  /// Push an arg to the back of the arg deque
  pub(crate) fn bpush_arg(&mut self, arg: ShStr) {
    self.sh_argv.push_back(arg);
  }
  /// Pop an arg from the front of the arg deque
  pub(crate) fn fpop_arg(&mut self) -> Option<ShStr> {
    self.sh_argv.pop_front()
  }
  pub(crate) fn vars(&self) -> &HashMap<ShStr, Var> {
    &self.vars
  }
  pub(crate) fn vars_mut(&mut self) -> &mut HashMap<ShStr, Var> {
    &mut self.vars
  }
  /// Clear the export attribute (`export -n`), leaving the variable's value
  /// intact as an ordinary shell variable.
  pub(crate) fn unexport_var(&mut self, var_name: &[u8]) {
    if let Some(var) = self.vars.get_mut(var_name)
      && var.flags.contains(VarFlags::EXPORT)
    {
      var.flags.remove(VarFlags::EXPORT);
      Shed::meta_mut(MetaTab::clear_envp);
    }
  }
  pub(crate) fn try_get_local(&self, var_name: &[u8]) -> Option<ShStr> {
    // A declared-but-unset variable is present in the scope (so it shadows
    // outer scopes and keeps later assignments local) but has no value: it
    // reads as unset for `${x-}`/`${x+}` etc
    match self.vars.get(var_name) {
      Some(var) if matches!(var.kind(), VarKind::Unset) => None,
      other => other.map(ShStr::from),
    }
  }
  pub(crate) fn try_take_local(&mut self, var_name: &[u8]) -> ShResult<Option<Var>> {
    let Some(var) = self.try_take_var(var_name)? else {
      return Ok(None);
    };

    match var.kind() {
      VarKind::Unset => {
        self.vars.insert(var_name.into(), var);
        Ok(None)
      }
      _ => Ok(Some(var)),
    }
  }
  pub(crate) fn try_get_var_meta(&self, var: &[u8]) -> Option<Var> {
    self.vars.get(var).cloned()
  }
  pub(crate) fn try_get_var_kind_tag(&self, var: &[u8]) -> Option<VarKindTag> {
    self.vars.get(var).map(|v| v.kind().tag())
  }
  pub(crate) fn get_var_flags(&self, var_name: &[u8]) -> Option<VarFlags> {
    self.vars.get(var_name).map(|var| var.flags)
  }
  pub(crate) fn try_take_param(&mut self, param: ShellParam) -> Option<ShStr> {
    self.params.get_mut(&param).map(std::mem::take)
  }
  pub(crate) fn try_take_var(&mut self, var_name: &[u8]) -> ShResult<Option<Var>> {
    if let Some(var) = self.vars.get(var_name) {
      let var_name = var_name.to_str_lossy();

      if var.flags.contains(VarFlags::READONLY) {
        return Err(sherr!(ExecFail, "cannot unset readonly variable '{var_name}'"));
      }
      if var.flags.contains(VarFlags::EXPORT) {
        Shed::meta_mut(MetaTab::clear_envp);
      }
    }
    Ok(self.vars.remove(var_name))
  }
  pub(crate) fn unset_var(&mut self, var_name: &[u8]) -> ShResult<()> {
    self.try_take_var(var_name).map(|_| ())
  }
  pub(crate) fn set_index(&mut self, var_name: &[u8], idx: ArrIndex, val: ShStr) -> ShResult<()> {
    // 'idx' must already be resolved at this point
    if self.var_exists(var_name)
      && let Some(var) = self.vars_mut().get_mut(var_name)
    {
      let var_name = var_name.to_str_lossy();

      match var.kind_mut() {
        VarKind::Arr(items) => {
          let idx = match idx {
            ArrIndex::Literal(n) => n,
            ArrIndex::FromBack(n) => {
              if items.len() >= n {
                items.len() - n
              } else {
                return Err(sherr!(ExecFail, "Index {n} out of bounds for array '{var_name}'"));
              }
            }
            _ => {
              return Err(sherr!(ExecFail, "Cannot index all elements of array '{var_name}'"));
            }
          };

          if idx >= items.len() {
            items.resize(idx + 1, ShStr::default());
          }
          items[idx] = val;
          return Ok(());
        }
        VarKind::AssocArr(items) => {
          // resolve_for guarantees `idx` is `Key(_)` here (or one of the
          // wildcards below, which don't make sense for assignment).
          let ArrIndex::Key(key) = idx else {
            return Err(sherr!(
              ExecFail,
              "Cannot assign to all elements of associative array '{var_name}'",
            ));
          };
          for (k, v) in items.iter_mut() {
            if k == &key {
              *v = val;
              return Ok(());
            }
          }
          items.push((key, val));
          return Ok(());
        }
        _ => {
          return Err(sherr!(ExecFail, "Variable '{var_name}' is not an array"));
        }
      }
    }
    let var_name = var_name.to_str_lossy();

    let kind = match idx {
      ArrIndex::Literal(n) => {
        let mut items: VecDeque<ShStr> = VecDeque::with_capacity(n + 1);
        items.resize(n, ShStr::default());
        items.push_back(val);
        VarKind::Arr(items)
      }
      ArrIndex::Key(key) => VarKind::AssocArr(vec![(key, val)]),
      ArrIndex::FromBack(n) => {
        return Err(sherr!(ExecFail, "Index {n} out of bounds for array '{var_name}'"));
      }
      _ => {
        return Err(sherr!(ExecFail, "Cannot index all elements of array '{var_name}'"));
      }
    };

    self.set_var(var_name.as_bytes(), kind, VarFlags::empty())
  }
  pub(crate) fn unset_index(&mut self, var_name: &[u8], idx: ArrIndex) -> ShResult<()> {
    let Some(var) = self.vars.get_mut(var_name) else {
      return Ok(());
    };
    let var_name = var_name.to_str_lossy();

    if var.flags.contains(VarFlags::READONLY) {
      return Err(sherr!(ExecFail, "cannot unset readonly variable '{var_name}'"));
    }
    match var.kind_mut() {
      VarKind::Arr(items) => {
        let i = match idx {
          ArrIndex::Literal(n) => n,
          ArrIndex::FromBack(n) if items.len() >= n => items.len() - n,
          ArrIndex::FromBack(_) => return Ok(()),
          _ => {
            return Err(sherr!(ExecFail, "Cannot unset all elements of array '{var_name}'"));
          }
        };
        if i < items.len() {
          items.remove(i);
        }
        Ok(())
      }
      VarKind::AssocArr(items) => {
        let ArrIndex::Key(key) = idx else {
          return Err(sherr!(
            ExecFail,
            "Cannot unset all elements of associative array '{var_name}'"
          ));
        };
        items.retain(|(k, _)| k != &key);
        Ok(())
      }
      _ => Err(sherr!(ExecFail, "Variable '{var_name}' is not an array")),
    }
  }
  pub(crate) fn put_var(&mut self, name: &[u8], var: Var) {
    // zero-validation insertion of variables
    self.vars.insert(name.into(), var);
  }
  pub(crate) fn set_var(&mut self, var_name: &[u8], val: VarKind, flags: VarFlags) -> ShResult<()> {
    let flags = if SHELL_MANAGED_VARS.contains(&var_name) {
      flags | VarFlags::SHELL
    } else {
      flags
    };
    if let Some(var) = self.vars.get_mut(var_name) {
      let var_name = var_name.to_str_lossy();

      if var.flags.contains(VarFlags::READONLY) && !flags.contains(VarFlags::READONLY) {
        return Err(sherr!(ExecFail, "Variable '{var_name}' is readonly"));
      }
      var.kind = val;
      var.flags |= flags;
      if var.flags.contains(VarFlags::EXPORT) || flags.contains(VarFlags::EXPORT) {
        if flags.contains(VarFlags::EXPORT) && !var.flags.contains(VarFlags::EXPORT) {
          var.mark_for_export();
        }

        Shed::meta_mut(MetaTab::clear_envp);
      }
    } else {
      let mut var = Var::new(val, flags);
      if flags.contains(VarFlags::EXPORT) {
        Shed::meta_mut(MetaTab::clear_envp);
        var.mark_for_export();
      }
      self.vars.insert(var_name.into(), var);
    }
    Ok(())
  }
  /// OR additional attribute flags onto an existing variable, leaving its value
  /// untouched. Used when re-declaring an already-set name (`x=5; declare x`),
  /// which adds attributes without clobbering the value.
  pub(crate) fn merge_flags(&mut self, var_name: &[u8], flags: VarFlags) {
    if let Some(var) = self.vars.get_mut(var_name) {
      let newly_exported =
        flags.contains(VarFlags::EXPORT) && !var.flags.contains(VarFlags::EXPORT);
      var.flags |= flags;
      if newly_exported {
        Shed::meta_mut(MetaTab::clear_envp);
      }
    }
  }
  pub(crate) fn var_exists(&self, var_name: &[u8]) -> bool {
    if ShellParam::is_param(var_name)
      && let Ok(param) = ShellParam::from_bytes(var_name)
    {
      return self.params.contains_key(&param);
    }
    self.vars.contains_key(var_name)
  }
  pub(crate) fn pos_args(&self) -> impl Iterator<Item = &ShStr> {
    self.sh_argv.iter().skip(1)
  }
  pub(crate) fn set_param(&mut self, param: ShellParam, val: &ShStr) {
    self.params.insert(param, val.clone());
  }
  pub(crate) fn try_get_param(&self, param: ShellParam) -> Option<ShStr> {
    match param {
      ShellParam::Pos(n) => self.sh_argv().get(n).cloned(),
      ShellParam::ArgCount => {
        let n = self.sh_argv.len().saturating_sub(1);
        Some(varstr!("{n}"))
      }
      ShellParam::AllArgsStr => {
        let ifs = params::get_separator();
        (!self.sh_argv.is_empty()).then(|| {
          self.pos_args().fold(ShStr::default(), |mut acc, arg| {
            if acc.is_empty() {
              acc = arg.clone();
            } else {
              acc.push_slice(&ifs);
              acc.push_slice(arg);
            }
            acc
          })
        })
      }

      _ => self.params.get(&param).cloned(),
    }
  }
}

// magic variable functions

fn get_status_str() -> Option<ShStr> {
  let status = Shed::get_status();
  Some(varstr!("{status}"))
}
fn get_seconds() -> Option<ShStr> {
  let shell_time = Shed::meta(MetaTab::shell_time);
  let secs       = Instant::now().duration_since(shell_time).as_secs();
  Some(varstr!("{secs}"))
}
fn get_epoch_realtime() -> Option<ShStr> {
  let epoch = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or(Duration::from_secs(0))
    .as_secs_f64();
  Some(varstr!("{epoch:.6}"))
}

fn get_epoch_seconds() -> Option<ShStr> {
  let epoch = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or(Duration::from_secs(0))
    .as_secs();
  Some(varstr!("{epoch}"))
}

fn get_random() -> Option<ShStr> {
  let random = random::random_range(0..32768);
  Some(varstr!("{random}"))
}

fn get_lines() -> Option<ShStr> {
  let rows = Shed::term(Terminal::t_rows);
  Some(varstr!("{rows}"))
}

fn get_columns() -> Option<ShStr> {
  let cols = Shed::term(Terminal::t_cols);
  Some(varstr!("{cols}"))
}

fn get_set_flags() -> Option<ShStr> {
  let mut set_opts = util::scratch_buf();
  Shed::shopts(|o| {
    if o.set.allexport {
      set_opts.push(b'a');
    }
    if o.set.notify {
      set_opts.push(b'b');
    }
    if o.set.noclobber {
      set_opts.push(b'C');
    }
    if o.set.errexit {
      set_opts.push(b'e');
    }
    if o.set.noglob {
      set_opts.push(b'f');
    }
    if o.set.hashall {
      set_opts.push(b'h');
    }
    if Shed::term(Terminal::interactive) {
      set_opts.push(b'i');
    }
    if o.set.monitor {
      set_opts.push(b'm');
    }
    if o.set.localvars {
      set_opts.push(b'l');
    }
    if o.set.noexec {
      set_opts.push(b'n');
    }
    if o.set.nounset {
      set_opts.push(b'u');
    }
    if o.set.verbose {
      set_opts.push(b'v');
    }
    if o.set.xtrace {
      set_opts.push(b'x');
    }
  });
  (!set_opts.is_empty()).then(|| set_opts.into())
}

#[cfg(test)]
mod parse_stream_slice_tests {
  use super::*;
  use crate::expand::stream::SegStream;

  fn slice_of(src: &str) -> (Option<usize>, Option<usize>) {
    let (parsed, _) = VarName::parse_stream(&SegStream::from(src), false).unwrap();
    (parsed.slice_start(), parsed.slice_len())
  }

  fn consumed(src: &str) -> usize {
    VarName::parse_stream(&SegStream::from(src), false)
      .unwrap()
      .1
  }

  #[test]
  fn both_bounds_split_at_the_top_level_colon() {
    assert_eq!(slice_of("arr[@]:1:2"), (Some(1), Some(2)));
  }

  #[test]
  fn start_only_leaves_the_length_unset() {
    assert_eq!(slice_of("arr[@]:1"), (Some(1), None));
  }

  #[test]
  fn empty_start_is_unset() {
    assert_eq!(slice_of("arr[@]::2"), (None, Some(2)));
  }

  #[test]
  fn empty_length_is_unset() {
    assert_eq!(slice_of("arr[@]:1:"), (Some(1), None));
  }

  #[test]
  fn no_slice_suffix_leaves_both_unset() {
    assert_eq!(slice_of("arr[@]"), (None, None));
  }

  #[test]
  fn colon_inside_braces_does_not_split() {
    // The split must land on the second top-level colon, not the one in `${u:-1}`.
    assert_eq!(slice_of("arr[@]:${u:-1}:2").1, Some(2));
  }

  #[test]
  fn colon_inside_nested_braces_does_not_split() {
    assert_eq!(slice_of("arr[@]:${a:-${b:-1}}:2").1, Some(2));
  }

  #[test]
  fn colon_inside_parens_does_not_split() {
    assert_eq!(slice_of("arr[@]:$(( 1 ? 2 : 3 )):4").1, Some(4));
  }

  #[test]
  fn a_third_colon_terminates_the_suffix() {
    assert_eq!(slice_of("arr[@]:1:2:3"), (Some(1), Some(2)));
  }

  #[test]
  fn quoted_colon_does_not_split() {
    // `':'` is a literal colon, so the split is the one after it.
    assert_eq!(slice_of("arr[@]:':':1").1, Some(1));
  }

  #[test]
  fn slice_suffix_stops_at_an_operator() {
    // `:-` is the default operator, not a slice: the name ends at `]`.
    assert_eq!(consumed("arr[@]:-fallback"), "arr[@]".len());
  }

  #[test]
  fn slice_bound_stops_at_an_operator() {
    assert_eq!(consumed("arr[@]:1-x"), "arr[@]:1".len());
  }

  #[test]
  fn consumed_covers_the_whole_slice_suffix() {
    assert_eq!(consumed("arr[@]:1:2"), "arr[@]:1:2".len());
  }

  #[test]
  fn slice_is_ignored_for_a_scalar_index() {
    assert_eq!(slice_of("arr[0]:1:2"), (None, None));
  }
}

#[cfg(test)]
mod shell_param_fmt_tests {
  use super::*;

  #[test]
  fn shpid_formats_as_dollar() {
    assert_eq!(ShellParam::ShPid.to_string(), "$");
  }

  #[test]
  fn last_job_formats_as_bang() {
    assert_eq!(ShellParam::LastJob.to_string(), "!");
  }

  #[test]
  fn shell_name_formats_as_zero() {
    assert_eq!(ShellParam::ShellName.to_string(), "0");
  }

  #[test]
  fn pos_formats_as_number() {
    assert_eq!(ShellParam::Pos(1).to_string(), "1");
    assert_eq!(ShellParam::Pos(99).to_string(), "99");
  }

  #[test]
  fn all_args_formats_as_at() {
    assert_eq!(ShellParam::AllArgs.to_string(), "@");
  }

  #[test]
  fn all_args_str_formats_as_star() {
    assert_eq!(ShellParam::AllArgsStr.to_string(), "*");
  }

  #[test]
  fn arg_count_formats_as_hash() {
    assert_eq!(ShellParam::ArgCount.to_string(), "#");
  }

  // ─── round-trip via FromStr ───────────────────────────────────────

  #[test]
  fn every_variant_round_trips_through_from_str() {
    let cases = [
      ShellParam::ShPid,
      ShellParam::LastJob,
      ShellParam::ShellName,
      ShellParam::Pos(7),
      ShellParam::AllArgs,
      ShellParam::AllArgsStr,
      ShellParam::ArgCount,
    ];
    for v in cases {
      let s      = v.to_string();
      let parsed = s.as_bytes().parse_bytes::<ShellParam>().unwrap();
      assert_eq!(parsed, v, "round-trip mismatch for {v:?} via {s:?}");
    }
  }
}

#[cfg(test)]
mod set_index_tests {
  use super::*;
  use crate::tests::testutil::TestGuard;

  fn make_tab_with_arr(name: &str, items: Vec<&str>) -> VarTab {
    let mut tab = VarTab::new();
    tab
      .set_var(
        name.as_bytes(),
        VarKind::Arr(items.into_iter().map(ShStr::from).collect()),
        VarFlags::empty(),
      )
      .unwrap();
    tab
  }

  fn arr_items(tab: &VarTab, name: &str) -> Vec<ShStr> {
    match tab.vars.get(name.as_bytes()).map(super::Var::kind) {
      Some(VarKind::Arr(items)) => items.iter().cloned().collect(),
      other => panic!("expected Arr for {name}, got {other:?}"),
    }
  }

  fn assoc_items(tab: &VarTab, name: &str) -> Vec<(ShStr, ShStr)> {
    match tab.vars.get(name.as_bytes()).map(super::Var::kind) {
      Some(VarKind::AssocArr(items)) => items.clone(),
      other => panic!("expected AssocArr for {name}, got {other:?}"),
    }
  }

  #[test]
  fn literal_index_replaces_existing_slot() {
    let     _g  = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a", "b", "c"]);
    tab
      .set_index(b"arr", ArrIndex::Literal(1), "B!".into())
      .unwrap();
    assert_eq!(arr_items(&tab, "arr"), vec!["a", "B!", "c"]);
  }

  #[test]
  fn literal_index_past_end_resizes_with_empty_strings() {
    let     _g  = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a"]);
    tab
      .set_index(b"arr", ArrIndex::Literal(3), "z".into())
      .unwrap();
    assert_eq!(arr_items(&tab, "arr"), vec!["a", "", "", "z"]);
  }

  #[test]
  fn from_back_index_targets_correct_slot() {
    let     _g  = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a", "b", "c"]);
    // FromBack(1) → items.len() - 1 = index 2.
    tab
      .set_index(b"arr", ArrIndex::FromBack(1), "C!".into())
      .unwrap();
    assert_eq!(arr_items(&tab, "arr"), vec!["a", "b", "C!"]);
  }

  #[test]
  fn from_back_index_out_of_bounds_errors() {
    let     _g  = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a", "b"]);
    let     res = tab.set_index(b"arr", ArrIndex::FromBack(5), "x".into());
    assert!(res.is_err());
  }

  #[test]
  fn assoc_array_new_key_appended() {
    let     _g  = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var(b"h", VarKind::AssocArr(vec![]), VarFlags::empty())
      .unwrap();
    tab
      .set_index(b"h", ArrIndex::Key("k".into()), "v".into())
      .unwrap();
    assert_eq!(assoc_items(&tab, "h"), vec![("k".into(), "v".into())]);
  }

  #[test]
  fn assoc_from_raw_handles_escaped_quote_in_value() {
    // `'\''` (a single quote escaped between single-quoted spans) must not
    // throw off the whitespace-delimited value-boundary scan.
    let kind = VarKind::assoc_arr_from_raw(br"([bar]='biz ba'\''zz buzz' [bam]='foo')").unwrap();
    let VarKind::AssocArr(pairs) = kind else {
      panic!("expected assoc array");
    };
    assert_eq!(
      pairs,
      vec![
        ("bar".into(), "biz ba'zz buzz".into()),
        ("bam".into(), "foo".into())
      ]
    );
  }

  #[test]
  fn assoc_array_existing_key_overwritten_in_place() {
    let     _g  = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var(
        b"h",
        VarKind::AssocArr(vec![("k1".into(), "old".into()), ("k2".into(), "y".into())]),
        VarFlags::empty(),
      )
      .unwrap();
    tab
      .set_index(b"h", ArrIndex::Key("k1".into()), "new".into())
      .unwrap();
    assert_eq!(
      assoc_items(&tab, "h"),
      vec![("k1".into(), "new".into()), ("k2".into(), "y".into())]
    );
  }

  #[test]
  fn set_index_on_scalar_errors() {
    let     _g  = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var(b"scalar", VarKind::Str("plain".into()), VarFlags::empty())
      .unwrap();
    // After resolve_for on a Str, the literal index applies but the
    // catchall arm returns "Variable '...' is not an array".
    let res = tab.set_index(b"scalar", ArrIndex::Literal(0), "x".into());
    assert!(res.is_err());
  }

  #[test]
  fn missing_var_literal_index_creates_array() {
    let     _g  = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_index(b"never_existed", ArrIndex::Literal(0), "x".into())
      .unwrap();
    assert_eq!(arr_items(&tab, "never_existed"), vec!["x"]);
  }

  #[test]
  fn missing_var_sparse_index_pads_with_empties() {
    let     _g  = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_index(b"sparse", ArrIndex::Literal(3), "v".into())
      .unwrap();
    assert_eq!(arr_items(&tab, "sparse"), vec!["", "", "", "v"]);
  }

  #[test]
  fn missing_var_key_index_creates_assoc() {
    let     _g  = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_index(b"fresh", ArrIndex::Key("k".into()), "v".into())
      .unwrap();
    assert_eq!(
      assoc_items(&tab, "fresh"),
      vec![(ShStr::from("k"), ShStr::from("v"))]
    );
  }

  #[test]
  fn missing_var_from_back_index_errors() {
    let     _g  = TestGuard::new();
    let mut tab = VarTab::new();
    let     res = tab.set_index(b"fresh", ArrIndex::FromBack(1), "v".into());
    assert!(
      res.is_err(),
      "counting back from an absent array has no length"
    );
    assert!(!tab.vars.contains_key(b"fresh".as_slice()));
  }

  #[test]
  fn wildcard_index_into_indexed_array_errors() {
    let     _g  = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a", "b"]);
    let     res = tab.set_index(b"arr", ArrIndex::AllSplit, "x".into());
    assert!(res.is_err());
  }

  #[test]
  fn wildcard_index_into_assoc_array_errors() {
    let     _g  = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var(
        b"h",
        VarKind::AssocArr(vec![("k".into(), "v".into())]),
        VarFlags::empty(),
      )
      .unwrap();
    // Wildcard (AllSplit / AllJoined) on assoc array → inner not-a-Key arm errors.
    let res = tab.set_index(b"h", ArrIndex::AllSplit, "x".into());
    assert!(res.is_err());
  }

  #[test]
  fn literal_index_on_assoc_array_becomes_string_key() {
    // `resolve_for` converts Literal(n) → Key(n.to_string()) for assoc
    // arrays. Pin this stringification behavior.
    //
    // Note: as of the issue #93 fix, `set_index` no longer calls
    // `resolve_for` internally — callers must pre-resolve outside of
    // any write borrow on var_scopes (because arithmetic-eval on Raw
    // indices re-enters the var table). This test mirrors that flow:
    // peek at the kind, resolve, then set.
    let     _g  = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var(b"h", VarKind::AssocArr(vec![]), VarFlags::empty())
      .unwrap();
    let tag      = tab.try_get_var_kind_tag(b"h").unwrap();
    let resolved = ArrIndex::Literal(7).resolve_for(tag).unwrap();
    tab.set_index(b"h", resolved, "v".into()).unwrap();
    assert_eq!(assoc_items(&tab, "h"), vec![("7".into(), "v".into())]);
  }
}
