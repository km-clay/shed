use std::{
  collections::VecDeque,
  iter::Peekable,
  os::unix::{ffi::OsStrExt, fs::PermissionsExt},
  path::Path,
  str::CharIndices,
};

use bitflags::bitflags;

use crate::{Shed, state::vars::VarStr, util};

use super::{
  builtin::BUILTIN_NAMES,
  editmode::{ExCommand, ExLineAddr, ExTk, ExTkRule},
  eval::{
    execute::{in_cd_path, is_in_path},
    lex::{LexFlags, LexStream, Span, Tk, TkFlags, TkRule},
  },
  expand::{expand_raw_inner, unescape_str},
  match_loop, shopt,
  state::{self, meta::UtilKind, util::get_exec_wrappers, vars::ShellParam},
  util::{QuoteState, has_unescaped, split_at_unescaped},
};

/*
 * Context Lexing
 *
 * When it comes to things like syntax highlighting and tab completion,
 * we need a specialized approach to figuring out what a span of text "is".
 * Normal lexing via LexStream works for getting arguments and stuff, but
 * is not great for problems like "the user pressed tab, what is the cursor sitting on?"
 *
 * This module proposes a solution to this issue. We'll use the LexStream as usual,
 * but every Tk will be processed into a CtxTk, which is a wrapper for Tk that contains
 * more contextual metadata.
 *
 * Tk is itself a wrapper for a Span, and as such is just a flat line of text.
 * CtxTk carries a vector of sub-tokens, which allows us to take something
 * like `foo"bar $(echo biz) baz"buzz` which is itself a single token, and construct
 * a tree-like structure from the tokens nested within it. This tree can then be walked
 * to find what a specific byte position represents.
 *
 */

/// Turn raw shell input into `CtxTks`
pub fn get_context_tokens(input: &str) -> Vec<CtxTk> {
  let out: Vec<CtxTk> = LexStream::new(input.as_bytes(), LexFlags::LEX_UNFINISHED)
    .filter_map(Result::ok)
    .filter(Tk::filter_meta)
    .flat_map(|value: Tk| CtxTk::from_tk(&value))
    .collect();

  process_ctx_tokens(out)
}

pub fn get_ex_context_tokens(input: &str) -> Vec<CtxTk> {
  let out: Vec<CtxTk> = super::editmode::ExLexer::new(input)
    .lex()
    .into_iter()
    .flat_map(CtxTk::from_ex_tk)
    .collect();

  process_ctx_tokens(out)
}

/// A command, backtick, or process substitution found nested inside a word.
pub(crate) enum NestedSub {
  Cmd(VarStr),
  Proc,
}

/// Collect every command, backtick, or process
/// substitution nested inside a word of `input`
pub(crate) fn nested_subs(input: &str) -> Vec<NestedSub> {
  fn collect(tk: &CtxTk, out: &mut Vec<NestedSub>) {
    match tk.class() {
      CtxTkRule::ProcSubIn | CtxTkRule::ProcSubOut => {
        out.push(NestedSub::Proc);
        return;
      }
      CtxTkRule::CmdSub => {
        let s = tk.span().as_bytes();
        let body = s
          .strip_prefix(b"$(".as_slice())
          .and_then(|s| s.strip_suffix(b")".as_slice()))
          .unwrap_or(s);
        out.push(NestedSub::Cmd(body.into()));
        return;
      }
      CtxTkRule::BacktickSub => {
        let s = tk.span().as_bytes();
        let body = s
          .strip_prefix(b"`".as_slice())
          .and_then(|s| s.strip_suffix(b"`".as_slice()))
          .unwrap_or(s);
        out.push(NestedSub::Cmd(body.into()));
        return;
      }
      _ => {}
    }
    for sub in tk.sub_tokens() {
      collect(sub, out);
    }
  }

  let mut out = vec![];
  for tk in get_context_tokens(input) {
    collect(&tk, &mut out);
  }
  out
}

fn process_ctx_tokens(mut out: Vec<CtxTk>) -> Vec<CtxTk> {
  out.sort_by_key(|t| t.span.range().start);

  // promote exec wrappers like 'sudo' and 'strace' to keyword status
  promote_exec_wrappers(&mut out);

  // subdivide arguments at comp_wordbreaks
  subdivide_arguments(&mut out);

  out
}

fn is_exec_wrapper(tk: &CtxTk) -> bool {
  get_exec_wrappers()
    .into_iter()
    .any(|wr| wr.as_bytes() == tk.span().as_bytes())
    && is_valid_cmd(&tk.as_tk()).is_some()
}

fn promote_exec_wrappers(tokens: &mut [CtxTk]) {
  let mut tokens = tokens.iter_mut().peekable();
  let mut skip_to_next = false;
  'outer: while let Some(tk) = tokens.next() {
    promote_exec_wrappers(&mut tk.sub_tokens);

    if skip_to_next {
      if let CtxTkRule::ValidCommand(_) = tk.class() {
        skip_to_next = false;
      } else {
        continue;
      }
    }

    if !is_exec_wrapper(tk) {
      skip_to_next = true;
      continue;
    }

    tk.class = CtxTkRule::Keyword;

    while let Some(target) = tokens.peek() {
      match target.class {
        CtxTkRule::Argument | CtxTkRule::ArgumentFile => {
          if target.span.as_bytes().starts_with(b"-") || has_unescaped(target.span.as_bytes(), b"=")
          {
            // looks like an option or an assignment
            tokens.next();
            continue;
          }
          if get_exec_wrappers().contains(&target.span.as_bytes().into()) {
            // chaining exec wrappers is a thing people do, e.g. `sudo strace cmd`
            // continue the outer loop and let it get picked up by the next iteration
            // we don't use is_exec_wrapper() for this since it doesnt have the ValidCommand rule
            continue 'outer;
          }
          let target = tokens.next().unwrap();
          target.class = match is_valid_cmd(&target.as_tk()) {
            Some(kind) => CtxTkRule::ValidCommand(kind),
            None => CtxTkRule::InvalidCommand,
          };
          break;
        }
        CtxTkRule::HereDocStart => {
          tokens.next();
        }
        CtxTkRule::Redirect => {
          tokens.next(); // consume it
          let redir_target = tokens.next();
          if redir_target
            .is_none_or(|t| !matches!(t.class, CtxTkRule::Argument | CtxTkRule::ArgumentFile))
          {
            break;
          }
        }
        _ => break,
      }
    }
  }
}

