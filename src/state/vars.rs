use std::{
  borrow::Cow,
  collections::VecDeque,
  ffi::OsStr,
  fmt::{self, Display},
  ops::Deref,
  os::unix::ffi::{OsStrExt, OsStringExt},
  path::{Path, PathBuf},
  rc::Rc,
  str::FromStr,
  time::{Duration, Instant},
};

use bitflags::bitflags;
use bstr::ByteSlice;
// Rc-backed (thread-local) HipByt: `VarStr` never crosses a thread boundary on
// the hot path, so we avoid the atomic refcount RMW of the thread-safe backend.
// This is a `!Send` type — the compiler enforces the no-cross-threads invariant;
// the few real thread boundaries do an owned String round-trip instead.
use hipstr::LocalHipByt as HipByt;
use nix::{
  sys::stat,
  unistd::{self, Pid, User},
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
  expand::{Expander, arithmetic, escape, markers, stream::SegStream, var},
  match_loop, procio,
  readline::Candidate,
  sherr,
  state::{Shed, params, paths},
  util::{
    self,
    error::{LabelBuilder, ShResult},
    random,
    strops::{ByteCursor, QuoteState, SliceCursor},
  },
  varstr,
};

use super::{meta::MetaTab, scopes::ScopeStack, terminal::Terminal};

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

impl ValueBytes for VarStr {
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
        let mut out = Vec::new();
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
        let mut out = Vec::new();
        let mut item_iter = items.iter().peekable();
        while let Some((k, v)) = item_iter.next() {
          out.extend_from_slice(&escape::shell_quote_bytes(k.as_bytes()));
          out.push(b'=');
          out.extend_from_slice(&escape::shell_quote_bytes(v.as_bytes()));
          if item_iter.peek().is_some() {
            out.push(b' ');
          }
        }
        out
      }
      VarKind::Magic(func) => func().unwrap_or_default().as_bytes().to_vec(),
      VarKind::Unset => Vec::new(),
    }
  }
}

/// Join lines with `\n` (no trailing newline), byte-native.
fn join_lines(mut lines: Vec<Vec<u8>>) -> Vec<u8> {
  lines.sort();
  let mut out = Vec::new();
  let mut iter = lines.into_iter().peekable();
  while let Some(line) = iter.next() {
    out.extend_from_slice(&line);
    if iter.peek().is_some() {
      out.push(b'\n');
    }
  }
  out
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
  let name = name.as_ref();
  let value = escape::shell_quote_bytes(&value.value_bytes());
  let mut out = Vec::with_capacity(name.len() + value.len() + 1);
  out.extend_from_slice(name);
  out.push(b'=');
  out.extend_from_slice(&value);
  out
}

fn display_vars_internal(vars: &ScopeStack, filter: Option<VarFlags>) -> Vec<u8> {
  let vars = vars.flatten_vars().into_iter();

  if let Some(flags) = filter {
    display_as_vars(vars.filter(|(_, v)| v.flags().contains(flags)))
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
  display_vars_internal(vars, None)
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
  pub fn is_global(&self) -> bool {
    matches!(self, Self::ShPid | Self::LastJob | Self::ShellName)
  }

  pub fn from_char(c: char) -> Option<Self> {
    match c {
      '$' => Some(Self::ShPid),
      '!' => Some(Self::LastJob),
      '0' => Some(Self::ShellName),
      '@' => Some(Self::AllArgs),
      '*' => Some(Self::AllArgsStr),
      '#' => Some(Self::ArgCount),
      _ => None,
    }
  }
}

impl Display for ShellParam {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::ShPid => write!(f, "$"),
      Self::LastJob => write!(f, "!"),
      Self::ShellName => write!(f, "0"),
      Self::Pos(n) => write!(f, "{n}"),
      Self::AllArgs => write!(f, "@"),
      Self::AllArgsStr => write!(f, "*"),
      Self::ArgCount => write!(f, "#"),
    }
  }
}

impl FromStr for ShellParam {
  type Err = ();
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "$" => Ok(Self::ShPid),
      "!" => Ok(Self::LastJob),
      "0" => Ok(Self::ShellName),
      "@" => Ok(Self::AllArgs),
      "*" => Ok(Self::AllArgsStr),
      "#" => Ok(Self::ArgCount),
      n if let Ok(idx) = n.parse::<usize>() => Ok(Self::Pos(idx)),
      _ => Err(()),
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
  }
}

#[derive(Clone, Debug)]
pub(crate) enum ArrIndex {
  Literal(usize),
  FromBack(usize),
  ArgCount,
  AllJoined,
  AllSplit,
  Key(String),

  /// Unresolved index, parsed depending on whether we are targeting an
  /// indexed array or an associative array
  Raw(String),
}

