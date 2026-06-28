use std::{
  cmp::Ordering,
  fmt::Display,
  ops::{Bound, Range, RangeBounds},
  rc::Rc,
};

use bitflags::bitflags;

use crate::{
  assert_sorted,
  state::vars::{VarStr, VarStrSliceExt},
  util,
};

use super::{
  Shed,
  builtin::BUILTIN_NAMES,
  match_loop, sherr,
  util::{Pos, QuoteState, ShResult, ends_with_unescaped, scan_param_exp, scan_parens},
};

pub const KEYWORDS: [&str; 21] = [
  "!", "case", "catch", "defer", "do", "done", "elif", "else", "esac", "fi", "for", "function",
  "if", "in", "not", "select", "then", "time", "try", "until", "while",
];

assert_sorted!(KEYWORDS);

pub const MIDDLES: [&str; 3] = ["elif", "else", "catch"];

pub const CLOSERS: [&str; 6] = ["fi", "done", "esac", "}", ")", ";;"];

pub trait TkVecUtils<Tk> {
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
	($lexer:expr, $pos:expr, $range: expr, $($arg:tt)*) => {{
		$lexer.cursor = $pos;
		sherr!(ParseErr @ $lexer.get_span($range), $($arg)*)
	}}
}

#[derive(Clone, PartialEq, Default, Debug, Eq, Hash)]
pub struct SpanSource {
  name: Rc<str>,
  content: Rc<str>,
}

impl SpanSource {
  pub fn name(&self) -> &str {
    &self.name
  }
  pub fn content(&self) -> Rc<str> {
    self.content.clone()
  }
}

impl Display for SpanSource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    use yansi::Paint;
    write!(f, "{}", self.name.cyan().bold().underline())
  }
}

#[derive(Clone, PartialEq, Default, Debug)]
/// A slice of some source text. Ultimately wraps an `Rc<str>`, which means these are cheap to clone.
///
/// Load-bearing struct. Used extensively throughout the codebase for slicing shell input for various reasons (error reporting, tab completion, etc)
pub(crate) struct Span {
  range: Range<usize>,
  pos: Pos,
  source: SpanSource,
}

impl Span {
  /// New `Span`. Wraps a range and a string that it refers to.
  pub fn new(range: Range<usize>, source: Rc<str>) -> Self {
    let source = SpanSource {
      name: "<stdin>".into(),
      content: source,
    };
    Span {
      range,
      pos: Pos::MIN,
      source,
    }
  }
  pub fn merge_inplace(&mut self, other: &Span) {
    // make sure these two spans originate from the same input
    if !Rc::ptr_eq(&self.source.content, &other.source.content) {
      return;
    }

    if other.range.start < self.range.start {
      self.pos = other.pos;
    }
    self.range.start = self.range.start.min(other.range.start);
    self.range.end = self.range.end.max(other.range.end);
  }
  pub fn merge_with(mut self, other: &Span) -> Option<Self> {
    // make sure these two spans originate from the same input
    if !Rc::ptr_eq(&self.source.content, &other.source.content) {
      return None;
    }

    if other.range.start < self.range.start {
      self.pos = other.pos;
    }
    self.range.start = self.range.start.min(other.range.start);
    self.range.end = self.range.end.max(other.range.end);
    Some(self)
  }
  pub fn at(mut self, pos: Pos) -> Self {
    self.pos = pos;
    self
  }
  pub fn rename(&mut self, name: Rc<str>) {
    self.source.name = name;
  }
  pub fn line_and_col(&self) -> (usize, usize) {
    (self.pos.row, self.pos.col)
  }
  /// Slice the source string at the wrapped range
  pub fn as_str(&self) -> &str {
    &self.source.content[self.range().start..self.range().end]
  }
  pub fn get_source(&self) -> Rc<str> {
    self.source.content.clone()
  }
  pub fn span_source(&self) -> &SpanSource {
    &self.source
  }
  pub fn range(&self) -> Range<usize> {
    self.range.clone()
  }
  /// With great power comes great responsibility
  /// Only use this in the most dire of circumstances
  pub fn set_range(&mut self, range: Range<usize>) {
    self.range = range;
  }

  pub fn shift_by(&mut self, delta: isize) {
    let new_start = self.range.start as isize + delta;
    let new_end = self.range.end as isize + delta;
    debug_assert!(new_start >= 0 && new_end >= 0, "shift_by underflow");
    self.range = (new_start as usize)..(new_end as usize);
  }

  pub fn rebase_into(&mut self, outer_span: &Span, offset: usize) {
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
/// * A `TkRule` which identifies what kind of token it is
/// * A `Span` which represents the slice of the original input the token refers to
/// * `TkFlags` which is a bitfield containing simple metadata
///
/// Generally speaking, these are very cheap to clone. The only time cloning a `Tk` is a heavy operation
/// is if the wrapped `TkRule` is `TkRule::Expanded`, which contains a `Vec<String>` that needs to be cloned.
/// However, `TkRule::Expanded` is never created through lexing. You can assume that if you are cloning a `Tk`,
/// it will not have this `TkRule`.
/// Therefore, you can generally consider cloning a token to be effectively as cheap as cloning an Rc<T>.
///
/// `TkRule::Expanded` is only created during token expansion, which generally happens much later in an execution cycle.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct Tk {
  pub class: TkRule,
  pub span: Span,
  pub flags: TkFlags,
}

impl Tk {
  pub fn new(class: TkRule, span: Span) -> Self {
    Self {
      class,
      span,
      flags: TkFlags::empty(),
    }
  }
  pub fn replaced(&self, other: &str) -> String {
    let mut content = self.span.source.content().to_string();
    content.replace_range(self.span.range(), other);
    content
  }
  pub fn is_literal(&self) -> bool {
    self.filter_meta()
      && self
        .span
        .as_str()
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
  }
  pub fn as_str(&self) -> &str {
    self.span.as_str()
  }
  /// The token's effective text as a `VarStr`: the joined expansion for an
  /// expanded token, or the raw span otherwise. Mirrors `Display` without
  /// routing through the formatter.
  pub fn word(&self) -> VarStr {
    match &self.class {
      TkRule::Expanded { exp } => exp.join_with(" "),
      _ => self.span.as_str().into(),
    }
  }
  pub fn source(&self) -> Rc<str> {
    self.span.source.content.clone()
  }
  pub fn mark(&mut self, flag: TkFlags) {
    self.flags |= flag;
  }
  /// Used to see if a separator is ';;' for case statements
  pub fn has_double_semi(&self) -> bool {
    let TkRule::Sep = self.class else {
      return false;
    };
    self.span.as_str().trim() == ";;"
  }

