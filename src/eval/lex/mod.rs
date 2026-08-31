//! This module contains `shed`'s lexer: [`LexStream`]
//! `LexStream` operates by taking a slice of bytes, and then iterating over them one byte at a time.
//! As it iterates over the bytes, it slices them into separate tokens which are returned through the `next()` method.
//!
//! `LexStream` implements `Iterator`, so lexing is a lazy operation.

use std::{
  borrow::Cow,
  cmp::Ordering,
  collections::VecDeque,
  fmt::Display,
  ops::{Bound, Index, Range, RangeBounds, RangeFrom, RangeTo, RangeToInclusive},
  rc::Rc,
};

use bitflags::bitflags;
use bstr::ByteSlice;

use crate::{
  assert_sorted,
  builtin::BUILTIN_NAMES,
  match_loop, sherr,
  state::{
    Shed,
    vars::{VarStr, VarStrSliceExt},
  },
  util::{
    self,
    error::ShResult,
    pos::Pos,
    strops::{self, ByteCursor, QuoteState, SliceCursor},
  },
};

pub(crate) const KEYWORDS: [&[u8]; 21] = [
  b"!",
  b"case",
  b"catch",
  b"defer",
  b"do",
  b"done",
  b"elif",
  b"else",
  b"esac",
  b"fi",
  b"for",
  b"function",
  b"if",
  b"in",
  b"not",
  b"select",
  b"then",
  b"time",
  b"try",
  b"until",
  b"while",
];

assert_sorted!(KEYWORDS);

pub(crate) const MIDDLES: [&str; 3] = ["elif", "else", "catch"];

pub(crate) const CLOSERS: [&str; 6] = ["fi", "done", "esac", "}", ")", ";;"];

pub(crate) trait TkVecUtils<Tk> {
  fn get_span(&self) -> Option<Span>;
}

impl TkVecUtils<Tk> for &[Tk] {
  fn get_span(&self) -> Option<Span> {
    if let Some(first_tk) = self.first() {
      self.last().map(|last_tk| {
        Span::new(
          first_tk.span.range().start..last_tk.span.range().end,
          first_tk.source(),
        )
      })
    } else {
      None
    }
  }
}

impl TkVecUtils<Tk> for Vec<Tk> {
  fn get_span(&self) -> Option<Span> {
    self.as_slice().get_span()
  }
}

/// Constructs a parse error and commits cursor position for the lexer
///
/// All error returns from `LexStream` ***MUST*** advance the cursor past
/// the offending input, otherwise the caller will backtrack and read the bad input again.
/// This causes an infinite loop. This macro enforces that invariant structurally,
/// if you can't pass a new cursor position, you can't build an error.
///
/// In cases where the error occurs at the very end of input, `LexFlags::STALE` is used instead.
macro_rules! lex_err {
	($lexer:expr, $range: expr, $($arg:tt)*) => {{
		sherr!(ParseErr @ $lexer.get_span($range), $($arg)*)
	}}
}

#[derive(Clone, PartialEq, Default, Debug, Eq, Hash)]
pub(crate) struct SpanSource {
  name: VarStr,
  content: VarStr,
}

thread_local! {
  /// Cached default source name, so `Span::new`
  /// doesn't re-allocate `"<stdin>"` on every call.
  static STDIN_NAME: VarStr = VarStr::from("<stdin>");
}

fn stdin_name() -> VarStr {
  STDIN_NAME.with(VarStr::clone)
}

impl SpanSource {
  pub(crate) fn new(name: VarStr, content: VarStr) -> Self {
    Self { name, content }
  }
  pub(crate) fn name(&self) -> VarStr {
    self.name.clone()
  }
  pub(crate) fn content(&self) -> VarStr {
    self.content.clone()
  }
  pub(crate) fn len(&self) -> usize {
    self.content.len()
  }
}

impl Index<usize> for SpanSource {
  type Output = u8;

  fn index(&self, index: usize) -> &Self::Output {
    &self.content[index]
  }
}

impl Index<RangeTo<usize>> for SpanSource {
  type Output = [u8];

  fn index(&self, index: RangeTo<usize>) -> &Self::Output {
    &self.content[index]
  }
}

impl Index<RangeToInclusive<usize>> for SpanSource {
  type Output = [u8];

  fn index(&self, index: RangeToInclusive<usize>) -> &Self::Output {
    &self.content[index]
  }
}

impl Index<RangeFrom<usize>> for SpanSource {
  type Output = [u8];

  fn index(&self, index: RangeFrom<usize>) -> &Self::Output {
    &self.content[index]
  }
}

impl Index<Range<usize>> for SpanSource {
  type Output = [u8];

  fn index(&self, index: Range<usize>) -> &Self::Output {
    &self.content[index]
  }
}

impl Display for SpanSource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    use yansi::Paint;
    write!(f, "{}", self.name.cyan().bold().underline())
  }
}

/// A slice of some source text. Ultimately wraps a [`crate::state::vars::VarStr`], which means these are cheap to clone.
///
/// Load-bearing struct. Used extensively throughout the codebase for slicing shell input for various reasons (error reporting, tab completion, etc)
#[derive(Clone, PartialEq, Default, Debug)]
pub(crate) struct Span {
  range: Range<usize>,
  pos: Pos,
  source: SpanSource,
}

impl Span {
  /// New `Span`. Wraps a range and a string that it refers to.
  pub(crate) fn new(range: Range<usize>, content: VarStr) -> Self {
    Span {
      range,
      pos: Pos::MIN,
      source: SpanSource {
        name: stdin_name(),
        content,
      },
    }
  }
  /// Like `new`, but reuses an already-built, shared `Rc<SpanSource>` — no
  /// allocation and no per-token rename.
  pub(crate) fn with_source(range: Range<usize>, source: SpanSource) -> Self {
    Span {
      range,
      pos: Pos::MIN,
      source,
    }
  }
  pub(crate) fn merge_inplace(&mut self, other: &Span) {
    if !VarStr::ptr_eq(&self.source.content, &other.source.content) {
      return;
    }

    if other.range.start < self.range.start {
      self.pos = other.pos;
    }
    self.range.start = self.range.start.min(other.range.start);
    self.range.end = self.range.end.max(other.range.end);
  }
  pub(crate) fn merge_with(mut self, other: &Span) -> Option<Self> {
    // make sure these two spans originate from the same input. See
    // `merge_inplace` for why the `ptr_eq` fast path needs a value fallback.
    if !VarStr::ptr_eq(&self.source.content, &other.source.content)
      && self.source.content != other.source.content
    {
      return None;
    }

    if other.range.start < self.range.start {
      self.pos = other.pos;
    }
    self.range.start = self.range.start.min(other.range.start);
    self.range.end = self.range.end.max(other.range.end);
    Some(self)
  }
  pub(crate) fn at(mut self, pos: Pos) -> Self {
    self.pos = pos;
    self
  }
  pub(crate) fn rename(&mut self, name: VarStr) {
    // Fork this span's shared source (copy-on-write) so renaming it — e.g. to
    // attribute a function body to its name for error blame — doesn't rename
    // every other span sharing the source.
    self.source.name = name;
  }
  pub(crate) fn line_and_col(&self) -> (usize, usize) {
    (self.pos.row, self.pos.col)
  }
  /// Slice the source string at the wrapped range
  pub(crate) fn to_str_lossy(&self) -> Cow<'_, str> {
    self.as_bytes().to_str_lossy()
  }
  pub(crate) fn as_var_str(&self) -> VarStr {
    self.as_bytes().into()
  }
  pub(crate) fn as_bytes(&self) -> &[u8] {
    &self.source.content[self.range().start..self.range().end]
  }
  pub(crate) fn bytes(&self) -> impl Iterator<Item = u8> + '_ {
    self.source.content[self.range().start..self.range().end]
      .iter()
      .copied()
  }
  pub(crate) fn get_source(&self) -> VarStr {
    self.source.content.clone()
  }
  pub(crate) fn span_source(&self) -> &SpanSource {
    &self.source
  }
  pub(crate) fn range(&self) -> Range<usize> {
    self.range.clone()
  }
  /// With great power comes great responsibility
  /// Only use this in the most dire of circumstances
  pub(crate) fn set_range(&mut self, range: Range<usize>) {
    self.range = range;
  }

  pub(crate) fn shift_by(&mut self, delta: isize) {
    let new_start = self.range.start as isize + delta;
    let new_end = self.range.end as isize + delta;
    debug_assert!(new_start >= 0 && new_end >= 0, "shift_by underflow");
    self.range = (new_start as usize)..(new_end as usize);
  }

  pub(crate) fn rebase_into(&mut self, outer_span: &Span, offset: usize) {
    self.range = (self.range.start + offset)..(self.range.end + offset);
    self.source = outer_span.source.clone();
  }
}