impl ArrIndex {
  /// Parse an array index expression.
  ///
  /// the `allow_side_effects` parameter controls whether or not mutating parameter
  /// expansions and command substitutions will be evaluated.
  pub fn parse(s: &str, allow_side_effects: bool) -> ShResult<Self> {
    let input = SegStream::from_bytes(s.as_bytes());
    let expanded = var::expand_raw_inner(&mut input.cursor(), allow_side_effects, false)?;
    let s = String::from_utf8_lossy(&expanded.into_bytes()).into_owned();
    match s.as_str() {
      "@" => Ok(Self::AllSplit),
      "*" => Ok(Self::AllJoined),
      "#" => Ok(Self::ArgCount),
      _ if s.starts_with('-')
        && !s[1..].is_empty()
        && s[1..].chars().all(|c| c.is_ascii_digit()) =>
      {
        let idx = s[1..].parse::<usize>().unwrap();
        Ok(Self::FromBack(idx))
      }
      _ if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => {
        let idx = s.parse::<usize>().unwrap();
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
  pub fn resolve_for(self, tag: VarKindTag) -> ShResult<Self> {
    match self {
      Self::Raw(s) => match tag {
        VarKindTag::Unset => Err(sherr!(
          ParseErr,
          "Cannot resolve array index '{s}': variable is unset"
        )),
        VarKindTag::AssocArr => Ok(Self::Key(s)),
        VarKindTag::Arr | VarKindTag::Str | VarKindTag::Int | VarKindTag::Magic => {
          let evaluated = arithmetic::expand_arithmetic(s.as_bytes())?;
          let n: usize = evaluated
            .to_str_lossy()
            .parse()
            .map_err(|_| sherr!(ParseErr, "Invalid array index '{s}': not a number"))?;
          Ok(Self::Literal(n))
        }
      },
      Self::Literal(n) if matches!(tag, VarKindTag::AssocArr) => Ok(Self::Key(n.to_string())),
      _ => Ok(self),
    }
  }
}

/// Find the first `:` that isn't nested inside `${}`, `$()`, or `(())`
fn top_level_colon(s: &str) -> Option<usize> {
  let mut brace_depth = 0;
  let mut paren_depth = 0;
  for (i, ch) in s.char_indices() {
    match ch {
      '{' => brace_depth += 1,
      '}' => brace_depth -= 1,
      '(' => paren_depth += 1,
      ')' => paren_depth -= 1,
      ':' if brace_depth == 0 && paren_depth == 0 => return Some(i),
      _ => {}
    }
  }
  None
}

/// A parsed variable name, optionally with an array index and slice.
/// Index expansion happens at construction time, so it's safe
/// to use inside `Shed::vars`/`write_vars` closures without
/// causing re-entrant borrows.
#[derive(Clone, Debug)]
pub(crate) struct VarName {
  name: String,
  index: Option<ArrIndex>,
  slice_start: Option<usize>,
  slice_len: Option<usize>,
}

impl VarName {
  pub fn parse(raw: &str, allow_side_effects: bool) -> ShResult<Self> {
    let Some(bracket_start) = raw.find('[') else {
      return Ok(Self {
        name: raw.to_string(),
        index: None,
        slice_start: None,
        slice_len: None,
      });
    };

    // Find the matching ']' by tracking depth, since the index
    // content may contain nested brackets (e.g. ${arr[${i[0]}]})
    let mut depth = 0;
    let mut bracket_end = None;
    for (i, ch) in raw[bracket_start..].char_indices() {
      match ch {
        '[' => depth += 1,
        ']' => {
          depth -= 1;
          if depth == 0 {
            bracket_end = Some(bracket_start + i);
            break;
          }
        }
        _ => {}
      }
    }

    let Some(bracket_end) = bracket_end else {
      return Ok(Self {
        name: raw.to_string(),
        index: None,
        slice_start: None,
        slice_len: None,
      });
    };

    let name = raw[..bracket_start].to_string();
    let idx_str = &raw[bracket_start + 1..bracket_end];
    let index = ArrIndex::parse(idx_str, allow_side_effects)?;

    // Array slicing only applies to [@] and [*] indexes
    let (slice_start, slice_len) = if matches!(index, ArrIndex::AllSplit | ArrIndex::AllJoined) {
      let after_bracket = &raw[bracket_end + 1..];
      if let Some(rest) = after_bracket.strip_prefix(':') {
        // Split on ':' at the top level only (not inside ${} or $())
        if let Some(split_pos) = top_level_colon(rest) {
          let s = &rest[..split_pos];
          let l = &rest[split_pos + 1..];
          let s_input = SegStream::from_bytes(s.as_bytes());
          let l_input = SegStream::from_bytes(l.as_bytes());
          let s_exp = var::expand_raw(&mut s_input.cursor()).map_or_else(
            |_| s.to_string(),
            |sg| String::from_utf8_lossy(&sg.into_bytes()).into_owned(),
          );
          let l_exp = var::expand_raw(&mut l_input.cursor()).map_or_else(
            |_| l.to_string(),
            |sg| String::from_utf8_lossy(&sg.into_bytes()).into_owned(),
          );
          (s_exp.parse::<usize>().ok(), l_exp.parse::<usize>().ok())
        } else {
          let rest_input = SegStream::from_bytes(rest.as_bytes());
          let expanded = var::expand_raw(&mut rest_input.cursor()).map_or_else(
            |_| rest.to_string(),
            |sg| String::from_utf8_lossy(&sg.into_bytes()).into_owned(),
          );
          (expanded.parse::<usize>().ok(), None)
        }
      } else {
        (None, None)
      }
    } else {
      (None, None)
    };

    Ok(Self {
      name,
      index: Some(index),
      slice_start,
      slice_len,
    })
  }

  pub fn name(&self) -> &str {
    &self.name
  }
  pub fn index(&self) -> Option<&ArrIndex> {
    self.index.as_ref()
  }
  /// Replace the parsed index. Used to substitute a pre-resolved index
  /// (e.g. one whose `expand_arithmetic` was performed outside a borrow
  /// to avoid forking under a held `RefCell` guard).
  pub fn set_index(&mut self, idx: ArrIndex) {
    self.index = Some(idx);
  }
  pub fn slice_start(&self) -> Option<usize> {
    self.slice_start
  }
  pub fn slice_len(&self) -> Option<usize> {
    self.slice_len
  }
}

#[derive(Clone)]
pub(crate) struct MagicVar(Rc<dyn Fn() -> Option<VarStr>>);

impl<F: Fn() -> Option<VarStr> + 'static> From<F> for MagicVar {
  fn from(value: F) -> Self {
    Self(Rc::new(value))
  }
}

impl Deref for MagicVar {
  type Target = dyn Fn() -> Option<VarStr>;
  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl std::fmt::Debug for MagicVar {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "<magic var>")
  }
}

pub trait VarStrSliceExt {
  fn join_with(&self, sep: &str) -> VarStr;
}

impl VarStrSliceExt for [VarStr] {
  fn join_with(&self, sep: &str) -> VarStr {
    let mut out = vec![];
    let mut iter = self.iter();
    if let Some(first) = iter.next() {
      out.extend_from_slice(first.as_bytes());
      for v in iter {
        out.extend_from_slice(sep.as_bytes());
        out.extend_from_slice(v.as_bytes());
      }
    }
    VarStr::from(out)
  }
}

impl VarStrSliceExt for [&VarStr] {
  fn join_with(&self, sep: &str) -> VarStr {
    let mut out = vec![];
    let mut iter = self.iter();
    if let Some(first) = iter.next() {
      out.extend_from_slice(first.as_bytes());
      for v in iter {
        out.extend_from_slice(sep.as_bytes());
        out.extend_from_slice(v.as_bytes());
      }
    }
    VarStr::from(out)
  }
}

/// shed's internal string type. Basically just a very fancy `&[u8]`. Used mainly when you need
/// ownership of a byte slice that doesn't necessarily allocate like `Vec<u8>` would.
///
/// `VarStr` is immutable, unlike [`String`].
///
/// It is a wrapper around [`hipstr::HipByt`], which is itself a shared-ownership byte sequence.
/// It does not have the same UTF-8 guarantees as [`String`] and [`str`], which is necessary to preserve
/// exact byte structure instead of mangling with functions like `to_string_lossy()`.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Default, Hash)]
pub(crate) struct VarStr(HipByt<'static>);

impl VarStr {
  pub fn as_bytes(&self) -> &[u8] {
    &self.0
  }

  pub fn contains_slice(&self, needle: &[u8]) -> bool {
    self.0.windows(needle.len()).any(|window| window == needle)
  }

  /// Check to see if two instances of [`VarStr`] point to the same data.
  ///
  /// For small `VarStr` instances (under 24 bytes), just checks content equality since `HipByt` remains
  /// stack allocated for small buffers.
  pub fn ptr_eq(&self, other: &VarStr) -> bool {
    if self.0.len() >= 24 {
      std::ptr::eq(self.0.as_ptr(), other.0.as_ptr())
    } else {
      self.0 == other.0
    }
  }

  /// Borrowed UTF-8 view, or `None` if the bytes aren't valid UTF-8. Cheap
  /// (validation only, no allocation) — use for value paths that must be text.
  pub fn to_str(&self) -> Option<&str> {
    std::str::from_utf8(&self.0).ok()
  }

  /// Lossy UTF-8 view (invalid bytes → `U+FFFD`). For display and text-only
  /// contexts like identifier lookups, where invalid bytes can't occur anyway.
  pub fn to_str_lossy(&self) -> Cow<'_, str> {
    String::from_utf8_lossy(&self.0)
  }
}

impl Deref for VarStr {
  type Target = [u8];
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl ToSql for VarStr {
  fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
    // Bind valid UTF-8 as TEXT so callers reading columns back as `String`
    // keep working; fall back to BLOB only when the bytes aren't UTF-8.
    match self.to_str() {
      Some(s) => Ok(rusqlite::types::ToSqlOutput::from(s)),
      None => Ok(rusqlite::types::ToSqlOutput::from(self.as_bytes())),
    }
  }
}

impl FromSql for VarStr {
  fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
    match value {
      // `ToSql` binds values as BLOBs (VarStr may hold non-UTF-8 bytes), but
      // accept TEXT too so rows written by older, UTF-8-only builds still read.
      rusqlite::types::ValueRef::Text(s) | rusqlite::types::ValueRef::Blob(s) => {
        Ok(Self(HipByt::from(s)))
      }
      _ => Err(rusqlite::types::FromSqlError::InvalidType),
    }
  }
}