  pub fn filter_meta(&self) -> bool {
    !matches!(self.class, TkRule::Soi | TkRule::Eoi | TkRule::Null)
  }

  /// used when lexing recursively, to replace the token's span with the original source
  pub fn rebase_into(mut self, outer_span: &Span, offset: usize) -> Self {
    self.span.rebase_into(outer_span, offset);
    self
  }
}

impl Display for Tk {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match &self.class {
      TkRule::Expanded { exp } => write!(f, "{}", exp.join_with(" ")),
      _ => write!(f, "{}", self.span.as_str()),
    }
  }
}

bitflags! {
  #[derive(Debug,Clone,Copy,PartialEq,Default)]
  pub struct TkFlags: u32 {
    const KEYWORD      = 0b0000_0000_0000_0001;
    const OPENER       = 0b0000_0000_0000_0010;
    const IS_CMD       = 0b0000_0000_0000_0100;
    const IS_SUBSH     = 0b0000_0000_0000_1000;
    const IS_CMDSUB    = 0b0000_0000_0001_0000;
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
  #[derive(Debug, Clone, Copy)]
  pub struct LexFlags: u32 {
    /// The lexer is operating in interactive mode
    const INTERACTIVE    = 1 << 0;
    /// Allow unfinished input
    const LEX_UNFINISHED_STRUCTURES = 1 << 1;
    const LEX_UNFINISHED_QUOTES   = 1 << 2;
    /// The next string-type token is a command name
    const NEXT_IS_CMD    = 1 << 3;
    /// Only lex strings; used in expansions
    const RAW            = 1 << 4;
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

#[expect(clippy::similar_names)]
pub fn clean_input(input: &str) -> String {
  let input = input.to_string();
  let mut chars = input.char_indices().peekable();
  let mut output = String::new();
  let mut in_squote = false;
  // FIFO queue: heredocs on the same line are consumed in order
  let mut heredoc_queue: std::collections::VecDeque<VarStr> = std::collections::VecDeque::new();
  match_loop!(chars.next() => (i,ch) => ch, {
    '\'' => {
      in_squote = !in_squote;
      output.push(ch);
    }
    '\\' if !in_squote && chars.peek().is_some_and(|(_,c)| *c == '\n') => {
      chars.next();
      while chars.peek().is_some_and(|(_,c)| c.is_whitespace() && *c != '\n') {
        chars.next();
      }
    }
    '\r' => {
      if let Some(&(_, '\n')) = chars.peek() {
        chars.next();
      }
      output.push('\n');
    }
    '\n' if !heredoc_queue.is_empty() => {
      output.push('\n');
      let delim = heredoc_queue.pop_front().unwrap();
      // Strip leading '-' (<<- style) to get the bare delimiter word
      let match_delim = delim.trim_start_matches('-');
      let start = i + 1;
      let mut end = start;
      for line in input[start..].split('\n') {
        output.push_str(line);
        output.push('\n');
        end += line.len() + 1;
        if line.trim_start_matches('\t') == match_delim {
          // Advance chars iterator past all bytes we just copied
          while chars.peek().is_some_and(|&(j, _)| j < end) {
            chars.next();
          }
          break;
        }
      }
    }
    '<' if !in_squote && chars.peek().is_some_and(|(_,c)| *c == '<') => {
      output.push(ch);
      let (_, second) = chars.next().unwrap();
      output.push(second);

      // <<< is a here-string — no multi-line body, don't push to queue
      if chars.peek().is_some_and(|(_,c)| *c == '<') {
        let (_, third) = chars.next().unwrap();
        output.push(third);
      } else {
        // Skip optional '-' for <<-
        let mut tab_strip = false;
        if chars.peek().is_some_and(|(_,c)| *c == '-') {
          tab_strip = true;
        }

        // Skip horizontal whitespace between << and delimiter
        while chars.peek().is_some_and(|(_,c)| *c == ' ' || *c == '\t') {
          let (_, wc) = chars.next().unwrap();
          output.push(wc);
        }

        // Collect delimiter word, stripping quotes for the match key
        let mut delim = util::scratch_buf();
        if tab_strip {
          delim.push('-');
        }
        let mut in_dquote = false;
        let mut in_squote_inner = false;
        while let Some(&(_, c)) = chars.peek() {
          match c {
            '\'' if !in_dquote => in_squote_inner = !in_squote_inner,
            '"' if !in_squote_inner => in_dquote = !in_dquote,
            c if (c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '<' | '>')) && !in_dquote && !in_squote_inner => break,
            _ => {}
          }
          // Add to match key only if it's not a quote character
          if c != '\'' && c != '"' {
            delim.push(c);
          }
          output.push(c);
          chars.next();
        }
        if !delim.trim_start_matches('-').is_empty() {
          heredoc_queue.push_back(delim.into());
        }
      }
    }
    _ => output.push(ch),
  });
  output
}

/// The main struct for lexical analysis of shell input.
/// Wraps the source string and a cursor position, as well as some state for handling things like quoting and brace groups.
///
/// This struct is useful for more than just the lex-parse-execute pipeline. A single input will be lexed multiple times in many places throughout the codebase. Examples include the syntax highlighter, the line editor auto-indent logic, the bodies of subshells, etc
///
/// Notes:
/// The first and last lexed token will be an empty token with class `TkRule::Soi` and `TkRule::Eoi` respectively. These tokens must be handled specially if you are using the lexer for internal stuff like the cases mentioned above.
pub(crate) struct LexStream {
  source: Rc<str>,
  pub cursor: usize,
  pos_offset: usize,
  pos: Pos,
  pub name: Rc<str>,
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
  pub fn new(source: Rc<str>, flags: LexFlags) -> Self {
    let flags = flags | LexFlags::FRESH | LexFlags::NEXT_IS_CMD;
    Self {
      flags,
      source,
      name: "<stdin>".into(),
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
  pub fn slice<R: RangeBounds<usize>>(&self, range: R) -> Option<&str> {
    let start = match range.start_bound() {
      Bound::Included(&start) => start,
      Bound::Excluded(&start) => start + 1,
      Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
      Bound::Included(&end) => end + 1,
      Bound::Excluded(&end) => end,
      Bound::Unbounded => self.source.len(),
    };
    self.source.get(start..end)
  }
  pub fn with_name(mut self, name: Rc<str>) -> Self {
    self.name = name;
    self
  }
  pub fn in_brc_grp(&self) -> bool {
    self.brc_grp_depth > 0
  }
  pub fn in_subsh(&self) -> bool {
    self.subsh_depth > 0
  }
  pub fn update_pos(&mut self) {
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
  pub fn update_cursor(&mut self, new_cursor: usize) {
    assert!(new_cursor <= self.source.len());
    self.cursor = new_cursor;
    self.update_pos();
  }
  pub fn inc_cursor(&mut self, amt: usize) {
    self.update_cursor(self.cursor + amt);
  }
  pub fn enter_subsh(&mut self) {
    if self.subsh_depth == 0 {
      self.subsh_start = Some(self.cursor);
    }
    self.subsh_depth += 1;
  }
  pub fn leave_subsh(&mut self) {
    self.subsh_depth -= 1;
    if self.subsh_depth == 0 {
      self.subsh_start = None;
    }
  }
  pub fn enter_brc_grp(&mut self) {
    if self.brc_grp_depth == 0 {
      self.brc_grp_start = Some(self.cursor);
    }
    self.brc_grp_depth += 1;
  }
  pub fn leave_brc_grp(&mut self) {
    self.brc_grp_depth -= 1;
    if self.brc_grp_depth == 0 {
      self.brc_grp_start = None;
    }
  }
  pub fn next_is_cmd(&self) -> bool {
    self.flags.contains(LexFlags::NEXT_IS_CMD)
  }
  /// Set whether the next string token is a command name
  pub fn set_next_is_cmd(&mut self, is: bool) {
    if is {
      self.flags |= LexFlags::NEXT_IS_CMD;
      self.flags &= !LexFlags::NEXT_IS_REDIR;
      self.flags &= !LexFlags::NEXT_IS_FUNC;
    } else {
      self.flags &= !LexFlags::NEXT_IS_CMD;
    }
  }
  #[expect(clippy::too_many_lines)]
  pub fn read_redir(&mut self) -> Option<ShResult<Tk>> {
    assert!(self.cursor <= self.source.len());
    let slice = self.slice(self.cursor..)?;
    let mut pos = self.cursor;
    let mut chars = slice.chars().peekable();
    let mut tk = Tk::default();

    match_loop!(chars.next() => ch, {
      '&' if chars.peek() == Some(&'>') => {
      }
      '>' => {
        if chars.peek() == Some(&'(') {
          return None; // It's a process sub
        }
        pos += 1;
        if let Some('|') = chars.peek() {
          // noclobber force '>|'
          chars.next();
          pos += 1;
          tk = self.get_token(self.cursor..pos, TkRule::Redir);
          break;
        }

        if let Some('>') = chars.peek() {
          chars.next();
          pos += 1;
        }
        let Some('&') = chars.peek() else {
          tk = self.get_token(self.cursor..pos, TkRule::Redir);
          break;
        };

        chars.next();
        pos += 1;

        let mut found_fd = false;
        if chars.peek().is_some_and(|ch| *ch == '-') {
          chars.next();
          found_fd = true;
          pos += 1;
        } else {
          while chars.peek().is_some_and(char::is_ascii_digit) {
            chars.next();
            found_fd = true;
            pos += 1;
          }
        }

        if !found_fd && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
          let span_start = self.cursor;
          return Some(Err(lex_err!(
            self,
            pos,
            span_start..pos,
            "Invalid redirection",
          )));
        }
        tk = self.get_token(self.cursor..pos, TkRule::Redir);
        break;
      }
      '<' => {
        if chars.peek() == Some(&'(') {
          return None; // It's a process sub
        }
        pos += 1;

        match chars.peek() {
          Some('<') => {
            chars.next();
            pos += 1;

            match chars.peek() {
              Some('<') => {
                chars.next();
                pos += 1;
              }

              Some(ch) => {
                let mut ch = *ch;
                // skip whitespace
                while is_field_sep(ch) {
                  let consumed = chars.next().unwrap();
                  pos += consumed.len_utf8();
                  match chars.peek() {
                    Some(next) => ch = *next,
                    None => break, // ran out, handled below
                  }
                }

                if is_field_sep(ch) {
                  // Ran out of input while skipping whitespace, fall through
                } else {
                  let saved_cursor = self.cursor;
                  match self.read_heredoc(pos) {
                    Ok(Some(heredoc_tk)) => {
                      // cursor is set to after the delimiter word;
                      // heredoc_skip is set to after the body
                      pos = self.cursor;
                      self.update_cursor(saved_cursor);
                      tk = heredoc_tk;
                      break;
                    }
                    Ok(None) => {
                      // Incomplete heredoc - restore cursor and fall through
                      self.update_cursor(saved_cursor);
                    }
                    Err(e) => return Some(Err(e)),
                  }
                }
              }
              _ => {
                // No delimiter yet - input is incomplete
                // Fall through to emit the << as a Redir token
              }
            }
          }
          Some('>') => {
            chars.next();
            pos += 1;
            tk = self.get_token(self.cursor..pos, TkRule::Redir);
            break;
          }
          Some('&') => {
            chars.next();
            pos += 1;

            let mut found_fd = false;
            if chars.peek().is_some_and(|ch| *ch == '-') {
              chars.next();
              found_fd = true;
              pos += 1;
            } else {
              while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
                found_fd = true;
                pos += 1;
              }
            }

            if !found_fd && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
              let span_start = self.cursor;
              return Some(Err(lex_err!(
                self,
                pos,
                span_start..pos,
                "Invalid redirection",
              )));
            }
            tk = self.get_token(self.cursor..pos, TkRule::Redir);
            break;
          }
          _ => {}
        }

        tk = self.get_token(self.cursor..pos, TkRule::Redir);
        break;
      }
      '0'..='9' => {
        pos += 1;
        while chars.peek().is_some_and(char::is_ascii_digit) {
          chars.next();
          pos += 1;
        }
      }
      _ => {
        return None;
      }
    });

    if tk == Tk::default() {
      return None;
    }

    self.update_cursor(pos);
    Some(Ok(tk))
  }
  #[expect(clippy::too_many_lines)]
  pub fn read_heredoc(&mut self, mut pos: usize) -> ShResult<Option<Tk>> {
    let source = self.source.clone();
    let slice = source.get(pos..).unwrap_or_default();
    let span_start = pos;
    let mut chars = slice.chars().peekable();
    let mut delim = util::scratch_buf();
    let mut flags = TkFlags::empty();
    let mut first_char = true;
    // Parse the delimiter word, stripping quotes
    while let Some(ch) = chars.next() {
      match ch {
        '-' if first_char => {
          pos += 1;
          flags |= TkFlags::TAB_HEREDOC;
          // skip whitespace
          while chars.peek().is_some_and(|c| is_field_sep(*c)) {
            let c = chars.next().unwrap();
            pos += c.len_utf8();
          }
        }
        '\"' => {
          pos += 1;
          self.quote_state.toggle_double();
          flags |= TkFlags::LIT_HEREDOC;
        }
        '\'' => {
          pos += 1;
          self.quote_state.toggle_single();
          flags |= TkFlags::LIT_HEREDOC;
        }
        _ if self.quote_state.in_quote() => {
          pos += ch.len_utf8();
          delim.push(ch);
        }
        ch if is_hard_sep(ch) => {
          break;
        }
        ch => {
          pos += ch.len_utf8();
          delim.push(ch);
        }
      }
      first_char = false;
    }

    // pos is now right after the delimiter word, this is where
    // the cursor should return so the rest of the line gets lexed
    let cursor_after_delim = pos;

    // Re-slice from cursor_after_delim so iterator and pos are in sync
    // (the old chars iterator consumed the hard_sep without advancing pos)
    let source = self.source.clone();
    let rest = source.get(cursor_after_delim..).unwrap_or_default();
    let mut chars = rest.chars();

    // Scan forward to the newline (or use heredoc_skip from a previous heredoc)
    let body_start = if let Some(skip) = self.heredoc_skip {
      // A previous heredoc on this line already read its body;
      // our body starts where that one ended
      let skip_offset = skip - cursor_after_delim;
      for _ in 0..skip_offset {
        chars.next();
      }
      skip
    } else {
      // Skip the rest of the current line to find where the body begins
      let mut scan = pos;
      let mut found_newline = false;
      while let Some(ch) = chars.next() {
        scan += ch.len_utf8();
        if ch == '\n' {
          found_newline = true;
          break;
        }
      }
      if !found_newline {
        return Err(lex_err!(
          self,
          pos,
          span_start..pos,
          "Heredoc delimiter not found",
        ));
      }
      scan
    };

    pos = body_start;
    let start = pos;

    // Read lines until we find one that matches the delimiter exactly
    let mut line = String::new();
    let mut line_start = pos;
    let mut leading_tabs = true;
    while let Some(ch) = chars.next() {
      pos += ch.len_utf8();
      if leading_tabs && ch == '\t' {
        continue;
      }
      if ch == '\n' {
        let trimmed = line.trim_end_matches('\r');
        if trimmed == delim {
          let start_delim = Box::new(self.get_span(span_start..cursor_after_delim));
          let end_delim = Box::new(self.get_span(line_start..pos));
          let rule = TkRule::HereDoc {
            start_delim,
            end_delim: Some(end_delim),
          };
          let mut tk = self.get_token(start..line_start, rule);
          tk.flags |= TkFlags::IS_HEREDOC | flags;
          log::debug!("heredoc lex: delim={:?} body={:?}", delim, tk.span.as_str());
          self.heredoc_skip = Some(pos);
          self.update_cursor(cursor_after_delim);
          return Ok(Some(tk));
        }
        line.clear();
        leading_tabs = true;
        line_start = pos;
      } else {
        line.push(ch);
      }
    }
    // Check the last line (no trailing newline)
    let trimmed = line.trim_end_matches('\r');
    if trimmed == delim {
      let start_delim = Box::new(self.get_span(span_start..cursor_after_delim));
      let end_delim = Box::new(self.get_span(line_start..pos));
      let rule = TkRule::HereDoc {
        start_delim,
        end_delim: Some(end_delim),
      };
      let mut tk = self.get_token(start..line_start, rule);
      log::debug!("heredoc lex: delim={:?} body={:?}", delim, tk.span.as_str());
      tk.flags |= TkFlags::IS_HEREDOC | flags;
      self.heredoc_skip = Some(pos);
      self.update_cursor(cursor_after_delim);
      return Ok(Some(tk));
    }

    if self.flags.contains(LexFlags::LEX_UNFINISHED_QUOTES) {
      let start_delim = Box::new(self.get_span(span_start..cursor_after_delim));
      let rule = TkRule::HereDoc {
        start_delim,
        end_delim: None,
      };
      let mut tk = self.get_token(start..pos, rule);
      tk.flags |= TkFlags::IS_HEREDOC | flags;
      self.heredoc_skip = Some(pos);
      self.update_cursor(cursor_after_delim);
      Ok(Some(tk))
    } else {
      Err(lex_err!(
        self,
        pos,
        span_start..pos,
        "Heredoc delimiter '{}' not found",
        delim
      ))
    }
  }
  #[expect(clippy::too_many_lines)]
  pub fn read_string(&mut self) -> ShResult<Tk> {
    assert!(self.cursor <= self.source.len());
    let source = self.source.clone();
    let slice = source.get(self.cursor..).unwrap_or_default();
    let mut pos = self.cursor;
    let mut chars = slice.chars().peekable();
    let can_be_subshell = chars.peek() == Some(&'(');

    match_loop!(chars.next() => ch, {
      _ if self.flags.contains(LexFlags::RAW) => {
        if ch.is_whitespace() {
          break;
        }
        pos += ch.len_utf8();
      }
      '\\' if !self.quote_state.in_single() => {
        pos += 1;
        if let Some(ch) = chars.next() {
          pos += ch.len_utf8();
          if ch == '\n' || ch == '\r' {
            while let Some(&c) = chars.peek() {
              if matches!(c, ' ' | '\t') {
                chars.next();
                pos += 1;
              } else {
                break;
              }
            }
          }
        }
      }
      '$' if !self.quote_state.in_single() && chars.peek() == Some(&'\'') => {
        pos += 1;        // '$'
        chars.next();    // consume opening '
        pos += 1;
        // this needs its own branch
        // because escaping a single quote in $'...' is valid
        while let Some(c) = chars.next() {
          pos += c.len_utf8();
          if c == '\\' {
            if let Some(esc) = chars.next() {
              pos += esc.len_utf8();
            }
          } else if c == '\'' {
            break;
          }
        }
      }
      '\'' => {
        pos += 1;
        self.quote_state.toggle_single();
      }
      '`' if !self.quote_state.in_single() => {
        pos += 1;
        match_loop!(chars.next() => ch, {
          '\\' => {
            pos += 1;
            if let Some(next_ch) = chars.next() {
              pos += next_ch.len_utf8();
            }
          }
          '$' if chars.peek() == Some(&'(') => {
            pos += 2;
            chars.next();
            let paren_pos = pos;
            if !scan_parens(&mut chars, &mut pos, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
              return Err(lex_err!(
                self,
                pos,
                paren_pos..paren_pos + 1,
                "Unclosed subshell",
              ));
            }
          }
          '`' => {
            pos += 1;
            break;
          }
          _ => pos += ch.len_utf8(),
        });
      }
      _ if self.quote_state.in_single() => pos += ch.len_utf8(),
      '$' if chars.peek() == Some(&'(') => {
        pos += 2;
        chars.next();
        let paren_pos = pos;
        if !scan_parens(&mut chars, &mut pos, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(
            self,
            pos,
            paren_pos..paren_pos + 1,
            "Unclosed subshell",
          ));
        }
      }
      '$' if chars.peek() == Some(&'{') => {
        pos += 2;
        chars.next();
        let open_pos = pos - 2;
        if !scan_param_exp(&mut chars, &mut pos, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(
            self,
            pos,
            open_pos..open_pos + 2,
            "Unclosed parameter expansion",
          ));
        }
      }
      '"' => {
        pos += 1;
        self.quote_state.toggle_double();
      }
      _ if self.quote_state.in_double() => pos += ch.len_utf8(),
      '<' | '>' if chars.peek() == Some(&'(') => {
        pos += 2;
        chars.next();
        let paren_pos = pos;
        if !scan_parens(&mut chars, &mut pos, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(
            self,
            pos,
            paren_pos..paren_pos + 1,
            "Unclosed subshell",
          ));
        }
      }
      '(' if self.next_is_cmd() && chars.peek() == Some(&')') && pos != self.cursor => {
        // standalone "()" - function definition marker
        // this will be handled below by self.func_paren_lookahead()
        break;
      }
      '(' if self.flags.contains(LexFlags::CASE_PAT_EXPECTED) && can_be_subshell => {
        pos += 1;
        let tk = self.get_token(self.cursor..pos, TkRule::SubshStart);
        self.update_cursor(pos);
        return Ok(tk);
      }
      '(' if (self.next_is_cmd() || chars.peek() == Some(&'(')) && can_be_subshell => {
        pos += 1;
        let mut paren_count = 1;
        let paren_pos = pos;
        let mut flags = TkFlags::IS_CMD;
        if chars.peek() == Some(&'(') {
          // arithmetic
          paren_count += 1;
          chars.next();
          pos += 1;
          flags |= TkFlags::IS_ARITH;
        } else {
          let mut tk = self.get_token(self.cursor..pos, TkRule::SubshStart);
          tk.flags |= TkFlags::IS_CMD;
          self.enter_subsh();
          self.update_cursor(pos);
          self.set_next_is_cmd(true);

          return Ok(tk);
        }
        if !scan_parens(&mut chars, &mut pos, paren_count) && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(
            self,
            pos,
            paren_pos..paren_pos + 1,
            "Unclosed subshell",
          ));
        }
        let mut tk = self.get_token(self.cursor..pos, TkRule::Str);
        tk.flags |= flags;
        self.update_cursor(pos);
        self.set_next_is_cmd(true);
        return Ok(tk);
      }
      '{' if pos == self.cursor && self.next_is_cmd() => {
        pos += 1;
        let mut tk = self.get_token(self.cursor..pos, TkRule::BraceGrpStart);
        tk.flags |= TkFlags::IS_CMD;
        self.enter_brc_grp();
        self.set_next_is_cmd(true);

        self.update_cursor(pos);
        return Ok(tk);
      }
      '}' if pos == self.cursor && self.in_brc_grp() && self.next_is_cmd() => {
        pos += 1;
        let tk = self.get_token(self.cursor..pos, TkRule::BraceGrpEnd);
        self.leave_brc_grp();
        self.set_next_is_cmd(true);
        self.update_cursor(pos);
        return Ok(tk);
      }
      ')' if pos == self.cursor
        && (self.in_subsh() || self.flags.contains(LexFlags::CASE_PAT_EXPECTED)) =>
      {
        pos += 1;
        let tk = self.get_token(self.cursor..pos, TkRule::SubshEnd);
        if self.flags.contains(LexFlags::CASE_PAT_EXPECTED) {
          // this paren closes a case pattern. consume it and continue
          self.flags &= !LexFlags::CASE_PAT_EXPECTED;
        } else {
          self.leave_subsh();
        }
        self.set_next_is_cmd(true);
        self.update_cursor(pos);
        return Ok(tk);
      }
      '=' if chars.peek() == Some(&'(') => {
        pos += 1; // '='
        let mut depth = 1;
        chars.next();
        pos += 1; // '('
                  // looks like an array
        let mut found_end = false;
        match_loop!(chars.next() => arr_ch, {
          '\\' => {
            pos += 1;
            if let Some(next_ch) = chars.next() {
              pos += next_ch.len_utf8();
            }
          }
          '(' => {
            depth += 1;
            pos += 1;
          }
          ')' => {
            depth -= 1;
            pos += 1;
            if depth == 0 {
              found_end = true;
              break;
            }
          }
          _ => pos += arr_ch.len_utf8(),
        });

        if !found_end && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
          return Err(lex_err!(
            self,
            pos,
            pos..pos + 1,
            "Unclosed array assignment",
          ));
        }
      }
      ')' => {
        if !self.in_subsh() && !self.flags.contains(LexFlags::CASE_PAT_EXPECTED) {
          pos += 1;
          let bad_pos = pos;
          self.update_cursor(pos);
          return Err(lex_err!(
            self,
            pos,
            bad_pos..pos,
            "Unexpected ')'",
          ));
        }
        break
      }
      '|' => break, // pipe operator outside of quotes
      _ if is_hard_sep(ch) => break,
      _ => pos += ch.len_utf8(),
    });
    let mut new_tk = self.get_token(self.cursor..pos, TkRule::Str);
    if self.quote_state.in_quote() && !self.flags.contains(LexFlags::LEX_UNFINISHED_QUOTES) {
      self.update_cursor(pos);
      return Err(sherr!(
        ParseErr @ new_tk.span,
        "Unterminated quote",
      ));
    }

    let text = new_tk.span.as_str();
    let is_cmd = self.flags.contains(LexFlags::NEXT_IS_CMD)
      && !self.flags.contains(LexFlags::NEXT_IS_REDIR)
      && !self.flags.contains(LexFlags::CASE_PAT_EXPECTED);
    if is_cmd {
      match text {
        "function" => {
          new_tk.mark(TkFlags::KEYWORD);
          self.flags |= LexFlags::NEXT_IS_FUNC;
        }
        _ if self.func_paren_lookahead(&mut pos) => {
          new_tk.mark(TkFlags::FUNCNAME);
          self.set_next_is_cmd(true);
        }
        "case" => {
          new_tk.mark(TkFlags::KEYWORD);
          self.flags |= LexFlags::EXPECTING_IN | LexFlags::EXPECTING_CASE_IN;
          self.case_depth += 1;
          self.set_next_is_cmd(false);
        }
        "select" | "for" => {
          new_tk.mark(TkFlags::KEYWORD);
          self.flags |= LexFlags::EXPECTING_IN;
          self.set_next_is_cmd(false);
        }
        "in" if self.flags.contains(LexFlags::EXPECTING_IN) => {
          new_tk.mark(TkFlags::KEYWORD);
          self.flags &= !LexFlags::EXPECTING_IN;
          if self.flags.contains(LexFlags::EXPECTING_CASE_IN) {
            self.flags &= !LexFlags::EXPECTING_CASE_IN;
            self.flags |= LexFlags::CASE_PAT_EXPECTED;
          }
        }
        _ if is_keyword(text) => {
          if text == "esac" && self.case_depth > 0 {
            self.case_depth -= 1;
            self.flags &= !LexFlags::CASE_PAT_EXPECTED;
          }
          new_tk.mark(TkFlags::KEYWORD);
        }
        _ if is_assignment(text) => {
          new_tk.mark(TkFlags::ASSIGN);
        }
        _ if is_cmd_sub(text) => {
          new_tk.mark(TkFlags::IS_CMDSUB);
          if self.next_is_cmd() {
            new_tk.mark(TkFlags::IS_CMD);
          }
          self.set_next_is_cmd(false);
        }
        _ if self.flags.contains(LexFlags::NEXT_IS_FUNC) => {
          new_tk.mark(TkFlags::FUNCNAME);
          self.set_next_is_cmd(true);
        }
        _ => {
          new_tk.flags |= TkFlags::IS_CMD;
          if BUILTIN_NAMES.binary_search(&text).is_ok() {
            new_tk.mark(TkFlags::BUILTIN);
          }
          self.set_next_is_cmd(false);
        }
      }
    } else if self.flags.contains(LexFlags::EXPECTING_IN) && text == "in" {
      new_tk.mark(TkFlags::KEYWORD);
      self.flags &= !LexFlags::EXPECTING_IN;
      if self.flags.contains(LexFlags::EXPECTING_CASE_IN) {
        self.flags &= !LexFlags::EXPECTING_CASE_IN;
        self.flags |= LexFlags::CASE_PAT_EXPECTED;
      }
    } else if text == "esac" && self.case_depth > 0 {
      // `esac` reached in pattern position (empty case body or right after `;;`).
      // The is_cmd block above is short-circuited by CASE_PAT_EXPECTED,
      // so do the keyword recognition and depth bookkeeping here.
      new_tk.mark(TkFlags::KEYWORD);
      self.case_depth -= 1;
      self.flags &= !LexFlags::CASE_PAT_EXPECTED;
    } else if is_cmd_sub(text) {
      new_tk.mark(TkFlags::IS_CMDSUB);
    }
    self.update_cursor(pos);
    Ok(new_tk)
  }
  pub fn func_paren_lookahead(&mut self, pos: &mut usize) -> bool {
    let saved_pos = *pos;
    let slice = self.slice(*pos..).unwrap_or_default();
    let mut chars = slice.chars().peekable();
    match_loop!(chars.next() => ch, {
      ' ' | '\t' => {
        *pos += 1;
      }
      '(' => {
        *pos += 1;

        if chars.next() == Some(')') {
          *pos += 1;
          self.update_cursor(*pos);
          return true;
        }
        // Not "()" - restore pos
        *pos = saved_pos;
        return false;
      }
      _ => {
        *pos = saved_pos;
        return false;
      }
    });
    *pos = saved_pos;
    false
  }
  pub fn get_span(&mut self, range: Range<usize>) -> Span {
    self.update_pos();
    Span::new(range, self.source.clone()).at(self.pos)
  }
  pub fn get_token(&mut self, range: Range<usize>, class: TkRule) -> Tk {
    let mut span = self.get_span(range);
    span.rename(self.name.clone());
    Tk::new(class, span)
  }
}

