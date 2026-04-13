use std::{
  fmt::Display,
  iter::Peekable,
  ops::{Bound, Range, RangeBounds},
  rc::Rc,
  str::Chars,
};

use bitflags::bitflags;

use crate::{
  builtin::BUILTINS,
  libsh::{error::ShResult, strops::{QuoteState, ends_with_unescaped, scan_braces, scan_parens}},
  match_loop, sherr,
};

pub const KEYWORDS: [&str; 18] = [
  "if", "then", "elif", "else", "fi", "while", "until", "select", "for", "in", "do", "done",
  "case", "esac", "[[", "]]", "!", "time",
];

pub const OPENERS: [&str; 6] = ["if", "while", "until", "for", "select", "case"];

pub const MIDDLES: [&str; 2] = ["elif", "else"];

pub const CLOSERS: [&str; 5] = ["fi", "done", "esac", "}", ";;"];

pub fn not_marker(tk: &ShResult<Tk>) -> bool {
  tk.is_err()
    || !tk
      .as_ref()
      .is_ok_and(|tk| matches!(tk.class, TkRule::SOI | TkRule::EOI))
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
  pub fn rename(&mut self, name: Rc<str>) {
    self.name = name;
  }
}

impl Display for SpanSource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.name)
  }
}

#[derive(Clone, PartialEq, Default, Debug)]
/// A slice of some source text. Ultimately wraps an Rc<String>, which means these are cheap to clone.
/// Used extensively throughout the codebase for slicing shell input for various reasons (error reporting, tab completion, etc)
pub struct Span {
  range: Range<usize>,
  source: SpanSource,
}

impl Span {
  /// New `Span`. Wraps a range and a string that it refers to.
  pub fn new(range: Range<usize>, source: Rc<str>) -> Self {
    let source = SpanSource {
      name: "<stdin>".into(),
      content: source,
    };
    Span { range, source }
  }
  pub fn from_span_source(range: Range<usize>, source: SpanSource) -> Self {
    Span { range, source }
  }
  pub fn rename(&mut self, name: Rc<str>) {
    self.source.name = name;
  }
  pub fn with_name(mut self, name: Rc<str>) -> Self {
    self.source.name = name;
    self
  }
  pub fn line_and_col(&self) -> (usize, usize) {
    let content = &self.source.content[..self.range.start];
    let line = content.bytes().filter(|&b| b == b'\n').count();
    let col = content.len() - content.rfind('\n').map(|i| i + 1).unwrap_or(0);
    (line, col)
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
}

impl ariadne::Span for Span {
  type SourceId = SpanSource;

  fn source(&self) -> &Self::SourceId {
    &self.source
  }

  fn start(&self) -> usize {
    self.range.start
  }

  fn end(&self) -> usize {
    self.range.end
  }
}

#[derive(Clone, PartialEq, Debug)]
/// The "class" of a token, i.e. what kind of token it is. This is the result of lexing, and is used during parsing to determine how to interpret the token.
pub enum TkRule {
  /// A normal string token. By far the most common type of token. Used for command names, keywords, arguments, basically any "words".
  /// String tokens are further disambiguated using the TkFlags on the token itself, which can mark a string token as a keyword, a command name, a subshell, etc.
  Str,

  /// The start of a given input.
  SOI,
  /// The end of a given input.
  EOI,

  Null,
  Pipe,
  ErrPipe,
  And,
  Or,
  Bang,
  Bg,
  Sep,
  Redir,
  CasePattern,
  BraceGrpStart,
  BraceGrpEnd,
  Comment,