impl PartialOrd for Span {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    use ariadne::Span as ASpan;
    if self.get_source() != other.get_source() {
      return None;
    }
    Some((self.start(), self.end()).cmp(&(other.start(), other.end())))
  }
}

impl ariadne::Span for Span {
  type SourceId = SpanSource;

  fn source(&self) -> &Self::SourceId {
    &self.source
  }

  fn start(&self) -> usize {
    let max = self.source.content.len();
    self.range.start.min(max).min(self.range.end)
  }

  fn end(&self) -> usize {
    let max = self.source.content.len();
    self.range.end.min(max)
  }
}

#[derive(Clone, PartialEq, Debug)]
/// The "class" of a token, i.e. what kind of token it is. This is the result of lexing, and is used during parsing to determine how to interpret the token.
pub(crate) enum TkRule {
  /// A normal string token. By far the most common type of token. Used for command names, keywords, arguments, basically any "words".
  /// String tokens are further disambiguated using the `TkFlags` on the token itself, which can mark a string token as a keyword, a command name, a subshell, etc.
  Str,

  /// The start of a given input.
  Soi,
  /// The end of a given input.
  Eoi,

  Null,
  Pipe,
  ErrPipe,
  And,
  Or,
  Bang,
  Bg,
  Sep,
  Redir,
  BraceGrpStart,
  BraceGrpEnd,
  SubshStart,
  SubshEnd,
  Comment,
  HereDoc {
    start_delim: Box<Span>,
    end_delim: Option<Box<Span>>, // is None if not found when lexing unfinished input
  },

  /// These are only used as an intermediate state for tokens that are in the process of being expanded.
  /// You can be confident that any token you are working on does not have this rule.
  Expanded {
    exp: Rc<[VarStr]>,
  },
}

impl Default for TkRule {
  fn default() -> Self {
    TkRule::Null
  }
}

/// A single input token. Wraps three things:
/// * A [`TkRule`] which identifies what kind of token it is
/// * A [`Span`] which represents the slice of the original input the token refers to
/// * [`TkFlags`] which is a bitfield containing simple metadata
///
/// Generally speaking, these are very cheap to clone. All of the data held in a `Tk` is either
/// trivially `Copy` (like `TkFlags`), or shared-ownership (like `Span` and `TkRule::Expanded`).
///
/// `TkRule::Expanded` is only created during token expansion, which generally happens much later in an execution cycle.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Tk {
  pub class: TkRule,
  pub span: Span,
  pub flags: TkFlags,
}

impl Tk {
  pub(crate) fn new(class: TkRule, span: Span) -> Self {
    Self {
      class,
      span,
      flags: TkFlags::empty(),
    }
  }
  /// Returns a new string with the token's span replaced by the given string.
  pub(crate) fn replaced(&self, other: &str) -> String {
    let mut content = self.span.source.content().to_string();
    content.replace_range(self.span.range(), other);
    content
  }
  /// Returns true if the token is a literal string, i.e. it does not contain any special characters that would require quoting or escaping.
  pub(crate) fn is_literal(&self) -> bool {
    self.filter_meta()
      && self
        .span
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
  }
  pub(crate) fn as_bytes(&self) -> &[u8] {
    self.span.as_bytes()
  }
  pub(crate) fn to_str_lossy(&self) -> Cow<'_, str> {
    self.span.to_str_lossy()
  }
  /// The token's effective text as a `VarStr`: the joined expansion for an
  /// expanded token, or the raw span otherwise. Mirrors `Display` without
  /// routing through the formatter.
  ///
  /// Cheap for small tokens, but may allocate for large expanded tokens.
  pub(crate) fn word(&self) -> VarStr {
    match &self.class {
      TkRule::Expanded { exp } => exp.join_with(" "),
      _ => self.span.as_bytes().into(),
    }
  }
  pub(crate) fn source(&self) -> VarStr {
    self.span.source.content.clone()
  }
  pub(crate) fn mark(&mut self, flag: TkFlags) {
    self.flags |= flag;
  }
  /// Used to see if a separator is ';;' for case statements
  pub(crate) fn has_double_semi(&self) -> bool {
    let TkRule::Sep = self.class else {
      return false;
    };
    self.span.as_bytes().trim() == b";;"
  }

  /// Returns false for tokens that are not part of the actual input, like `TkRule::Soi`, `TkRule::Eoi`, and `TkRule::Null`.
  pub(crate) fn filter_meta(&self) -> bool {
    !matches!(self.class, TkRule::Soi | TkRule::Eoi | TkRule::Null)
  }

  /// used when lexing recursively, to replace the token's span with the original source
  pub(crate) fn rebase_into(mut self, outer_span: &Span, offset: usize) -> Self {
    self.span.rebase_into(outer_span, offset);
    self
  }

  /// Strip the leading/trailing parenthesis of an arithmetic statement
  ///
  /// returns a new `Tk` instead of mutating in-place. Altering spans directly
  /// feels like a potential footgun.
  pub(crate) fn strip_arith_header(&self) -> ShResult<Self> {
    let s = self.as_bytes();
    let trimmed = s.trim();

    if trimmed.len() < 4 || !trimmed.starts_with(b"((") || !trimmed.ends_with(b"))") {
      return Err(sherr!(ParseErr @ self.span.clone(), "malformed arithmetic for-loop header"));
    }

    let base = self.span.range.start;
    let start = base + (s.len() - s.trim_start().len()) + 2;
    let end = base + (s.trim_end().len()) - 2;

    Ok(Self::new(
      self.class.clone(),
      Span::new(start..end, self.source()),
    ))
  }
}

bitflags! {
  /// A bitfield of flags that can be set on a token.
  /// These are used to disambiguate string tokens, which can be keywords, command names, subshells, etc.
  #[derive(Debug,Clone,Copy,PartialEq,Default)]
  pub struct TkFlags: u32 {
    const KEYWORD      = 0b0000_0000_0000_0001;
    const OPENER       = 0b0000_0000_0000_0010;
    const IS_CMD       = 0b0000_0000_0000_0100;
    const IS_SUBSH     = 0b0000_0000_0000_1000;
    const IS_OP        = 0b0000_0000_0010_0000;
    const ASSIGN       = 0b0000_0000_0100_0000;
    const BUILTIN      = 0b0000_0000_1000_0000;
    const IS_PROCSUB   = 0b0000_0001_0000_0000;
    const IS_HEREDOC   = 0b0000_0010_0000_0000;
    const LIT_HEREDOC  = 0b0000_0100_0000_0000;
    const TAB_HEREDOC  = 0b0000_1000_0000_0000;
    const IS_ARITH     = 0b0001_0000_0000_0000;
    const FUNCNAME		 = 0b0010_0000_0000_0000;
    const REDIR_ALL		 = 0b0100_0000_0000_0000;
    const HERESTRING	 = 0b1000_0000_0000_0000;
  }
}

bitflags! {
  /// A bitfield of flags that can be set on the lexer itself.
  #[derive(Debug, Clone, Default, PartialEq, Copy)]
  pub struct LexFlags: u32 {
    /// The lexer is operating in interactive mode
    const INTERACTIVE    = 1 << 0;
    /// Allow unfinished input
    const LEX_UNFINISHED_STRUCTURES = 1 << 1;
    const LEX_UNFINISHED_QUOTES   = 1 << 2;
    /// The next string-type token is a command name
    const NEXT_IS_CMD    = 1 << 3;
    /// Only lex strings; used in expansions
    /// The lexer has not produced any tokens yet
    const FRESH          = 1 << 5;
    /// The lexer has no more tokens to produce
    const STALE          = 1 << 6;
    const EXPECTING_IN   = 1 << 7;
    const NEXT_IS_REDIR  = 1 << 8;
    const NEXT_IS_FUNC   = 1 << 9;
    /// Set alongside EXPECTING_IN when a `case` keyword is lexed; consumed
    const EXPECTING_CASE_IN = 1 << 10;
    /// Expecting a closing ')' in a case statement pattern
    const CASE_PAT_EXPECTED = 1 << 11;

    const LEX_UNFINISHED = Self::LEX_UNFINISHED_STRUCTURES.bits() | Self::LEX_UNFINISHED_QUOTES.bits();
  }
}