impl Iterator for LexStream {
  type Item = ShResult<Tk>;
  #[expect(clippy::too_many_lines)]
  fn next(&mut self) -> Option<Self::Item> {
    assert!(self.cursor <= self.source.len());
    // We are at the end of the input
    if self.flags.contains(LexFlags::STALE) {
      return None;
    }

    if self.cursor == self.source.len() {
      // Return the Eoi token
      if self.in_brc_grp() && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
        let start = self.brc_grp_start.unwrap_or(self.cursor.saturating_sub(1));
        self.flags |= LexFlags::STALE;
        return Err(sherr!(
            ParseErr @ self.get_span(start..self.cursor),
            "Unclosed brace group",
        ))
        .into();
      }
      if self.in_subsh() && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
        let start = self.subsh_start.unwrap_or(self.cursor.saturating_sub(1));
        self.flags |= LexFlags::STALE;
        return Err(sherr!(
            ParseErr @ self.get_span(start..self.cursor),
            "Unclosed subshell",
        ))
        .into();
      }
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

    // If we are just reading raw words, short circuit here
    // Used for word splitting variable values
    if self.flags.contains(LexFlags::RAW) {
      return Some(self.read_string());
    }

    loop {
      let pos = self.cursor;
      if self.slice(pos..pos + 2) == Some("\\\n") || self.slice(pos..pos + 3) == Some("\\\r\n") {
        self.inc_cursor(2);
      } else if pos < self.source.len() && is_field_sep(get_char(&self.source, pos).unwrap()) {
        self.inc_cursor(1);
      } else {
        break;
      }
    }