impl From<SmallVec<[u8; 24]>> for VarStr {
  fn from(value: SmallVec<[u8; 24]>) -> Self {
    Self(HipByt::from(value.as_slice()))
  }
}

impl From<VarStr> for PathBuf {
  fn from(value: VarStr) -> Self {
    PathBuf::from(OsStr::from_bytes(value.as_bytes()))
  }
}

impl From<Vec<u8>> for VarStr {
  fn from(value: Vec<u8>) -> Self {
    Self::from(value.as_slice())
  }
}

impl From<&[u8]> for VarStr {
  fn from(value: &[u8]) -> Self {
    Self(HipByt::from(value))
  }
}

impl FromIterator<char> for VarStr {
  fn from_iter<T: IntoIterator<Item = char>>(iter: T) -> Self {
    let mut builder = SmolStrBuilder::new();
    for ch in iter {
      builder.push(ch);
    }
    Self(HipByt::from(builder.finish().as_bytes()))
  }
}

impl From<Var> for VarStr {
  fn from(value: Var) -> Self {
    Self::from(&value)
  }
}

impl From<&Var> for VarStr {
  fn from(value: &Var) -> Self {
    let is_scalar = matches!(value.kind(), VarKind::Str(_) | VarKind::Int(_));
    if is_scalar {
      let Var { kind, .. } = value;
      match kind {
        VarKind::Str(var_str) => var_str.clone(),
        VarKind::Int(n) => (*n).into(),
        _ => unreachable!(),
      }
    } else {
      value.to_string().into()
    }
  }
}

impl From<VarStr> for Vec<u8> {
  fn from(value: VarStr) -> Self {
    value.as_bytes().to_vec()
  }
}

macro_rules! impl_varstr_from {
  ($($t:ty),*) => {
    $(impl From<$t> for VarStr {
      fn from(value: $t) -> VarStr {
        $crate::varstr!("{value}")
      }
    })*
  };
}

