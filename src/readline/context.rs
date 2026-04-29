use std::{iter::Peekable, os::unix::fs::PermissionsExt, path::Path, str::CharIndices};

use bitflags::bitflags;

use crate::{expand::{unescape_str, var::expand_raw_inner}, match_loop, parse::lex::{LexFlags, LexStream, Span, Tk, TkFlags, TkRule}, readline::{linebuf::Delim, markers::strip_markers}, state::{self, ShellParam, read_meta, read_shopts}, util::strops::QuoteState};


pub fn get_context_tokens(input: &str) -> Vec<CtxTk> {
  let mut out: Vec<CtxTk> = LexStream::new(input.into(), LexFlags::LEX_UNFINISHED)
    .filter_map(Result::ok)
    .filter(Tk::filter_meta)
    .flat_map(CtxTk::from_tk)
    .collect();

  // we unpacked the heredoc tokens, but they arent in their literal positions
  // so we now have to sort by span start
  out.sort_by_key(|t| t.span.range().start);
  out
}

/// Checks if a command name is valid (exists in PATH, is a function, or is an
/// alias)
///
/// Searches:
/// 1. Current directory if command is a path
/// 2. All directories in PATH environment variable
/// 3. Shell functions and aliases in the current shell state
fn is_valid(command: &str) -> bool {
  let cmd_path = Path::new(&command);

  if cmd_path.is_dir() && read_shopts(|o| o.core.autocd) {
    // this is a directory and autocd is enabled
    return true;
  }

  if cmd_path.is_absolute() {
    // the user has given us an absolute path
    let Ok(meta) = cmd_path.metadata() else {
      return false;
    };
    // this is a file that is executable by someone
    meta.permissions().mode() & 0o111 != 0
  } else {
    read_meta(|m| m.cache_contains(command))
  }
}

bitflags! {
  /// bitfield representing what syntax structures are valid in the current context
  pub struct ScanCtx: u16 {
    const VAR_SUB           = 1 << 0;  // $foo, ${foo}
    const CMD_SUB           = 1 << 1;  // $(...)
    const ARITHMETIC        = 1 << 2;  // $((...))
    const BACKTICK_SUB      = 1 << 3;  // `...`
    const PROC_SUB          = 1 << 4;  // <(...) >(...)
    const QUOTE             = 1 << 5;  // "..."
    const ESCAPE            = 1 << 6;  // \x
    const GLOB              = 1 << 7;  // *, ?, [...]
    const HIST_EXP          = 1 << 8; // !!
    const TILDE             = 1 << 9; // ~user
    const ESCAPE_QUOTE_ONLY = 1 << 10; // escapes inside single quotes
  }
}

impl ScanCtx {
  // useful constants
  pub const TOP_LEVEL: Self = Self::ESCAPE_QUOTE_ONLY.complement(); // everything but ESCAPE_QUOTE_ONLY

  pub const DOUBLE_QUOTE: Self = Self::VAR_SUB
    .union(Self::CMD_SUB)
    .union(Self::ARITHMETIC)
    .union(Self::HIST_EXP)
    .union(Self::ESCAPE)
    .union(Self::BACKTICK_SUB);

  pub const DOLLAR_QUOTE: Self = Self::ESCAPE;

  pub const SINGLE_QUOTE: Self = Self::ESCAPE_QUOTE_ONLY;

  pub const ARITH: Self = Self::VAR_SUB
    .union(Self::CMD_SUB)
    .union(Self::ARITHMETIC)
    .union(Self::ESCAPE)
    .union(Self::BACKTICK_SUB)
    .union(Self::QUOTE);
}

enum TerminatorCtx {
  Eof,
  ArithSub,
  VarIndex,
  Arith,
  ParamExpansion,
  DoubleQuote,
  SingleQuote,
}

impl TerminatorCtx {
  pub fn is_closer(&self, ch: char, chars: &mut Peekable<CharIndices>) -> bool {
    let next_is = |chars: &mut Peekable<CharIndices>, c: char| chars.peek().is_some_and(|(_,ch)| *ch == c);
    match self {
      Self::Eof => chars.peek().is_none(),
      Self::Arith |
      Self::ArithSub => ch == ')' && next_is(chars, ')'),
      Self::VarIndex => ch == ']',
      Self::ParamExpansion => ch == '}',
      Self::DoubleQuote => ch == '"',
      Self::SingleQuote => ch == '\'',
    }
  }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CtxTkRule {
  ValidCommand,
  InvalidCommand,
  Argument,
  ArgumentFile,
  Keyword,
  Subshell,
  CmdSub,
  BacktickSub,
  ProcSubIn,
  ProcSubOut,
  VarSub,
  Comment,
  Glob,
  CasePattern,
  HistExp,
  Escape,
  Tilde,
  Separator,

  Arithmetic,
  ArithOp,
  ArithNumber,
  ArithVar,

  ParamPrefix,
  ParamName,
  ParamIndex,
  ParamOp,
  ParamArg,

  DoubleString,
  SingleString,
  DollarString,

  AssignmentLeft,
  AssignmentOp,
  AssignmentRight,

  Operator,
  Redirect,
  BraceGroup,
  HereDoc,
  HereDocStart,
  HereDocBody,
  HereDocEnd,
  Null
}

/// A token with richer contextual data than `Tk`
///
/// These tokens exist somewhere inbetween 'token' and 'AST'.
/// This type allows for modeling the total analysis of stuff like `foo"bar $biz baz"buzz` which is ultimately read as a single token but contains nested context information that is relevant for things like autocompletion and syntax highlighting.
/// This nesting of 'subtokens' allows for entire trees to be created in cases of heavily nested expressions.
#[derive(Debug, Clone)]
pub struct CtxTk {
  span: Span,
  class: CtxTkRule,
  sub_tokens: Vec<CtxTk>,
}

impl CtxTk {
  pub fn span(&self)       -> &Span      { &self.span }
  pub fn class(&self)      -> &CtxTkRule { &self.class }
  pub fn sub_tokens(&self) -> &[CtxTk]   { &self.sub_tokens }
  pub fn range(&self)      -> std::ops::Range<usize> { self.span.range() }
  pub fn range_inclusive(&self) -> std::ops::RangeInclusive<usize> {
    let r = self.span.range();
    r.start..=r.end
  }