/// Cleans the input string by removing line continuations.
///
/// Honestly kind of a hack, but it works.
pub(crate) fn clean_input(input: &[u8]) -> VarStr {
  // PERF: Profile this function to see if it has any
  // meaningful impact on lex speed.
  let mut bytes = SliceCursor::new(input);
  let mut output = vec![];
  let mut quotes = QuoteState::default();
  let mut in_comment = false;
  // FIFO queue: heredocs on the same line are consumed in order
  let mut heredoc_queue: VecDeque<VarStr> = VecDeque::new();
  match_loop!(bytes.next_byte() => b, {
    _ if in_comment && b != b'\n' && b != b'\r' => output.push(b),
    b'\'' => {
      quotes.toggle_single();
      output.push(b);
    }
    b'"' => {
      quotes.toggle_double();
      output.push(b);
    }
    b'#' if quotes.outside()
      && matches!(output.bytes().next_back(), None | Some(b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')')) =>
    {
      in_comment = true;
      output.push(b);
    }
    b'\\' if !quotes.in_single() && matches!(bytes.peek_byte(), Some(b'\n' | b'\r')) => {
      // line continuation
      let nl = bytes.next_byte();
      if nl == Some(b'\r') {
        bytes.bump_if(|b| b == b'\n');
      }
    }
    b'\r' => {
      in_comment = false;
      bytes.bump_if(|b| b == b'\n');
      output.push(b'\n');
    }
    b'\n' if let Some(delim) = heredoc_queue.pop_front() => {
      in_comment = false;
      output.push(b'\n');
      let tab_strip = delim.starts_with(b"-");
      let match_delim = delim.trim_start_with(|b| b == '-');
      let start = bytes.pos();
      let mut end = start;
      for line in input[start..].split(|b| *b == b'\n') {
        output.extend_from_slice(line);
        output.push(b'\n');
        end += line.len() + 1;
        let line_to_match = if tab_strip {
          line.trim_start_with(|b| b == '\t')
        } else {
          line
        };
        if line_to_match == match_delim {
          // Advance the cursor past all the body bytes we just copied
          while bytes.pos() < end {
            if bytes.next_byte().is_none() {
              break;
            }
          }
          break;
        }
      }
    }
    b'\n' => {
      in_comment = false;
      output.push(b'\n');
    }
    b'<' if quotes.outside() && bytes.peek_byte() == Some(b'<') => {
      output.push(b); // first '<'
      output.push(bytes.next_byte().unwrap()); // second '<'

      // <<< is a here-string — no multi-line body, don't push to queue
      if let Some(third) = bytes.next_byte_if(|b| b == b'<') {
        output.push(third);
      } else {
        // Skip optional '-' for <<-
        let tab_strip = bytes.peek_byte() == Some(b'-');

        // Skip horizontal whitespace between << and delimiter
        while let Some(wc) = bytes.next_byte_if(|b| b == b' ' || b == b'\t') {
          output.push(wc);
        }

        // Collect delimiter word, stripping quotes for the match key
        let mut delim = util::scratch_buf();
        if tab_strip {
          delim.push(b'-');
        }
        let mut in_dquote = false;
        let mut in_squote_inner = false;
        while let Some(c) = bytes.peek_byte() {
          match c {
            b'\'' if !in_dquote => in_squote_inner = !in_squote_inner,
            b'"' if !in_squote_inner => in_dquote = !in_dquote,
            c if (c.is_ascii_whitespace()
              || matches!(c, b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>'))
              && !in_dquote
              && !in_squote_inner =>
            {
              break
            }
            _ => {}
          }
          // Add to match key only if it's not a quote character
          if c != b'\'' && c != b'"' {
            delim.push(c);
          }
          output.push(c);
          bytes.next_byte();
        }
        if !delim.trim_start_with(|b| b == '-').is_empty() {
          heredoc_queue.push_back(delim.as_slice().into());
        }
      }
    }
    _ => output.push(b),
  });
  output.into()
}

/// The state of the lexer at a given point in time.
///
/// This is used to save and restore the state of the lexer when a transactional operation fails.
#[derive(Clone, Debug, PartialEq, Default, Copy)]
struct LexState {
  pub cursor: usize,
  pos_offset: usize,
  pos: Pos,
  quote_state: QuoteState,
  brc_grp_depth: usize,
  brc_grp_start: Option<usize>,
  subsh_depth: usize,
  subsh_start: Option<usize>,
  case_depth: usize,
  heredoc_skip: Option<usize>,
  flags: LexFlags,
}

impl LexState {
  fn load_into(self, stream: &mut LexStream) {
    stream.cursor = self.cursor;
    stream.pos_offset = self.pos_offset;
    stream.pos = self.pos;
    stream.quote_state = self.quote_state;
    stream.brc_grp_depth = self.brc_grp_depth;
    stream.brc_grp_start = self.brc_grp_start;
    stream.subsh_depth = self.subsh_depth;
    stream.subsh_start = self.subsh_start;
    stream.case_depth = self.case_depth;
    stream.heredoc_skip = self.heredoc_skip;
    stream.flags = self.flags;
  }
}

impl From<&LexStream> for LexState {
  fn from(lexer: &LexStream) -> Self {
    Self {
      cursor: lexer.cursor,
      pos_offset: lexer.pos_offset,
      pos: lexer.pos,
      quote_state: lexer.quote_state,
      brc_grp_depth: lexer.brc_grp_depth,
      brc_grp_start: lexer.brc_grp_start,
      subsh_depth: lexer.subsh_depth,
      subsh_start: lexer.subsh_start,
      case_depth: lexer.case_depth,
      heredoc_skip: lexer.heredoc_skip,
      flags: lexer.flags,
    }
  }
}

/// The main struct for lexical analysis of shell input.
/// Wraps the source string and a cursor position, as well as some state for handling things like quoting and brace groups.
///
/// This struct is useful for more than just the lex-parse-execute pipeline. A single input will be lexed multiple times in many places throughout the codebase. Examples include the syntax highlighter, the line editor auto-indent logic, the bodies of subshells, etc
///
/// Notes:
/// The first and last lexed token will be an empty token with class `TkRule::Soi` and `TkRule::Eoi` respectively. These tokens must be handled specially if you are using the lexer for internal stuff like the cases mentioned above.
pub(crate) struct LexStream {
  source: SpanSource,
  pub cursor: usize,
  pos_offset: usize,
  pos: Pos,
  quote_state: QuoteState,
  brc_grp_depth: usize,
  brc_grp_start: Option<usize>,
  subsh_depth: usize,
  subsh_start: Option<usize>,
  case_depth: usize,
  heredoc_skip: Option<usize>,
  flags: LexFlags,
}

impl LexStream {
  pub(crate) fn new(source: &[u8], flags: LexFlags) -> Self {
    let flags = flags | LexFlags::FRESH | LexFlags::NEXT_IS_CMD;
    let source = SpanSource::new(stdin_name(), source.into());
    Self {
      flags,
      source,
      cursor: 0,
      pos_offset: 0,
      pos: Pos::new(0, 0),
      quote_state: QuoteState::default(),
      brc_grp_depth: 0,
      brc_grp_start: None,
      subsh_depth: 0,
      subsh_start: None,
      heredoc_skip: None,
      case_depth: 0,
    }
  }
  /// Returns a slice of the source input using the given range
  /// Returns None if the range is out of the bounds of the string slice
  ///
  /// Works with any kind of range
  /// examples:
  /// `LexStream.slice(1..10)`
  /// `LexStream.slice(1..=10)`
  /// `LexStream.slice(..10)`
  /// `LexStream.slice(1..)`
  pub(crate) fn slice<R: RangeBounds<usize>>(&self, range: R) -> Option<&[u8]> {
    let start = match range.start_bound() {
      Bound::Included(&start) => start,
      Bound::Excluded(&start) => start + 1,
      Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
      Bound::Included(&end) => end + 1,
      Bound::Excluded(&end) => end,
      Bound::Unbounded => self.source.content.len(),
    };
    self.source.content.get(start..end)
  }
  fn save_state(&self) -> LexState {
    LexState::from(self)
  }
  fn load_state(&mut self, state: LexState) {
    state.load_into(self);
  }
  /// Attempt a speculative parse.
  ///
  /// If the closure returns `Some`, the parse is committed and the result is returned.
  /// If the closure returns `None`, the parse is rolled back and `None` is returned.
  pub(crate) fn attempt<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
    let saved = self.save_state();
    if let Some(thing) = f(self) {
      Some(thing)
    } else {
      self.load_state(saved); // restore state
      None
    }
  }
  pub(crate) fn with_name(mut self, name: VarStr) -> Self {
    self.source.name = name;
    self
  }
  /// The source byte at an absolute index, if in bounds.
  fn byte_at(&self, idx: usize) -> Option<u8> {
    self.source.content.as_bytes().get(idx).copied()
  }
  pub(crate) fn in_brc_grp(&self) -> bool {
    self.brc_grp_depth > 0
  }
  pub(crate) fn in_subsh(&self) -> bool {
    self.subsh_depth > 0
  }
  /// Update the internal `Pos` that tracks the current line and column of the cursor.
  pub(crate) fn update_pos(&mut self) {
    if self.cursor < self.pos_offset {
      // cursor moved backwards? recompute I guess?
      // I think this only happens in heredocs but idk
      self.pos = Pos::new(0, 0);
      self.pos_offset = 0;
    }
    let slice = &self.source[self.pos_offset..self.cursor];
    for ch in slice.chars() {
      if ch == '\n' {
        self.pos.row += 1;
        self.pos.col = 0;
      } else {
        self.pos.col += 1;
      }
    }
    self.pos_offset = self.cursor;
  }
  pub(crate) fn update_cursor(&mut self, new_cursor: usize) {
    assert!(new_cursor <= self.source.len());
    self.cursor = new_cursor;
    self.update_pos();
  }
  pub(crate) fn inc_cursor(&mut self, amt: usize) {
    self.update_cursor(self.cursor + amt);
  }
  pub(crate) fn enter_subsh(&mut self) {
    if self.subsh_depth == 0 {
      self.subsh_start = Some(self.cursor);
    }
    self.subsh_depth += 1;
  }
  pub(crate) fn leave_subsh(&mut self) {
    self.subsh_depth -= 1;
    if self.subsh_depth == 0 {
      self.subsh_start = None;
    }
  }
  pub(crate) fn enter_brc_grp(&mut self) {
    if self.brc_grp_depth == 0 {
      self.brc_grp_start = Some(self.cursor);
    }
    self.brc_grp_depth += 1;
  }
  pub(crate) fn leave_brc_grp(&mut self) {
    self.brc_grp_depth -= 1;
    if self.brc_grp_depth == 0 {
      self.brc_grp_start = None;
    }
  }
  pub(crate) fn next_is_cmd(&self) -> bool {
    self.flags.contains(LexFlags::NEXT_IS_CMD)
  }
  /// Set whether the next string token is a command name
  pub(crate) fn set_next_is_cmd(&mut self, is: bool) {
    if is {
      self.flags |= LexFlags::NEXT_IS_CMD;
      self.flags &= !LexFlags::NEXT_IS_REDIR;
      self.flags &= !LexFlags::NEXT_IS_FUNC;
    } else {
      self.flags &= !LexFlags::NEXT_IS_CMD;
    }
  }
  /// Attempt to parse a redirection token.
  ///
  /// This is a speculative operation; if it fails, the lexer will roll back to
  /// its previous state.
  fn read_redir(&mut self) -> Option<ShResult<Tk>> {
    self.attempt(|this| {
      let start = this.cursor;

      match_loop!(this.peek_byte() => b, {
        b'&' if this.peek_nth(1) == Some(b'>') => {
          this.bump();
        }
        b'>' => {
          if this.peek_nth(1) == Some(b'(') {
            return None; // It's a process sub
          }
          this.bump();
          if this.bump_if_eq(b'|') {
            // noclobber force '>|'
            let tk = this.get_token(start..this.cursor, TkRule::Redir);
            return Some(Ok(tk));
          }

          this.bump_if_eq(b'>'); // append '>>'

          if !this.bump_if_eq(b'&') {
            let tk = this.get_token(start..this.cursor, TkRule::Redir);
            return Some(Ok(tk));
          }

          // '&' consumed by bump_if_eq above; now lex the dup target (fd or '-')
          if !this.bump_if_eq(b'-') {
            this.bump_while(|b| b.is_ascii_digit());
          }

          let tk = this.get_token(start..this.cursor, TkRule::Redir);
          return Some(Ok(tk));
        }
        b'<' => {
          if this.peek_nth(1) == Some(b'(') {
            return None; // It's a process sub
          }
          this.bump();

          match this.peek_byte() {
            Some(b'<') => {
              this.bump();

              match this.peek_byte() {
                Some(b'<') => {
                  this.bump(); // herestring, '<<<'
                }

                Some(b) => {
                  let mut b = b;
                  // skip whitespace
                  while is_field_sep(b) {
                    this.bump();
                    match this.peek_byte() {
                      Some(next) => b = next,
                      None => break, // ran out, handled below
                    }
                  }

                  if !is_field_sep(b) {
                    return Some(this.read_heredoc())
                  }
                }
                _ => {
                  // No delimiter yet - input is incomplete
                  // Fall through to emit the << as a Redir token
                }
              }
            }
            Some(b'>') => {
              this.bump();
              let tk = this.get_token(start..this.cursor, TkRule::Redir);
              return Some(Ok(tk));
            }
            Some(b'&') => {
              this.bump();

              if !this.bump_if_eq(b'-') {
                this.bump_while(|b| b.is_ascii_digit());
              }

              let tk = this.get_token(start..this.cursor, TkRule::Redir);
              return Some(Ok(tk));
            }
            _ => {}
          }

          let tk = this.get_token(start..this.cursor, TkRule::Redir);
          return Some(Ok(tk));
        }
        b'0'..=b'9' => {
          this.bump_while(|b| b.is_ascii_digit());
        }
        _ => {
          return None;
        }
      });

      None
    })
  }

  /// Read a heredoc
  ///
  /// Heredocs are very strange. They seem intuitive to parse *(just go to the next line!)*
  /// but when you get several on one line it quickly becomes a mess.
  ///
  /// `self.heredoc_skip` is used to track the end of the last heredoc body,
  /// so that the next heredoc can start reading from there.
  fn read_heredoc(&mut self) -> ShResult<Tk> {
    let start = self.cursor;
    let mut flags = TkFlags::empty();
    let mut delim = util::scratch_buf();
    let mut qt_state = QuoteState::default();

    // lets read the delimiter.
    match_loop!(self.peek_byte() => b, {
      b'-' if start == self.cursor => {
        // the first char is a dash, so its one of those tab-stripping heredocs.
        self.bump();
        flags |= TkFlags::TAB_HEREDOC;

        // eat whitespace
        self.bump_while(is_field_sep);
      }
      b'"' => {
        self.bump();
        qt_state.toggle_double();
        flags |= TkFlags::LIT_HEREDOC;
      }
      b'\'' => {
        self.bump();
        qt_state.toggle_single();
        flags |= TkFlags::LIT_HEREDOC;
      }
      _ if qt_state.in_quote() => {
        self.bump();
        delim.push(b);
      }
      _ if is_hard_sep(b) => {
        break;
      }
      _ => {
        self.bump();
        delim.push(b);
      }
    });
    let delim_end = self.cursor;

    // Scan forward to the newline (or use heredoc_skip from a previous heredoc)
    let body_start = if let Some(skip) = self.heredoc_skip {
      // A previous heredoc on this line already read its body;
      // our body starts where that one ended
      debug_assert!(
        skip >= self.cursor,
        "heredoc_skip is before the current cursor"
      );
      let skip_offset = skip - self.cursor;
      self.inc_cursor(skip_offset);
      skip
    } else {
      self.bump_while(|b| b != b'\n'); // proceed to the end of the line
      if !self.bump_if_eq(b'\n') {
        // bump_while did not end at a newline, it hit EOF.
        return Err(lex_err!(
          self,
          start..self.cursor,
          "Heredoc delimiter not found",
        ));
      }

      self.cursor
    };

    let mut line_start = body_start;

    // throw-away macro for creating and returning the heredoc token
    // has one body for a well-formed heredoc (has a closing delimiter)
    // and one body for an unfinished heredoc
    macro_rules! ret_heredoc {
      ($delim_start:expr) => {{
        // well formed, found both delimiters
        let start_delim = Box::new(self.get_span(start..delim_end));
        let end_delim = Box::new(self.get_span($delim_start..self.cursor));
        let rule = TkRule::HereDoc {
          start_delim,
          end_delim: Some(end_delim),
        };
        let mut tk = self.get_token(body_start..line_start, rule);
        tk.flags |= TkFlags::IS_HEREDOC | flags;
        self.heredoc_skip = Some(self.cursor);
        self.update_cursor(delim_end);
        return Ok(tk);
      }};
      () => {{
        // missing a closing delimiter, but we are allowing unclosed quotes
        let start_delim = Box::new(self.get_span(start..delim_end));
        let rule = TkRule::HereDoc {
          start_delim,
          end_delim: None,
        };
        let mut tk = self.get_token(body_start..self.cursor, rule);
        tk.flags |= TkFlags::IS_HEREDOC | flags;
        self.heredoc_skip = Some(self.cursor);
        self.update_cursor(delim_end);
        Ok(tk)
      }};
    }

    // Read lines until we find one that matches the delimiter exactly
    // We don't care about the actual content of these lines nor do we store them anywhere.
    // We just check to see if it matches the delimiter.
    let mut line = util::scratch_buf();
    let mut leading_tabs = true;
    let strip_tabs = flags.contains(TkFlags::TAB_HEREDOC);
    while let Some(b) = self.next_byte() {
      if strip_tabs && leading_tabs && b == b'\t' {
        continue;
      }
      leading_tabs = false;

      if b == b'\n' {
        let trimmed = line.trim_end_with(|c| c == '\r');
        if *trimmed == *delim {
          // found our delimiter
          ret_heredoc!(line_start)
        }

        // no match, clear the line and go to the next
        line.clear();
        leading_tabs = true;
        line_start = self.cursor;
      } else {
        line.push(b);
      }
    }

    // Check the last line (no trailing newline)
    let trimmed = line.trim_end_with(|c| c == '\r');
    if *trimmed == *delim {
      ret_heredoc!(line_start)
    }

    if self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
      ret_heredoc!() // that macro from earlier
    } else {
      Err(lex_err!(
        self,
        start..self.cursor,
        "Heredoc delimiter '{}' not found",
        delim.to_str_lossy()
      ))
    }
  }

  /// Read a regular word
  ///
  /// This is responsible for most of the tokens that get created
  fn read_string(&mut self) -> ShResult<Tk> {
    let start = self.cursor;
    let can_be_subshell = self.peek_byte() == Some(b'(');

    match_loop!(self.peek_byte() => b, {
      b'\\' if !self.quote_state.in_single() => {
        // something is getting escaped
        self.bump(); // '\'
        if let Some(nb) = self.next_byte() && matches!(nb, b'\n' | b'\r') {
          // line continuation
          self.bump_while(|b| matches!(b, b' ' | b'\t'));
        }
      }
      b'$' if !self.quote_state.in_single() && self.peek_nth(1) == Some(b'\'') => {
        // ANSI-C quoting
        self.inc_cursor(2); // $'

        while let Some(b) = self.next_byte() {
          if b == b'\\' {
            self.bump();
          } else if b == b'\'' {
            break;
          }
        }
      }
      // single quote handling
      b'\'' => {
        self.quote_state.toggle_single();
        self.bump();
      }
      // if we are in single quotes, nothing under this branch can match
      _ if self.quote_state.in_single() => { self.bump(); }
      b'`' => {
        // backtick command substitution
        self.bump(); // opening `
        match_loop!(self.next_byte() => b, {
          b'\\' => self.bump(),
          b'$' if self.peek_byte() == Some(b'(') => {
            self.bump();
            let paren_pos = self.cursor;
            if !strops::scan_parens(self, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
              return Err(lex_err!(
                  self,
                  paren_pos..paren_pos + 1,
                  "Unclosed subshell",
              ));
            }
          }
          b'`' => break,
          _ => { /* do nothing */ }
        });
      }
      b'$' if self.peek_nth(1) == Some(b'(') && self.peek_nth(2) == Some(b'(') => {
        // arithmetic substitution
        self.inc_cursor(2); // '$('
        let paren_pos = self.cursor;
        if !strops::scan_parens(self, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(self, paren_pos..paren_pos + 1, "Unclosed subshell"));
        }
      }
      b'$' if self.peek_nth(1) == Some(b'(') => {
        // command substitution
        self.inc_cursor(2); // '$('
        let paren_pos = self.cursor;
        // Delimit `$(...)` with the case-aware subshell scanner rather than a
        // bare paren count, so a `case` pattern's `)` doesn't close it early.
        match scan_cmd_sub_body(self.slice(self.cursor..).unwrap_or_default()) {
          Some(close) => {
            let consumed = close + 1; // include the closing `)`
            self.inc_cursor(consumed);
          }
          None if !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) => {
            return Err(lex_err!(
                self,
                paren_pos..paren_pos + 1,
                "Unclosed subshell",
            ));
          }
          None => {
            // Tolerant of partial input (e.g. tab completion): consume the rest.
            let rest = self.slice(self.cursor..).unwrap_or_default();
            self.inc_cursor(rest.len());
          }
        }
      }
      b'$' if self.peek_nth(1) == Some(b'{') => {
        // parameter expansion
        self.inc_cursor(2); // '${'
        let open_pos = self.cursor - 2;
        if !strops::scan_param_exp(self, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(
              self,
              open_pos..open_pos + 2,
              "Unclosed parameter expansion",
          ));
        }
      }
      // double quote handling
      b'"' => {
        self.quote_state.toggle_double();
        self.bump();
      }
      // if we are in double quotes, nothing under this branch can match
      _ if self.quote_state.in_double() => { self.bump(); }
      b'<' | b'>' => {
        // maybe process substitution?
        if self.peek_nth(1) != Some(b'(') {
          // not a process sub; leave the operator for read_redir
          break
        }
        // it's a process sub
        self.inc_cursor(2); // '<' or '>', then '('
        let paren_pos = self.cursor;
        if !strops::scan_parens(self, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(
              self,
              paren_pos..paren_pos + 1,
              "Unclosed subshell",
          ));
        }
      }
      b'(' if self.next_is_cmd() && self.peek_nth(1) == Some(b')') && self.cursor != start => {
        // standalone "()", function definition marker thing
        // this will be handled below by self.func_paren_lookahead();
        // leave the '(' unconsumed for the next lex pass
        break;
      }
      b'(' if self.flags.contains(LexFlags::CASE_PAT_EXPECTED) && can_be_subshell => {
        // case pattern that has a leading open paren
        self.bump(); // '('
        let tk = self.get_token(start..self.cursor, TkRule::SubshStart);
        return Ok(tk);
      }
      b'(' if (self.next_is_cmd() || self.peek_nth(1) == Some(b'(')) && can_be_subshell => {
        // arithmetic or subshell
        self.bump(); // first '('
        let mut paren_count = 1;
        let paren_pos = self.cursor;
        let mut flags = TkFlags::IS_CMD;
        if self.peek_byte() == Some(b'(') {
          // arithmetic
          paren_count += 1;
          self.bump();
          flags |= TkFlags::IS_ARITH;
        } else {
          // subshell
          let mut tk = self.get_token(start..self.cursor, TkRule::SubshStart);
          tk.flags |= TkFlags::IS_CMD;
          self.enter_subsh();
          self.set_next_is_cmd(true);

          return Ok(tk);
        }
        // find our closing paren
        if !strops::scan_parens(self, paren_count) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(
              self,
              paren_pos..paren_pos + 1,
              "Unclosed subshell",
          ));
        }
        let mut tk = self.get_token(start..self.cursor, TkRule::Str);
        tk.flags |= flags;
        self.set_next_is_cmd(true);
        return Ok(tk);
      }
      b'{' if self.cursor == start && self.next_is_cmd() => {
        // brace group
        self.bump(); // '{'
        let mut tk = self.get_token(start..self.cursor, TkRule::BraceGrpStart);
        tk.flags |= TkFlags::IS_CMD;
        self.enter_brc_grp();
        self.set_next_is_cmd(true);

        return Ok(tk);
      }
      b'}' if start == self.cursor && self.in_brc_grp() && self.next_is_cmd() => {
        // brace group closer
        self.bump(); // '}'
        let tk = self.get_token(start..self.cursor, TkRule::BraceGrpEnd);
        self.leave_brc_grp();
        self.set_next_is_cmd(true);
        return Ok(tk);
      }
      b')' if start == self.cursor
        && (self.in_subsh() || self.flags.contains(LexFlags::CASE_PAT_EXPECTED)) =>
      {
        // subshell closer? case pattern closer?
        self.bump(); // ')'
        let tk = self.get_token(start..self.cursor, TkRule::SubshEnd);
        if self.flags.contains(LexFlags::CASE_PAT_EXPECTED) {
          // this paren closes a case pattern. consume it and continue
          self.flags &= !LexFlags::CASE_PAT_EXPECTED;
        } else {
          // subshell closer
          self.leave_subsh();
        }
        self.set_next_is_cmd(true);
        return Ok(tk);
      }
      b'=' if self.peek_nth(1) == Some(b'(') => {
        // looks like an array literal
        self.inc_cursor(2); // '=('
        if !strops::scan_parens(self, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(
              self,
              self.cursor..self.cursor + 1,
              "Unclosed array assignment",
          ));
        }
      }
      b')' => {
        // we hit a random closing paren?
        // that's not allowed
        if !self.in_subsh() && !self.flags.contains(LexFlags::CASE_PAT_EXPECTED) {
          let bad = self.cursor;
          self.bump(); // ')'
          return Err(lex_err!(self, bad..self.cursor, "Unexpected ')'"));
        }
        break
      }
      b'|' => break, // pipe operator outside of quotes, leave it
      _ if is_hard_sep(b) => break,
      _ => { self.bump(); }
    });

    self.interpret(start)
  }

  /// Interpret a string lexed by [`LexStream::read_string`]
  ///
  /// The second step of normal word lexing
  fn interpret(&mut self, start: usize) -> ShResult<Tk> {
    let mut new_tk = self.get_token(start..self.cursor, TkRule::Str);
    if self.quote_state.in_quote() && !self.flags.contains(LexFlags::LEX_UNFINISHED_QUOTES) {
      return Err(sherr!(
          ParseErr @ new_tk.span,
          "Unterminated quote",
      ));
    }

    let text = new_tk.span.as_bytes();
    let is_cmd = self.flags.contains(LexFlags::NEXT_IS_CMD)
      && !self.flags.contains(LexFlags::NEXT_IS_REDIR)
      && !self.flags.contains(LexFlags::CASE_PAT_EXPECTED);
    if is_cmd {
      match text {
        // keyword handling
        // here we set flags for the lexer to use when reading the next token
        b"function" => {
          new_tk.mark(TkFlags::KEYWORD);
          self.flags |= LexFlags::NEXT_IS_FUNC;
          // we are now expecting a function name
        }
        _ if self.attempt(Self::func_paren_lookahead).is_some() => {
          // found it
          new_tk.mark(TkFlags::FUNCNAME);
          self.set_next_is_cmd(true);
        }
        b"case" => {
          // next token should be a case pattern, followed by 'in'
          new_tk.mark(TkFlags::KEYWORD);
          self.flags |= LexFlags::EXPECTING_IN | LexFlags::EXPECTING_CASE_IN;
          self.case_depth += 1;
          self.set_next_is_cmd(false);
        }
        b"select" | b"for" => {
          // expecting something, and then 'in'
          // NOTE: we don't actually do anything with the word 'select' currently
          new_tk.mark(TkFlags::KEYWORD);
          self.flags |= LexFlags::EXPECTING_IN;
          self.set_next_is_cmd(false);
        }
        b"in" if self.flags.contains(LexFlags::EXPECTING_IN) => {
          // `in` in command position (rare) while we're expecting it
          self.consume_in_keyword(&mut new_tk);
        }
        _ if is_keyword(text) => {
          if text == b"esac" && self.case_depth > 0 {
            // close a case statement
            self.case_depth -= 1;
            self.flags &= !LexFlags::CASE_PAT_EXPECTED;
          }
          new_tk.mark(TkFlags::KEYWORD);
        }
        _ if is_assignment(text) => {
          new_tk.mark(TkFlags::ASSIGN);
        }
        _ if is_cmd_sub(text) => {
          if self.next_is_cmd() {
            new_tk.mark(TkFlags::IS_CMD);
          }
          self.set_next_is_cmd(false);
        }
        _ if self.flags.contains(LexFlags::NEXT_IS_FUNC) => {
          // found a function name, next token is a command
          new_tk.mark(TkFlags::FUNCNAME);
          self.set_next_is_cmd(true);
        }
        _ => {
          // if we are here, we have a regular command.
          new_tk.flags |= TkFlags::IS_CMD;
          if BUILTIN_NAMES.binary_search(&text).is_ok() {
            // a builtin command, even
            new_tk.mark(TkFlags::BUILTIN);
          }
          self.set_next_is_cmd(false);
        }
      }
    } else if self.flags.contains(LexFlags::EXPECTING_IN) && text == b"in" {
      self.consume_in_keyword(&mut new_tk);
    } else if self.flags.contains(LexFlags::EXPECTING_IN)
      && !self.flags.contains(LexFlags::EXPECTING_CASE_IN)
      && text == b"do"
    {
      // we got something like "for var; do ..."
      // "do" directly after the variable means that we implicitly
      // use the shell's positional parameters instead of an explicit array
      new_tk.mark(TkFlags::KEYWORD);
      self.flags &= !LexFlags::EXPECTING_IN;
      self.set_next_is_cmd(true);
    } else if text == b"esac"
      && self.case_depth > 0
      && self.flags.contains(LexFlags::CASE_PAT_EXPECTED)
    {
      // `esac` reached in pattern position (empty case body or right after `;;`).
      // The is_cmd block above is short-circuited by CASE_PAT_EXPECTED, so do the
      // keyword recognition and depth bookkeeping here. Gating on
      // CASE_PAT_EXPECTED keeps `esac` used as an ordinary argument (`echo esac`,
      // mid-arm-body) from being misread as the closer and corrupting case_depth.
      new_tk.mark(TkFlags::KEYWORD);
      self.case_depth -= 1;
      self.flags &= !LexFlags::CASE_PAT_EXPECTED;
    }
    Ok(new_tk)
  }
  /// Recognize the `in` keyword when the lexer is expecting it
  fn consume_in_keyword(&mut self, tk: &mut Tk) {
    tk.mark(TkFlags::KEYWORD);
    self.flags &= !LexFlags::EXPECTING_IN;
    if self.flags.contains(LexFlags::EXPECTING_CASE_IN) {
      // if we're in a case statement, a case pattern follows
      self.flags &= !LexFlags::EXPECTING_CASE_IN;
      self.flags |= LexFlags::CASE_PAT_EXPECTED;
    }
  }

  /// If the just-consumed separator `ch` is a line break and a heredoc body is
  /// pending on it, advance the cursor past that body.
  ///
  /// Returns true if the cursor was advanced, false otherwise.
  fn skip_pending_heredoc(&mut self, ch: u8) -> bool {
    if (ch == b'\n' || ch == b'\r')
      && let Some(skip) = self.heredoc_skip.take()
    {
      self.update_cursor(skip);
      true
    } else {
      false
    }
  }

  /// At EOF, detect an unclosed brace group or subshell.
  /// Returns the terminal error item if one is still open.
  fn unclosed_structure_error(&mut self) -> Option<ShResult<Tk>> {
    if self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
      return None;
    }
    if self.in_brc_grp() {
      self.flags |= LexFlags::STALE;
      let start = self.brc_grp_start.unwrap_or(self.cursor.saturating_sub(1));
      return Some(Err(sherr!(
        ParseErr @ self.get_span(start..self.cursor),
        "Unclosed brace group",
      )));
    }
    if self.in_subsh() {
      self.flags |= LexFlags::STALE;
      let start = self.subsh_start.unwrap_or(self.cursor.saturating_sub(1));
      return Some(Err(sherr!(
        ParseErr @ self.get_span(start..self.cursor),
        "Unclosed subshell",
      )));
    }
    None
  }

  pub(crate) fn func_paren_lookahead(&mut self) -> Option<()> {
    // this returns Some(()) if it finds the parens.
    // kind of weird but it makes the function
    // directly usable as an argument to Self::attempt()

    match_loop!(self.next_byte() => b, {
      b' ' | b'\t' => {
        // continue
      }
      b'(' => {
        if self.next_byte() == Some(b')') {
          return Some(());
        }
        // Not "()" - restore pos
        return None;
      }
      _ => {
        return None;
      }
    });
    None
  }
  pub(crate) fn get_span(&mut self, range: Range<usize>) -> Span {
    self.update_pos();
    Span::with_source(range, self.source.clone()).at(self.pos)
  }
  /// Slice a token out of the original source input, based on the given range.
  pub(crate) fn get_token(&mut self, range: Range<usize>, class: TkRule) -> Tk {
    let span = self.get_span(range);
    Tk::new(class, span)
  }
}