impl_varstr_from!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl AsRef<std::ffi::OsStr> for VarStr {
  fn as_ref(&self) -> &std::ffi::OsStr {
    std::ffi::OsStr::from_bytes(self.as_bytes())
  }
}

impl AsRef<Path> for VarStr {
  fn as_ref(&self) -> &Path {
    Path::new(OsStr::from_bytes(self.as_bytes()))
  }
}

impl From<VarStr> for Rc<str> {
  fn from(value: VarStr) -> Self {
    Rc::from(value.to_str_lossy().as_ref())
  }
}

impl PartialEq<str> for VarStr {
  fn eq(&self, other: &str) -> bool {
    self.0 == other.as_bytes()
  }
}

impl PartialEq<&str> for VarStr {
  fn eq(&self, other: &&str) -> bool {
    self.0 == other.as_bytes()
  }
}

impl PartialEq<String> for VarStr {
  fn eq(&self, other: &String) -> bool {
    self.0 == other.as_bytes()
  }
}

impl Display for VarStr {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.0.to_str_lossy().fmt(f)
  }
}

impl From<&mut str> for VarStr {
  fn from(s: &mut str) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}

impl From<&str> for VarStr {
  fn from(s: &str) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}
impl From<String> for VarStr {
  fn from(s: String) -> Self {
    // Owning: reuse the String's allocation instead of copying.
    Self(HipByt::from(s.into_bytes()))
  }
}
impl From<&String> for VarStr {
  fn from(s: &String) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}
impl From<SmolStr> for VarStr {
  fn from(s: SmolStr) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}
impl From<Cow<'_, str>> for VarStr {
  fn from(s: Cow<'_, str>) -> Self {
    Self(HipByt::from(s.as_bytes()))
  }
}

#[derive(Clone, Debug)]
pub(crate) enum VarKind {
  Str(VarStr),
  Int(i32),
  Arr(VecDeque<VarStr>),
  AssocArr(Vec<(VarStr, VarStr)>),

  /// A "magic" variable. Lazily evaluated on access by calling the wrapped function, which can return `None`vars
  /// It wraps an `Rc<dyn Fn() -> Option<String>>`
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
    Self::Str(VarStr::default())
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
  pub fn tag(&self) -> VarKindTag {
    match self {
      Self::Str(_) => VarKindTag::Str,
      Self::Int(_) => VarKindTag::Int,
      Self::Arr(_) => VarKindTag::Arr,
      Self::AssocArr(_) => VarKindTag::AssocArr,
      Self::Magic(_) => VarKindTag::Magic,
      Self::Unset => VarKindTag::Unset,
    }
  }
}

impl VarKind {
  pub fn arr_from_tk(tk: &Tk) -> ShResult<Self> {
    let raw = tk.as_bytes();
    Self::arr_from_raw(raw)
  }

  pub fn arr_from_raw(raw: &[u8]) -> ShResult<Self> {
    if !raw.starts_with(b"(") || !raw.ends_with(b")") {
      return Err(sherr!(
        ParseErr,
        "Invalid array syntax: {}",
        raw.to_str_lossy(),
      ));
    }
    let raw = &raw[1..raw.len() - 1];

    let tokens = LexStream::new(raw, LexFlags::empty())
      .filter(|tk| {
        !tk.as_ref().is_ok_and(|tk| {
          matches!(
            tk.class,
            TkRule::Sep | TkRule::Soi | TkRule::Eoi | TkRule::Comment
          )
        })
      })
      .map(|tk| tk.and_then(|tk| tk.expand()).map(|tk| tk.get_words()))
      .try_fold(Vec::new(), |mut acc, wrds| {
        match wrds {
          Ok(wrds) => acc.extend(wrds.iter().cloned()),
          Err(e) => return Err(e),
        }
        Ok(acc)
      })?;

    Ok(Self::Arr(tokens.into()))
  }

  pub fn parse(raw: &[u8]) -> Self {
    Self::arr_from_raw(raw).unwrap_or_else(|_| Self::Str(raw.into()))
  }

  pub fn string(raw: VarStr) -> Self {
    Self::Str(raw)
  }

  pub fn arr<I: IntoIterator<Item = VarStr>>(iter: I) -> Self {
    let vec: VecDeque<VarStr> = iter.into_iter().collect();
    Self::Arr(vec)
  }

  pub fn assoc_arr<K: Into<VarStr>, V: Into<VarStr>, I: IntoIterator<Item = (K, V)>>(
    iter: I,
  ) -> Self {
    let pairs = iter
      .into_iter()
      .map(|(k, v)| (k.into(), v.into()))
      .collect();
    Self::AssocArr(pairs)
  }

  pub fn assoc_arr_from_raw(raw: &[u8]) -> ShResult<Self> {
    if !raw.starts_with(b"(") || !raw.ends_with(b")") {
      return Err(sherr!(
        ParseErr,
        "Invalid associative array syntax: {}",
        raw.to_str_lossy(),
      ));
    }
    let body = &raw[1..raw.len() - 1];
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
      let mut key = util::scratch_buf();
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
        return Err(sherr!(
          ParseErr,
          "Expected '=' after ']' in associative array element",
        ));
      }

      // Read the value up to top-level whitespace. Quote chars are kept (and
      // tracked only to tell when whitespace is inside them) so the expander
      // still handles the original quoting, e.g. $'...' ANSI-C strings.
      let mut val = util::scratch_buf();
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

impl<K: Into<VarStr>, V: Into<VarStr>> From<Vec<(K, V)>> for VarKind {
  fn from(value: Vec<(K, V)>) -> Self {
    Self::assoc_arr(value)
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
          let key = escape::shell_quote(&k.to_str_lossy());
          let val = escape::shell_quote(&v.to_str_lossy());
          write!(f, "{key}={val}")?;
          if item_iter.peek().is_some() {
            write!(f, " ")?;
          }
        }
        Ok(())
      }
      VarKind::Magic(func) => write!(f, "{}", func().unwrap_or_default()),
      VarKind::Unset => Ok(()),
    }
  }
}