  /// A special token class used for tokens that are the result of expansion, not direct lexing. The contained Vec<String> is the result of splitting the expanded text into words according to shell field splitting rules. This is used to allow expansions to produce multiple tokens, which is necessary for things like `echo *` where the `*` may expand to multiple filenames.
  Expanded {
    exp: Vec<String>,
  },
}

impl Default for TkRule {
  fn default() -> Self {
    TkRule::Null
  }
}

#[derive(Clone, Debug, PartialEq, Default)]
/// A single input token. Wraps three things:
/// * A `TkRule` which identifies what kind of token it is
/// * A `Span` which represents the slice of the original input the token refers to
/// * `TkFlags` which is a bitfield containing simple metadata
///
/// Generally speaking, these are very cheap to clone. The only time cloning a `Tk` is a heavy operation
/// is if the wrapped `TkRule` is `TkRule::Expanded`, which contains a `Vec<String>` that needs to be cloned.
/// However, `TkRule::Expanded` is never created through lexing, so it is very rare that a cloned token will have this rule.
/// Therefore, you can generally consider cloning a token to be effectively as cheap as cloning an Rc<T>.
///
/// `TkRule::Expanded` is only created during token expansion, which generally happens much later in an execution cycle.
pub struct Tk {
  pub class: TkRule,
  pub span: Span,
  pub flags: TkFlags,
}

// There's one impl here and then another in expand.rs which has the expansion
// logic
impl Tk {
  pub fn new(class: TkRule, span: Span) -> Self {
    Self {
      class,
      span,
      flags: TkFlags::empty(),
    }
  }
  pub fn as_str(&self) -> &str {
    self.span.as_str()
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

  pub fn is_opener(&self) -> bool {
    OPENERS.contains(&self.as_str())
      || matches!(self.class, TkRule::BraceGrpStart)
      || matches!(self.class, TkRule::CasePattern)
  }
  pub fn is_middle(&self) -> bool {
    MIDDLES.contains(&self.as_str())
  }

  pub fn is_closer(&self) -> bool {
		CLOSERS.contains(&self.as_str())
  }

  pub fn is_closer_for(&self, other: &Tk) -> bool {
    if (matches!(other.class, TkRule::BraceGrpStart) && matches!(self.class, TkRule::BraceGrpEnd))
      || (matches!(other.class, TkRule::CasePattern) && self.has_double_semi())
    {
      return true;
    }
    match other.as_str() {
      "for" | "while" | "until" => matches!(self.as_str(), "done"),
      "if" => matches!(self.as_str(), "fi"),
      "case" => matches!(self.as_str(), "esac"),
      _ => false,
    }
  }
}

impl Display for Tk {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match &self.class {
      TkRule::Expanded { exp } => write!(f, "{}", exp.join(" ")),
      _ => write!(f, "{}", self.span.as_str()),
    }
  }
}

bitflags! {
  #[derive(Debug,Clone,Copy,PartialEq,Default)]
  pub struct TkFlags: u32 {
    const KEYWORD      = 0b0000000000000001;
    const OPENER       = 0b0000000000000010;
    const IS_CMD       = 0b0000000000000100;
    const IS_SUBSH     = 0b0000000000001000;
    const IS_CMDSUB    = 0b0000000000010000;
    const IS_OP        = 0b0000000000100000;
    const ASSIGN       = 0b0000000001000000;
    const BUILTIN      = 0b0000000010000000;
    const IS_PROCSUB   = 0b0000000100000000;
    const IS_HEREDOC   = 0b0000001000000000;
    const LIT_HEREDOC  = 0b0000010000000000;
    const TAB_HEREDOC  = 0b0000100000000000;
		const IS_ARITH     = 0b0001000000000000;
  }
}

bitflags! {
  #[derive(Debug, Clone, Copy)]
  pub struct LexFlags: u32 {
    /// The lexer is operating in interactive mode
    const INTERACTIVE    = 0b0000000001;
    /// Allow unfinished input
    const LEX_UNFINISHED = 0b0000000010;
    /// The next string-type token is a command name
    const NEXT_IS_CMD    = 0b0000000100;
    /// We are in a quotation, so quoting rules apply
    const IN_QUOTE       = 0b0000001000;
    /// Only lex strings; used in expansions
    const RAW            = 0b0000010000;
    /// The lexer has not produced any tokens yet
    const FRESH          = 0b0000100000;
    /// The lexer has no more tokens to produce
    const STALE          = 0b0001000000;
    const EXPECTING_IN   = 0b0010000000;
    const NEXT_IS_REDIR  = 0b0100000000;
  }
}

pub fn clean_input(input: &str) -> String {
  let mut chars = input.chars().peekable();
  let mut output = String::new();
  match_loop!(chars.next() => ch, {
    '\\' if chars.peek() == Some(&'\n') => {
      chars.next();
			while chars.peek().is_some_and(|c| c.is_whitespace() && *c != '\n') {
				chars.next();
			}
    }
    '\r' => {
      if chars.peek() == Some(&'\n') {
        chars.next();
      }
      output.push('\n');
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
/// The first and last lexed token will be an empty token with class TkRule::SOI and TkRule::EOI respectively. These tokens must be handled specially if you are using the lexer for internal stuff like the cases mentioned above.
pub struct LexStream {
  source: Rc<str>,
  pub cursor: usize,
  pub name: Rc<str>,
  quote_state: QuoteState,
  brc_grp_depth: usize,
  brc_grp_start: Option<usize>,
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
      quote_state: QuoteState::default(),
      brc_grp_depth: 0,
      brc_grp_start: None,
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
  pub fn slice_from_cursor(&self) -> Option<&str> {
    self.slice(self.cursor..)
  }
  pub fn in_brc_grp(&self) -> bool {
    self.brc_grp_depth > 0
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
    } else {
      self.flags &= !LexFlags::NEXT_IS_CMD;
    }
  }
  pub fn read_redir(&mut self) -> Option<ShResult<Tk>> {
    assert!(self.cursor <= self.source.len());
    let slice = self.slice(self.cursor..)?.to_string();
    let mut pos = self.cursor;
    let mut chars = slice.chars().peekable();
    let mut tk = Tk::default();

    match_loop!(chars.next() => ch, {
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
          while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            chars.next();
            found_fd = true;
            pos += 1;
          }
        }

        if !found_fd && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
          let span_start = self.cursor;
          self.cursor = pos;
          return Some(Err(sherr!(
                ParseErr @ Span::new(span_start..pos, self.source.clone()),
                "Invalid redirection",
          )));
        } else {
          tk = self.get_token(self.cursor..pos, TkRule::Redir);
          break;
        }
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
                while is_field_sep(ch) {
                  let Some(next_ch) = chars.next() else {
                    // Incomplete input - fall through to emit << as Redir
                    break;
                  };
                  pos += next_ch.len_utf8();
                  ch = next_ch;
                }

                if is_field_sep(ch) {
                  // Ran out of input while skipping whitespace - fall through
                } else {
                  let saved_cursor = self.cursor;
                  match self.read_heredoc(pos) {
                    Ok(Some(heredoc_tk)) => {
                      // cursor is set to after the delimiter word;
                      // heredoc_skip is set to after the body
                      pos = self.cursor;
                      self.cursor = saved_cursor;
                      tk = heredoc_tk;
                      break;
                    }
                    Ok(None) => {
                      // Incomplete heredoc - restore cursor and fall through
                      self.cursor = saved_cursor;
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
              while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                chars.next();
                found_fd = true;
                pos += 1;
              }
            }

            if !found_fd && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
              let span_start = self.cursor;
              self.cursor = pos;
              return Some(Err(sherr!(
                    ParseErr @ Span::new(span_start..pos, self.source.clone()),
                    "Invalid redirection",
              )));
            } else {
              tk = self.get_token(self.cursor..pos, TkRule::Redir);
              break;
            }
          }
          _ => {}
        }

        tk = self.get_token(self.cursor..pos, TkRule::Redir);
        break;
      }
      '0'..='9' => {
        pos += 1;
        while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
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

    self.cursor = pos;
    Some(Ok(tk))
  }
  pub fn read_heredoc(&mut self, mut pos: usize) -> ShResult<Option<Tk>> {
    let slice = self.slice(pos..).unwrap_or_default().to_string();
    let mut chars = slice.chars();
    let mut delim = String::new();
    let mut flags = TkFlags::empty();
    let mut first_char = true;
    // Parse the delimiter word, stripping quotes
    while let Some(ch) = chars.next() {
      match ch {
        '-' if first_char => {
          pos += 1;
          flags |= TkFlags::TAB_HEREDOC;
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

    // pos is now right after the delimiter word - this is where
    // the cursor should return so the rest of the line gets lexed
    let cursor_after_delim = pos;

    // Re-slice from cursor_after_delim so iterator and pos are in sync
    // (the old chars iterator consumed the hard_sep without advancing pos)
    let rest = self
      .slice(cursor_after_delim..)
      .unwrap_or_default()
      .to_string();
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
        if self.flags.contains(LexFlags::LEX_UNFINISHED) {
          return Ok(None);
        } else {
          return Err(sherr!(
            ParseErr @ Span::new(pos..pos, self.source.clone()),
            "Heredoc delimiter not found",
          ));
        }
      }
      scan
    };

    pos = body_start;
    let start = pos;

    // Read lines until we find one that matches the delimiter exactly
    let mut line = String::new();
    let mut line_start = pos;
    while let Some(ch) = chars.next() {
      pos += ch.len_utf8();
      if ch == '\n' {
        let trimmed = line.trim_end_matches('\r');
        if trimmed == delim {
          let mut tk = self.get_token(start..line_start, TkRule::Redir);
          tk.flags |= TkFlags::IS_HEREDOC | flags;
          self.heredoc_skip = Some(pos);
          self.cursor = cursor_after_delim;
          return Ok(Some(tk));
        }
        line.clear();
        line_start = pos;
      } else {
        line.push(ch);
      }
    }
    // Check the last line (no trailing newline)
    let trimmed = line.trim_end_matches('\r');
    if trimmed == delim {
      let mut tk = self.get_token(start..line_start, TkRule::Redir);
      tk.flags |= TkFlags::IS_HEREDOC | flags;
      self.heredoc_skip = Some(pos);
      self.cursor = cursor_after_delim;
      return Ok(Some(tk));
    }

    if !self.flags.contains(LexFlags::LEX_UNFINISHED) {
      Err(sherr!(
        ParseErr @ Span::new(start..pos, self.source.clone()),
        "Heredoc delimiter '{delim}' not found"
      ))
    } else {
      Ok(None)
    }
  }
  pub fn read_string(&mut self) -> ShResult<Tk> {
    assert!(self.cursor <= self.source.len());
    let slice = self.slice_from_cursor().unwrap().to_string();
    let mut pos = self.cursor;
    let mut chars = slice.chars().peekable();
    let can_be_subshell = chars.peek() == Some(&'(');

    if self.case_depth > 0
      && let Some(count) = case_pat_lookahead(chars.clone())
    {
      pos += count;
      let casepat_tk = self.get_token(self.cursor..pos, TkRule::CasePattern);
      self.cursor = pos;
      self.set_next_is_cmd(true);
      return Ok(casepat_tk);
    }

    match_loop!(chars.next() => ch, {
      _ if self.flags.contains(LexFlags::RAW) => {
        if ch.is_whitespace() {
          break;
        } else {
          pos += ch.len_utf8()
        }
      }
      '\\' => {
        pos += 1;
        if let Some(ch) = chars.next() {
          pos += ch.len_utf8();
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
            if !scan_parens(&mut chars, &mut pos, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
              self.cursor = pos;
              return Err(sherr!(
                  ParseErr @ Span::new(paren_pos..paren_pos + 1, self.source.clone()),
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
        if !scan_parens(&mut chars, &mut pos, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
          self.cursor = pos;
          return Err(sherr!(
              ParseErr @ Span::new(paren_pos..paren_pos + 1, self.source.clone()),
              "Unclosed subshell",
          ));
        }
      }
      '$' if chars.peek() == Some(&'{') => {
        pos += 2;
        chars.next();
				if !scan_braces(&mut chars, &mut pos, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
					self.cursor = pos;
					return Err(sherr!(
							ParseErr @ Span::new(pos..pos + 1, self.source.clone()),
							"Unclosed parameter expansion",
					));
				}
      }
      '"' => {
        pos += 1;
        self.quote_state.toggle_double();
      }
      _ if self.quote_state.in_double() => pos += ch.len_utf8(),
      '<' if chars.peek() == Some(&'(') => {
        pos += 2;
        chars.next();
        let paren_pos = pos;
        if !scan_parens(&mut chars, &mut pos, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
          self.cursor = pos;
          return Err(sherr!(
              ParseErr @ Span::new(paren_pos..paren_pos + 1, self.source.clone()),
              "Unclosed subshell",
          ));
        }
      }
      '>' if chars.peek() == Some(&'(') => {
        pos += 2;
        chars.next();
        let paren_pos = pos;
        if !scan_parens(&mut chars, &mut pos, 1) && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
          self.cursor = pos;
          return Err(sherr!(
              ParseErr @ Span::new(paren_pos..paren_pos + 1, self.source.clone()),
              "Unclosed subshell",
          ));
        }
      }
      '(' if can_be_subshell && chars.peek() == Some(&')') => {
        // standalone "()" - function definition marker
        pos += 2;
        chars.next();
        let mut tk = self.get_token(self.cursor..pos, TkRule::Str);
        tk.mark(TkFlags::KEYWORD);
        self.cursor = pos;
        self.set_next_is_cmd(true);
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
					//subshell
					flags |= TkFlags::IS_SUBSH;
				}
        if !scan_parens(&mut chars, &mut pos, paren_count) && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
          self.cursor = pos;
          return Err(sherr!(
              ParseErr @ Span::new(paren_pos..paren_pos + 1, self.source.clone()),
              "Unclosed subshell",
          ));
        }
        let mut tk = self.get_token(self.cursor..pos, TkRule::Str);
        tk.flags |= flags;
        self.cursor = pos;
        self.set_next_is_cmd(true);
        return Ok(tk);
      }
      '{' if pos == self.cursor && self.next_is_cmd() => {
        pos += 1;
        let mut tk = self.get_token(self.cursor..pos, TkRule::BraceGrpStart);
        tk.flags |= TkFlags::IS_CMD;
        self.enter_brc_grp();
        self.set_next_is_cmd(true);

        self.cursor = pos;
        return Ok(tk);
      }
      '}' if pos == self.cursor && self.in_brc_grp() && self.next_is_cmd() => {
        pos += 1;
        let tk = self.get_token(self.cursor..pos, TkRule::BraceGrpEnd);
        self.leave_brc_grp();
        self.set_next_is_cmd(true);
        self.cursor = pos;
        return Ok(tk);
      }
      '=' if chars.peek() == Some(&'(') => {
        pos += 1; // '='
        let mut depth = 1;
        chars.next();
        pos += 1; // '('
                  // looks like an array
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
              break;
            }
          }
          _ => pos += arr_ch.len_utf8(),
        });
      }
      _ if is_hard_sep(ch) => break,
      _ => pos += ch.len_utf8(),
    });
    let mut new_tk = self.get_token(self.cursor..pos, TkRule::Str);
    if self.quote_state.in_quote() && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
      self.cursor = pos;
      return Err(sherr!(
        ParseErr @ new_tk.span,
        "Unterminated quote",
      ));
    }

    let text = new_tk.span.as_str();
    let is_cmd =
      self.flags.contains(LexFlags::NEXT_IS_CMD) && !self.flags.contains(LexFlags::NEXT_IS_REDIR);
    if is_cmd {
      match text {
        "case" | "select" | "for" => {
          new_tk.mark(TkFlags::KEYWORD);
          self.flags |= LexFlags::EXPECTING_IN;
          self.case_depth += 1;
          self.set_next_is_cmd(false);
        }
        "in" if self.flags.contains(LexFlags::EXPECTING_IN) => {
          new_tk.mark(TkFlags::KEYWORD);
          self.flags &= !LexFlags::EXPECTING_IN;
        }
        _ if is_keyword(text) => {
          if text == "esac" && self.case_depth > 0 {
            self.case_depth -= 1;
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
        _ => {
          new_tk.flags |= TkFlags::IS_CMD;
          if BUILTINS.contains(&text) {
            new_tk.mark(TkFlags::BUILTIN);
          }
          self.set_next_is_cmd(false);
        }
      }
    } else if self.flags.contains(LexFlags::EXPECTING_IN) && text == "in" {
      new_tk.mark(TkFlags::KEYWORD);
      self.flags &= !LexFlags::EXPECTING_IN;
    } else if is_cmd_sub(text) {
      new_tk.mark(TkFlags::IS_CMDSUB)
    }
    self.cursor = pos;
    Ok(new_tk)
  }
  pub fn get_token(&self, range: Range<usize>, class: TkRule) -> Tk {
    let mut span = Span::new(range, self.source.clone());
    span.rename(self.name.clone());
    Tk::new(class, span)
  }
}