impl ByteCursor for LexStream {
  fn peek_byte(&self) -> Option<u8> {
    self.source.content.get(self.cursor).copied()
  }
  fn peek_nth(&self, n: usize) -> Option<u8> {
    self.source.content.get(self.cursor + n).copied()
  }
  fn next_byte(&mut self) -> Option<u8> {
    let b = self.peek_byte()?;
    self.inc_cursor(1);
    Some(b)
  }
}

// LexStream implements Iterator, so lexing as an operation is actually lazy.
// The lexer essentially acts as a streaming cursor over the original input.
impl Iterator for LexStream {
  type Item = ShResult<Tk>;
  fn next(&mut self) -> Option<Self::Item> {
    assert!(self.cursor <= self.source.len());
    // We are at the end of the input
    if self.flags.contains(LexFlags::STALE) {
      return None;
    }

    if self.cursor == self.source.len() {
      // we have hit the end of the source input; check for unclosed structures
      if let Some(err) = self.unclosed_structure_error() {
        return Some(err);
      }
      // Return the Eoi token
      let token = self.get_token(self.cursor..self.cursor, TkRule::Eoi);
      self.flags |= LexFlags::STALE;
      return Some(Ok(token));
    }

    // Return the Soi token
    if self.flags.contains(LexFlags::FRESH) {
      self.flags &= !LexFlags::FRESH;
      let token = self.get_token(self.cursor..self.cursor, TkRule::Soi);
      return Some(Ok(token));
    }

    // more line continuation handling
    loop {
      let pos = self.cursor;
      if self.slice(pos..pos + 2) == Some(b"\\\n".as_slice())
        || self.slice(pos..pos + 3) == Some(b"\\\r\n".as_slice())
      {
        self.inc_cursor(2);
      } else if pos < self.source.len() && is_field_sep(self.byte_at(pos).unwrap()) {
        self.inc_cursor(1);
      } else {
        break;
      }
    }

    if self.cursor == self.source.len() {
      if let Some(err) = self.unclosed_structure_error() {
        return Some(err);
      }
      return None;
    }

    // now we can finally do some lexing.
    let token = match self.byte_at(self.cursor).unwrap() {
      b'\r' | b'\n' | b';' => {
        // this ends a statement
        let ch = self.byte_at(self.cursor).unwrap();
        let ch_idx = self.cursor;
        self.inc_cursor(1);
        let mut heredoc_skipped = false;
        self.set_next_is_cmd(true);

        // If a heredoc was parsed on this line, skip past its body.
        heredoc_skipped |= self.skip_pending_heredoc(ch);

        match_loop!(self.byte_at(self.cursor) => ch, {
          b'\\' if self.byte_at(self.cursor + 1) == Some(b'\n') => {
            self.update_cursor((self.cursor + 2).min(self.source.len()));
          }
          _ if is_hard_sep(ch) => {
            self.inc_cursor(1);
            // A later separator in this run may itself carry a pending heredoc.
            heredoc_skipped |= self.skip_pending_heredoc(ch);
          }
          _ => break,
        });

        // If a heredoc skip occurred, cap the separator span to just the
        // triggering character so it doesn't cover the heredoc body
        let sep_end = if heredoc_skipped {
          ch_idx + 1
        } else {
          self.cursor
        };
        let sep_tk = self.get_token(ch_idx..sep_end, TkRule::Sep);
        // `;;` inside a case body starts a new pattern; mark it so the
        // next `)` is recognized as the pattern terminator.
        if self.case_depth > 0 && sep_tk.has_double_semi() {
          self.flags |= LexFlags::CASE_PAT_EXPECTED;
        }
        if self.flags.contains(LexFlags::CASE_PAT_EXPECTED) {
          // next is a case pattern, not a command.
          self.set_next_is_cmd(false);
        }
        sep_tk
      }
      b'#'
        if !self.flags.contains(LexFlags::INTERACTIVE)
          || Shed::shopts(|s| s.core.interactive_comments) =>
      {
        // this is a comment.
        // we still lex these and emit as tokens so that
        // stuff like the syntax highlighter can see them
        let ch_idx = self.cursor;
        self.inc_cursor(1);

        while let Some(ch) = self.byte_at(self.cursor) {
          if ch == b'\n' {
            break;
          }
          self.inc_cursor(1);
        }

        if self.flags.contains(LexFlags::LEX_UNFINISHED) {
          self.get_token(ch_idx..self.cursor, TkRule::Comment)
        } else {
          return self.next();
        }
      }
      b'!'
        if self.next_is_cmd()
          && self
            .byte_at(self.cursor + 1)
            .is_none_or(|c| c.is_ascii_whitespace() || matches!(c, b';' | b'|' | b'&')) =>
      {
        // negation keyword
        self.inc_cursor(1);
        let tk_type = TkRule::Bang;

        let mut tk = self.get_token((self.cursor - 1)..self.cursor, tk_type);
        tk.flags |= TkFlags::KEYWORD;
        tk
      }
      b'|' => {
        // pipe operator? logical or?
        let ch_idx = self.cursor;
        self.inc_cursor(1);
        self.set_next_is_cmd(true);

        let tk_type = if let Some(b'|') = self.byte_at(self.cursor) {
          // logical or
          self.inc_cursor(1);
          TkRule::Or
        } else if let Some(b'&') = self.byte_at(self.cursor) {
          // error pipe
          self.inc_cursor(1);
          TkRule::ErrPipe
        } else {
          // pipe operator
          TkRule::Pipe
        };

        self.get_token(ch_idx..self.cursor, tk_type)
      }
      b'&' => {
        // background operator? logical and? redirection?
        let ch_idx = self.cursor;
        self.inc_cursor(1);
        self.set_next_is_cmd(true);
        let mut flags = TkFlags::empty();

        let tk_type = match self.byte_at(self.cursor) {
          Some(b'&') => {
            // logical and
            self.inc_cursor(1);
            TkRule::And
          }
          Some(b'|') => {
            // error pipe
            self.inc_cursor(1);
            TkRule::ErrPipe
          }
          Some(b'>') => {
            // redirection
            self.inc_cursor(1);
            let append = matches!(self.byte_at(self.cursor), Some(b'>'));
            if append {
              self.inc_cursor(1);
            }

            flags |= TkFlags::REDIR_ALL;
            self.flags |= LexFlags::NEXT_IS_REDIR;
            TkRule::Redir
          }
          _ => TkRule::Bg, // background
        };

        let mut tk = self.get_token(ch_idx..self.cursor, tk_type);
        tk.flags |= flags;
        tk
      }
      _ => {
        // ok nothing else matched, so it's probably a word
        if let Some(tk_result) = self.read_redir() {
          let tk = match tk_result {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
          };
          // we gotta check to see if this wants a file target or not
          // if already points at a number or has '-', it doesn't.
          let dup_style = tk
            .span
            .as_bytes()
            .last()
            .is_some_and(|b| b.is_ascii_digit() || *b == b'-');

          let is_heredoc = matches!(tk.class, TkRule::HereDoc { .. });

          if dup_style || is_heredoc {
            self.flags &= !LexFlags::NEXT_IS_REDIR;
          } else {
            self.flags |= LexFlags::NEXT_IS_REDIR;
          }
          tk
        } else {
          let res = match self.read_string() {
            Ok(tk) => tk,
            Err(e) => {
              return Some(Err(e));
            }
          };
          self.flags &= !LexFlags::NEXT_IS_REDIR;
          res
        }
      }
    };

    // hooray we did it
    Some(Ok(token))
  }
}