  pub fn rule_for(class: &TkRule) -> Option<CtxTkRule> {
    match class {
      TkRule::Pipe
      | TkRule::Bang
      | TkRule::ErrPipe
      | TkRule::And
      | TkRule::Or
      | TkRule::Bg
      | TkRule::BraceGrpStart
      | TkRule::BraceGrpEnd => Some(CtxTkRule::Operator),
      TkRule::Sep           => Some(CtxTkRule::Separator),
      TkRule::Redir         => Some(CtxTkRule::Redirect),
      TkRule::Comment       => Some(CtxTkRule::Comment),
      TkRule::HereDocStart  => Some(CtxTkRule::HereDoc),
      TkRule::HereDocEnd    => Some(CtxTkRule::HereDoc),

      TkRule::Expanded { exp: _ }
      | TkRule::HereDoc { .. }
      | TkRule::HereDocBody
      | TkRule::EOI
      | TkRule::SOI
      | TkRule::Null
      | TkRule::Str
      | TkRule::CasePattern => None,
    }
  }

  fn from_cmd_sub<F>(
    chars: &mut Peekable<CharIndices>,
    new_class: CtxTkRule,
    consumed: &mut usize,
    start_pos: usize,
    lexer: F,
    span: &Span,
  ) -> Self
  where F: Fn(&mut Peekable<CharIndices>) -> (bool,usize) {
    let (closed,inner_consumed) = lexer(chars);
    *consumed += inner_consumed;

    let opener_size = match new_class {
      CtxTkRule::ProcSubIn |
      CtxTkRule::ProcSubOut |
      CtxTkRule::CmdSub => 2,
      CtxTkRule::BacktickSub => 1,
      _ => unreachable!(),
    };

    let token_start = start_pos + span.range().start;
    let body_start = token_start + opener_size; // skip the opening backtick
    let token_end = body_start + inner_consumed;
    let body_end = if closed {
      token_end.saturating_sub(1) // exclude closing backtick in span if it exists
    } else {
      token_end
    };

    let sub_src = &span.get_source()[body_start..body_end];
    let inner_tokens = LexStream::new(sub_src.into(), LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .map(|tk| tk.rebase_into(span, body_start)) // map to outer span
      .flat_map(CtxTk::from_tk)
      .collect();

    Self {
      span: Span::new(token_start..token_end, span.get_source()),
      class: new_class,
      sub_tokens: inner_tokens,
    }
  }

  // split functions
  // not splitting on points that fall inside of child tokens is an invariant
  // similar to char boundaries in strings.

  pub fn can_split_at(&self, at: usize) -> bool {
    let r = self.span.range();
    if !(r.start..r.end).contains(&at) { return false; }
    !self.sub_tokens.iter().any(|c| {
      let cr = c.span.range();
      (cr.start..cr.end).contains(&at)
    })
  }

  pub fn split_at(self, at: usize) -> (CtxTk, CtxTk) {
    assert!(self.can_split_at(at), "split point falls inside of child token span");
    let CtxTk { span, class, sub_tokens } = self;
    let r = span.range();

    let mut left = vec![];
    let mut right = vec![];
    for child in sub_tokens {
      let cr = child.span.range();
      if cr.end <= at {
        left.push(child);
      } else if cr.start >= at {
        right.push(child);
      } else {
        unreachable!(); // guaranteed by can_split_at
      }
    }
    let src = span.get_source();
    (
      CtxTk {
        span: Span::new(r.start..at, src.clone()),
        class,
        sub_tokens: left,
      },
      CtxTk {
        span: Span::new(at..r.end, src),
        class,
        sub_tokens: right,
      }
    )
  }

  pub fn relative_cursor_pos(&self, at: usize) -> Option<usize> {
    if !self.range_inclusive().contains(&at) {
      return None;
    }
    Some(at - self.span.range().start)
  }

  pub fn split_str_at(&self, at: usize) -> Option<(&str,&str)> {
    let cursor_pos = self.relative_cursor_pos(at)?;

    self.span().as_str().split_at_checked(cursor_pos)
  }

  pub fn is_argument(&self) -> bool {
    matches!(self.class(), CtxTkRule::Argument | CtxTkRule::ArgumentFile)
  }

  pub fn prefix_from(&self, at: usize) -> Option<&str> {
    self.split_str_at(at).map(|(prefix,_)| prefix)
  }

  pub fn get_leaf(&self, cursor_pos: usize) -> Option<&CtxTk> {
    if !self.range_inclusive().contains(&cursor_pos) {
      return None;
    }

    for token in &self.sub_tokens {
      if let Some(tk) = token.get_leaf(cursor_pos) {
        return Some(tk);
      }
    }

    Some(self) // cursor is in our span, so..
  }

  /// Create a CtxTk from a Tk
  ///
  /// returns a Vec<CtxTk> because heredocs are a very unfortunate posix feature.
  pub fn from_tk(value: Tk) -> Vec<CtxTk> {
    let Tk { ref class, ref span, flags } = value;
    if let Some(class) = Self::rule_for(class) {
      return vec![Self {
        span: span.clone(),
        class,
        sub_tokens: vec![]
      }]
    };

    let mut chars = span.as_str().char_indices().peekable();
    let (_,sub_tokens) = scan_subspans(
      &mut chars,
      span,
      ScanCtx::TOP_LEVEL,
      TerminatorCtx::Eof,
    );

    if flags.contains(TkFlags::IS_HEREDOC)
    && let TkRule::HereDoc { start_delim, end_delim } = class {
      let body_tokens = if flags.contains(TkFlags::LIT_HEREDOC) {
        vec![]
      } else {
        sub_tokens
      };
      if let Some(end_delim) = end_delim {
        return vec![
          Self { span: (**start_delim).clone(), class: CtxTkRule::HereDocStart, sub_tokens: vec![] },
          Self { span: span.clone(), class: CtxTkRule::HereDocBody, sub_tokens: body_tokens },
          Self { span: (**end_delim).clone(), class: CtxTkRule::HereDocEnd, sub_tokens: vec![] },
        ]
      } else {
        return vec![
          Self { span: (**start_delim).clone(), class: CtxTkRule::HereDocStart, sub_tokens: vec![] },
          Self { span: span.clone(), class: CtxTkRule::HereDocBody, sub_tokens: body_tokens },
        ]
      };
    }

    let new_class = if flags.intersects(TkFlags::BUILTIN | TkFlags::IS_CMD) {
      if is_valid(value.as_str()) {
        CtxTkRule::ValidCommand
      } else {
        CtxTkRule::InvalidCommand
      }
    } else if flags.intersects(TkFlags::KEYWORD | TkFlags::FUNCNAME) {
      CtxTkRule::Keyword
    } else if check_path_exists(value.as_str()) {
      CtxTkRule::ArgumentFile
    } else {
      // regular argument. lets subdivide it further on COMP_WORDBREAKS members
      let tk = Self {
        span: span.clone(),
        class: CtxTkRule::Argument,
        sub_tokens
      };
      return subdivide_argument(tk);
    };

    vec![Self {
      span: span.clone(),
      class: new_class,
      sub_tokens
    }]
  }
}

/// Check if a given path refers to a file or is a prefix of an existing filename
fn check_path_exists(path: &str) -> bool {
  // NOTE: keep an eye on this. this might have pretty significant overhead on network mounts
  if path.is_empty() { return false; }
  if Path::new(path).exists() { return true; }

  let unescaped = unescape_str(path);
  let Ok(expanded) = expand_raw_inner(&mut unescaped.chars().peekable(), false) else {
    return false;
  };
  let stripped = strip_markers(&expanded);
  if stripped.is_empty() { return false; }

  let pat = format!("{}*", glob::Pattern::escape(&stripped));
  glob::glob(&pat).ok()
    .and_then(|mut it| it.next())
    .is_some()
}

/// Break a token at comp_wordbreaks
///
/// This allows for styling filenames in tokens like `--foo=/path/to/bar`
/// And also allows the completer to get more fine-grained context
fn subdivide_argument(mut tk: CtxTk) -> Vec<CtxTk> {
  let wordbreaks = state::get_comp_wordbreaks();
  let mut tokens = vec![];

  let push_token = |tks: &mut Vec<CtxTk>, mut tk: CtxTk| {
    if check_path_exists(tk.span.as_str()) {
      tk.class = CtxTkRule::ArgumentFile;
    }
    tks.push(tk);
  };

  loop {
    let raw = tk.span().as_str();
    let span_start = tk.span().range().start;
    let span_end = tk.span().range().end;

    let split_at = raw.char_indices().find_map(|(byte,ch)| {
      if !wordbreaks.contains(ch) { return None; }
      let after = span_start + byte + ch.len_utf8();
      let can_split = after < span_end && tk.can_split_at(after);

      can_split.then_some(after)
    });

    match split_at {
      Some(pos) => {
        let (left, right) = tk.split_at(pos);
        push_token(&mut tokens, left);
        tk = right;
      }
      None => {
        push_token(&mut tokens, tk);
        break
      }
    }
  }

  tokens
}

fn next_is(chars: &mut Peekable<CharIndices>, ch: char) -> bool {
  chars.peek().is_some_and(|(_,c)| *c == ch)
}

/// After the op chars have been consumed, push a ParamOp token covering them
/// and a ParamArg token covering the body up to the closing `}`. Returns the
/// new position-after-last-consumed (which will be one past the `}` if found).
///
/// `op_start` is the absolute position where the op chars begin.
/// `op_size` is the number of bytes the op occupies (1 for `-`, 2 for `:-` etc.)
fn parse_op_body(
  chars: &mut Peekable<CharIndices>,
  consumed: &mut usize,
  span: &Span,
  op_start: usize,
  op_size: usize,
  var_sub_tokens: &mut Vec<CtxTk>,
) -> usize {
  let op_end = op_start + op_size;
  var_sub_tokens.push(CtxTk {
    span: Span::new(op_start..op_end, span.get_source()),
    class: CtxTkRule::ParamOp,
    sub_tokens: vec![],
  });

  let (inner_consumed, inner) = scan_subspans(
    chars, span,
    ScanCtx::TOP_LEVEL,
    TerminatorCtx::ParamExpansion,
  );
  *consumed += inner_consumed;

  let arg_end = op_end + inner_consumed;
  let arg_text_end = if inner_consumed > 0
    && span.get_source().as_bytes().get(arg_end - 1) == Some(&b'}')
  {
    arg_end - 1
  } else {
    arg_end
  };
  var_sub_tokens.push(CtxTk {
    span: Span::new(op_end..arg_text_end, span.get_source()),
    class: CtxTkRule::ParamArg,
    sub_tokens: inner,
  });

  arg_end
}

#[allow(clippy::too_many_arguments)]
fn get_subtoken(
  chars: &mut Peekable<CharIndices>,
  span: &Span,
  term_ctx: TerminatorCtx,
  scan_ctx: ScanCtx,
  opener_len: usize,
  token_start: usize,
  consumed: &mut usize,
  rule: CtxTkRule
) -> CtxTk {
  let (inner_consumed, inner) = scan_subspans(chars, span, scan_ctx, term_ctx);
  *consumed += inner_consumed;

  let token_end = token_start + opener_len + inner_consumed; // include the opening
  let span = Span::new(token_start..token_end, span.get_source());
  CtxTk {
    span,
    class: rule,
    sub_tokens: inner,
  }
}

fn scan_subspans(
  chars: &mut Peekable<CharIndices>,
  span: &Span,
  scan_ctx: ScanCtx,
  term_ctx: TerminatorCtx
) -> (usize, Vec<CtxTk>) {
  use ScanCtx as S;
  let consumed = &mut 0;
  let mut sub_tokens = vec![];
  let consume = |chars: &mut Peekable<CharIndices>, cons: &mut usize| {
    chars.next().map(|(i,c)| { *cons += c.len_utf8(); (i,c) })
  };
  // True only when we're inside an arithmetic body (not just when arith subs
  // are recognized). Inside arith bodies, alpha/digit/op chars are atoms.
  let in_arith = matches!(
    term_ctx,
    TerminatorCtx::Arith | TerminatorCtx::ArithSub | TerminatorCtx::VarIndex
  );

  while let Some((i, ch)) = consume(chars, consumed) {

    if term_ctx.is_closer(ch, chars) {
      if matches!(term_ctx, TerminatorCtx::Arith | TerminatorCtx::ArithSub) {
        consume(chars, consumed); // consume the second ')'
      }

      return (*consumed, sub_tokens)
    }

    match ch {
      '\\' => {
        let escapes_any = scan_ctx.contains(S::ESCAPE);
        let escapes_quote = scan_ctx.contains(S::ESCAPE_QUOTE_ONLY)
          && chars.peek().is_some_and(|(_,c)| *c == '\'');
        if escapes_any || escapes_quote {
          let esc_start = i + span.range().start;
          if let Some((_, esc_ch)) = consume(chars, consumed) {
            let esc_end = esc_start + 1 + esc_ch.len_utf8();
            sub_tokens.push(CtxTk {
              span: Span::new(esc_start..esc_end, span.get_source()),
              class: CtxTkRule::Escape,
              sub_tokens: vec![],
            });
          }
        }
      }
      '"' if scan_ctx.contains(S::QUOTE) => {
        let sub_tk = get_subtoken(
          chars, span,
          TerminatorCtx::DoubleQuote,
          ScanCtx::DOUBLE_QUOTE,
          1, i + span.range().start, consumed,
          CtxTkRule::DoubleString,
        );
        sub_tokens.push(sub_tk);
      }
      '\'' if scan_ctx.contains(S::QUOTE) => {
        let sub_tk = get_subtoken(
          chars, span,
          TerminatorCtx::SingleQuote,
          ScanCtx::SINGLE_QUOTE,
          1, i + span.range().start, consumed,
          CtxTkRule::SingleString,
        );
        sub_tokens.push(sub_tk);
      }
      glob @ ('*' | '?' | '[') if scan_ctx.contains(S::GLOB) => {
        match glob {
          '*' | '?' => {
            let span = Span::new(i + span.range().start..i + span.range().start + 1, span.get_source());
            sub_tokens.push(CtxTk {
              span,
              class: CtxTkRule::Glob,
              sub_tokens: vec![],
            });
          }
          '[' => {
            let span_start = i + span.range().start;
            let orig_consumed = *consumed;
            while let Some(&(_,ch)) = chars.peek() {
              consume(chars, consumed);
              if ch == '\\' {
                consume(chars, consumed); // skip the escaped char
                continue
              }
              if ch == ']' { break }
            }
            if *consumed == orig_consumed {
              continue; // no valid glob chars, skip
            }
            let span = Span::new(span_start..(span_start + 1 + (*consumed - orig_consumed)), span.get_source());
            sub_tokens.push(CtxTk {
              span,
              class: CtxTkRule::Glob,
              sub_tokens: vec![],
            });
          }
          _ => unreachable!()
        }
      }
      '~' if scan_ctx.contains(S::TILDE) => {
        let span = Span::new(i + span.range().start..i + span.range().start + 1, span.get_source());
        sub_tokens.push(CtxTk {
          span,
          class: CtxTkRule::Tilde,
          sub_tokens: vec![],
        });
      }
      '!' if scan_ctx.contains(S::HIST_EXP) => {
        let Some(&(_,ch)) = chars.peek() else { continue };
        match ch {
          '!' | '$' => {
            consume(chars, consumed);
            let span_start = i + span.range().start;
            let span = Span::new(span_start..(span_start + 1 + ch.len_utf8()), span.get_source());
            sub_tokens.push(CtxTk {
              span,
              class: CtxTkRule::HistExp,
              sub_tokens: vec![],
            })
          }
          c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => {
            let span_start = i + span.range().start;
            let orig_consumed = *consumed;
            while let Some(&(_,hexp_ch)) = chars.peek() {
              match hexp_ch {
                c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => consume(chars, consumed),
                _ => break
              };
            }
            if *consumed == orig_consumed {
              continue; // no valid history expansion token chars, skip
            }
            let delta = *consumed - orig_consumed;
            let span = Span::new(span_start..(span_start + 1 + delta), span.get_source());
            sub_tokens.push(CtxTk {
              span,
              class: CtxTkRule::HistExp,
              sub_tokens: vec![],
             })
          }
          _ => { /* '!' by itself i guess */ }
        }
      }
      '`' if scan_ctx.contains(S::BACKTICK_SUB) => {
        let sub_tk = CtxTk::from_cmd_sub(
          chars,
          CtxTkRule::BacktickSub,
          consumed,
          i,
          lex_backtick,
          span,
        );
        sub_tokens.push(sub_tk);
      }
      dir @('<' | '>') if next_is(chars, '(') => {
        if consume(chars, consumed).is_none() { continue };
        let class = match dir {
          '<' => CtxTkRule::ProcSubIn,
          '>' => CtxTkRule::ProcSubOut,
          _ => unreachable!(),
        };

        let sub_tk = CtxTk::from_cmd_sub(
          chars,
          class,
          consumed,
          i,
          lex_subshell,
          span,
        );
        sub_tokens.push(sub_tk)
      }
      '$' => {
        if next_is(chars, '(') {
          if consume(chars, consumed).is_none() { continue };

          if next_is(chars, '(') && scan_ctx.contains(S::VAR_SUB) {
            let sub_tk = get_subtoken(
              chars, span,
              TerminatorCtx::ArithSub,
              ScanCtx::ARITH,
              2, i + span.range().start, consumed,
              CtxTkRule::Arithmetic,
            );
            sub_tokens.push(sub_tk);
          } else if scan_ctx.contains(S::CMD_SUB) {
            let sub_tk = CtxTk::from_cmd_sub(
              chars,
              CtxTkRule::CmdSub,
              consumed,
              i,
              lex_subshell,
              span,
            );
            sub_tokens.push(sub_tk);
          }
        } else if next_is(chars, '\'') && scan_ctx.contains(S::QUOTE) {
          // $'...' ANSI-C quoting
          consume(chars, consumed); // consume the opening quote

          let (inner_consumed, inner) = scan_subspans(
            chars,
            span,
            ScanCtx::DOLLAR_QUOTE,
            TerminatorCtx::SingleQuote,
          );
          *consumed += inner_consumed;

          let token_start = i + span.range().start;
          let token_end = token_start + 2 + inner_consumed; // include the $'
          let span = Span::new(token_start..token_end, span.get_source());
          sub_tokens.push(CtxTk {
            span,
            class: CtxTkRule::DollarString,
            sub_tokens: inner,
          })
        } else if next_is(chars, '{') && scan_ctx.contains(S::VAR_SUB) {
          // parameter expansion
          // welcome to the posix house of horrors
          consume(chars, consumed); // consume the '{'
          let var_start = i + span.range().start;
          let mut var_sub_tokens = vec![];
          // Track position-after-last-consumed within the param expansion.
          // Starts right after `${` and grows as we consume each piece.
          let mut pos = var_start + 2;

          // Prefix (#, !)
          if let Some(&(_, ch)) = chars.peek()
          && (ch == '#' || ch == '!') {
            consume(chars, consumed);
            let prefix_span = Span::new(pos..(pos + 1), span.get_source());
            var_sub_tokens.push(CtxTk {
              span: prefix_span,
              class: CtxTkRule::ParamPrefix,
              sub_tokens: vec![],
            });
            pos += 1;
          }

          // Name
          let name_start = pos;
          while let Some(&(_, ch)) = chars.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
              consume(chars, consumed);
              pos += ch.len_utf8();
            } else {
              break;
            }
          }
          if pos == name_start {
            sub_tokens.push(CtxTk {
              span: Span::new(var_start..pos, span.get_source()),
              class: CtxTkRule::VarSub,
              sub_tokens: var_sub_tokens,
            });
            continue;
          }
          var_sub_tokens.push(CtxTk {
            span: Span::new(name_start..pos, span.get_source()),
            class: CtxTkRule::ParamName,
            sub_tokens: vec![],
          });

          // Optional array index
          if let Some(&(_, '[')) = chars.peek() {
            let index_start = pos;
            consume(chars, consumed); // consume '['
            pos += 1;
            let (inner_consumed, inner) = scan_subspans(
              chars, span,
              ScanCtx::ARITH,
              TerminatorCtx::VarIndex,
            );
            *consumed += inner_consumed;
            pos += inner_consumed; // includes the closing ']'
            var_sub_tokens.push(CtxTk {
              span: Span::new(index_start..pos, span.get_source()),
              class: CtxTkRule::ParamIndex,
              sub_tokens: inner,
            });
          }

          // Operator (or close)
          let Some(&(_, ch)) = chars.peek() else {
            // End of input right after the name. Push the partial wrapper
            // (covers `${prefix?name?index?`) so completion can dispatch.
            sub_tokens.push(CtxTk {
              span: Span::new(var_start..pos, span.get_source()),
              class: CtxTkRule::VarSub,
              sub_tokens: var_sub_tokens,
            });
            continue;
          };
          let parsed = match ch {
            '}' => {
              consume(chars, consumed);
              pos += 1;
              true
            }
            ':' => {
              consume(chars, consumed);
              pos += 1;
              match chars.peek().map(|(_,c)| *c) {
                Some('-' | '=' | '?' | '+') => {
                  consume(chars, consumed);
                  pos += 1;
                  // op span covers ":-" / ":=" / etc. (2 bytes)
                  pos = parse_op_body(
                    chars,
                    consumed,
                    span,
                    pos - 2,
                    2,
                    &mut var_sub_tokens,
                  );
                  true
                }
                Some(_) => {
                  // Substring: ${var:N} or ${var:N:M}
                  // Flat scan for offset, optionally followed by `:` and length.
                  // The leading `:` already consumed; record it as ParamOp.
                  var_sub_tokens.push(CtxTk {
                    span: Span::new(pos - 1..pos, span.get_source()),
                    class: CtxTkRule::ParamOp,
                    sub_tokens: vec![],
                  });

                  // Offset arg, scan until ':' or '}' at brace depth 0
                  let offset_start = pos;
                  let mut depth: i32 = 0;
                  let mut hit_colon = false;
                  while let Some(&(_, c)) = chars.peek() {
                    if depth == 0 && (c == ':' || c == '}') {
                      if c == ':' { hit_colon = true; }
                      break;
                    }
                    if c == '{' { depth += 1; }
                    if c == '}' { depth -= 1; }
                    consume(chars, consumed);
                    pos += c.len_utf8();
                  }
                  var_sub_tokens.push(CtxTk {
                    span: Span::new(offset_start..pos, span.get_source()),
                    class: CtxTkRule::ParamArg,
                    sub_tokens: vec![],
                  });

                  if hit_colon {
                    // Consume `:` separator and record as another ParamOp
                    consume(chars, consumed);
                    pos += 1;
                    var_sub_tokens.push(CtxTk {
                      span: Span::new(pos - 1..pos, span.get_source()),
                      class: CtxTkRule::ParamOp,
                      sub_tokens: vec![],
                    });

                    // Length arg, scan until '}' at brace depth 0
                    let length_start = pos;
                    let mut depth: i32 = 0;
                    while let Some(&(_, c)) = chars.peek() {
                      if depth == 0 && c == '}' { break; }
                      if c == '{' { depth += 1; }
                      if c == '}' { depth -= 1; }
                      consume(chars, consumed);
                      pos += c.len_utf8();
                    }
                    var_sub_tokens.push(CtxTk {
                      span: Span::new(length_start..pos, span.get_source()),
                      class: CtxTkRule::ParamArg,
                      sub_tokens: vec![],
                    });
                  }

                  // Consume the closing `}` if present
                  if next_is(chars, '}') {
                    consume(chars, consumed);
                    pos += 1;
                  }
                  true
                }
                None => false,
              }
            }
            '-' | '=' | '?' | '+' => {
              consume(chars, consumed);
              pos += 1;
              pos = parse_op_body(
                chars,
                consumed,
                span,
                pos - 1,
                1,
                &mut var_sub_tokens,
              );
              true
            }
            '#' | '%' => {
              let op_char = ch;
              consume(chars, consumed);
              pos += 1;
              let op_size = if next_is(chars, op_char) {
                consume(chars, consumed);
                pos += 1;
                2
              } else {
                1
              };
              pos = parse_op_body(
                chars,
                consumed,
                span,
                pos - op_size,
                op_size,
                &mut var_sub_tokens,
              );
              true
            }
            '^' | ',' => {
              // Case modification: ${var^}, ${var^^}, ${var,}, ${var,,}
              // Optional pattern after the operator.
              let op_char = ch;
              consume(chars, consumed);
              pos += 1;
              let op_size = if next_is(chars, op_char) {
                consume(chars, consumed);
                pos += 1;
                2
              } else {
                1
              };
              pos = parse_op_body(
                chars,
                consumed,
                span,
                pos - op_size,
                op_size,
                &mut var_sub_tokens,
              );
              true
            }
            '/' => {
              // Substitution: ${var/pat/rep}, ${var//pat/rep}, ${var/#pat/rep}, ${var/%pat/rep}
              // Flat-scan structure: ParamOp ("/" or "//" or "/#" or "/%"), ParamArg (pattern),
              // optional ParamOp ("/"), ParamArg (replacement), then "}".
              consume(chars, consumed);
              pos += 1;
              let op_size = match chars.peek().map(|(_, c)| *c) {
                Some('/') | Some('#') | Some('%') => {
                  consume(chars, consumed);
                  pos += 1;
                  2
                }
                _ => 1,
              };
              var_sub_tokens.push(CtxTk {
                span: Span::new(pos - op_size..pos, span.get_source()),
                class: CtxTkRule::ParamOp,
                sub_tokens: vec![],
              });

              // Pattern arg, scan until '/' or '}' at brace depth 0
              let pat_start = pos;
              let mut depth: i32 = 0;
              let mut hit_slash = false;
              while let Some(&(_, c)) = chars.peek() {
                if depth == 0 && (c == '/' || c == '}') {
                  if c == '/' { hit_slash = true; }
                  break;
                }
                if c == '{' { depth += 1; }
                if c == '}' { depth -= 1; }
                consume(chars, consumed);
                pos += c.len_utf8();
              }
              var_sub_tokens.push(CtxTk {
                span: Span::new(pat_start..pos, span.get_source()),
                class: CtxTkRule::ParamArg,
                sub_tokens: vec![],
              });

              if hit_slash {
                // Consume the separating '/' as a ParamOp
                consume(chars, consumed);
                pos += 1;
                var_sub_tokens.push(CtxTk {
                  span: Span::new(pos - 1..pos, span.get_source()),
                  class: CtxTkRule::ParamOp,
                  sub_tokens: vec![],
                });

                // Replacement arg, scan until '}' at brace depth 0
                let rep_start = pos;
                let mut depth: i32 = 0;
                while let Some(&(_, c)) = chars.peek() {
                  if depth == 0 && c == '}' { break; }
                  if c == '{' { depth += 1; }
                  if c == '}' { depth -= 1; }
                  consume(chars, consumed);
                  pos += c.len_utf8();
                }
                var_sub_tokens.push(CtxTk {
                  span: Span::new(rep_start..pos, span.get_source()),
                  class: CtxTkRule::ParamArg,
                  sub_tokens: vec![],
                });
              }

              if next_is(chars, '}') {
                consume(chars, consumed);
                pos += 1;
              }
              true
            }
            _ => false,
          };

          // Wrap and push (whether or not the op was fully recognized - a
          // partial wrapper is better than none for completion dispatch).
          sub_tokens.push(CtxTk {
            span: Span::new(var_start..pos, span.get_source()),
            class: CtxTkRule::VarSub,
            sub_tokens: var_sub_tokens,
          });
          let _ = parsed; // keep variable visible for clarity; unused now

        } else if scan_ctx.contains(S::VAR_SUB) {
          let Some(&(_,first)) = chars.peek() else { continue };
          let sub_start = i + span.range().start;
          let orig_consumed = *consumed;

          let is_param = ShellParam::from_char(&first).is_some();
          let is_digit = first.is_ascii_digit();
          let is_var_char = first.is_ascii_alphabetic() || first == '_';

          if is_param || is_digit {
            consume(chars, consumed);
          } else if !is_var_char {
            continue
          } else {
            while let Some(&(_,ch)) = chars.peek() {
              if !(ch.is_ascii_alphanumeric() || ch == '_') {
                break
              }

              consume(chars, consumed);
            }
          }

          let var_size = *consumed - orig_consumed;
          let sub_end = sub_start + 1 + var_size; // include the '$' in the span

          let span = Span::new(sub_start..sub_end, span.get_source());
          let sub_span = Span::new(sub_start + 1..sub_end, span.get_source());
          let sub_token = CtxTk {
            span: sub_span,
            class: CtxTkRule::ParamName,
            sub_tokens: vec![],
          };
          sub_tokens.push(CtxTk {
            span,
            class: CtxTkRule::VarSub,
            sub_tokens: vec![sub_token],
           })
        }
      }
      'a'..='z' | 'A'..='Z' | '_' if in_arith => {
        let var_start = i + span.range().start;
        let mut var_consumed = 0;
        while let Some(&(_,ch)) = chars.peek() {
          if !(ch.is_ascii_alphanumeric() || ch == '_') {
            break
          }
          consume(chars, consumed);
          var_consumed += ch.len_utf8();
        }
        let var_span = Span::new(var_start..(var_start + var_consumed), span.get_source());
        sub_tokens.push(CtxTk {
          span: var_span,
          class: CtxTkRule::ArithVar,
          sub_tokens: vec![],
        })
      }
      '0'..='9' if in_arith => {
        let num_start = i + span.range().start;
        let mut num_consumed = 0;
        while let Some(&(_,ch)) = chars.peek() {
          if !ch.is_ascii_digit() {
            break
          }
          consume(chars, consumed);
          num_consumed += ch.len_utf8();
        }
        let num_span = Span::new(num_start..(num_start + num_consumed), span.get_source());
        sub_tokens.push(CtxTk {
          span: num_span,
          class: CtxTkRule::Argument,
          sub_tokens: vec![],
        })
      }
      '+' | '/' | '%' | '-' | '*' | '=' if in_arith => {
        let op_start = i + span.range().start;
        let op_end = op_start + 1;
        let op_span = Span::new(op_start..op_end, span.get_source());
        sub_tokens.push(CtxTk {
          span: op_span,
          class: CtxTkRule::ArithOp,
          sub_tokens: vec![],
        })
      }
      _ => {}
    }
  }

