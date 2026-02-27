use std::{
  collections::VecDeque,
  fmt::Display,
  iter::Peekable,
  ops::{Bound, Deref, Range, RangeBounds},
  str::Chars,
  sync::Arc,
};

use bitflags::bitflags;

use crate::{
  builtin::BUILTINS,
  libsh::{
    error::{ShErr, ShErrKind, ShResult},
    utils::CharDequeUtils,
  },
  prelude::*,
};

pub const KEYWORDS: [&str; 16] = [
  "if", "then", "elif", "else", "fi", "while", "until", "select", "for", "in", "do", "done",
  "case", "esac", "[[", "]]",
];

pub const OPENERS: [&str; 6] = ["if", "while", "until", "for", "select", "case"];

/// Span::new(10..20)
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Span {
  range: Range<usize>,
  source: Arc<String>,
}

impl Span {
  /// New `Span`. Wraps a range and a string slice that it refers to.
  pub fn new(range: Range<usize>, source: Arc<String>) -> Self {
    Span { range, source }
  }
  /// Slice the source string at the wrapped range
  pub fn as_str(&self) -> &str {
    &self.source[self.start..self.end]
  }
  pub fn get_source(&self) -> Arc<String> {
    self.source.clone()
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

/// Allows simple access to the underlying range wrapped by the span
impl Deref for Span {
  type Target = Range<usize>;
  fn deref(&self) -> &Self::Target {
    &self.range
  }
}

#[derive(Clone, PartialEq, Debug)]
pub enum TkRule {
  Null,
  SOI, // Start-of-Input
  Str,
  Pipe,
  ErrPipe,
  And,
  Or,
  Bg,
  Sep,
  Redir,
  CasePattern,
  BraceGrpStart,
  BraceGrpEnd,
  Expanded { exp: Vec<String> },
  Comment,
  EOI, // End-of-Input
}

impl Default for TkRule {
  fn default() -> Self {
    TkRule::Null
  }
}

#[derive(Clone, Debug, PartialEq, Default)]
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
  pub fn source(&self) -> Arc<String> {
    self.span.source.clone()
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
    /// This is a      keyword that opens a new block statement, like 'if' and 'while'
    const OPENER       = 0b0000000000000010;
    const IS_CMD       = 0b0000000000000100;
    const IS_SUBSH     = 0b0000000000001000;
    const IS_CMDSUB    = 0b0000000000010000;
    const IS_OP        = 0b0000000000100000;
    const ASSIGN       = 0b0000000001000000;
    const BUILTIN      = 0b0000000010000000;
    const IS_PROCSUB   = 0b0000000100000000;
  }
}

pub struct LexStream {
  source: Arc<String>,
  pub cursor: usize,
  in_quote: bool,
  brc_grp_start: Option<usize>,
  flags: LexFlags,
}

bitflags! {
  #[derive(Debug, Clone, Copy)]
  pub struct LexFlags: u32 {
    /// The lexer is operating in interactive mode
    const INTERACTIVE     = 0b000000001;
    /// Allow unfinished input
    const LEX_UNFINISHED = 0b000000010;
    /// The next string-type token is a command name
    const NEXT_IS_CMD    = 0b000000100;
    /// We are in a quotation, so quoting rules apply
    const IN_QUOTE       = 0b000001000;
    /// Only lex strings; used in expansions
    const RAW            = 0b000010000;
    /// The lexer has not produced any tokens yet
    const FRESH          = 0b000010000;
    /// The lexer has no more tokens to produce
    const STALE          = 0b000100000;
    /// The lexer's cursor is in a brace group
    const IN_BRC_GRP     = 0b001000000;
    const EXPECTING_IN   = 0b010000000;
    const IN_CASE        = 0b100000000;
  }
}

impl LexStream {
  pub fn new(source: Arc<String>, flags: LexFlags) -> Self {
    let flags = flags | LexFlags::FRESH | LexFlags::NEXT_IS_CMD;
    Self {
      source,
      cursor: 0,
      in_quote: false,
      brc_grp_start: None,
      flags,
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
      Bound::Included(&end) => end,
      Bound::Excluded(&end) => end + 1,
      Bound::Unbounded => self.source.len(),
    };
    self.source.get(start..end)
  }
  pub fn slice_from_cursor(&self) -> Option<&str> {
    self.slice(self.cursor..)
  }
  pub fn in_brc_grp(&self) -> bool {
    self.flags.contains(LexFlags::IN_BRC_GRP)
  }
  pub fn set_in_brc_grp(&mut self, is: bool) {
    if is {
      self.flags |= LexFlags::IN_BRC_GRP;
      self.brc_grp_start = Some(self.cursor);
    } else {
      self.flags &= !LexFlags::IN_BRC_GRP;
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
    } else {
      self.flags &= !LexFlags::NEXT_IS_CMD;
    }
  }
  pub fn read_redir(&mut self) -> Option<ShResult<Tk>> {
    assert!(self.cursor <= self.source.len());
    let slice = self.slice(self.cursor..)?;
    let mut pos = self.cursor;
    let mut chars = slice.chars().peekable();
    let mut tk = Tk::default();

    while let Some(ch) = chars.next() {
      match ch {
        '>' => {
          if chars.peek() == Some(&'(') {
            return None; // It's a process sub
          }
          pos += 1;
          if let Some('>') = chars.peek() {
            chars.next();
            pos += 1;
          }
          if let Some('&') = chars.peek() {
            chars.next();
            pos += 1;

            let mut found_fd = false;
            while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
              chars.next();
              found_fd = true;
              pos += 1;
            }

            if !found_fd && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
              let span_start = self.cursor;
              self.cursor = pos;
              return Some(Err(ShErr::full(
                ShErrKind::ParseErr,
                "Invalid redirection",
                Span::new(span_start..pos, self.source.clone()),
              )));
            } else {
              tk = self.get_token(self.cursor..pos, TkRule::Redir);
              break;
            }
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

          for _ in 0..2 {
            if let Some('<') = chars.peek() {
              chars.next();
              pos += 1;
            } else {
              break;
            }
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
      }
    }

    if tk == Tk::default() {
      return None;
    }

    self.cursor = pos;
    Some(Ok(tk))
  }
  pub fn read_string(&mut self) -> ShResult<Tk> {
    assert!(self.cursor <= self.source.len());
    let slice = self.slice_from_cursor().unwrap().to_string();
    let mut pos = self.cursor;
    let mut chars = slice.chars().peekable();
    let can_be_subshell = chars.peek() == Some(&'(');

    if self.flags.contains(LexFlags::IN_CASE)
      && let Some(count) = case_pat_lookahead(chars.clone())
    {
      pos += count;
      let casepat_tk = self.get_token(self.cursor..pos, TkRule::CasePattern);
      self.cursor = pos;
      self.set_next_is_cmd(true);
      return Ok(casepat_tk);
    }

    while let Some(ch) = chars.next() {
      match ch {
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
        '$' if chars.peek() == Some(&'{') => {
          pos += 2;
          chars.next();
          let mut brace_count = 1;
          while let Some(brc_ch) = chars.next() {
            match brc_ch {
              '\\' => {
                pos += 1;
                if let Some(next_ch) = chars.next() {
                  pos += next_ch.len_utf8()
                }
              }
              '{' => {
                pos += 1;
                brace_count += 1;
              }
              '}' => {
                pos += 1;
                brace_count -= 1;
                if brace_count == 0 {
                  break;
                }
              }
              _ => pos += ch.len_utf8(),
            }
          }
        }
        '<' if chars.peek() == Some(&'(') => {
          pos += 2;
          chars.next();
          let mut paren_count = 1;
          let paren_pos = pos;
          while let Some(ch) = chars.next() {
            match ch {
              '\\' => {
                pos += 1;
                if let Some(next_ch) = chars.next() {
                  pos += next_ch.len_utf8();
                }
              }
              '(' => {
                pos += 1;
                paren_count += 1;
              }
              ')' => {
                pos += 1;
                paren_count -= 1;
                if paren_count <= 0 {
                  break;
                }
              }
              _ => pos += ch.len_utf8(),
            }
          }
          if !paren_count == 0 && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
            self.cursor = pos;
            return Err(ShErr::full(
              ShErrKind::ParseErr,
              "Unclosed subshell",
              Span::new(paren_pos..paren_pos + 1, self.source.clone()),
            ));
          }
        }
        '>' if chars.peek() == Some(&'(') => {
          pos += 2;
          chars.next();
          let mut paren_count = 1;
          let paren_pos = pos;
          while let Some(ch) = chars.next() {
            match ch {
              '\\' => {
                pos += 1;
                if let Some(next_ch) = chars.next() {
                  pos += next_ch.len_utf8();
                }
              }
              '(' => {
                pos += 1;
                paren_count += 1;
              }
              ')' => {
                pos += 1;
                paren_count -= 1;
                if paren_count <= 0 {
                  break;
                }
              }
              _ => pos += ch.len_utf8(),
            }
          }
          if !paren_count == 0 && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
            self.cursor = pos;
            return Err(ShErr::full(
              ShErrKind::ParseErr,
              "Unclosed subshell",
              Span::new(paren_pos..paren_pos + 1, self.source.clone()),
            ));
          }
        }
        '$' if chars.peek() == Some(&'(') => {
          pos += 2;
          chars.next();
          let mut paren_count = 1;
          let paren_pos = pos;
          while let Some(ch) = chars.next() {
            match ch {
              '\\' => {
                pos += 1;
                if let Some(next_ch) = chars.next() {
                  pos += next_ch.len_utf8();
                }
              }
              '(' => {
                pos += 1;
                paren_count += 1;
              }
              ')' => {
                pos += 1;
                paren_count -= 1;
                if paren_count <= 0 {
                  break;
                }
              }
              _ => pos += ch.len_utf8(),
            }
          }
          if !paren_count == 0 && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
            self.cursor = pos;
            return Err(ShErr::full(
              ShErrKind::ParseErr,
              "Unclosed subshell",
              Span::new(paren_pos..paren_pos + 1, self.source.clone()),
            ));
          }
        }
        '(' if self.next_is_cmd() && can_be_subshell => {
          pos += 1;
          let mut paren_count = 1;
          let paren_pos = pos;
          while let Some(ch) = chars.next() {
            match ch {
              '\\' => {
                pos += 1;
                if let Some(next_ch) = chars.next() {
                  pos += next_ch.len_utf8();
                }
              }
              '(' => {
                pos += 1;
                paren_count += 1;
              }
              ')' => {
                pos += 1;
                paren_count -= 1;
                if paren_count <= 0 {
                  break;
                }
              }
              _ => pos += ch.len_utf8(),
            }
          }
          if paren_count != 0 && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
            self.cursor = pos;
            return Err(ShErr::full(
              ShErrKind::ParseErr,
              "Unclosed subshell",
              Span::new(paren_pos..paren_pos + 1, self.source.clone()),
            ));
          }
          let mut subsh_tk = self.get_token(self.cursor..pos, TkRule::Str);
          subsh_tk.flags |= TkFlags::IS_CMD;
          subsh_tk.flags |= TkFlags::IS_SUBSH;
          self.cursor = pos;
          self.set_next_is_cmd(true);
          return Ok(subsh_tk);
        }
        '{' if pos == self.cursor && self.next_is_cmd() => {
          pos += 1;
          let mut tk = self.get_token(self.cursor..pos, TkRule::BraceGrpStart);
          tk.flags |= TkFlags::IS_CMD;
          self.set_in_brc_grp(true);
          self.set_next_is_cmd(true);

          self.cursor = pos;
          return Ok(tk);
        }
        '}' if pos == self.cursor && self.in_brc_grp() => {
          pos += 1;
          let tk = self.get_token(self.cursor..pos, TkRule::BraceGrpEnd);
          self.set_in_brc_grp(false);
          self.set_next_is_cmd(true);
          self.cursor = pos;
          return Ok(tk);
        }
        '\'' => {
          self.in_quote = true;
          pos += 1;
          while let Some(q_ch) = chars.next() {
            match q_ch {
              '\\' => {
                pos += 1;
                if chars.next().is_some() {
                  pos += 1;
                }
              }
              _ if q_ch == '\'' => {
                pos += 1;
                self.in_quote = false;
                break;
              }
              // Any time an ambiguous character is found
              // we must push the cursor by the length of the character
              // instead of just assuming a length of 1.
              // Allows spans to work for wide characters
              _ => pos += q_ch.len_utf8(),
            }
          }
        }
        '"' => {
          self.in_quote = true;
          pos += 1;
          while let Some(q_ch) = chars.next() {
            match q_ch {
              '\\' => {
                pos += 1;
                if chars.next().is_some() {
                  pos += 1;
                }
              }
              '$' if chars.peek() == Some(&'(') => {
                pos += 2;
                chars.next();
                let mut cmdsub_count = 1;
                while let Some(cmdsub_ch) = chars.next() {
                  match cmdsub_ch {
                    '\\' => {
                      pos += 1;
                      if chars.next().is_some() {
                        pos += 1;
                      }
                    }
                    '$' if chars.peek() == Some(&'(') => {
                      cmdsub_count += 1;
                      pos += 2;
                      chars.next();
                    }
                    ')' => {
                      cmdsub_count -= 1;
                      pos += 1;
                      if cmdsub_count <= 0 {
                        break;
                      }
                    }
                    _ => pos += cmdsub_ch.len_utf8(),
                  }
                }
              }
              _ if q_ch == '"' => {
                pos += 1;
                self.in_quote = false;
                break;
              }
              // Any time an ambiguous character is found
              // we must push the cursor by the length of the character
              // instead of just assuming a length of 1.
              // Allows spans to work for wide characters
              _ => pos += q_ch.len_utf8(),
            }
          }
        }
        '=' if chars.peek() == Some(&'(') => {
          pos += 1; // '='
          let mut depth = 1;
          chars.next();
          pos += 1; // '('
          // looks like an array
          while let Some(arr_ch) = chars.next() {
            match arr_ch {
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
            }
          }
        }
        _ if !self.in_quote && is_op(ch) => break,
        _ if is_hard_sep(ch) => break,
        _ => pos += ch.len_utf8(),
      }
    }
    let mut new_tk = self.get_token(self.cursor..pos, TkRule::Str);
    if self.in_quote && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
      self.cursor = pos;
      return Err(ShErr::full(
        ShErrKind::ParseErr,
        "Unterminated quote",
        new_tk.span,
      ));
    }

    let text = new_tk.span.as_str();
    if self.flags.contains(LexFlags::NEXT_IS_CMD) {
      match text {
        "case" | "select" | "for" => {
          new_tk.mark(TkFlags::KEYWORD);
          self.flags |= LexFlags::EXPECTING_IN;
          self.flags |= LexFlags::IN_CASE;
          self.set_next_is_cmd(false);
        }
        "in" if self.flags.contains(LexFlags::EXPECTING_IN) => {
          new_tk.mark(TkFlags::KEYWORD);
          self.flags &= !LexFlags::EXPECTING_IN;
        }
        _ if is_keyword(text) => {
          if text == "esac" && self.flags.contains(LexFlags::IN_CASE) {
            self.flags &= !LexFlags::IN_CASE;
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
    let span = Span::new(range, self.source.clone());
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
          return Err(ShErr::full(
            ShErrKind::ParseErr,
            "Unclosed brace group",
            Span::new(start..self.cursor, self.source.clone()),
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
        return Err(ShErr::full(
          ShErrKind::ParseErr,
          "Unclosed brace group",
          Span::new(start..self.cursor, self.source.clone()),
        ))
        .into();
      }
      return None;
    }

    let token = match get_char(&self.source, self.cursor).unwrap() {
      '\r' | '\n' | ';' => {
        let ch_idx = self.cursor;
        self.cursor += 1;
        self.set_next_is_cmd(true);

        while let Some(ch) = get_char(&self.source, self.cursor) {
          if is_hard_sep(ch) {
            // Combine consecutive separators into one, including whitespace
            self.cursor += 1;
          } else {
            break;
          }
        }
        self.get_token(ch_idx..self.cursor, TkRule::Sep)
      }
      '#'
        if !self.flags.contains(LexFlags::INTERACTIVE)
          || crate::state::read_shopts(|s| s.core.interactive_comments) =>
      {
        let ch_idx = self.cursor;
        self.cursor += 1;

        while let Some(ch) = get_char(&self.source, self.cursor) {
          self.cursor += 1;
          if ch == '\n' {
            break;
          }
        }

        self.get_token(ch_idx..self.cursor, TkRule::Comment)
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
          self.set_next_is_cmd(false);
          match tk {
            Ok(tk) => tk,
            Err(e) => return Some(Err(e)),
          }
        } else {
          match self.read_string() {
            Ok(tk) => tk,
            Err(e) => {
              return Some(Err(e));
            }
          }
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

  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        chars.next();
      }
      '=' => return true,
      _ => continue,
    }
  }
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

pub fn ends_with_unescaped(slice: &str, pat: &str) -> bool {
  slice.ends_with(pat) && !pos_is_escaped(slice, slice.len() - pat.len())
}

pub fn pos_is_escaped(slice: &str, pos: usize) -> bool {
  let bytes = slice.as_bytes();
  let mut escaped = false;
  let mut i = pos;
  while i > 0 && bytes[i - 1] == b'\\' {
    escaped = !escaped;
    i -= 1;
  }
  escaped
}

pub fn lookahead(pat: &str, mut chars: Chars) -> Option<usize> {
  let mut pos = 0;
  let mut char_deque = VecDeque::new();
  while let Some(ch) = chars.next() {
    char_deque.push_back(ch);
    if char_deque.len() > pat.len() {
      char_deque.pop_front();
    }
    if char_deque.starts_with(pat) {
      return Some(pos);
    }
    pos += 1;
  }
  None
}

pub fn case_pat_lookahead(mut chars: Peekable<Chars>) -> Option<usize> {
  let mut pos = 0;
  while let Some(ch) = chars.next() {
    pos += 1;
    match ch {
      _ if is_hard_sep(ch) => return None,
      '\\' => {
        chars.next();
      }
      ')' => return Some(pos),
      '(' => return None,
      _ => { /* continue */ }
    }
  }
  None
}