#[derive(Clone, Debug)]
pub(crate) struct Var {
  flags: VarFlags,
  kind: VarKind,
}

impl Default for Var {
  fn default() -> Self {
    Self {
      flags: VarFlags::default(),
      kind: VarKind::Str(VarStr::default()),
    }
  }
}

impl Var {
  pub fn env_var(val: &str) -> Self {
    Self {
      flags: VarFlags::EXPORT,
      kind: VarKind::Str(val.into()),
    }
  }
  pub fn new(kind: VarKind, flags: VarFlags) -> Self {
    Self { flags, kind }
  }
  pub fn kind(&self) -> &VarKind {
    &self.kind
  }
  pub fn kind_mut(&mut self) -> &mut VarKind {
    &mut self.kind
  }
  pub fn into_kind(self) -> VarKind {
    self.kind
  }
  pub fn mark_for_export(&mut self) {
    self.flags.set(VarFlags::EXPORT, true);
  }
  pub fn flags(&self) -> VarFlags {
    self.flags
  }
}

impl Display for Var {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.kind.fmt(f)
  }
}

impl From<Vec<String>> for Var {
  fn from(value: Vec<String>) -> Self {
    let arr = value.into_iter().map(VarStr::from).collect();
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
    new.extend(value.iter().map(VarStr::from));
    Self::new(VarKind::Arr(new), VarFlags::empty())
  }
}

impl From<VarStr> for Var {
  fn from(value: VarStr) -> Self {
    Self::new(VarKind::Str(value), VarFlags::empty())
  }
}