// misc helper functions

pub(crate) fn is_assignment(text: &[u8]) -> bool {
  let mut bytes = text.bytes();

  match_loop!(bytes.next() => b, {
    b'\\' => {
      bytes.next();
    }
    b'=' => return true,
    _ => continue,
  });
  false
}

/// Is whitespace or a semicolon
pub(crate) fn is_hard_sep(ch: u8) -> bool {
  matches!(ch, b' ' | b'\t' | b'\n' | b';')
}

/// Is whitespace, but not a newline
pub(crate) fn is_field_sep(ch: u8) -> bool {
  matches!(ch, b' ' | b'\t')
}

pub(crate) fn is_keyword(slice: &[u8]) -> bool {
  KEYWORDS.binary_search(&slice).is_ok()
}

pub(crate) fn scan_cmd_sub_body(body: &[u8]) -> Option<usize> {
  // Prepend `(` so the lexer enters a subshell context
  let mut prefixed = Vec::with_capacity(body.len() + 1);
  prefixed.push(b'(');
  prefixed.extend_from_slice(body);
  let mut lex = LexStream::new(&prefixed, LexFlags::LEX_UNFINISHED);
  let mut entered = false;
  while let Some(tk) = lex.next() {
    let tk = tk.ok()?;
    if lex.in_subsh() {
      entered = true;
    } else if entered {
      // `tk` is the `)` that closed the subshell. Its span end is the byte just
      // past `)` in `(`+body; strip the prepended `(` and the `)` itself.
      return tk.span.range.end.checked_sub(2);
    }
  }
  None
}