fn subdivide_arguments(tokens: &mut Vec<CtxTk>) {
  let mut out = Vec::with_capacity(tokens.len());
  for mut tk in tokens.drain(..) {
    subdivide_arguments(&mut tk.sub_tokens);
    match tk.class {
      CtxTkRule::Argument => out.extend(subdivide_argument(tk)),
      _ => out.push(tk),
    }
  }
  *tokens = out;
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum CmdKind {
  External,
  Function,
  Builtin,
  Alias,

  Directory, // if auto-cd is enabled
}

/// Checks if a command name is valid
///
/// Searches:
/// 1. Checks if we have autocd enabled and it is autocd'able
/// 2. Current directory if command is a path
/// 3. All directories in PATH environment variable
/// 4. Shell functions and aliases in the current shell state
fn is_valid(command: &Tk) -> Option<CmdKind> {
  if shopt!(core.autocd) && in_cd_path(command) && !is_in_path(command) {
    // this is a directory and autocd is enabled
    return Some(CmdKind::Directory);
  }

  is_valid_cmd(command)
}

fn is_valid_cmd(command: &Tk) -> Option<CmdKind> {
  let expanded = command.expand_no_side_effects().ok()?;
  let name = expanded.get_first_word()?;
  let cmd_path = Path::new(&name);

  if cmd_path.is_absolute()
    || name.to_str_lossy().starts_with('/')
    || name.to_str_lossy().starts_with("../")
  {
    let meta = cmd_path.metadata().ok()?;

    (!meta.is_dir() && meta.permissions().mode() & 0o111 != 0).then_some(CmdKind::External)
  } else if BUILTIN_NAMES.contains(&name.as_bytes()) {
    Some(CmdKind::Builtin)
  } else {
    let util = state::util::which_util(&name.to_str_lossy())?;
    match util.kind() {
      UtilKind::Alias => Some(CmdKind::Alias),
      UtilKind::Function => Some(CmdKind::Function),
      UtilKind::Builtin => Some(CmdKind::Builtin),
      UtilKind::Command(_) | UtilKind::File(_) => Some(CmdKind::External),
    }
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
    const HIST_EXP          = 1 << 8;  // !!
    const TILDE             = 1 << 9;  // ~user
  }
}

impl ScanCtx {
  // useful constants
  pub const TOP_LEVEL: Self = Self::all();

  pub const DOUBLE_QUOTE: Self = Self::VAR_SUB
    .union(Self::CMD_SUB)
    .union(Self::ARITHMETIC)
    .union(Self::HIST_EXP)
    .union(Self::ESCAPE)
    .union(Self::BACKTICK_SUB);

  pub const DOLLAR_QUOTE: Self = Self::ESCAPE;

  pub const SINGLE_QUOTE: Self = Self::empty();

  pub const ARITH: Self = Self::VAR_SUB
    .union(Self::CMD_SUB)
    .union(Self::ARITHMETIC)
    .union(Self::ESCAPE)
    .union(Self::BACKTICK_SUB)
    .union(Self::QUOTE);
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
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
  pub fn is_closer(self, ch: char, chars: &mut Peekable<CharIndices>) -> bool {
    let next_is =
      |chars: &mut Peekable<CharIndices>, c: char| chars.peek().is_some_and(|(_, ch)| *ch == c);
    match self {
      Self::Eof => false,
      Self::Arith | Self::ArithSub => ch == ')' && next_is(chars, ')'),
      Self::VarIndex => ch == ']',
      Self::ParamExpansion => ch == '}',
      Self::DoubleQuote => ch == '"',
      Self::SingleQuote => ch == '\'',
    }
  }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CtxTkRule {
  ValidCommand(CmdKind),
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
  ArrayLiteral,

  Operator,
  Redirect,
  HereDocStart,
  HereDocBody,
  HereDocEnd,

  // ex mode rules
  ValidExCommand,
  InvalidExCommand,
  ExAddress,
  ExBang,
  ExPattern,
}

impl CtxTkRule {
  pub fn is_ex_tk(self) -> bool {
    matches!(
      self,
      Self::ValidExCommand
        | Self::InvalidExCommand
        | Self::ExAddress
        | Self::ExBang
        | Self::ExPattern
    )
  }
}

/// A token with richer contextual data than `Tk`
///
/// These tokens exist somewhere inbetween 'token' and 'AST'.
/// This type allows for modeling the total analysis of stuff like `foo"bar $biz baz"buzz` which is
/// ultimately read as a single token but contains nested context information that is relevant
/// for things like autocompletion and syntax highlighting.
///
/// This nesting of 'subtokens' allows for entire trees to be created in cases of heavily nested expressions.
#[derive(Debug, Clone)]
pub struct CtxTk {
  span: Span,
  class: CtxTkRule,
  sub_tokens: Vec<CtxTk>,
}

impl CtxTk {
  pub fn span(&self) -> &Span {
    &self.span
  }
  pub fn class(&self) -> &CtxTkRule {
    &self.class
  }
  pub fn sub_tokens(&self) -> &[CtxTk] {
    &self.sub_tokens
  }
  pub fn range(&self) -> std::ops::Range<usize> {
    self.span.range()
  }
  pub fn range_inclusive(&self) -> std::ops::RangeInclusive<usize> {
    let r = self.span.range();
    r.start..=r.end
  }

  pub fn shift_by(&mut self, delta: isize) {
    self.span.shift_by(delta);
    for sub in &mut self.sub_tokens {
      sub.shift_by(delta);
    }
  }

  pub fn rebase_into(&mut self, outer_span: &Span, offset: usize) {
    self.span.rebase_into(outer_span, offset);
    for sub in &mut self.sub_tokens {
      sub.rebase_into(outer_span, offset);
    }
  }

  pub fn rule_for(class: &TkRule) -> Option<CtxTkRule> {
    match class {
      TkRule::Pipe
      | TkRule::Bang
      | TkRule::ErrPipe
      | TkRule::And
      | TkRule::Or
      | TkRule::Bg
      | TkRule::SubshStart
      | TkRule::SubshEnd
      | TkRule::BraceGrpStart
      | TkRule::BraceGrpEnd => Some(CtxTkRule::Operator),
      TkRule::Sep => Some(CtxTkRule::Separator),
      TkRule::Redir => Some(CtxTkRule::Redirect),
      TkRule::Comment => Some(CtxTkRule::Comment),

      TkRule::Expanded { exp: _ }
      | TkRule::HereDoc { .. }
      | TkRule::Eoi
      | TkRule::Soi
      | TkRule::Null
      | TkRule::Str => None,
    }
  }

  /// Lossy conversion back to Tk. Useful for feeding subtokens back into functions that expect Tks, like `is_valid`.
  fn as_tk(&self) -> Tk {
    Tk {
      span: self.span.clone(),
      class: TkRule::Str,
      flags: TkFlags::empty(),
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
  where
    F: Fn(&mut Peekable<CharIndices>) -> (bool, usize),
  {
    let (closed, inner_consumed) = lexer(chars);
    *consumed += inner_consumed;

    let opener_size = match new_class {
      CtxTkRule::ProcSubIn | CtxTkRule::ProcSubOut | CtxTkRule::CmdSub => 2,
      CtxTkRule::Subshell | CtxTkRule::BacktickSub => 1,
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
    let inner_tokens = LexStream::new(sub_src, LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .map(|tk| tk.rebase_into(span, body_start)) // map to outer span
      .flat_map(|value: Tk| CtxTk::from_tk(&value))
      .collect();

    Self {
      span: Span::new(token_start..token_end, span.get_source()),
      class: new_class,
      sub_tokens: inner_tokens,
    }
  }

  /// Check if a position is a valid split point
  ///
  /// Valid split points are those that are strictly within the token's span,
  /// and do not fall within any of its subtokens' spans.
  ///
  /// This is used to determine if we can split a token at a given position without breaking any nested structures.
  pub fn can_split_at(&self, at: usize) -> bool {
    let r = self.span.range();
    if !(r.start..r.end).contains(&at) {
      return false;
    }
    !self.sub_tokens.iter().any(|c| {
      let cr = c.span.range();
      cr.start < at && at < cr.end
    })
  }

  /// Split a `CtxTk` at a specific byte position
  ///
  /// The split point must be a valid split point as defined by `can_split_at`.
  /// Panics if the split point is invalid.
  pub fn split_at(self, at: usize) -> (CtxTk, CtxTk) {
    assert!(
      self.can_split_at(at),
      "split point falls inside of child token span"
    );
    let CtxTk {
      span,
      class,
      sub_tokens,
    } = self;
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
      },
    )
  }

  /// Get the position of the cursor relative to the start of this token, if it falls within the token's span
  ///
  /// Returns None if the cursor is outside the token's span
  pub fn relative_cursor_pos(&self, at: usize) -> Option<usize> {
    if !self.range_inclusive().contains(&at) {
      return None;
    }
    Some(at - self.span.range().start)
  }

  pub fn split_str_at(&self, at: usize) -> Option<(&[u8], &[u8])> {
    let cursor_pos = self.relative_cursor_pos(at)?;

    self.span().as_bytes().split_at_checked(cursor_pos)
  }

  pub fn prefix_from(&self, at: usize) -> Option<&[u8]> {
    self.split_str_at(at).map(|(prefix, _)| prefix)
  }

  /// Get the entire vertical slice that the cursor intersects with
  ///
  /// Sorted by depth, deepest are at the end.
  /// Calling `.pop()` on the result will give you the most specific token under the cursor,
  /// and the rest of the vector will be its parents up to the root.
  pub fn get_branch(&self, cursor_pos: usize) -> Vec<&CtxTk> {
    self.get_branch_inner(cursor_pos, vec![])
  }
  pub fn get_branch_inner<'a>(
    &'a self,
    cursor_pos: usize,
    mut nodes: Vec<&'a CtxTk>,
  ) -> Vec<&'a CtxTk> {
    if !self.range_inclusive().contains(&cursor_pos) {
      return nodes;
    }
    nodes.push(self);

    for token in &self.sub_tokens {
      if token.range_inclusive().contains(&cursor_pos) {
        return token.get_branch_inner(cursor_pos, nodes);
      }
    }

    nodes
  }

  pub fn find_nodes<F: Fn(&CtxTk) -> bool>(&self, pred: F) -> Vec<&CtxTk> {
    let mut found = vec![];
    let mut work: VecDeque<&CtxTk> = self.sub_tokens().iter().collect();

    while let Some(child) = work.pop_front() {
      for sub_token in &child.sub_tokens {
        work.push_back(sub_token);
      }
      if pred(child) {
        found.push(child);
      }
    }

    found
  }

  pub fn leaf(span: Span, class: CtxTkRule) -> Self {
    Self {
      span,
      class,
      sub_tokens: vec![],
    }
  }

  pub fn from_ex_tk(tk: ExTk) -> Vec<Self> {
    let (class, span) = tk.unpack();
    match class {
      ExTkRule::Bang => vec![Self::leaf(span, CtxTkRule::ExBang)],
      ExTkRule::Append => vec![Self::leaf(span, CtxTkRule::Operator)],
      ExTkRule::NormalSeq | ExTkRule::Argument => vec![Self::leaf(span, CtxTkRule::Argument)],
      ExTkRule::Address(addr) => match addr {
        ExLineAddr::Number
        | ExLineAddr::Dot
        | ExLineAddr::Dollar
        | ExLineAddr::Percent
        | ExLineAddr::Offset
        | ExLineAddr::Mark => vec![Self::leaf(span, CtxTkRule::ExAddress)],

        ExLineAddr::Pattern | ExLineAddr::PatternRev => {
          vec![Self::leaf(span, CtxTkRule::ExPattern)]
        }

        ExLineAddr::Comma => vec![Self::leaf(span, CtxTkRule::Separator)],
      },
      ExTkRule::Command(cmd) => {
        if let ExCommand::Unknown = cmd {
          if Shed::logic(|l| l.get_ex_alias(&span.to_str_lossy())).is_some() {
            vec![Self::leaf(span, CtxTkRule::ValidExCommand)]
          } else {
            vec![Self::leaf(span, CtxTkRule::InvalidExCommand)]
          }
        } else {
          vec![Self::leaf(span, CtxTkRule::ValidExCommand)]
        }
      }
      ExTkRule::ShellTk(tk) => Self::from_tk(&tk),
    }
  }

  #[expect(clippy::too_many_lines)]
  /// Create a `CtxTk` from a Tk
  ///
  /// returns a Vec<CtxTk> because this is used to recursively classify child tokens as well
  pub fn from_tk(value: &Tk) -> Vec<CtxTk> {
    let Tk { class, span, flags } = value;
    if let Some(class) = Self::rule_for(class) {
      return vec![Self {
        span: span.clone(),
        class,
        sub_tokens: vec![],
      }];
    }

    let raw = span.to_str_lossy();
    let mut chars = raw.char_indices().peekable();

    let new_class = if flags.contains(TkFlags::IS_ARITH) {
      CtxTkRule::Arithmetic
    } else if flags.contains(TkFlags::IS_SUBSH | TkFlags::IS_CMD) {
      chars.next(); // consume '('
      return vec![CtxTk::from_cmd_sub(
        // lets just build this here. simple enough
        &mut chars,
        CtxTkRule::Subshell,
        &mut 0,
        0,
        lex_subshell,
        span,
      )];
    } else if flags.intersects(TkFlags::BUILTIN | TkFlags::IS_CMD) {
      if let Some(kind) = is_valid(value) {
        CtxTkRule::ValidCommand(kind)
      } else {
        CtxTkRule::InvalidCommand
      }
    } else if flags.intersects(TkFlags::KEYWORD | TkFlags::FUNCNAME) {
      // Keywords are atomic literal text, we know exactly what they look like
      // So we aren't going to scan the token's sub spans, we're just gonna return it.
      return vec![Self {
        span: span.clone(),
        class: CtxTkRule::Keyword,
        sub_tokens: vec![],
      }];
    } else if flags.contains(TkFlags::ASSIGN) && !value.as_bytes().starts_with(b"=") {
      return parse_assignment(span, *flags);
    } else if check_path_exists(&value.to_str_lossy()) {
      CtxTkRule::ArgumentFile
    } else {
      // regular argument. lets subdivide it further on COMP_WORDBREAKS members
      let (_, sub_tokens) = scan_subspans(
        &mut chars,
        span,
        *flags,
        &ScanCtx::TOP_LEVEL,
        TerminatorCtx::Eof,
      );
      return vec![Self {
        span: span.clone(),
        class: CtxTkRule::Argument,
        sub_tokens,
      }];
    };
    let scan_ctx = if flags.contains(TkFlags::IS_ARITH) {
      ScanCtx::ARITH
    } else {
      ScanCtx::TOP_LEVEL
    };

    let (_, sub_tokens) = scan_subspans(&mut chars, span, *flags, &scan_ctx, TerminatorCtx::Eof);

    if flags.contains(TkFlags::IS_HEREDOC)
      && let TkRule::HereDoc {
        start_delim,
        end_delim,
      } = class
    {
      let body_tokens = if flags.contains(TkFlags::LIT_HEREDOC) {
        vec![]
      } else {
        sub_tokens
      };
      if let Some(end_delim) = end_delim {
        return vec![
          Self {
            span: (**start_delim).clone(),
            class: CtxTkRule::HereDocStart,
            sub_tokens: vec![],
          },
          Self {
            span: span.clone(),
            class: CtxTkRule::HereDocBody,
            sub_tokens: body_tokens,
          },
          Self {
            span: (**end_delim).clone(),
            class: CtxTkRule::HereDocEnd,
            sub_tokens: vec![],
          },
        ];
      }
      return vec![
        Self {
          span: (**start_delim).clone(),
          class: CtxTkRule::HereDocStart,
          sub_tokens: vec![],
        },
        Self {
          span: span.clone(),
          class: CtxTkRule::HereDocBody,
          sub_tokens: body_tokens,
        },
      ];
    }

    vec![Self {
      span: span.clone(),
      class: new_class,
      sub_tokens,
    }]
  }
}

/// Check if a given path refers to a file or is a prefix of an existing filename
fn check_path_exists(path: &str) -> bool {
  // NOTE: keep an eye on this. this might have pretty significant overhead on network mounts
  if !shopt!(highlight.check_files) {
    return false;
  }

  if path.is_empty() {
    return false;
  }
  if Path::new(path).exists() {
    return true;
  }

  let unescaped = unescape_str(path.as_bytes());
  let Ok(expanded) = expand_raw_inner(&mut unescaped.cursor(), false, false) else {
    return false;
  };
  let stripped = String::from_utf8_lossy(&expanded.into_bytes()).into_owned();
  if stripped.is_empty() {
    return false;
  }

  // Does any entry start with `stripped`?
  let bytes = stripped.as_bytes();
  let (dir, prefix): (&Path, &[u8]) = match bytes.iter().rposition(|&b| b == b'/') {
    Some(idx) => (util::path_from_bytes(&bytes[..=idx]), &bytes[idx + 1..]),
    None => (Path::new("."), bytes),
  };
  util::path_entries(dir).any(|e| e.file_name().as_bytes().starts_with(prefix))
}

/// Break a token at `comp_wordbreaks`
///
/// This allows for styling filenames in tokens like `--foo=/path/to/bar`
/// And also allows the completer to get more fine-grained context
fn subdivide_argument(mut tk: CtxTk) -> Vec<CtxTk> {
  let wordbreaks = state::util::get_comp_wordbreaks();
  let mut tokens = vec![];

  let push_token = |tks: &mut Vec<CtxTk>, mut tk: CtxTk| {
    if check_path_exists(&tk.span.to_str_lossy()) {
      tk.class = CtxTkRule::ArgumentFile;
    }
    tks.push(tk);
  };

  loop {
    let raw = tk.span().to_str_lossy();
    let span_start = tk.span().range().start;
    let span_end = tk.span().range().end;

    let split_at = raw.char_indices().find_map(|(byte, ch)| {
      if !wordbreaks.to_str_lossy().contains(ch) {
        return None;
      }
      let after = span_start + byte + ch.len_utf8();
      let can_split = after < span_end && tk.can_split_at(after);

      can_split.then_some(after)
    });

    if let Some(pos) = split_at {
      let (left, right) = tk.split_at(pos);
      push_token(&mut tokens, left);
      tk = right;
    } else {
      push_token(&mut tokens, tk);
      break;
    }
  }

  tokens
}

/// Tokenize an assignment-shaped Tk like `arr[$(echo foo)]=biz` into
/// structured `AssignmentLeft` / `AssignmentOp` / `AssignmentRight` tokens.
/// The index and the RHS get recursively scanned so nested expansions
/// (`$(...)`, `${...}`, etc.) are properly recognized for highlighting
/// and completion. Without this, the whole string falls into
/// `subdivide_argument` and gets shredded on `=` / `[` / `(` from
/// `COMP_WORDBREAKS` with no awareness of the underlying structure.
fn parse_assignment(span: &Span, flags: TkFlags) -> Vec<CtxTk> {
  let raw = span.to_str_lossy();
  let span_start = span.range().start;

  // Find the `=` operator. ASSIGN was set, so this should always succeed.
  let Some((eq_off, eq_len)) = split_at_unescaped(raw.as_bytes(), b"=") else {
    return vec![CtxTk {
      span: span.clone(),
      class: CtxTkRule::Argument,
      sub_tokens: vec![],
    }];
  };
  let lhs_text = &raw[..eq_off];
  let lhs_end = span_start + eq_off;
  let op_end = lhs_end + eq_len;

  // LHS: ParamName + optional ParamIndex.
  let mut lhs_sub = vec![];
  let bracket_off = lhs_text.find('[');
  let name_end = bracket_off.map_or(lhs_end, |b| span_start + b);

  lhs_sub.push(CtxTk {
    span: Span::new(span_start..name_end, span.get_source()),
    class: CtxTkRule::ParamName,
    sub_tokens: vec![],
  });

  if let Some(b) = bracket_off {
    // Find matching `]` tracking depth (so `arr[a[0]]` parses correctly).
    let mut depth = 0;
    let mut close_off = lhs_text.len();
    for (i, ch) in lhs_text[b..].char_indices() {
      match ch {
        '[' => depth += 1,
        ']' => {
          depth -= 1;
          if depth == 0 {
            close_off = b + i + 1;
            break;
          }
        }
        _ => {}
      }
    }
    let index_start = span_start + b;
    let index_end = span_start + close_off;

    // Recursively scan the index contents (excluding the brackets).
    // ARITH context matches what `${arr[idx]}` already uses.
    let inner_text = &lhs_text[b + 1..close_off - 1];
    let inner_span = Span::new((index_start + 1)..(index_end - 1), span.get_source());
    let mut inner_chars = inner_text.char_indices().peekable();
    let (_, inner) = scan_subspans(
      &mut inner_chars,
      &inner_span,
      flags,
      &ScanCtx::ARITH,
      TerminatorCtx::Eof,
    );

    lhs_sub.push(CtxTk {
      span: Span::new(index_start..index_end, span.get_source()),
      class: CtxTkRule::ParamIndex,
      sub_tokens: inner,
    });
  }

  let lhs_tk = CtxTk {
    span: Span::new(span_start..lhs_end, span.get_source()),
    class: CtxTkRule::AssignmentLeft,
    sub_tokens: lhs_sub,
  };

  let op_tk = CtxTk {
    span: Span::new(lhs_end..op_end, span.get_source()),
    class: CtxTkRule::AssignmentOp,
    sub_tokens: vec![],
  };

  // RHS: scan as full top-level expansion context.
  let rhs_text = &raw[eq_off + eq_len..];
  let rhs_start = op_end;
  let rhs_end = span_start + raw.len();
  let rhs_span = Span::new(rhs_start..rhs_end, span.get_source());

  let rhs_tk = if rhs_text.starts_with('(') {
    let close_off = rhs_text.rfind(')').filter(|&c| c > 0);
    let inner_end = close_off.map_or(rhs_text.len(), |c| c);
    let inner_text = &rhs_text[1..inner_end];
    let inner_span = Span::new((rhs_start + 1)..(rhs_start + inner_end), span.get_source());
    let mut inner_chars = inner_text.char_indices().peekable();
    let (_, inner) = scan_subspans(
      &mut inner_chars,
      &inner_span,
      flags,
      &ScanCtx::TOP_LEVEL,
      TerminatorCtx::Eof,
    );
    CtxTk {
      span: rhs_span,
      class: CtxTkRule::ArrayLiteral,
      sub_tokens: inner,
    }
  } else {
    let mut rhs_chars = rhs_text.char_indices().peekable();
    let (_, rhs_sub) = scan_subspans(
      &mut rhs_chars,
      &rhs_span,
      flags,
      &ScanCtx::TOP_LEVEL,
      TerminatorCtx::Eof,
    );
    CtxTk {
      span: rhs_span,
      class: CtxTkRule::AssignmentRight,
      sub_tokens: rhs_sub,
    }
  };

  vec![lhs_tk, op_tk, rhs_tk]
}

fn next_is(chars: &mut Peekable<CharIndices>, ch: char) -> bool {
  chars.peek().is_some_and(|(_, c)| *c == ch)
}

/// After the op chars have been consumed, push a `ParamOp` token covering them
/// and a `ParamArg` token covering the body up to the closing `}`. Returns the
/// new position-after-last-consumed (which will be one past the `}` if found).
///
/// `op_start` is the absolute position where the op chars begin.
/// `op_size` is the number of bytes the op occupies (1 for `-`, 2 for `:-` etc.)
fn parse_op_body(
  chars: &mut Peekable<CharIndices>,
  consumed: &mut usize,
  span: &Span,
  flags: TkFlags,
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
    chars,
    span,
    flags,
    &ScanCtx::TOP_LEVEL,
    TerminatorCtx::ParamExpansion,
  );
  *consumed += inner_consumed;

  let arg_end = op_end + inner_consumed;
  let arg_text_end =
    if inner_consumed > 0 && span.get_source().as_bytes().get(arg_end - 1) == Some(&b'}') {
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

#[expect(clippy::too_many_arguments)] // teehee
fn get_subtoken(
  chars: &mut Peekable<CharIndices>,
  span: &Span,
  flags: TkFlags,
  term_ctx: TerminatorCtx,
  scan_ctx: &ScanCtx,
  opener_len: usize,
  token_start: usize,
  consumed: &mut usize,
  rule: CtxTkRule,
) -> CtxTk {
  let (inner_consumed, inner) = scan_subspans(chars, span, flags, scan_ctx, term_ctx);
  *consumed += inner_consumed;

  let token_end = token_start + opener_len + inner_consumed; // include the opening
  let span = Span::new(token_start..token_end, span.get_source());
  CtxTk {
    span,
    class: rule,
    sub_tokens: inner,
  }
}

#[expect(clippy::too_many_lines)]
fn scan_subspans(
  chars: &mut Peekable<CharIndices>,
  span: &Span,
  flags: TkFlags,
  scan_ctx: &ScanCtx,
  term_ctx: TerminatorCtx,
) -> (usize, Vec<CtxTk>) {
  use ScanCtx as S;
  let consumed = &mut 0;
  let mut sub_tokens = vec![];
  let consume = |chars: &mut Peekable<CharIndices>, cons: &mut usize| {
    chars.next().map(|(i, c)| {
      *cons += c.len_utf8();
      (i, c)
    })
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

      return (*consumed, sub_tokens);
    }

    match ch {
      '\\' if scan_ctx.contains(S::ESCAPE) => {
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
      '('
        if next_is(chars, '(')
          && scan_ctx.contains(S::ARITH)
          && flags.contains(TkFlags::IS_CMD) =>
      {
        consume(chars, consumed); // consume the second '('
        let sub_tk = get_subtoken(
          chars,
          span,
          flags,
          TerminatorCtx::Arith,
          &ScanCtx::ARITH,
          2,
          i + span.range().start,
          consumed,
          CtxTkRule::Arithmetic,
        );
        sub_tokens.push(sub_tk);
      }
      '(' if flags.contains(TkFlags::IS_CMD) => {
        let sub_tk =
          CtxTk::from_cmd_sub(chars, CtxTkRule::Subshell, consumed, i, lex_subshell, span);
        sub_tokens.push(sub_tk);
      }
      '"' if scan_ctx.contains(S::QUOTE) => {
        let sub_tk = get_subtoken(
          chars,
          span,
          flags,
          TerminatorCtx::DoubleQuote,
          &ScanCtx::DOUBLE_QUOTE,
          1,
          i + span.range().start,
          consumed,
          CtxTkRule::DoubleString,
        );
        sub_tokens.push(sub_tk);
      }
      '\'' if scan_ctx.contains(S::QUOTE) => {
        let sub_tk = get_subtoken(
          chars,
          span,
          flags,
          TerminatorCtx::SingleQuote,
          &ScanCtx::SINGLE_QUOTE,
          1,
          i + span.range().start,
          consumed,
          CtxTkRule::SingleString,
        );
        sub_tokens.push(sub_tk);
      }
      glob @ ('*' | '?' | '[') if scan_ctx.contains(S::GLOB) => {
        match glob {
          '*' | '?' => {
            let span = Span::new(
              i + span.range().start..i + span.range().start + 1,
              span.get_source(),
            );
            sub_tokens.push(CtxTk {
              span,
              class: CtxTkRule::Glob,
              sub_tokens: vec![],
            });
          }
          '[' => {
            let span_start = i + span.range().start;
            let orig_consumed = *consumed;
            while let Some(&(_, ch)) = chars.peek() {
              consume(chars, consumed);
              if ch == '\\' {
                consume(chars, consumed); // skip the escaped char
                continue;
              }
              if ch == ']' {
                break;
              }
            }
            if *consumed == orig_consumed {
              continue; // no valid glob chars, skip
            }
            let span = Span::new(
              span_start..(span_start + 1 + (*consumed - orig_consumed)),
              span.get_source(),
            );
            sub_tokens.push(CtxTk {
              span,
              class: CtxTkRule::Glob,
              sub_tokens: vec![],
            });
          }
          _ => unreachable!(),
        }
      }
      '~' if scan_ctx.contains(S::TILDE) => {
        let span = Span::new(
          i + span.range().start..i + span.range().start + 1,
          span.get_source(),
        );
        sub_tokens.push(CtxTk {
          span,
          class: CtxTkRule::Tilde,
          sub_tokens: vec![],
        });
      }
      '!' if scan_ctx.contains(S::HIST_EXP) => {
        let Some(&(_, ch)) = chars.peek() else {
          continue;
        };
        match ch {
          '!' | '$' => {
            consume(chars, consumed);
            let span_start = i + span.range().start;
            let span = Span::new(
              span_start..(span_start + 1 + ch.len_utf8()),
              span.get_source(),
            );
            sub_tokens.push(CtxTk {
              span,
              class: CtxTkRule::HistExp,
              sub_tokens: vec![],
            });
          }
          c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => {
            let span_start = i + span.range().start;
            let orig_consumed = *consumed;
            while let Some(&(_, hexp_ch)) = chars.peek() {
              match hexp_ch {
                c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => consume(chars, consumed),
                _ => break,
              };
            }
            if *consumed == orig_consumed {
              continue; // no valid history expansion token chars, skip
            }
            let delta = *consumed - orig_consumed;
            let span = Span::new(span_start..(span_start + 1 + delta), span.get_source());
            log::debug!("Found history expansion token: '{}'", span.to_str_lossy());
            sub_tokens.push(CtxTk {
              span,
              class: CtxTkRule::HistExp,
              sub_tokens: vec![],
            });
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
      dir @ ('<' | '>') if next_is(chars, '(') && scan_ctx.contains(S::VAR_SUB) => {
        if consume(chars, consumed).is_none() {
          continue;
        }
        let class = match dir {
          '<' => CtxTkRule::ProcSubIn,
          '>' => CtxTkRule::ProcSubOut,
          _ => unreachable!(),
        };

        let sub_tk = CtxTk::from_cmd_sub(chars, class, consumed, i, lex_subshell, span);
        sub_tokens.push(sub_tk);
      }
      '$' => {
        if next_is(chars, '(') {
          if consume(chars, consumed).is_none() {
            continue;
          }

          if next_is(chars, '(') && scan_ctx.contains(S::VAR_SUB) {
            consume(chars, consumed); // consume the inner arithmetic opener '('
            let sub_tk = get_subtoken(
              chars,
              span,
              flags,
              TerminatorCtx::ArithSub,
              &ScanCtx::ARITH,
              3,
              i + span.range().start,
              consumed,
              CtxTkRule::Arithmetic,
            );
            sub_tokens.push(sub_tk);
          } else if scan_ctx.contains(S::CMD_SUB) {
            let sub_tk =
              CtxTk::from_cmd_sub(chars, CtxTkRule::CmdSub, consumed, i, lex_subshell, span);
            sub_tokens.push(sub_tk);
          }
        } else if next_is(chars, '\'') && scan_ctx.contains(S::QUOTE) {
          // $'...' ANSI-C quoting
          consume(chars, consumed); // consume the opening quote

          let (inner_consumed, inner) = scan_subspans(
            chars,
            span,
            flags,
            &ScanCtx::DOLLAR_QUOTE,
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
          });
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
            && (ch == '#' || ch == '!')
          {
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
            // Empty body (`${}` or `${` at EOF). Pull in a trailing `}` if
            // present so the wrapper span covers it; otherwise we leave it
            // as a stray brace for the outer scanner.
            if let Some(&(_, '}')) = chars.peek() {
              consume(chars, consumed);
              pos += 1;
            }
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
            let (inner_consumed, inner) =
              scan_subspans(chars, span, flags, &ScanCtx::ARITH, TerminatorCtx::VarIndex);
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
          match ch {
            '}' => {
              consume(chars, consumed);
              pos += 1;
            }
            ':' => {
              consume(chars, consumed);
              pos += 1;
              match chars.peek().map(|(_, c)| *c) {
                Some('-' | '=' | '?' | '+') => {
                  consume(chars, consumed);
                  pos += 1;
                  // op span covers ":-" / ":=" / etc. (2 bytes)
                  pos = parse_op_body(
                    chars,
                    consumed,
                    span,
                    flags,
                    pos - 2,
                    2,
                    &mut var_sub_tokens,
                  );
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
                      if c == ':' {
                        hit_colon = true;
                      }
                      break;
                    }
                    if c == '{' {
                      depth += 1;
                    }
                    if c == '}' {
                      depth -= 1;
                    }
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
                      if depth == 0 && c == '}' {
                        break;
                      }
                      if c == '{' {
                        depth += 1;
                      }
                      if c == '}' {
                        depth -= 1;
                      }
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
                }
                None => {}
              }
            }
            '-' | '=' | '?' | '+' => {
              consume(chars, consumed);
              pos += 1;
              pos = parse_op_body(
                chars,
                consumed,
                span,
                flags,
                pos - 1,
                1,
                &mut var_sub_tokens,
              );
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
                flags,
                pos - op_size,
                op_size,
                &mut var_sub_tokens,
              );
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
                flags,
                pos - op_size,
                op_size,
                &mut var_sub_tokens,
              );
            }
            '/' => {
              // Substitution: ${var/pat/rep}, ${var//pat/rep}, ${var/#pat/rep}, ${var/%pat/rep}
              // Flat-scan structure: ParamOp ("/" or "//" or "/#" or "/%"), ParamArg (pattern),
              // optional ParamOp ("/"), ParamArg (replacement), then "}".
              consume(chars, consumed);
              pos += 1;
              let op_size = match chars.peek().map(|(_, c)| *c) {
                Some('/' | '#' | '%') => {
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
                  if c == '/' {
                    hit_slash = true;
                  }
                  break;
                }
                if c == '{' {
                  depth += 1;
                }
                if c == '}' {
                  depth -= 1;
                }
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
                  if depth == 0 && c == '}' {
                    break;
                  }
                  if c == '{' {
                    depth += 1;
                  }
                  if c == '}' {
                    depth -= 1;
                  }
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
            }
            _ => {}
          }

          // Wrap and push (whether or not the op was fully recognized - a
          // partial wrapper is better than none for completion dispatch).
          sub_tokens.push(CtxTk {
            span: Span::new(var_start..pos, span.get_source()),
            class: CtxTkRule::VarSub,
            sub_tokens: var_sub_tokens,
          });
        } else if scan_ctx.contains(S::VAR_SUB) {
          let sub_start = i + span.range().start;
          let orig_consumed = *consumed;
          let first = chars.peek().map(|(_, c)| *c);

          let is_param = first.is_some_and(|c| ShellParam::from_char(c).is_some());
          let is_digit = first.is_some_and(|c| c.is_ascii_digit());
          let is_var_char = first.is_some_and(|c| c.is_ascii_alphabetic() || c == '_');

          if is_param || is_digit {
            consume(chars, consumed);
          } else if is_var_char {
            while let Some(&(_, ch)) = chars.peek() {
              if !(ch.is_ascii_alphanumeric() || ch == '_') {
                break;
              }
              consume(chars, consumed);
            }
          }

          let var_size = *consumed - orig_consumed;
          let sub_end = sub_start + 1 + var_size; // include the '$' in the span

          let outer_span = Span::new(sub_start..sub_end, span.get_source());
          let inner_span = Span::new(sub_start + 1..sub_end, span.get_source());
          let sub_token = CtxTk {
            span: inner_span,
            class: CtxTkRule::ParamName,
            sub_tokens: vec![],
          };
          sub_tokens.push(CtxTk {
            span: outer_span,
            class: CtxTkRule::VarSub,
            sub_tokens: vec![sub_token],
          });
        }
      }
      'a'..='z' | 'A'..='Z' | '_' if in_arith => {
        let var_start = i + span.range().start;
        let mut var_consumed = ch.len_utf8();
        while let Some(&(_, ch)) = chars.peek() {
          if !(ch.is_ascii_alphanumeric() || ch == '_') {
            break;
          }
          consume(chars, consumed);
          var_consumed += ch.len_utf8();
        }
        let var_span = Span::new(var_start..(var_start + var_consumed), span.get_source());
        sub_tokens.push(CtxTk {
          span: var_span,
          class: CtxTkRule::ArithVar,
          sub_tokens: vec![],
        });
      }
      '0'..='9' if in_arith => {
        let num_start = i + span.range().start;
        let mut num_consumed = ch.len_utf8();
        while let Some(&(_, ch)) = chars.peek() {
          if !ch.is_ascii_digit() {
            break;
          }
          consume(chars, consumed);
          num_consumed += ch.len_utf8();
        }
        let num_span = Span::new(num_start..(num_start + num_consumed), span.get_source());
        sub_tokens.push(CtxTk {
          span: num_span,
          class: CtxTkRule::ArithNumber,
          sub_tokens: vec![],
        });
      }
      '+' | '/' | '%' | '-' | '*' | '=' | '&' | '^' | '|' | '~' | '!' | '<' | '>' | '?' | ':'
      | ','
        if in_arith =>
      {
        let op_start = i + span.range().start;
        let op_end = op_start + 1;
        let op_span = Span::new(op_start..op_end, span.get_source());
        sub_tokens.push(CtxTk {
          span: op_span,
          class: CtxTkRule::ArithOp,
          sub_tokens: vec![],
        });
      }
      _ => {}
    }
  }

  (*consumed, sub_tokens)
}

fn lex_backtick(chars: &mut Peekable<CharIndices>) -> (bool, usize) {
  let mut qt_state = QuoteState::default();
  let mut consumed = 0;
  let mut closed = false;
  let advance = |chars: &mut Peekable<CharIndices>, cons: &mut usize| {
    // advance iterator, increment consumed bytes
    chars.next().map(|(_, c)| {
      *cons += c.len_utf8();
      c
    })
  };

  match_loop!(advance(chars, &mut consumed) => ch, {
    '\\' if (!qt_state.in_single() || chars.peek().is_some_and(|(_,c)| *c == '\'')) => {
      advance(chars, &mut consumed);
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

fn lex_subshell(chars: &mut Peekable<CharIndices>) -> (bool, usize) {
  lex_delim(chars, '(')
}

fn lex_delim(chars: &mut Peekable<CharIndices>, opener: char) -> (bool, usize) {
  let closer = match opener {
    '(' => ')',
    '{' => '}',
    '[' => ']',
    '<' => '>',
    _ => unreachable!(),
  };
  let mut qt_state = QuoteState::default();
  let mut consumed = 0;
  let mut depth = 1;
  let mut is_closed = false;
  let advance = |chars: &mut Peekable<CharIndices>, cons: &mut usize| {
    // advance iterator, increment consumed bytes
    chars.next().map(|(_, c)| {
      *cons += c.len_utf8();
      c
    })
  };

  match_loop!(advance(chars, &mut consumed) => ch, {
    '\\' if (!qt_state.in_single() || chars.peek().is_some_and(|(_,c)| *c == '\'')) => {
      advance(chars, &mut consumed);
    }
    '"' => qt_state.toggle_double(),
    '\'' => qt_state.toggle_single(),

    _ if ch == opener && qt_state.outside() => depth += 1,
    _ if ch == closer && qt_state.outside() => {
      depth -= 1;
      if depth == 0 {
        // closer included in `consumed`; matches `lex_backtick` convention so
        // that `from_cmd_sub` can uniformly trim one byte to get the body.
        is_closed = true;
        break
      }
    }
    _ => {}
  });

  (is_closed, consumed)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Lex `src` and convert the first non-trivial token to a `CtxTk`.
  fn parse_first(src: &str) -> CtxTk {
    let tk = LexStream::new(src.as_bytes(), LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .find(|t| !matches!(t.class, TkRule::Soi | TkRule::Eoi | TkRule::Sep))
      .expect("expected at least one token");
    CtxTk::from_tk(&tk).pop().unwrap()
  }

  /// Depth-first search for the first sub-token matching `class`.
  fn find(tk: &CtxTk, class: CtxTkRule) -> Option<&CtxTk> {
    if tk.class == class {
      return Some(tk);
    }
    for sub in &tk.sub_tokens {
      if let Some(hit) = find(sub, class) {
        return Some(hit);
      }
    }
    None
  }

  fn span_str<'a>(tk: &CtxTk, src: &'a str) -> &'a str {
    let r = tk.span.range();
    &src[r]
  }

  #[test]
  fn tilde_expansion_strips_markers() {
    // expand_no_side_effects must strip in-band expansion markers, otherwise
    // is_valid_cmd sees a path with PUA chars in it and Path::is_absolute
    // returns false, misclassifying valid commands as InvalidCommand.
    let tk = LexStream::new(b"~/bin/foo", LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .find(|t| !matches!(t.class, TkRule::Soi | TkRule::Eoi | TkRule::Sep))
      .expect("token");
    let expanded = tk.expand_no_side_effects().expect("expand");
    let word = expanded.get_first_word().expect("word");
    assert!(
      word.to_str_lossy().starts_with('/'),
      "tilde-expanded path should be absolute, got {word:?}"
    );
    assert!(
      !word
        .to_str_lossy()
        .chars()
        .any(|c| ('\u{fdd0}'..='\u{fdef}').contains(&c)),
      "expanded path should not contain marker chars, got {word:?}"
    );
  }

  #[test]
  fn dbracket_classification() {
    let toks = get_context_tokens("[[ -f foo ]]");
    let find_class = |s: &str| {
      toks
        .iter()
        .find(|t| t.span.to_str_lossy() == s)
        .map(|t| t.class)
    };
    assert_eq!(
      find_class("[["),
      Some(CtxTkRule::ValidCommand(CmdKind::Builtin))
    );
    assert_eq!(find_class("-f"), Some(CtxTkRule::Argument));
    assert_eq!(find_class("foo"), Some(CtxTkRule::Argument));
    assert_eq!(find_class("]]"), Some(CtxTkRule::Argument));
  }

  #[test]
  fn var_sub_simple() {
    let src = "$foo";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    assert_eq!(span_str(v, src), "$foo");
  }

  #[test]
  fn bare_dollar_emits_var_sub_with_empty_param_name() {
    // A `$` not followed by a valid variable starter must still emit a
    // VarSub wrapper containing a zero-width ParamName, otherwise the
    // completion pipeline falls through to path completion instead of
    // dispatching variable-name completion.
    //
    // Two arms to cover:
    //   1. `$` at end of input (chars.peek() is None)
    //   2. `$` followed by a non-var-char (e.g. '.', ' ')
    let toks = get_context_tokens("echo $");
    let last = toks.last().expect("trailing token");
    let v = find(last, CtxTkRule::VarSub).expect("VarSub for bare $");
    assert_eq!(v.span().to_str_lossy(), "$");
    let n = find(v, CtxTkRule::ParamName).expect("zero-width ParamName under VarSub");
    assert_eq!(n.span().to_str_lossy(), "");
    // range_inclusive on the inner ParamName must catch the cursor at
    // the position immediately after the `$`, so resolve() can dispatch
    // to CompStrat::Var { prefix: "" }.
    let cursor = "echo $".len();
    assert!(n.range_inclusive().contains(&cursor));

    // Arm 2: `$` followed by a non-var character.
    let toks = get_context_tokens("echo $.");
    let v = toks
      .iter()
      .find_map(|t| find(t, CtxTkRule::VarSub))
      .expect("VarSub for `$` before `.`");
    assert_eq!(v.span().to_str_lossy(), "$");
    let n = find(v, CtxTkRule::ParamName).expect("ParamName for `$` before `.`");
    assert_eq!(n.span().to_str_lossy(), "");
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
    assert!(
      find(&tk, CtxTkRule::ArithVar).is_none(),
      "plain word should not produce ArithVar sub-tokens"
    );
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
    let inner = outer
      .sub_tokens
      .iter()
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

  // ===================== Subshell sub-token classification =====================

  #[test]
  fn subshell_paren_classified_as_operator() {
    // After the subshell refactor, `(` and `)` are separate SubshStart /
    // SubshEnd tokens at the lex level, both classified as Operator at the
    // CtxTk level. `(echo foo)` is no longer a single fat Subshell token.
    let src = "(echo foo)";
    let tks = get_context_tokens(src);
    let opener = tks.first().expect("at least one token");
    assert_eq!(opener.class, CtxTkRule::Operator);
    assert_eq!(span_str(opener, src), "(");

    let closer = tks.last().expect("at least one token");
    assert_eq!(closer.class, CtxTkRule::Operator);
    assert_eq!(span_str(closer, src), ")");
  }

  #[test]
  fn subshell_body_classified_as_commands() {
    // The body tokens between `(` and `)` lex normally, so `echo` should
    // pick up a command classification (Valid/Invalid depending on cache).
    let src = "(echo foo)";
    let tks = get_context_tokens(src);
    let cmds = tks
      .iter()
      .filter(|t| {
        matches!(
          t.class,
          CtxTkRule::ValidCommand(_) | CtxTkRule::InvalidCommand
        )
      })
      .count();
    assert!(
      cmds >= 1,
      "subshell body should classify echo as a command, tokens = {tks:#?}"
    );
  }

  #[test]
  fn arithmetic_dollar_form_atoms() {
    // $((x + 5)) should produce an Arithmetic node with ArithVar/ArithOp/ArithNumber.
    let src = "$((x + 5))";
    let tk = parse_first(src);
    let a = find(&tk, CtxTkRule::Arithmetic).expect("Arithmetic");
    assert_eq!(span_str(a, src), "$((x + 5))");
    assert!(find(a, CtxTkRule::ArithVar).is_some(), "expected ArithVar");
    assert!(find(a, CtxTkRule::ArithOp).is_some(), "expected ArithOp");
    assert!(
      find(a, CtxTkRule::ArithNumber).is_some(),
      "expected ArithNumber"
    );
  }

  #[test]
  fn arithmetic_var_span_is_complete() {
    // Off-by-one regression test: ArithVar span must cover all chars of the var name.
    let src = "$((foo))";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::ArithVar).expect("ArithVar");
    assert_eq!(span_str(v, src), "foo");
  }

  #[test]
  fn arithmetic_number_span_is_complete() {
    let src = "$((42))";
    let tk = parse_first(src);
    let n = find(&tk, CtxTkRule::ArithNumber).expect("ArithNumber");
    assert_eq!(span_str(n, src), "42");
  }

  // ─── scan_subspans branch coverage ──────────────────────────────────
  //
  // The branches not exercised by the tests above. Each test pokes one
  // arm of scan_subspans's giant match.

  // SingleString — '...' with no interior expansion

  #[test]
  fn single_string_simple() {
    let src = "'hello world'";
    let tk = parse_first(src);
    let s = find(&tk, CtxTkRule::SingleString).expect("SingleString");
    assert_eq!(span_str(s, src), "'hello world'");
  }

  #[test]
  fn single_string_suppresses_dollar_and_bang() {
    // Inside single quotes, $foo and !foo should NOT produce sub-tokens.
    let src = "'$foo and !bang'";
    let tk = parse_first(src);
    let s = find(&tk, CtxTkRule::SingleString).expect("SingleString");
    assert!(
      find(s, CtxTkRule::VarSub).is_none(),
      "VarSub should not appear inside single-quoted string"
    );
    assert!(
      find(s, CtxTkRule::HistExp).is_none(),
      "HistExp should not appear inside single-quoted string"
    );
  }

  // Glob: *, ?, [...]

  #[test]
  fn glob_star() {
    let src = "*.rs";
    let tk = parse_first(src);
    let g = find(&tk, CtxTkRule::Glob).expect("Glob");
    assert_eq!(span_str(g, src), "*");
  }

  #[test]
  fn glob_question() {
    let src = "a?.rs";
    let tk = parse_first(src);
    let g = find(&tk, CtxTkRule::Glob).expect("Glob");
    assert_eq!(span_str(g, src), "?");
  }

  #[test]
  fn glob_bracket_class() {
    let src = "[abc].txt";
    let tk = parse_first(src);
    let g = find(&tk, CtxTkRule::Glob).expect("Glob");
    assert_eq!(span_str(g, src), "[abc]");
  }

  #[test]
  fn glob_bracket_handles_escaped_close() {
    let src = r"[a\]b]";
    let tk = parse_first(src);
    let g = find(&tk, CtxTkRule::Glob).expect("Glob");
    // The scanner should consume up through the *unescaped* ']'.
    assert_eq!(span_str(g, src), r"[a\]b]");
  }

  // Tilde

  #[test]
  fn tilde_classified_as_tilde() {
    let src = "~/bin";
    let tk = parse_first(src);
    let t = find(&tk, CtxTkRule::Tilde).expect("Tilde");
    assert_eq!(span_str(t, src), "~");
  }

  // History expansion

  #[test]
  fn hist_exp_double_bang() {
    let src = "echo !!";
    let tks = get_context_tokens(src);
    let h = tks
      .iter()
      .find_map(|t| find(t, CtxTkRule::HistExp))
      .expect("HistExp");
    assert_eq!(span_str(h, src), "!!");
  }

  #[test]
  fn hist_exp_bang_dollar() {
    let src = "echo !$";
    let tks = get_context_tokens(src);
    let h = tks
      .iter()
      .find_map(|t| find(t, CtxTkRule::HistExp))
      .expect("HistExp");
    assert_eq!(span_str(h, src), "!$");
  }

  #[test]
  fn hist_exp_bang_word() {
    let src = "echo !cmd";
    let tks = get_context_tokens(src);
    let h = tks
      .iter()
      .find_map(|t| find(t, CtxTkRule::HistExp))
      .expect("HistExp");
    assert_eq!(span_str(h, src), "!cmd");
  }

  // Backtick command substitution

  #[test]
  fn backtick_cmd_sub() {
    let src = "`echo hi`";
    let tk = parse_first(src);
    let b = find(&tk, CtxTkRule::BacktickSub).expect("BacktickSub");
    assert_eq!(span_str(b, src), "`echo hi`");
  }

  // Process substitution

  #[test]
  fn proc_sub_in() {
    let src = "cat <(echo hi)";
    let tks = get_context_tokens(src);
    let p = tks
      .iter()
      .find_map(|t| find(t, CtxTkRule::ProcSubIn))
      .expect("ProcSubIn");
    assert_eq!(span_str(p, src), "<(echo hi)");
  }

  #[test]
  fn proc_sub_out() {
    let src = "tee >(grep foo)";
    let tks = get_context_tokens(src);
    let p = tks
      .iter()
      .find_map(|t| find(t, CtxTkRule::ProcSubOut))
      .expect("ProcSubOut");
    assert_eq!(span_str(p, src), ">(grep foo)");
  }

  // Dollar-quoted string $'...'

  #[test]
  fn dollar_quoted_string() {
    let src = r"$'hello\n'";
    let tk = parse_first(src);
    let d = find(&tk, CtxTkRule::DollarString).expect("DollarString");
    assert_eq!(span_str(d, src), r"$'hello\n'");
    // The escape inside should be recognized.
    assert!(
      find(d, CtxTkRule::Escape).is_some(),
      "expected Escape inside $'...'"
    );
  }

  // Parameter expansion variations

  #[test]
  fn param_expansion_bare() {
    let src = "${foo}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    assert_eq!(span_str(v, src), "${foo}");
    let name = find(v, CtxTkRule::ParamName).expect("ParamName");
    assert_eq!(span_str(name, src), "foo");
  }

  #[test]
  fn param_expansion_length_prefix() {
    let src = "${#foo}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    assert_eq!(span_str(v, src), "${#foo}");
    let prefix = find(v, CtxTkRule::ParamPrefix).expect("ParamPrefix");
    assert_eq!(span_str(prefix, src), "#");
  }

  #[test]
  fn param_expansion_indirect_prefix() {
    let src = "${!foo}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    let prefix = find(v, CtxTkRule::ParamPrefix).expect("ParamPrefix");
    assert_eq!(span_str(prefix, src), "!");
  }

  #[test]
  fn param_expansion_assign_default() {
    // Use a command context — at the top level, `${foo:=...}` looks too
    // assignment-like for the lexer to split correctly.
    let src = "echo ${foo:=bar}";
    let tks = get_context_tokens(src);
    let v = tks
      .iter()
      .find_map(|t| find(t, CtxTkRule::VarSub))
      .expect("VarSub");
    let op = find(v, CtxTkRule::ParamOp).expect("ParamOp");
    assert_eq!(span_str(op, src), ":=");
  }

  #[test]
  fn param_expansion_error_if_unset() {
    let src = "${foo:?nope}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    let op = find(v, CtxTkRule::ParamOp).expect("ParamOp");
    assert_eq!(span_str(op, src), ":?");
  }

  #[test]
  fn param_expansion_alternate_if_set() {
    let src = "${foo:+bar}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    let op = find(v, CtxTkRule::ParamOp).expect("ParamOp");
    assert_eq!(span_str(op, src), ":+");
  }

  #[test]
  fn param_expansion_no_colon_operators() {
    // The non-colon ops -, =, ?, + (treat empty differently from :-).
    // Wrap in a command context to avoid assignment-like ambiguity for `=`.
    for (op_str, body) in [
      ("-", "${foo-x}"),
      ("=", "${foo=x}"),
      ("?", "${foo?x}"),
      ("+", "${foo+x}"),
    ] {
      let src = format!("echo {body}");
      let tks = get_context_tokens(&src);
      let v = tks
        .iter()
        .find_map(|t| find(t, CtxTkRule::VarSub))
        .unwrap_or_else(|| panic!("VarSub missing for {src}"));
      let op = find(v, CtxTkRule::ParamOp).unwrap_or_else(|| panic!("ParamOp missing for {src}"));
      assert_eq!(span_str(op, &src), op_str, "src was {src}");
    }
  }

  #[test]
  fn param_expansion_hash_prefix_strip() {
    let src = "${foo#prefix}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    let op = find(v, CtxTkRule::ParamOp).expect("ParamOp");
    assert_eq!(span_str(op, src), "#");
  }

  #[test]
  fn param_expansion_double_hash_greedy_prefix() {
    let src = "${foo##prefix}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    let op = find(v, CtxTkRule::ParamOp).expect("ParamOp");
    assert_eq!(span_str(op, src), "##");
  }

  #[test]
  fn param_expansion_percent_suffix_strip() {
    let src = "${foo%suffix}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    let op = find(v, CtxTkRule::ParamOp).expect("ParamOp");
    assert_eq!(span_str(op, src), "%");
  }

  #[test]
  fn param_expansion_double_percent_greedy_suffix() {
    let src = "${foo%%suffix}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    let op = find(v, CtxTkRule::ParamOp).expect("ParamOp");
    assert_eq!(span_str(op, src), "%%");
  }

  #[test]
  fn param_expansion_substring_offset_only() {
    let src = "${foo:1}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    assert_eq!(span_str(v, src), "${foo:1}");
  }

  #[test]
  fn param_expansion_substring_offset_and_length() {
    let src = "${foo:1:3}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    assert_eq!(span_str(v, src), "${foo:1:3}");
  }

  #[test]
  fn param_expansion_array_index() {
    let src = "${arr[0]}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    assert_eq!(span_str(v, src), "${arr[0]}");
    let idx = find(v, CtxTkRule::ParamIndex).expect("ParamIndex");
    assert_eq!(span_str(idx, src), "[0]");
  }

  #[test]
  fn param_expansion_empty_body() {
    let src = "${}";
    let tk = parse_first(src);
    let v = find(&tk, CtxTkRule::VarSub).expect("VarSub");
    // Empty body still produces a wrapper span covering both braces.
    assert_eq!(span_str(v, src), "${}");
  }

  // Escape handling outside strings

  #[test]
  fn escape_outside_string() {
    // Bare escape — should produce Escape sub-token.
    let src = r"a\b";
    let tk = parse_first(src);
    let _ = find(&tk, CtxTkRule::Escape).expect("Escape");
  }

  // Quotes inside double strings should NOT toggle quote state out

  #[test]
  fn single_quote_inside_double_is_literal() {
    let src = r#""it's""#;
    let tk = parse_first(src);
    let s = find(&tk, CtxTkRule::DoubleString).expect("DoubleString");
    assert_eq!(span_str(s, src), r#""it's""#);
  }

  // Mixed: command sub containing single quotes containing !
  // (the regression case from the comment in linebuf/mod.rs)

  #[test]
  fn cmd_sub_with_inner_single_quotes() {
    let src = r#""foo $(echo 'bar!') biz""#;
    let tk = parse_first(src);
    let cs = find(&tk, CtxTkRule::CmdSub).expect("CmdSub");
    assert_eq!(span_str(cs, src), "$(echo 'bar!')");
    // The HistExp inside should NOT trigger because it's inside single quotes.
    let inner_ss = find(cs, CtxTkRule::SingleString).expect("SingleString");
    assert!(
      find(inner_ss, CtxTkRule::HistExp).is_none(),
      "HistExp must not appear inside single-quoted body of cmd sub"
    );
  }

  // ===================== promote_exec_wrappers =====================

  use crate::tests::testutil::{TestGuard, has_cmd};

  /// Find the first token in `tokens` whose span text equals `text`.
  fn find_by_text<'a>(tokens: &'a [CtxTk], text: &str, src: &str) -> Option<&'a CtxTk> {
    tokens.iter().find(|t| span_str(t, src) == text)
  }

  #[test]
  fn sudo_is_promoted_to_keyword() {
    if !has_cmd("sudo") {
      return;
    }
    let _g = TestGuard::new();
    let src = "sudo something";
    let tokens = get_context_tokens(src);
    let sudo_tk = find_by_text(&tokens, "sudo", src).expect("sudo token");
    assert_eq!(*sudo_tk.class(), CtxTkRule::Keyword);
  }

  #[test]
  fn token_following_sudo_promoted_to_command() {
    if !has_cmd("sudo") || !has_cmd("cat") {
      return;
    }
    let _g = TestGuard::new();
    let src = "sudo cat";
    let tokens = get_context_tokens(src);
    let cat_tk = find_by_text(&tokens, "cat", src).expect("cat token");
    assert!(matches!(*cat_tk.class(), CtxTkRule::ValidCommand(_)));
  }

  #[test]
  fn unknown_command_after_sudo_is_invalid_command() {
    if !has_cmd("sudo") {
      return;
    }
    let _g = TestGuard::new();
    let src = "sudo definitely_not_a_real_cmd_xyzzy";
    let tokens = get_context_tokens(src);
    let cmd_tk = find_by_text(&tokens, "definitely_not_a_real_cmd_xyzzy", src).expect("token");
    assert_eq!(*cmd_tk.class(), CtxTkRule::InvalidCommand);
  }

  #[test]
  fn dash_option_after_sudo_skipped_and_next_promoted() {
    if !has_cmd("sudo") || !has_cmd("cat") {
      return;
    }
    let _g = TestGuard::new();
    let src = "sudo -E cat";
    let tokens = get_context_tokens(src);
    let cat_tk = find_by_text(&tokens, "cat", src).expect("cat token");
    assert!(matches!(*cat_tk.class(), CtxTkRule::ValidCommand(_)));
  }

  #[test]
  fn assignment_after_sudo_skipped_and_next_promoted() {
    if !has_cmd("sudo") || !has_cmd("cat") {
      return;
    }
    let _g = TestGuard::new();
    let src = "sudo FOO=bar cat";
    let tokens = get_context_tokens(src);
    let cat_tk = find_by_text(&tokens, "cat", src).expect("cat token");
    assert!(matches!(*cat_tk.class(), CtxTkRule::ValidCommand(_)));
  }

  #[test]
  fn chained_wrappers_promote_target_correctly() {
    // `sudo strace cmd`: both sudo and strace become Keywords; cmd gets
    // ValidCommand. The inner-loop `continue 'outer` ensures strace
    // gets reprocessed.
    if !has_cmd("sudo") || !has_cmd("strace") || !has_cmd("cat") {
      return;
    }
    let _g = TestGuard::new();
    let src = "sudo strace cat";
    let tokens = get_context_tokens(src);
    let sudo_tk = find_by_text(&tokens, "sudo", src).expect("sudo");
    let strace_tk = find_by_text(&tokens, "strace", src).expect("strace");
    let cat_tk = find_by_text(&tokens, "cat", src).expect("cat");
    assert_eq!(*sudo_tk.class(), CtxTkRule::Keyword);
    assert_eq!(*strace_tk.class(), CtxTkRule::Keyword);
    assert!(matches!(*cat_tk.class(), CtxTkRule::ValidCommand(_)));
  }

  #[test]
  fn run0_is_recognized_as_wrapper() {
    if !has_cmd("cat") {
      return;
    }
    let _g = TestGuard::new();
    let src = "run0 cat";
    let tokens = get_context_tokens(src);
    let run0_tk = find_by_text(&tokens, "run0", src);
    let cat_tk = find_by_text(&tokens, "cat", src).expect("cat");
    // run0 is in EXEC_WRAPPERS, but `is_exec_wrapper` also checks
    // is_valid_cmd — so this fires only if run0 is itself in PATH.
    // We don't assume it is; just assert "if recognized as wrapper,
    // cat is promoted; otherwise cat is whatever the leader would be".
    if let Some(tk) = run0_tk
      && *tk.class() == CtxTkRule::Keyword
    {
      assert!(matches!(*cat_tk.class(), CtxTkRule::ValidCommand(_)));
    }
  }

  #[test]
  fn non_wrapper_command_not_promoted() {
    // `echo foo` — echo isn't an exec wrapper.
    let _g = TestGuard::new();
    let src = "echo foo";
    let tokens = get_context_tokens(src);
    let echo_tk = find_by_text(&tokens, "echo", src).expect("echo");
    // echo should NOT be Keyword (it's not a wrapper).
    assert_ne!(*echo_tk.class(), CtxTkRule::Keyword);
  }

  // ===================== CtxTk::from_tk extra branches =====================

  /// An argument that resolves to an existing path is tagged as
  /// `CtxTkRule::ArgumentFile`. We use /tmp which always exists on
  /// Unix-like CI.
  #[test]
  fn existing_path_arg_classified_as_argument_file() {
    let _g = TestGuard::new();
    let src = "echo /tmp";
    let tokens = get_context_tokens(src);
    let path_tk = find_by_text(&tokens, "/tmp", src).expect("path token");
    assert_eq!(*path_tk.class(), CtxTkRule::ArgumentFile);
  }

  /// A non-existent path-shaped argument falls through to the default
  /// `Argument` classification.
  #[test]
  fn nonexistent_path_arg_classified_as_plain_argument() {
    let _g = TestGuard::new();
    let src = "echo /this/does/not/exist/zzzqqq";
    let tokens = get_context_tokens(src);
    let arg_tk = find_by_text(&tokens, "/this/does/not/exist/zzzqqq", src).expect("arg token");
    assert_eq!(*arg_tk.class(), CtxTkRule::Argument);
  }

  /// Keyword classification: `if` is a shell keyword and should not
  /// be subdivided into sub-tokens.
  #[test]
  fn keyword_token_has_no_subtokens() {
    let _g = TestGuard::new();
    let src = "if true; then :; fi";
    let tokens = get_context_tokens(src);
    let if_tk = find_by_text(&tokens, "if", src).expect("if token");
    assert_eq!(*if_tk.class(), CtxTkRule::Keyword);
    assert!(if_tk.sub_tokens.is_empty());
  }
}