impl From<String> for Var {
  fn from(value: String) -> Self {
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
  // Variable *names* are always valid identifiers (`[A-Za-z_]\w*`), never byte
  // data — so they stay `String`, and `&str` lookups keep working without a
  // `Borrow` shim. Only variable *values* (`Var`) carry arbitrary bytes.
  vars: HashMap<String, Var>,
  params: HashMap<ShellParam, VarStr>,
  sh_argv: VecDeque<VarStr>, /* Using a VecDeque makes the implementation of `shift` straightforward */

  kind: ScopeKind,
  deferred_cmds: Vec<DeferredAst>,
}

impl VarTab {
  pub fn bare() -> Self {
    Self {
      vars: HashMap::default(),
      params: HashMap::default(),
      sh_argv: VecDeque::new(),
      kind: ScopeKind::Block,
      deferred_cmds: Vec::new(),
    }
  }
  pub fn new() -> Self {
    let vars = Self::init_sh_vars();
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
  pub fn set_kind(&mut self, scope_kind: ScopeKind) {
    self.kind = scope_kind;
  }
  pub fn is_ceiling(&self) -> bool {
    matches!(self.kind, ScopeKind::Ceiling)
  }
  pub fn is_function(&self) -> bool {
    matches!(self.kind, ScopeKind::Function)
  }
  fn init_params() -> HashMap<ShellParam, VarStr> {
    let mut params = HashMap::default();
    params.insert(ShellParam::ArgCount, "0".into()); // Number of positional parameters
    params.insert(ShellParam::ShPid, Pid::this().to_string().into()); // PID of the shell
    params.insert(ShellParam::LastJob, VarStr::default()); // PID of the last background job (if any)
    params
  }
  fn init_sh_vars() -> HashMap<String, Var> {
    let mut vars = HashMap::default();
    vars.insert("COMP_WORDBREAKS".into(), " \t\n\"'@><=;|&(:".into());
    vars.insert("OPTIND".into(), "1".into());
    let env_vars = Self::init_env();
    vars.extend(env_vars);
    vars
  }
  fn init_env() -> Vec<(String, Var)> {
    let pathbuf_to_string =
      |pb: Result<PathBuf, std::io::Error>| pb.unwrap_or_default().to_string_lossy().to_string();

    let term = if unistd::isatty(procio::stdin_fileno()).unwrap_or_default() {
      std::env::var("TERM").unwrap_or_else(|_| "linux".to_string())
    } else {
      "xterm-256color".to_string()
    };

    let home_fallback;
    let username_fallback;
    let uid;
    if let Some(user) = User::from_uid(nix::unistd::Uid::current()).ok().flatten() {
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
    let mut env: HashMap<String, VarStr> = std::env::vars_os()
      .filter_map(|(k, v)| Some((k.into_string().ok()?, VarStr::from(v.into_vec()))))
      .collect();

    // Insert-if-missing: prefer the inherited value, fall back otherwise.
    env
      .entry("HOME".into())
      .or_insert_with(|| home_fallback.clone().into());
    env
      .entry("USER".into())
      .or_insert_with(|| username_fallback.clone().into());
    let resolved_home = env["HOME"].to_str_lossy().into_owned();
    let resolved_user = env["USER"].clone();

    let mut state_dir = env.get("XDG_STATE_HOME").map_or_else(
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
      .or_insert_with(|| pathbuf_to_string(std::env::current_exe()).into());
    env
      .entry("SHED_HISTDB".into())
      .or_insert_with(|| shed_db.display().to_string().into());

    // SHED_HPATH: prepend install_dir if it isn't already present.
    if let Some(install_dir) = super::builtin::HELP_PAGE_INSTALL_DIR {
      let new_hpath = match env.get("SHED_HPATH") {
        Some(hpath)
          if !paths::split_path_list(&hpath.to_str_lossy())
            .any(|p| p.as_os_str() == install_dir) =>
        {
          Some(format!("{install_dir}:{}", hpath.to_str_lossy()))
        }
        None => Some(install_dir.to_string()),
        _ => None,
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

    let mut vars: Vec<(String, Var)> = env
      .into_iter()
      .map(|(k, v)| (k, Var::new(VarKind::Str(v), VarFlags::EXPORT)))
      .collect();

    let orig = stat::umask(stat::Mode::empty());
    let umask = stat::umask(orig);
    let mut umask_var = Var::env_var(&format!("{umask:04o}"));
    let underline = Var::new(
      VarKind::Str(VarStr::from(
        std::env::args_os().next().unwrap_or_default().into_vec(),
      )),
      VarFlags::EXPORT,
    );
    umask_var.flags |= VarFlags::READONLY;
    vars.push(("UMASK".to_string(), umask_var));
    vars.push(("_".to_string(), underline));

    vars
  }
  pub fn init_magic_vars(&mut self) {
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
  pub fn init_sh_argv(&mut self) {
    for arg in std::env::args_os() {
      self.bpush_arg(VarStr::from(arg.into_vec()));
    }
  }
  pub fn defer_cmd(&mut self, ast: Ast, ctx: LabelBuilder) {
    self.deferred_cmds.push(DeferredAst { ast, ctx });
  }
  pub fn take_deferred_cmds(&mut self) -> Vec<DeferredAst> {
    std::mem::take(&mut self.deferred_cmds)
  }
  pub fn has_deferred_cmds(&self) -> bool {
    !self.deferred_cmds.is_empty()
  }
  pub fn sh_argv(&self) -> &VecDeque<VarStr> {
    &self.sh_argv
  }
  pub fn sh_argv_mut(&mut self) -> &mut VecDeque<VarStr> {
    &mut self.sh_argv
  }
  pub fn clear_args(&mut self) {
    let first = self.sh_argv.pop_front();
    self.sh_argv.clear();

    // preserve the first arg, which is conventionally the name of the shell, script, or function
    if let Some(arg) = first {
      self.bpush_arg(arg);
    }
  }
  fn update_arg_params(&mut self) {
    // sh_argv[0] is $0 (script/function name); [1..] are positionals.
    // When the deque is entirely empty, treat as zero positionals.
    let positional_count = self.sh_argv.len().saturating_sub(1);
    let positional_join = self
      .sh_argv
      .iter()
      .skip(1)
      .cloned()
      .collect::<Vec<_>>()
      .join_with(&markers::ARG_SEP.to_string());

    self.set_param(ShellParam::AllArgs, &positional_join);
    self.set_param(ShellParam::ArgCount, &positional_count.to_string().into());
  }
  /// Push an arg to the back of the arg deque
  pub fn bpush_arg(&mut self, arg: VarStr) {
    self.sh_argv.push_back(arg);
    self.update_arg_params();
  }
  /// Pop an arg from the front of the arg deque
  pub fn fpop_arg(&mut self) -> Option<VarStr> {
    let arg = self.sh_argv.pop_front();
    self.update_arg_params();
    arg
  }
  pub fn vars(&self) -> &HashMap<String, Var> {
    &self.vars
  }
  pub fn vars_mut(&mut self) -> &mut HashMap<String, Var> {
    &mut self.vars
  }
  /// Clear the export attribute (`export -n`), leaving the variable's value
  /// intact as an ordinary shell variable.
  pub fn unexport_var(&mut self, var_name: &str) {
    if let Some(var) = self.vars.get_mut(var_name)
      && var.flags.contains(VarFlags::EXPORT)
    {
      var.flags.remove(VarFlags::EXPORT);
      Shed::meta_mut(MetaTab::clear_envp);
    }
  }
  pub fn try_get_local(&self, var_name: &str) -> Option<VarStr> {
    // A declared-but-unset variable is present in the scope (so it shadows
    // outer scopes and keeps later assignments local) but has no value: it
    // reads as unset for `${x-}`/`${x+}` and friends.
    match self.vars.get(var_name) {
      Some(var) if matches!(var.kind(), VarKind::Unset) => None,
      other => other.map(VarStr::from),
    }
  }
  pub fn try_get_var_meta(&self, var: &str) -> Option<Var> {
    self.vars.get(var).cloned()
  }
  pub fn try_get_var_kind_tag(&self, var: &str) -> Option<VarKindTag> {
    self.vars.get(var).map(|v| v.kind().tag())
  }
  pub fn get_var_flags(&self, var_name: &str) -> Option<VarFlags> {
    self.vars.get(var_name).map(|var| var.flags)
  }
  pub fn unset_var(&mut self, var_name: &str) -> ShResult<()> {
    if let Some(var) = self.vars.get(var_name) {
      if var.flags.contains(VarFlags::READONLY) {
        return Err(sherr!(
          ExecFail,
          "cannot unset readonly variable '{}'",
          var_name,
        ));
      }
      if var.flags.contains(VarFlags::EXPORT) {
        Shed::meta_mut(MetaTab::clear_envp);
      }
    }
    self.vars.remove(var_name);
    Ok(())
  }
  pub fn set_index(&mut self, var_name: &str, idx: ArrIndex, val: String) -> ShResult<()> {
    // 'idx' must already be resolved at this point
    if self.var_exists(var_name)
      && let Some(var) = self.vars_mut().get_mut(var_name)
    {
      match var.kind_mut() {
        VarKind::Arr(items) => {
          let idx = match idx {
            ArrIndex::Literal(n) => n,
            ArrIndex::FromBack(n) => {
              if items.len() >= n {
                items.len() - n
              } else {
                return Err(sherr!(
                  ExecFail,
                  "Index {} out of bounds for array '{}'",
                  n,
                  var_name,
                ));
              }
            }
            _ => {
              return Err(sherr!(
                ExecFail,
                "Cannot index all elements of array '{}'",
                var_name,
              ));
            }
          };

          if idx >= items.len() {
            items.resize(idx + 1, VarStr::default());
          }
          items[idx] = val.into();
          return Ok(());
        }
        VarKind::AssocArr(items) => {
          // resolve_for guarantees `idx` is `Key(_)` here (or one of the
          // wildcards below, which don't make sense for assignment).
          let ArrIndex::Key(key) = idx else {
            return Err(sherr!(
              ExecFail,
              "Cannot assign to all elements of associative array '{}'",
              var_name,
            ));
          };
          for (k, v) in items.iter_mut() {
            if k == &key {
              *v = val.into();
              return Ok(());
            }
          }
          items.push((key.into(), val.into()));
          return Ok(());
        }
        _ => {
          return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name,));
        }
      }
    }
    Ok(())
  }
  pub fn unset_index(&mut self, var_name: &str, idx: ArrIndex) -> ShResult<()> {
    let Some(var) = self.vars.get_mut(var_name) else {
      return Ok(());
    };
    if var.flags.contains(VarFlags::READONLY) {
      return Err(sherr!(
        ExecFail,
        "cannot unset readonly variable '{}'",
        var_name
      ));
    }
    match var.kind_mut() {
      VarKind::Arr(items) => {
        let i = match idx {
          ArrIndex::Literal(n) => n,
          ArrIndex::FromBack(n) if items.len() >= n => items.len() - n,
          ArrIndex::FromBack(_) => return Ok(()),
          _ => {
            return Err(sherr!(
              ExecFail,
              "Cannot unset all elements of array '{}'",
              var_name,
            ));
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
            "Cannot unset all elements of associative array '{}'",
            var_name,
          ));
        };
        items.retain(|(k, _)| k != &key);
        Ok(())
      }
      _ => Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name)),
    }
  }
  pub fn set_var(&mut self, var_name: &str, val: VarKind, flags: VarFlags) -> ShResult<()> {
    if let Some(var) = self.vars.get_mut(var_name) {
      if var.flags.contains(VarFlags::READONLY) && !flags.contains(VarFlags::READONLY) {
        return Err(sherr!(ExecFail, "Variable '{}' is readonly", var_name,));
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
      self.vars.insert(var_name.to_string(), var);
    }
    Ok(())
  }
  /// OR additional attribute flags onto an existing variable, leaving its value
  /// untouched. Used when re-declaring an already-set name (`x=5; declare x`),
  /// which adds attributes without clobbering the value.
  pub fn merge_flags(&mut self, var_name: &str, flags: VarFlags) {
    if let Some(var) = self.vars.get_mut(var_name) {
      let newly_exported =
        flags.contains(VarFlags::EXPORT) && !var.flags.contains(VarFlags::EXPORT);
      var.flags |= flags;
      if newly_exported {
        Shed::meta_mut(MetaTab::clear_envp);
      }
    }
  }
  pub fn var_exists(&self, var_name: &str) -> bool {
    if let Ok(param) = var_name.parse::<ShellParam>() {
      return self.params.contains_key(&param);
    }
    self.vars.contains_key(var_name)
  }
  pub fn set_param(&mut self, param: ShellParam, val: &VarStr) {
    self.params.insert(param, val.clone());
  }
  pub fn try_get_param(&self, param: ShellParam) -> Option<VarStr> {
    match param {
      ShellParam::Pos(n) => self.sh_argv().get(n).cloned(),
      ShellParam::AllArgsStr => {
        // FIXME: re-entrancy???
        let ifs = params::get_separator();
        self.params.get(&ShellParam::AllArgs).map(|s| {
          let mut buf = [0u8; 4];
          s.replace(markers::ARG_SEP.encode_utf8(&mut buf), ifs.as_bytes())
            .into()
        })
      }

      _ => self.params.get(&param).cloned(),
    }
  }
}

// magic variable functions

fn get_status_str() -> Option<VarStr> {
  let status = Shed::get_status();
  Some(varstr!("{status}"))
}
fn get_seconds() -> Option<VarStr> {
  let shell_time = Shed::meta(MetaTab::shell_time);
  let secs = Instant::now().duration_since(shell_time).as_secs();
  Some(varstr!("{secs}"))
}
fn get_epoch_realtime() -> Option<VarStr> {
  let epoch = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or(Duration::from_secs(0))
    .as_secs_f64();
  Some(varstr!("{epoch:.6}"))
}

fn get_epoch_seconds() -> Option<VarStr> {
  let epoch = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or(Duration::from_secs(0))
    .as_secs();
  Some(varstr!("{epoch}"))
}

fn get_random() -> Option<VarStr> {
  let random = random::random_range(0..32768);
  Some(varstr!("{random}"))
}

fn get_lines() -> Option<VarStr> {
  let rows = Shed::term(Terminal::t_rows);
  Some(varstr!("{rows}"))
}

fn get_columns() -> Option<VarStr> {
  let cols = Shed::term(Terminal::t_cols);
  Some(varstr!("{cols}"))
}

fn get_set_flags() -> Option<VarStr> {
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
mod top_level_colon_tests {
  use super::top_level_colon;

  #[test]
  fn simple_colon_at_position() {
    assert_eq!(top_level_colon("foo:bar"), Some(3));
  }

  #[test]
  fn no_colon_returns_none() {
    assert_eq!(top_level_colon("foobar"), None);
    assert_eq!(top_level_colon(""), None);
  }

  #[test]
  fn colon_at_start() {
    assert_eq!(top_level_colon(":foo"), Some(0));
  }

  #[test]
  fn colon_at_end() {
    assert_eq!(top_level_colon("foo:"), Some(3));
  }

  #[test]
  fn colon_inside_braces_skipped() {
    // `${foo:bar}` — the `:` is nested inside braces.
    assert_eq!(top_level_colon("${foo:bar}"), None);
  }

  #[test]
  fn colon_inside_parens_skipped() {
    // `$(cmd:arg)` — the `:` is nested inside parens.
    assert_eq!(top_level_colon("$(cmd:arg)"), None);
  }

  #[test]
  fn outer_colon_found_when_inner_nested() {
    // Outer colon at index 1, inner colon nested inside `${}`.
    assert_eq!(top_level_colon("x:${foo:bar}"), Some(1));
  }

  #[test]
  fn first_colon_wins_when_multiple_top_level() {
    assert_eq!(top_level_colon("a:b:c"), Some(1));
  }

  #[test]
  fn nested_braces_keep_inner_colon_hidden() {
    assert_eq!(top_level_colon("{{a:b}c}"), None);
  }

  #[test]
  fn colon_after_closing_brace() {
    // `${x}:y` — after `}` we're back to depth 0, the `:` is top-level.
    assert_eq!(top_level_colon("${x}:y"), Some(4));
  }

  #[test]
  fn outer_then_inner_colons() {
    // First colon at index 1 (top-level), second at 7 (inside parens).
    assert_eq!(top_level_colon("a:$(cmd:arg):b"), Some(1));
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
    use std::str::FromStr;
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
      let s = v.to_string();
      let parsed = ShellParam::from_str(&s).unwrap();
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
        name,
        VarKind::Arr(items.into_iter().map(VarStr::from).collect()),
        VarFlags::empty(),
      )
      .unwrap();
    tab
  }

  fn arr_items(tab: &VarTab, name: &str) -> Vec<VarStr> {
    match tab.vars.get(name).map(super::Var::kind) {
      Some(VarKind::Arr(items)) => items.iter().cloned().collect(),
      other => panic!("expected Arr for {name}, got {other:?}"),
    }
  }

  fn assoc_items(tab: &VarTab, name: &str) -> Vec<(VarStr, VarStr)> {
    match tab.vars.get(name).map(super::Var::kind) {
      Some(VarKind::AssocArr(items)) => items.clone(),
      other => panic!("expected AssocArr for {name}, got {other:?}"),
    }
  }

  #[test]
  fn literal_index_replaces_existing_slot() {
    let _g = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a", "b", "c"]);
    tab
      .set_index("arr", ArrIndex::Literal(1), "B!".into())
      .unwrap();
    assert_eq!(arr_items(&tab, "arr"), vec!["a", "B!", "c"]);
  }

  #[test]
  fn literal_index_past_end_resizes_with_empty_strings() {
    let _g = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a"]);
    tab
      .set_index("arr", ArrIndex::Literal(3), "z".into())
      .unwrap();
    assert_eq!(arr_items(&tab, "arr"), vec!["a", "", "", "z"]);
  }

  #[test]
  fn from_back_index_targets_correct_slot() {
    let _g = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a", "b", "c"]);
    // FromBack(1) → items.len() - 1 = index 2.
    tab
      .set_index("arr", ArrIndex::FromBack(1), "C!".into())
      .unwrap();
    assert_eq!(arr_items(&tab, "arr"), vec!["a", "b", "C!"]);
  }

  #[test]
  fn from_back_index_out_of_bounds_errors() {
    let _g = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a", "b"]);
    let res = tab.set_index("arr", ArrIndex::FromBack(5), "x".into());
    assert!(res.is_err());
  }

  #[test]
  fn assoc_array_new_key_appended() {
    let _g = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var("h", VarKind::AssocArr(vec![]), VarFlags::empty())
      .unwrap();
    tab
      .set_index("h", ArrIndex::Key("k".into()), "v".into())
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
    let _g = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var(
        "h",
        VarKind::AssocArr(vec![("k1".into(), "old".into()), ("k2".into(), "y".into())]),
        VarFlags::empty(),
      )
      .unwrap();
    tab
      .set_index("h", ArrIndex::Key("k1".into()), "new".into())
      .unwrap();
    assert_eq!(
      assoc_items(&tab, "h"),
      vec![("k1".into(), "new".into()), ("k2".into(), "y".into())]
    );
  }

  #[test]
  fn set_index_on_scalar_errors() {
    let _g = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var("scalar", VarKind::Str("plain".into()), VarFlags::empty())
      .unwrap();
    // After resolve_for on a Str, the literal index applies but the
    // catchall arm returns "Variable '...' is not an array".
    let res = tab.set_index("scalar", ArrIndex::Literal(0), "x".into());
    assert!(res.is_err());
  }

  #[test]
  fn missing_var_is_silent_ok() {
    // var_exists guard at the top is false → function returns Ok
    // without creating anything.
    let _g = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_index("never_existed", ArrIndex::Literal(0), "x".into())
      .unwrap();
    assert!(!tab.vars.contains_key("never_existed"));
  }

  #[test]
  fn wildcard_index_into_indexed_array_errors() {
    let _g = TestGuard::new();
    let mut tab = make_tab_with_arr("arr", vec!["a", "b"]);
    let res = tab.set_index("arr", ArrIndex::AllSplit, "x".into());
    assert!(res.is_err());
  }

  #[test]
  fn wildcard_index_into_assoc_array_errors() {
    let _g = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var(
        "h",
        VarKind::AssocArr(vec![("k".into(), "v".into())]),
        VarFlags::empty(),
      )
      .unwrap();
    // Wildcard (AllSplit / AllJoined) on assoc array → inner not-a-Key arm errors.
    let res = tab.set_index("h", ArrIndex::AllSplit, "x".into());
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
    let _g = TestGuard::new();
    let mut tab = VarTab::new();
    tab
      .set_var("h", VarKind::AssocArr(vec![]), VarFlags::empty())
      .unwrap();
    let tag = tab.try_get_var_kind_tag("h").unwrap();
    let resolved = ArrIndex::Literal(7).resolve_for(tag).unwrap();
    tab.set_index("h", resolved, "v".into()).unwrap();
    assert_eq!(assoc_items(&tab, "h"), vec![("7".into(), "v".into())]);
  }
}