impl Iterator for LexStream {
  type Item = ShResult<Tk>;
  fn next(&mut self) -> Option<Self::Item> {
    assert!(self.cursor <= self.source.len());
    // We are at the end of the input
    if self.cursor == self.source.len() {
      if self.flags.contains(LexFlags::STALE) {
        // We've already returned an EOI token, nothing left to do
        return None;
      } else {
        // Return the EOI token
        if self.in_brc_grp() && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
          let start = self.brc_grp_start.unwrap_or(self.cursor.saturating_sub(1));
          self.flags |= LexFlags::STALE;
          return Err(sherr!(
            ParseErr @ Span::new(start..self.cursor, self.source.clone()),
            "Unclosed brace group",
          ))
          .into();
        }
        let token = self.get_token(self.cursor..self.cursor, TkRule::EOI);
        self.flags |= LexFlags::STALE;
        return Some(Ok(token));
      }
    }
    // Return the SOI token
    if self.flags.contains(LexFlags::FRESH) {
      self.flags &= !LexFlags::FRESH;
      let token = self.get_token(self.cursor..self.cursor, TkRule::SOI);
      return Some(Ok(token));
    }

    // If we are just reading raw words, short circuit here
    // Used for word splitting variable values
    if self.flags.contains(LexFlags::RAW) {
      return Some(self.read_string());
    }