pub(crate) fn is_cmd_sub(slice: &[u8]) -> bool {
  slice.starts_with(b"$(") && strops::ends_with_unescaped(slice, b")")
}

#[cfg(test)]
mod tests {
  use super::*;

  fn lex_classes(src: &str) -> Vec<TkRule> {
    LexStream::new(src.as_bytes(), LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .filter(|t| !matches!(t.class, TkRule::Soi | TkRule::Eoi))
      .map(|t| t.class)
      .collect()
  }

  fn lex_first_nontrivial_text(src: &str) -> String {
    LexStream::new(src.as_bytes(), LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .find(|t| !matches!(t.class, TkRule::Soi | TkRule::Eoi | TkRule::Sep))
      .map(|t| t.span.to_str_lossy().into_owned())
      .unwrap_or_default()
  }

  fn lex_toks(src: &str) -> Vec<Tk> {
    LexStream::new(src.as_bytes(), LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .filter(|t| !matches!(t.class, TkRule::Soi | TkRule::Eoi))
      .collect()
  }

  // ===================== dedup regression pins =====================
  // Behaviors locked in before extracting shared helpers: `consume_in_keyword`
  // (the `in`-keyword body, duplicated across the is_cmd match arm and the
  // else-if) and `skip_pending_heredoc` (the heredoc-body skip, duplicated in
  // the separator loop). These pin observable behavior so the extractions can
  // be proven behavior-preserving.

  #[test]
  fn for_in_marks_in_keyword() {
    let toks = lex_toks("for x in a b");
    let in_tok = toks
      .iter()
      .find(|t| t.span.to_str_lossy() == "in")
      .expect("expected an `in` token");
    assert!(
      in_tok.flags.contains(TkFlags::KEYWORD),
      "`in` in a for-loop should be marked KEYWORD; flags = {:?}",
      in_tok.flags
    );
  }

  #[test]
  fn case_in_enables_pattern_terminator() {
    // The `)` closing a case pattern only lexes as SubshEnd because the `in`
    // handling sets CASE_PAT_EXPECTED. Pins that side effect.
    let classes = lex_classes("case x in a) echo hi;; esac");
    assert!(
      classes.contains(&TkRule::SubshEnd),
      "case pattern `)` should lex as SubshEnd; got {classes:?}"
    );
  }

  #[test]
  fn heredoc_body_skipped_before_trailing_command() {
    let src = "cat <<EOF\nbody\nEOF\necho after";
    let texts: Vec<String> = lex_toks(src)
      .into_iter()
      .filter(|t| t.class == TkRule::Str)
      .map(|t| t.span.to_str_lossy().into_owned())
      .collect();
    assert!(
      texts.iter().any(|t| t == "echo") && texts.iter().any(|t| t == "after"),
      "command after heredoc should lex; got {texts:?}"
    );
    assert!(
      !texts.iter().any(|t| t == "body"),
      "heredoc body must be skipped, not lexed as a command; got {texts:?}"
    );
  }

  #[test]
  fn unclosed_subshell_with_trailing_ws_errors_at_lexer() {
    // `(echo hi ` — the trailing space makes the pre-lex continuation loop
    // advance to EOF, so the *second* EOF structure check must catch the
    // unclosed subshell. Before unifying both EOF checks into one helper, that
    // second check only looked at brace groups and let the parser report a
    // different error later. Lexed with empty (non-tolerant) flags so the
    // unclosed structure is an error rather than tolerated.
    let err = LexStream::new(b"(echo hi ", LexFlags::empty())
      .find_map(Result::err)
      .expect("expected a lex error for the unclosed subshell");
    let msg = err.to_string();
    assert!(
      msg.contains("Unclosed subshell"),
      "expected 'Unclosed subshell' lex error; got {msg:?}"
    );
  }

  #[test]
  fn unclosed_subshell_no_trailing_ws_errors_at_lexer() {
    // Control: the no-trailing-whitespace case was already caught by the first
    // EOF check and must keep the same error after the unify.
    let err = LexStream::new(b"(echo hi", LexFlags::empty())
      .find_map(Result::err)
      .expect("expected a lex error for the unclosed subshell");
    assert!(
      err.to_string().contains("Unclosed subshell"),
      "expected 'Unclosed subshell' lex error"
    );
  }

  // ===================== `!` lexer disambiguation =====================
  //
  // `! cmd` (with space)  -> TkRule::Bang (negation operator)
  // `!cmd`  (no space)    -> TkRule::Str (so CtxTk's inner scan can find
  //                          it as HistExp)

  #[test]
  fn bang_with_space_is_negation() {
    let classes = lex_classes("! true");
    assert!(
      classes.contains(&TkRule::Bang),
      "expected Bang token in '! true'; got {classes:?}"
    );
  }

  #[test]
  fn bang_with_semicolon_is_negation() {
    let classes = lex_classes("!;");
    assert!(
      classes.contains(&TkRule::Bang),
      "expected Bang token before ';'; got {classes:?}"
    );
  }

  #[test]
  fn bang_with_pipe_is_negation() {
    let classes = lex_classes("!|");
    assert!(
      classes.contains(&TkRule::Bang),
      "expected Bang token before '|'; got {classes:?}"
    );
  }

  #[test]
  fn bang_followed_by_alpha_is_word() {
    // `!cmd` should be lexed as one Str token, not Bang followed by `cmd`.
    let classes = lex_classes("!cmd");
    assert!(
      !classes.contains(&TkRule::Bang),
      "'!cmd' should NOT produce a Bang token; got {classes:?}"
    );
    let text = lex_first_nontrivial_text("!cmd");
    assert_eq!(
      text, "!cmd",
      "the whole `!cmd` should be one token; got {text:?}"
    );
  }

  #[test]
  fn bang_followed_by_digit_is_word() {
    let classes = lex_classes("!42");
    assert!(
      !classes.contains(&TkRule::Bang),
      "'!42' should NOT produce a Bang; got {classes:?}"
    );
  }

  #[test]
  fn bang_followed_by_bang_is_word() {
    // `!!` is the hist-exp "last command" — not two Bang operators.
    let classes = lex_classes("!!");
    let bang_count = classes.iter().filter(|c| matches!(c, TkRule::Bang)).count();
    assert!(
      bang_count <= 1,
      "'!!' should not produce two Bang tokens; got {classes:?}"
    );
  }

  #[test]
  fn bang_followed_by_dollar_is_word() {
    let classes = lex_classes("!$");
    assert!(
      !classes.contains(&TkRule::Bang),
      "'!$' should NOT produce a Bang; got {classes:?}"
    );
  }

  // ===================== line continuation (`\<newline>`) =====================

  #[test]
  fn continuation_preserves_next_line_whitespace() {
    // `\<newline>` drops only the pair; the next line's indentation stays, so
    // `a=1\<nl>    b=2` splits into two words (issue #119).
    assert_eq!(
      clean_input(b"export a=1\\\n    b=2").as_bytes(),
      b"export a=1    b=2"
    );
  }

  #[test]
  fn continuation_joins_adjacent_words() {
    assert_eq!(clean_input(b"echo one\\\ntwo").as_bytes(), b"echo onetwo");
  }

  #[test]
  fn continuation_not_applied_in_single_quotes() {
    // Inside single quotes a `\<newline>` is literal, not a continuation.
    let src = b"'a\\\nb'";
    assert_eq!(clean_input(src).as_bytes(), src);
  }

  #[test]
  fn continuation_applied_in_double_quotes() {
    assert_eq!(clean_input(b"\"a\\\nb\"").as_bytes(), b"\"ab\"");
  }

  #[test]
  fn trailing_backslash_in_comment_is_not_continuation() {
    // A `\` ending a comment line does not splice the next line (issue #120):
    // the newline ends the comment, so the source is unchanged.
    let src = b"# comment \\\na=1";
    assert_eq!(clean_input(src).as_bytes(), src);
  }

  #[test]
  fn hash_mid_word_is_not_a_comment() {
    // `#` not at a word boundary stays literal, so the continuation still fires.
    assert_eq!(clean_input(b"a#b\\\nc").as_bytes(), b"a#bc");
  }
}