  (*consumed, sub_tokens)
}

fn lex_backtick(chars: &mut Peekable<CharIndices>) -> (bool,usize) {
  let mut qt_state = QuoteState::default();
  let mut consumed = 0;
  let mut closed = false;
  let advance = |chars: &mut Peekable<CharIndices>, cons: &mut usize| {
    // advance iterator, increment consumed bytes
    chars.next().map(|(_,c)| { *cons += c.len_utf8(); c })
  };

  match_loop!(advance(chars, &mut consumed) => ch, {
    '\\' => {
      if !qt_state.in_single() || chars.peek().is_some_and(|(_,c)| *c == '\'') {
        advance(chars, &mut consumed);
      }
    }
    '"' => qt_state.toggle_double(),
    '\'' => qt_state.toggle_single(),

    '`' if qt_state.outside() => {
      closed = true;
      break
    }
    _ => {}
  });

  (closed, consumed)
}

fn lex_subshell(chars: &mut Peekable<CharIndices>) -> (bool,usize) {
  lex_delim(chars, Delim::Paren)
}

fn lex_delim(chars: &mut Peekable<CharIndices>, delim: Delim) -> (bool,usize) {
  let (opener, closer) = match delim {
    Delim::Paren => ('(', ')'),
    Delim::Brace => ('{', '}'),
    Delim::Bracket => ('[', ']'),
    Delim::Angle => ('<', '>'),
  };
  let mut qt_state = QuoteState::default();
  let mut consumed = 0;
  let mut depth = 1;
  let mut closed = false;
  let advance = |chars: &mut Peekable<CharIndices>, cons: &mut usize| {
    // advance iterator, increment consumed bytes
    chars.next().map(|(_,c)| { *cons += c.len_utf8(); c })
  };

  match_loop!(advance(chars, &mut consumed) => ch, {
    '\\' => {
      if !qt_state.in_single() || chars.peek().is_some_and(|(_,c)| *c == '\'') {
        advance(chars, &mut consumed);
      }
    }
    '"' => qt_state.toggle_double(),
    '\'' => qt_state.toggle_single(),

    _ if ch == opener && qt_state.outside() => depth += 1,
    _ if ch == closer && qt_state.outside() => {
      depth -= 1;
      if depth == 0 {
        // closer included in `consumed`; matches `lex_backtick` convention so
        // that `from_cmd_sub` can uniformly trim one byte to get the body.
        closed = true;
        break
      }
    }
    _ => {}
  });

  (closed, consumed)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::rc::Rc;

  /// Lex `src` and convert the first non-trivial token to a `CtxTk`.
  fn parse_first(src: &str) -> CtxTk {
    let rc: Rc<str> = src.into();
    let tk = LexStream::new(rc, LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .find(|t| !matches!(t.class, TkRule::SOI | TkRule::EOI | TkRule::Sep))
      .expect("expected at least one token");
    CtxTk::from_tk(tk).pop().unwrap()
  }

  /// Depth-first search for the first sub-token matching `class`.
  fn find(tk: &CtxTk, class: CtxTkRule) -> Option<&CtxTk> {
    if tk.class == class { return Some(tk); }
    for sub in &tk.sub_tokens {
      if let Some(hit) = find(sub, class) { return Some(hit); }
    }
    None
  }

  fn span_str<'a>(tk: &CtxTk, src: &'a str) -> &'a str {
    let r = tk.span.range();
    &src[r]
  }

  #[test]
  fn var_sub_simple() {
    let src = "$foo";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    assert_eq!(span_str(v, src), "$foo");
  }

  #[test]
  fn param_expansion_default() {
    let src = "${foo:-bar}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    assert_eq!(span_str(v, src), "${foo:-bar}");

    let name = find(v, CtxTkRule::ParamName).expect("ParamName");
    assert_eq!(span_str(name, src), "foo");

    let op = find(v, CtxTkRule::ParamOp).expect("ParamOp");
    assert_eq!(span_str(op, src), ":-");
  }

  #[test]
  fn cmd_sub_span_includes_closer() {
    // Regression for the lex_delim/from_cmd_sub mismatch: the closing `)` must
    // be inside the CmdSub span.
    let src = "$(echo hi)";
    let tk = parse_first(src);
    let c = find(&tk, CtxTkRule::CmdSub).expect("CmdSub");
    assert_eq!(span_str(c, src), "$(echo hi)");
  }

  #[test]
  fn arithmetic_atoms() {
    let src = "$((1+2))";
    let tk = parse_first(src);
    let a = find(&tk, CtxTkRule::Arithmetic).expect("Arithmetic");
    assert_eq!(span_str(a, src), "$((1+2))");
    assert!(find(a, CtxTkRule::ArithOp).is_some(), "expected ArithOp");
  }

  #[test]
  fn top_level_word_is_not_arith() {
    // Regression for the bug where `ARITHMETIC` in `TOP_LEVEL` caused
    // identifier chars in plain words to be classified as ArithVar.
    let src = "foo";
    let tk = parse_first(src);
    assert!(find(&tk, CtxTkRule::ArithVar).is_none(),
      "plain word should not produce ArithVar sub-tokens");
  }

  #[test]
  fn double_string_with_escape_keeps_alignment() {
    // Regression for the `\\` arm not updating *consumed.
    let src = r#""a\"b""#;
    let tk = parse_first(src);
    let s = find(&tk, CtxTkRule::DoubleString).expect("DoubleString");
    assert_eq!(span_str(s, src), r#""a\"b""#);
  }

  #[test]
  fn double_string_with_var_sub() {
    let src = r#""hi $foo""#;
    let tk = parse_first(src);
    let s = find(&tk, CtxTkRule::DoubleString).expect("DoubleString");
    assert_eq!(span_str(s, src), r#""hi $foo""#);
    let v = find(s, CtxTkRule::VarSub).expect("VarSub inside DoubleString");
    assert_eq!(span_str(v, src), "$foo");
  }

  #[test]
  fn utf8_multibyte_in_double_string() {
    let src = "\"αβγ\"";
    let tk = parse_first(src);
    let s = find(&tk, CtxTkRule::DoubleString).expect("DoubleString");
    assert_eq!(span_str(s, src), "\"αβγ\"");
  }

  #[test]
  fn unclosed_quote_does_not_panic() {
    let src = r#""abc"#;
    let tk = parse_first(src);
    let s = find(&tk, CtxTkRule::DoubleString).expect("DoubleString");
    // Unclosed: span runs to end of input.
    assert_eq!(span_str(s, src), r#""abc"#);
  }

  #[test]
  fn nested_cmd_subs() {
    let src = "$(echo $(echo hi))";
    let tk = parse_first(src);
    let outer = find(&tk, CtxTkRule::CmdSub).expect("outer CmdSub");
    assert_eq!(span_str(outer, src), "$(echo $(echo hi))");
    let inner = outer.sub_tokens.iter()
      .find_map(|t| find(t, CtxTkRule::CmdSub))
      .expect("inner CmdSub");
    assert_eq!(span_str(inner, src), "$(echo hi)");
  }

  #[test]
  fn nested_param_default() {
    let src = "${foo:-${bar}}";
    let tk = parse_first(src);
    let outer = find(&tk, CtxTkRule::VarSub).expect("outer VarSub");
    assert_eq!(span_str(outer, src), "${foo:-${bar}}");
  }
}