    if self.cursor == self.source.len() {
      if self.in_brc_grp() && !self.flags.contains(LexFlags::LEX_UNFINISHED_STRUCTURES) {
        let start = self.brc_grp_start.unwrap_or(self.cursor.saturating_sub(1));
        self.flags |= LexFlags::STALE;
        return Err(sherr!(
          ParseErr @ self.get_span(start..self.cursor),
          "Unclosed brace group",
        ))
        .into();
      }
      return None;
    }

    let token = match get_char(&self.source, self.cursor).unwrap() {
      '\r' | '\n' | ';' => {
        let ch = get_char(&self.source, self.cursor).unwrap();
        let ch_idx = self.cursor;
        self.inc_cursor(1);
        let mut heredoc_skipped = false;
        self.set_next_is_cmd(true);

        // If a heredoc was parsed on this line, skip past the body
        // Only on newline - ';' is a command separator within the same line
        if (ch == '\n' || ch == '\r')
          && let Some(skip) = self.heredoc_skip.take()
        {
          heredoc_skipped = true;
          self.update_cursor(skip);
        }

        match_loop!(get_char(&self.source, self.cursor) => ch, {
          '\\' if get_char(&self.source, self.cursor + 1) == Some('\n') => {
            self.update_cursor((self.cursor + 2).min(self.source.len()));
          }
          _ if is_hard_sep(ch) => {
            self.inc_cursor(1);
            // If we just consumed a newline and there's a pending heredoc, skip past the body
            if (ch == '\n' || ch == '\r')
              && let Some(skip) = self.heredoc_skip.take()
            {
              heredoc_skipped = true;
              self.update_cursor(skip);
            }
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
      '#'
        if !self.flags.contains(LexFlags::INTERACTIVE)
          || Shed::shopts(|s| s.core.interactive_comments) =>
      {
        let ch_idx = self.cursor;
        self.inc_cursor(1);

        while let Some(ch) = get_char(&self.source, self.cursor) {
          if ch == '\n' {
            break;
          }
          self.inc_cursor(ch.len_utf8());
        }

        if self.flags.contains(LexFlags::LEX_UNFINISHED) {
          self.get_token(ch_idx..self.cursor, TkRule::Comment)
        } else {
          return self.next();
        }
      }
      '!'
        if self.next_is_cmd()
          && get_char(&self.source, self.cursor + 1)
            .is_none_or(|c| c.is_whitespace() || matches!(c, ';' | '|' | '&')) =>
      {
        self.inc_cursor(1);
        let tk_type = TkRule::Bang;

        let mut tk = self.get_token((self.cursor - 1)..self.cursor, tk_type);
        tk.flags |= TkFlags::KEYWORD;
        tk
      }
      '|' => {
        let ch_idx = self.cursor;
        self.inc_cursor(1);
        self.set_next_is_cmd(true);

        let tk_type = if let Some('|') = get_char(&self.source, self.cursor) {
          self.inc_cursor(1);
          TkRule::Or
        } else if let Some('&') = get_char(&self.source, self.cursor) {
          self.inc_cursor(1);
          TkRule::ErrPipe
        } else {
          TkRule::Pipe
        };

        self.get_token(ch_idx..self.cursor, tk_type)
      }
      '&' => {
        let ch_idx = self.cursor;
        self.inc_cursor(1);
        self.set_next_is_cmd(true);
        let mut flags = TkFlags::empty();

        let tk_type = match get_char(&self.source, self.cursor) {
          Some('&') => {
            self.inc_cursor(1);
            TkRule::And
          }
          Some('|') => {
            self.inc_cursor(1);
            TkRule::ErrPipe
          }
          Some('>') => {
            self.inc_cursor(1);
            let append = matches!(get_char(&self.source, self.cursor), Some('>'));
            if append {
              self.inc_cursor(1);
            }

            flags |= TkFlags::REDIR_ALL;
            self.flags |= LexFlags::NEXT_IS_REDIR;
            TkRule::Redir
          }
          _ => TkRule::Bg,
        };

        let mut tk = self.get_token(ch_idx..self.cursor, tk_type);
        tk.flags |= flags;
        tk
      }
      _ => {
        if let Some(tk_result) = self.read_redir() {
          let tk = match tk_result {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
          };
          // we gotta check to see if this wants a file target or not
          // if already points at a number or has '-', it doesn't.
          let dup_style = tk
            .span
            .as_str()
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_digit() || c == '-');

          if dup_style {
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
    Some(Ok(token))
  }
}

pub fn get_char(src: &str, idx: usize) -> Option<char> {
  src.get(idx..)?.chars().next()
}

pub fn is_assignment(text: &str) -> bool {
  let mut chars = text.chars();

  match_loop!(chars.next() => ch, {
    '\\' => {
      chars.next();
    }
    '=' => return true,
    _ => continue,
  });
  false
}

/// Is whitespace or a semicolon
pub fn is_hard_sep(ch: char) -> bool {
  matches!(ch, ' ' | '\t' | '\n' | ';')
}

/// Is whitespace, but not a newline
pub fn is_field_sep(ch: char) -> bool {
  matches!(ch, ' ' | '\t')
}

pub fn is_keyword(slice: &str) -> bool {
  KEYWORDS.binary_search(&slice).is_ok()
}

pub fn is_cmd_sub(slice: &str) -> bool {
  slice.starts_with("$(") && ends_with_unescaped(slice, ")")
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::rc::Rc;

  fn lex_classes(src: &str) -> Vec<TkRule> {
    let rc: Rc<str> = src.into();
    LexStream::new(rc, LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .filter(|t| !matches!(t.class, TkRule::Soi | TkRule::Eoi))
      .map(|t| t.class)
      .collect()
  }

  fn lex_first_nontrivial_text(src: &str) -> String {
    let rc: Rc<str> = src.into();
    LexStream::new(rc, LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .find(|t| !matches!(t.class, TkRule::Soi | TkRule::Eoi | TkRule::Sep))
      .map(|t| t.span.as_str().to_string())
      .unwrap_or_default()
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
}