    loop {
      let pos = self.cursor;
      if self.slice(pos..pos + 2) == Some("\\\n") {
        self.cursor += 2;
      } else if pos < self.source.len() && is_field_sep(get_char(&self.source, pos).unwrap()) {
        self.cursor += 1;
      } else {
        break;
      }
    }

    if self.cursor == self.source.len() {
      if self.in_brc_grp() && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
        let start = self.brc_grp_start.unwrap_or(self.cursor.saturating_sub(1));
        return Err(sherr!(
          ParseErr @ Span::new(start..self.cursor, self.source.clone()),
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
        self.cursor += 1;
        self.set_next_is_cmd(true);

        // If a heredoc was parsed on this line, skip past the body
        // Only on newline - ';' is a command separator within the same line
        if (ch == '\n' || ch == '\r')
          && let Some(skip) = self.heredoc_skip.take()
        {
          self.cursor = skip;
        }

        match_loop!(get_char(&self.source, self.cursor) => ch, {
          '\\' if get_char(&self.source, self.cursor + 1) == Some('\n') => {
            self.cursor = (self.cursor + 2).min(self.source.len());
          }
          _ if is_hard_sep(ch) => {
            self.cursor += 1;
          }
          _ => break,
        });

        self.get_token(ch_idx..self.cursor, TkRule::Sep)
      }
      '#'
        if !self.flags.contains(LexFlags::INTERACTIVE)
          || crate::state::read_shopts(|s| s.core.interactive_comments) =>
      {
        let ch_idx = self.cursor;
        self.cursor += 1;

        while let Some(ch) = get_char(&self.source, self.cursor) {
          if ch == '\n' {
            break;
          }
          self.cursor += ch.len_utf8();
        }

        if self.flags.contains(LexFlags::LEX_UNFINISHED) {
          self.get_token(ch_idx..self.cursor, TkRule::Comment)
        } else {
          return self.next();
        }
      }
      '!' if self.next_is_cmd() => {
        self.cursor += 1;
        let tk_type = TkRule::Bang;

        let mut tk = self.get_token((self.cursor - 1)..self.cursor, tk_type);
        tk.flags |= TkFlags::KEYWORD;
        tk
      }
      '|' => {
        let ch_idx = self.cursor;
        self.cursor += 1;
        self.set_next_is_cmd(true);

        let tk_type = if let Some('|') = get_char(&self.source, self.cursor) {
          self.cursor += 1;
          TkRule::Or
        } else if let Some('&') = get_char(&self.source, self.cursor) {
          self.cursor += 1;
          TkRule::ErrPipe
        } else {
          TkRule::Pipe
        };

        self.get_token(ch_idx..self.cursor, tk_type)
      }
      '&' => {
        let ch_idx = self.cursor;
        self.cursor += 1;
        self.set_next_is_cmd(true);

        let tk_type = if let Some('&') = get_char(&self.source, self.cursor) {
          self.cursor += 1;
          TkRule::And
        } else {
          TkRule::Bg
        };
        self.get_token(ch_idx..self.cursor, tk_type)
      }
      _ => {
        if let Some(tk) = self.read_redir() {
          self.flags |= LexFlags::NEXT_IS_REDIR;
          match tk {
            Ok(tk) => tk,
            Err(e) => return Some(Err(e)),
          }
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

/// Is '|', '&', '>', or '<'
pub fn is_op(ch: char) -> bool {
  matches!(ch, '|' | '&' | '>' | '<')
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
  KEYWORDS.contains(&slice)
    || (ends_with_unescaped(slice, "()") && !ends_with_unescaped(slice, "=()"))
}

pub fn is_cmd_sub(slice: &str) -> bool {
  slice.starts_with("$(") && ends_with_unescaped(slice, ")")
}


pub fn case_pat_lookahead(mut chars: Peekable<Chars>) -> Option<usize> {
  let mut pos = 0;
  let mut qt_state = QuoteState::default();
  while let Some(ch) = chars.next() {
    pos += ch.len_utf8();
    match ch {
      _ if qt_state.outside() && is_hard_sep(ch) => return None,
      '\\' => {
        if let Some(esc) = chars.next() {
          pos += esc.len_utf8();
        }
      }
      '$' if qt_state.outside() && chars.peek() == Some(&'\'') => {
        // $'...' ANSI-C quoting - skip through to closing quote
        chars.next(); // consume opening '
        pos += 1;
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
      '$' if qt_state.outside() && chars.peek() == Some(&'(') => {
        // $(...) or $((...)) - skip through balanced parens
        chars.next(); // consume opening '('
        pos += 1;
        let mut depth = 1usize;
        while let Some(c) = chars.next() {
          pos += c.len_utf8();
          match c {
            '(' => depth += 1,
            ')' => {
              depth -= 1;
              if depth == 0 { break; }
            }
            '\\' => { if let Some(esc) = chars.next() { pos += esc.len_utf8(); } }
            _ => {}
          }
        }
      }
      '\'' => {
        qt_state.toggle_single();
      }
      '"' => {
        qt_state.toggle_double();
      }
      ')' if qt_state.outside() => return Some(pos),
      '(' if qt_state.outside() => return None,
      _ => { /* continue */ }
    }
  }
  None
}
