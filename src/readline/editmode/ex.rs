use std::iter::Peekable;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::str::CharIndices;
use std::vec::IntoIter;

use itertools::{Itertools, PeekingNext};

use super::{
  EditMode, LineBuf, ModeReport, ShResult, SimpleEditor,
  editcmd::{
    Anchor, CmdFlags, EditCmd, LineAddr, Motion, ReadSrc, StashArgs, StashListArg, Verb, WriteDest,
  },
  eval::lex::{self, LexFlags, LexStream, Span, Tk, TkVecUtils},
  history::History,
  key,
  keys::KeyEvent,
  match_loop,
  register::RegisterName,
  state::terminal::CursorStyle,
  status_msg,
};
use crate::{eval::lex::TkFlags, state::vars::VarStr, varstr, verb};
use bitflags::bitflags;

bitflags! {
  #[derive(Debug,Clone,Copy,PartialEq,Eq)]
  pub struct SubFlags: u16 {
    const GLOBAL           = 1 << 0; // g
    const CONFIRM          = 1 << 1; // c (probably not implemented)
    const IGNORE_CASE      = 1 << 2; // i
    const NO_IGNORE_CASE   = 1 << 3; // I
    const SHOW_COUNT       = 1 << 4; // n
    const PRINT_RESULT     = 1 << 5; // p
    const PRINT_NUMBERED   = 1 << 6; // #
    const PRINT_LEFT_ALIGN = 1 << 7; // l
  }
}

#[derive(Debug)]
struct ExEditor {
  editor: SimpleEditor,
}

impl Default for ExEditor {
  fn default() -> Self {
    Self {
      editor: SimpleEditor::new(Some("ex_history")),
    }
  }
}

impl ExEditor {
  pub fn new(has_select: bool) -> Self {
    let mut editor = SimpleEditor::new(Some("ex_history"));
    if has_select {
      editor.buf = editor.buf.with_initial("'<,'>", 6);
    }
    Self { editor }
  }
  pub fn clear(&mut self) {
    *self = Self::default();
  }
  pub fn is_empty(&self) -> bool {
    self.editor.buf.is_empty()
  }
}

#[derive(Default, Debug)]
pub struct ViEx {
  pending_cmd: ExEditor,
}

impl ViEx {
  pub fn new(has_select: bool) -> Self {
    Self {
      pending_cmd: ExEditor::new(has_select),
    }
  }
}

impl EditMode for ViEx {
  // Ex mode can return errors, so we use this fallible method instead of the normal one
  #[expect(clippy::unnested_or_patterns)]
  fn handle_key_fallible(&mut self, key: KeyEvent) -> ShResult<Option<EditCmd>> {
    match key {
      key!('\r') | key!(Enter) => {
        let input = self.pending_cmd.editor.buf.to_string();
        let res = match parse_ex_input(&input) {
          ExParseResult::Success(node) => {
            self.pending_cmd.clear();

            Ok(Some(EditCmd {
              register: RegisterName::default(),
              verb: Some(verb!(Verb::ExCmd(node))),
              motion: None,
              flags: CmdFlags::EXIT_CUR_MODE,
              raw_seq: input.clone(),
            }))
          }
          ExParseResult::Error(e) => {
            status_msg!("{e}");
            Ok(None)
          }
        };

        if let Some(hist) = self.history()
          && let Err(e) = hist.push(&input)
        {
          status_msg!("Failed to save ex command to history: {e}");
        }

        res
      }
      key!(Ctrl + 'c') => {
        self.pending_cmd.clear();
        Ok(None)
      }
      key!(Backspace) if self.pending_cmd.is_empty() => Ok(Some(EditCmd {
        register: RegisterName::default(),
        verb: None,
        motion: None,
        flags: CmdFlags::EXIT_CUR_MODE,
        raw_seq: String::new(),
      })),
      key!(Esc) => Ok(Some(EditCmd {
        register: RegisterName::default(),
        verb: None,
        motion: None,
        flags: CmdFlags::EXIT_CUR_MODE,
        raw_seq: String::new(),
      })),
      _ => self.pending_cmd.editor.handle_key(key).map(|()| None),
    }
  }
  fn handle_key(&mut self, key: KeyEvent) -> Option<EditCmd> {
    let result = self.handle_key_fallible(key);
    result.ok().flatten()
  }
  fn is_repeatable(&self) -> bool {
    false
  }

  fn as_replay(&self) -> Option<super::CmdReplay> {
    None
  }

  fn editor(&mut self) -> Option<&mut LineBuf> {
    Some(&mut self.pending_cmd.editor.buf)
  }

  fn history(&mut self) -> Option<&mut History> {
    self.pending_cmd.editor.history.as_mut()
  }

  fn cursor_style(&self) -> CursorStyle {
    CursorStyle::Underline(false)
  }

  fn is_input_mode(&self) -> bool {
    true
  }

  fn pending_seq(&self) -> Option<VarStr> {
    Some(varstr!("{}", self.pending_cmd.editor.buf))
  }

  fn pending_cursor(&self) -> Option<usize> {
    Some(self.pending_cmd.editor.buf.cursor_to_flat())
  }

  fn clamp_cursor(&self) -> bool {
    self.pending_cmd.editor.mode.clamp_cursor()
  }

  fn report_mode(&self) -> super::ModeReport {
    ModeReport::Ex
  }
}

fn parse_ex_input(input: &str) -> ExP<ExNode> {
  let lexer = ExLexer::new(input);
  let tokens = lexer.lex();
  let parser = ExParser::new(tokens);
  parser.parse()
}

bitflags! {
  struct ExLexFlags: u32 {
    const LEX_UNFINISHED = 1 << 0;
  }
}

/// The ex mode commands, mapped to their `ExCommand` enum members
///
/// Order matters because we allow prefix matching.
/// It iterates through these to find a prefix of a command.
/// This means that common ones like `edit`, `read`, and `write`
/// should be at the top
pub const COMMANDS: &[(&str, ExCommand)] = &[
  ("edit", ExCommand::Edit),
  ("read", ExCommand::Read),
  ("transfer", ExCommand::Transfer),
  ("trim", ExCommand::Trim),
  ("copy", ExCommand::Transfer),
  ("move", ExCommand::Move),
  ("join", ExCommand::Join),
  ("write", ExCommand::Write),
  ("quit", ExCommand::Quit),
  ("wq", ExCommand::WriteQuit),
  ("x", ExCommand::WriteQuit), // TODO: implement the "only write if modified" behavior for :x
  ("delete", ExCommand::Delete),
  ("yank", ExCommand::Yank),
  ("put", ExCommand::Put),
  ("substitute", ExCommand::Substitute),
  ("global", ExCommand::Global),
  ("normal", ExCommand::Normal),
  ("stash", ExCommand::Stash),
  ("expand", ExCommand::Expand),
  ("help", ExCommand::Help),
];

#[derive(Debug, Clone)]
pub enum ExTkRule {
  Bang,
  Append,
  NormalSeq,
  Argument,

  Address(ExLineAddr),
  Command(ExCommand),

  ShellTk(Tk), // delegate to LexStream after '!' appears
}

impl ExTkRule {
  pub fn unwrap_cmd(&self) -> ExCommand {
    if let ExTkRule::Command(cmd) = self {
      *cmd
    } else {
      panic!("called unwrap_cmd on non-command token")
    }
  }
  pub fn is_addr(&self) -> bool {
    matches!(self, ExTkRule::Address(_))
  }
  pub fn unwrap_addr(&self) -> ExLineAddr {
    if let ExTkRule::Address(addr) = self {
      *addr
    } else {
      panic!("called unwrap_addr on non-address token")
    }
  }
}

#[derive(Debug, Clone, Copy)]
pub enum ExLineAddr {
  Number,
  Dot,
  Dollar,
  Percent,
  Comma,
  Offset,
  Mark,
  Pattern,
  PatternRev,
}

#[derive(Debug, Clone, Copy)]
pub enum ExCommand {
  Expand,
  Substitute,
  Global,
  Normal,
  Delete,
  Yank,
  Put,
  Transfer,
  Trim,
  Join,
  Move,
  Edit,
  Read,
  Write,
  Stash,
  Quit,
  WriteQuit,
  Help,
  Shell,

  Unknown,
}

#[derive(Debug, Clone)]
pub struct ExTk {
  class: ExTkRule,
  span: Span,
}

impl ExTk {
  pub fn unpack(self) -> (ExTkRule, Span) {
    (self.class, self.span)
  }
}

impl From<Tk> for ExTk {
  fn from(value: Tk) -> Self {
    let span = value.span.clone();
    ExTk {
      class: ExTkRule::ShellTk(value),
      span,
    }
  }
}

pub struct ExLexer<'a> {
  input: Rc<str>,
  chars: Peekable<CharIndices<'a>>,
  tokens: Vec<ExTk>,

  flags: ExLexFlags,
}

impl<'a> ExLexer<'a> {
  pub fn new(input: &'a str) -> Self {
    Self {
      input: input.into(),
      chars: input.char_indices().peekable(),
      tokens: vec![],
      flags: ExLexFlags::empty(),
    }
  }
  pub fn lex(mut self) -> Vec<ExTk> {
    if self.chars.peek().is_none() {
      return self.tokens;
    }

    self.ignoring_ws(&[Self::parse_address, Self::parse_cmd_name, Self::parse_args]);

    self.tokens
  }
  fn skip_whitespace(&mut self) {
    self
      .chars
      .peeking_take_while(|(_, ch)| ch.is_whitespace())
      .for_each(drop);
  }
  fn take_alphabetic(&mut self, start: usize) -> usize {
    let mut end = start;
    while let Some(&(i, ch)) = self.chars.peek()
      && ch.is_ascii_alphabetic()
    {
      self.chars.next();
      end = i + ch.len_utf8();
    }
    end
  }
  fn consume_digits(&mut self, mut end: usize) -> usize {
    while let Some(&(i, ch)) = self.chars.peek()
      && ch.is_ascii_digit()
    {
      self.chars.next();
      end = i + ch.len_utf8();
    }
    end
  }
  fn consume_pattern(&mut self, mut end: usize, delim: char) -> usize {
    match_loop!(self.chars.next() => (i,ch) => ch, {
      '\\' => {
        let Some(&(j,ch)) = self.chars.peek() else {
          break
        };
        self.chars.next();
        end = j + ch.len_utf8();
      }
      _ if ch == delim => {
        end = i + ch.len_utf8();
        return end
      }
      _ => end = i + ch.len_utf8(),
    });

    self.flags |= ExLexFlags::LEX_UNFINISHED;
    end
  }
  fn peek_pos(&mut self) -> Option<usize> {
    self.chars.peek().map(|(i, _)| *i)
  }
  fn peeking_next<F>(&mut self, accept: F) -> Option<(usize, char)>
  where
    F: FnOnce(&char) -> bool,
  {
    let &(_, ch) = self.chars.peek()?;
    accept(&ch).then(|| self.chars.next()).flatten()
  }
  fn get_span(&self, range: Range<usize>) -> Option<Span> {
    self.input.get(range.clone())?;

    Some(Span::new(range, self.input.clone()))
  }
  fn is_addr_opener(ch: &(usize, char)) -> bool {
    let ch = ch.1;
    ch.is_ascii_digit() || ".$+-'/?".contains(ch)
  }
  fn get_token(&self, range: Range<usize>, rule: ExTkRule) -> Option<ExTk> {
    let span = self.get_span(range)?;
    Some(ExTk { class: rule, span })
  }
  fn get_pattern_token(&self, range: Range<usize>) -> Option<ExTk> {
    // strip delimiter chars
    let span = self.get_span(range.start + 1..range.end.saturating_sub(1))?;
    Some(ExTk {
      class: ExTkRule::Address(ExLineAddr::Pattern),
      span,
    })
  }
  /// Lets us skip whitespace between every individual parsing step.
  ///
  /// Takes a function pointer array and calls `self.skip_whitespace()` between
  /// function call.
  ///
  /// Panics if `funcs` is empty.
  fn ignoring_ws<T>(&mut self, funcs: &[fn(&mut Self) -> T]) -> T {
    debug_assert!(!funcs.is_empty());
    self.skip_whitespace();
    let mut res = None;
    for f in funcs {
      res = Some(f(self));
      self.skip_whitespace();
    }

    // if funcs is not empty, this is safe
    // and funcs is not empty as per the assertion above
    res.unwrap()
  }
  fn command(&self) -> Option<ExCommand> {
    // two passes: first, ignore bang and look for a command name
    // if not found, accept bang since ':!echo foo' is valid
    self
      .tokens
      .iter()
      .rev()
      .find(|tk| matches!(tk.class, ExTkRule::Command(_)))
      .map(|tk| tk.class.unwrap_cmd())
      .or_else(|| {
        self
          .tokens
          .iter()
          .rev()
          .find(|tk| matches!(tk.class, ExTkRule::Bang))
          .map(|_| ExCommand::Shell)
      })
  }
  fn parse_substitute_arg(&mut self) {
    let Some((start, first)) = self.chars.peeking_next(|(_, ch)| !ch.is_alphanumeric()) else {
      return;
    };
    let delim = first;

    let end_before = self.consume_pattern(start, delim);
    let Some(before) = self.get_pattern_token(start..end_before) else {
      return;
    };
    self.tokens.push(before);

    if self.chars.peek().is_none() {
      return;
    }

    let end_after = self.consume_pattern(end_before, delim);
    let Some(after) = self.get_pattern_token(end_before.saturating_sub(1)..end_after) else {
      return;
    };
    self.tokens.push(after);

    if self.chars.peek().is_none() {
      return;
    }

    let end_flags = self.take_alphabetic(end_after);
    let Some(flags) = self.get_token(end_after..end_flags, ExTkRule::Argument) else {
      return;
    };
    self.tokens.push(flags);
  }
  fn parse_global_arg(&mut self) {
    let Some((start, first)) = self.chars.peeking_next(|(_, ch)| !ch.is_alphanumeric()) else {
      return;
    };
    let delim = first;

    let end_before = self.consume_pattern(start, delim);
    let Some(before) = self.get_pattern_token(start..end_before) else {
      return;
    };
    self.tokens.push(before);

    if self.chars.peek().is_none() {
      return;
    }

    // recursively parse the command
    self.ignoring_ws(&[Self::parse_address, Self::parse_cmd_name, Self::parse_args]);
  }
  fn parse_normal_seq(&mut self) {
    // everything after this is parsed as a single literal arg
    let Some((start, ch)) = self.chars.next() else {
      return;
    };
    let mut end = start + ch.len_utf8();
    while let Some((i, ch)) = self.chars.next() {
      end = i + ch.len_utf8();
    }
    if let Some(tk) = self.get_token(start..end, ExTkRule::NormalSeq) {
      self.tokens.push(tk);
    }
  }
  fn parse_args(&mut self) {
    const FILE_ARG_STRIP: lex::TkFlags = lex::TkFlags::IS_CMD
      .union(lex::TkFlags::BUILTIN)
      .union(lex::TkFlags::KEYWORD)
      .union(lex::TkFlags::FUNCNAME)
      .union(lex::TkFlags::ASSIGN);
    let Some(cmd) = self.command() else { return };

    match cmd {
      ExCommand::Substitute => self.parse_substitute_arg(),
      ExCommand::Global => self.parse_global_arg(),
      ExCommand::Normal => self.parse_normal_seq(),
      ExCommand::Shell => self.parse_shell_cmd(),
      ExCommand::Move | ExCommand::Transfer => {
        self.skip_whitespace();
        self.parse_one_addr();
      }
      cmd => {
        // Some command like 'edit' or 'write' or something that just expects words
        // These are subject to shell expansion, so we use LexStream for these
        let Some(start_pos) = self.peek_pos() else {
          return;
        };
        // For `:r!` and `:w!`, the bang shifts the rest into shell-command
        // mode (pipe-to-shell / read-from-shell). For other commands a
        // trailing bang is the vim "force" modifier and args are still paths.
        let has_bang = self
          .tokens
          .iter()
          .any(|tk| matches!(tk.class, ExTkRule::Bang));
        let shell_mode = has_bang && matches!(cmd, ExCommand::Read | ExCommand::Write);

        let is_write = matches!(cmd, ExCommand::Write);
        let rest = self.chars.by_ref().map(|(_, ch)| ch).collect::<String>();
        let outer = Span::new(0..self.input.len(), self.input.clone());
        let stream = LexStream::new(rest.into(), LexFlags::LEX_UNFINISHED)
          .filter_map(Result::ok)
          .filter_map(|tk| tk.filter_meta().then_some(tk))
          .map(|tk| tk.rebase_into(&outer, start_pos));

        let mut pushed = false;

        for mut shell_tk in stream {
          if shell_mode {
            // Keep IS_CMD on the first token (the shell command name),
            // strip it from subsequent args so they're not all re-treated
            // as commands by the shell highlighter.
            if pushed {
              shell_tk.flags.remove(lex::TkFlags::IS_CMD);
            }
          } else {
            shell_tk.flags.remove(FILE_ARG_STRIP);
          }
          let outer_span = shell_tk.span.clone();
          let mut tk = ExTk {
            class: ExTkRule::ShellTk(shell_tk),
            span: outer_span,
          };
          if !pushed && is_write && tk.span.as_str() == ">>" {
            tk.class = ExTkRule::Append;
          }
          self.tokens.push(tk);
          pushed = true;
        }
      }
    }
  }
  fn classify_cmd(name: &str) -> Option<ExCommand> {
    if name.is_empty() {
      return None;
    }
    for (full, rule) in COMMANDS {
      if full.starts_with(name) {
        return Some(*rule);
      }
    }
    None
  }
  fn parse_cmd_name(&mut self) {
    let Some(&(start, first)) = self.chars.peek() else {
      return;
    };
    if !first.is_ascii_alphabetic() && first != '!' {
      return;
    }

    let mut end = start;
    match_loop!(self.chars.peek() => &(i,ch) => ch, {
      '!' => {
        let cmd_name = &self.input[start..i];
        let Some(cmd_rule) = Self::classify_cmd(cmd_name) else {
          let Some(tk) = self.get_token(i..i+1, ExTkRule::Bang) else { return };
          self.tokens.push(tk);
          self.chars.next();
          return;
        };

        if end > start {
          let Some(tk) = self.get_token(start..end, ExTkRule::Command(cmd_rule)) else { return };
          self.tokens.push(tk);
        }

        let Some(tk) = self.get_token(i..i+1, ExTkRule::Bang) else { return };
        self.tokens.push(tk);
        self.chars.next();

        return
      }
      _ if ch.is_ascii_alphabetic() || ch == '_' => {
        self.chars.next();
        end = i + ch.len_utf8();
      }
      _ => break
    });

    let cmd_name = &self.input[start..end];
    let cmd_rule = Self::classify_cmd(cmd_name).unwrap_or(ExCommand::Unknown);

    let Some(tk) = self.get_token(start..end, ExTkRule::Command(cmd_rule)) else {
      return;
    };
    self.tokens.push(tk);
  }
  fn parse_shell_cmd(&mut self) {
    let Some(start_pos) = self.peek_pos() else {
      return;
    };
    let mut rest = String::new();
    while let Some((_, ch)) = self.chars.next() {
      rest.push(ch);
    }

    let outer = Span::new(0..self.input.len(), self.input.clone());
    let stream = LexStream::new(rest.into(), LexFlags::LEX_UNFINISHED)
      .filter_map(Result::ok)
      .filter_map(|tk| tk.filter_meta().then_some(tk))
      .map(|tk| tk.rebase_into(&outer, start_pos));

    for shell_tk in stream {
      let outer_span = shell_tk.span.clone();
      self.tokens.push(ExTk {
        class: ExTkRule::ShellTk(shell_tk),
        span: outer_span,
      });
    }
  }
  fn parse_address(&mut self) {
    if let Some((i, _)) = self.chars.peeking_next(|&(_, ch)| ch == '%')
      && let Some(tk) = self.get_token(i..i + 1, ExTkRule::Address(ExLineAddr::Percent))
    {
      self.tokens.push(tk);
      return;
    }
    if !self.parse_one_addr() {
      return;
    }

    self.ignoring_ws(&[|this| {
      let Some((i, _)) = this.peeking_next(|c| *c == ',') else {
        return;
      };
      let Some(tk) = this.get_token(i..i + 1, ExTkRule::Address(ExLineAddr::Comma)) else {
        return;
      };
      this.tokens.push(tk);
    }]);

    self.parse_one_addr();
  }
  fn parse_one_addr(&mut self) -> bool {
    let Some((start, first)) = self.chars.peeking_next(Self::is_addr_opener) else {
      return false;
    };

    let (end, rule) = match first {
      '.' => (start + 1, ExTkRule::Address(ExLineAddr::Dot)),
      '$' => (start + 1, ExTkRule::Address(ExLineAddr::Dollar)),
      '0'..='9' => {
        let end = self.consume_digits(start + 1);
        (end, ExTkRule::Address(ExLineAddr::Number))
      }
      '+' | '-' => {
        let end = self.consume_digits(start + 1);
        (end, ExTkRule::Address(ExLineAddr::Offset))
      }
      '\'' => {
        let Some((i, c)) = self.chars.next() else {
          self.flags |= ExLexFlags::LEX_UNFINISHED;
          return false;
        };
        (i + c.len_utf8(), ExTkRule::Address(ExLineAddr::Mark))
      }
      ch @ ('/' | '?') => {
        let rule = match ch {
          '?' => ExTkRule::Address(ExLineAddr::PatternRev),
          _ => ExTkRule::Address(ExLineAddr::Pattern),
        };
        let end = self.consume_pattern(start + 1, first);
        (end, rule)
      }
      _ => return false, // unreachable probably
    };

    if matches!(rule, ExTkRule::Address(ExLineAddr::Pattern)) {
      if let Some(tk) = self.get_pattern_token(start..end) {
        self.tokens.push(tk);
        true
      } else {
        false
      }
    } else {
      if let Some(tk) = self.get_token(start..end, rule) {
        self.tokens.push(tk);
        true
      } else {
        false
      }
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExNdRule {
  Delete,
  Yank,
  Put(Anchor),
  Expand,
  Move(AddressRange),
  Transfer(AddressRange),
  Trim,
  Join,

  Edit(Box<[PathBuf]>),
  Read(ReadSrc),
  Write(WriteDest),
  Quit,
  WriteQuit,

  Substitute {
    pat: String,
    repl: String,
    flags: SubFlags,
  },
  Global {
    pat: String,
    nested: Box<ExNode>,
  },
  RepeatSubstitute,
  RepeatGlobal,

  Normal {
    seq: String,
  },
  Shell(String),

  Stash(StashArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressRange {
  Single(LineAddr),
  Range(LineAddr, LineAddr),
}

impl AddressRange {
  pub const fn all_lines() -> Self {
    Self::Range(LineAddr::Number(1), LineAddr::Last)
  }
  pub fn as_motion(&self) -> Motion {
    match self {
      AddressRange::Single(line) => Motion::Line(line.clone()),
      AddressRange::Range(s, e) => Motion::LineRange(s.clone(), e.clone()),
    }
  }
}

impl Default for AddressRange {
  fn default() -> Self {
    Self::Single(LineAddr::Current)
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExNode {
  pub address: Option<AddressRange>,
  pub bang: bool,
  pub kind: ExNdRule,
}

pub enum ExParseResult<T> {
  Success(T),
  Error(String),
}
use ExParseResult as ExP;

enum ExInnerParseResult<T> {
  Done(ExParseResult<T>),
  NoMatch,
}
use ExInnerParseResult as ExR;

impl<T> ExInnerParseResult<T> {
  pub fn success(value: T) -> Self {
    ExInnerParseResult::Done(ExP::Success(value))
  }
  pub fn error(msg: String) -> Self {
    ExInnerParseResult::Done(ExP::Error(msg))
  }
}

enum ExInnerPartialParseResult<T, P> {
  Full(T),
  Partial(P),
}
use ExInnerPartialParseResult as ExPR;

#[derive(Debug, Clone)]
pub struct ExParser {
  tokens: Peekable<IntoIter<ExTk>>,
  bang: bool,
}

impl ExParser {
  pub fn new(tokens: Vec<ExTk>) -> Self {
    let tokens = tokens.into_iter().peekable();
    Self {
      tokens,
      bang: false,
    }
  }
  pub fn parse(mut self) -> ExP<ExNode> {
    let address = match self.parse_address() {
      ExR::Done(ExP::Success(addr)) => Some(addr),
      ExR::Done(ExP::Error(msg)) => return ExP::Error(msg),
      ExR::NoMatch => None,
    };

    let kind = match self.parse_command() {
      ExR::Done(ExP::Success(cmd)) => cmd,
      ExR::Done(ExP::Error(msg)) => return ExP::Error(msg),
      ExR::NoMatch => return ExP::Error("expected command".into()),
    };

    let bang = self.bang;

    ExP::Success(ExNode {
      address,
      bang,
      kind,
    })
  }

  fn parse_address(&mut self) -> ExR<AddressRange> {
    let Some(tk) = self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::Address(_)))
    else {
      return ExR::NoMatch;
    };

    let resolved = match Self::parse_one_address(&tk) {
      ExPR::Partial(addr) => addr,
      ExPR::Full(motion) => return ExR::success(motion),
    };

    if self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::Address(ExLineAddr::Comma)))
      .is_none()
    {
      return ExR::success(AddressRange::Single(resolved));
    }

    let Some(end_tk) = self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::Address(_)))
    else {
      return ExR::error("expected second address after ','".into());
    };

    let resolved_end = match Self::parse_one_address(&end_tk) {
      ExPR::Partial(addr) => addr,
      ExPR::Full(motion) => return ExR::success(motion),
    };

    ExR::success(AddressRange::Range(resolved, resolved_end))
  }
  fn parse_one_address(tk: &ExTk) -> ExPR<AddressRange, LineAddr> {
    let addr = tk.class.unwrap_addr();

    match addr {
      ExLineAddr::Number => {
        let addr = tk.span.as_str().parse::<usize>().unwrap_or(1);
        ExPR::Partial(LineAddr::Number(addr))
      }
      ExLineAddr::Dot => ExPR::Partial(LineAddr::Current),
      ExLineAddr::Dollar => ExPR::Partial(LineAddr::Last),
      ExLineAddr::Percent => ExPR::Full(AddressRange::all_lines()),
      ExLineAddr::Offset => {
        let s = tk
          .span
          .as_str()
          .strip_prefix("+")
          .unwrap_or(tk.span.as_str());

        let offset = s.parse::<isize>().unwrap_or(1);
        ExPR::Partial(LineAddr::Offset(offset))
      }
      ExLineAddr::Mark => {
        let mark_name = tk.span.as_str().chars().nth(1).unwrap();

        ExPR::Partial(LineAddr::Mark(mark_name))
      }
      ExLineAddr::Pattern => {
        let pat = VarStr::from(tk.span.as_str());
        ExPR::Partial(LineAddr::Pattern(pat))
      }
      ExLineAddr::PatternRev => {
        let pat = VarStr::from(tk.span.as_str());
        ExPR::Partial(LineAddr::PatternRev(pat))
      }
      ExLineAddr::Comma => unreachable!(),
    }
  }
  fn parse_command(&mut self) -> ExR<ExNdRule> {
    let Some(tk) = self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::Command(_) | ExTkRule::Bang))
    else {
      return ExR::NoMatch;
    };

    // `!cmd` form: `!` itself is the command-starter; dispatch to
    // parse_shell directly and skip the trailing-bang consumption.
    if matches!(tk.class, ExTkRule::Bang) {
      return self.parse_shell();
    }

    let cmd = tk.class.unwrap_cmd();

    self.bang = self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::Bang))
      .is_some();

    match cmd {
      ExCommand::Read | ExCommand::Write => self.parse_read_write(&tk.class),
      ExCommand::Substitute => self.parse_substitute(),
      ExCommand::Global => self.parse_global(),
      ExCommand::Normal => self.parse_normal(),
      ExCommand::Edit => self.parse_edit(),
      ExCommand::Stash => self.parse_stash(),
      ExCommand::Help => self.parse_help(),
      ExCommand::Shell => self.parse_shell(),
      ExCommand::Move => self.parse_move(),
      ExCommand::Transfer => self.parse_transfer(),
      ExCommand::Trim => ExR::success(ExNdRule::Trim),
      ExCommand::Delete => ExR::success(ExNdRule::Delete),
      ExCommand::Yank => ExR::success(ExNdRule::Yank),
      ExCommand::Put => ExR::success(ExNdRule::Put(Anchor::After)),
      ExCommand::Join => ExR::success(ExNdRule::Join),
      ExCommand::Quit => ExR::success(ExNdRule::Quit),
      ExCommand::WriteQuit => ExR::success(ExNdRule::WriteQuit),
      ExCommand::Expand => ExR::success(ExNdRule::Expand),
      ExCommand::Unknown => ExR::error(format!("not an editor command: {}", tk.span.as_str())),
    }
  }
  fn parse_addr_rule(
    &mut self,
    cmd: &'static str,
    fact: fn(AddressRange) -> ExNdRule,
  ) -> ExR<ExNdRule> {
    let Some(addr) = self.tokens.next() else {
      return ExR::error(format!("expected address after {cmd} command"));
    };
    if !addr.class.is_addr() {
      return ExR::error(format!("expected address after {cmd} command"));
    };

    let dest = match Self::parse_one_address(&addr) {
      ExInnerPartialParseResult::Partial(addr) => addr,
      ExInnerPartialParseResult::Full(_) => {
        return ExR::error("'%' is not a valid destination".into());
      }
    };

    ExR::success(fact(AddressRange::Single(dest)))
  }
  fn parse_transfer(&mut self) -> ExR<ExNdRule> {
    self.parse_addr_rule("transfer", ExNdRule::Transfer)
  }
  fn parse_move(&mut self) -> ExR<ExNdRule> {
    self.parse_addr_rule("move", ExNdRule::Move)
  }
  fn parse_shell(&mut self) -> ExR<ExNdRule> {
    let mut args = vec![];
    while let Some(arg) = self.tokens.next() {
      // wrap in Tk so we can use get_span()
      args.push(Tk::new(lex::TkRule::Str, arg.span));
    }
    let args_raw = args
      .get_span() // extract total span of arg tokens
      .map(|s| s.as_str().to_string())
      .unwrap_or_default();

    ExR::success(ExNdRule::Shell(args_raw))
  }
  fn parse_help(&mut self) -> ExR<ExNdRule> {
    let mut args = vec![];
    while let Some(arg) = self.tokens.next() {
      args.push(Tk::new(lex::TkRule::Str, arg.span));
    }
    let args_raw = args
      .get_span() // extract total span of arg tokens
      .map(|s| s.as_str().to_string());

    let cmd = if let Some(args) = args_raw {
      ["help", &args].join(" ")
    } else {
      "help".to_string()
    };

    ExR::success(ExNdRule::Shell(cmd))
  }
  fn parse_normal(&mut self) -> ExR<ExNdRule> {
    let Some(tk) = self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::NormalSeq))
    else {
      return ExR::error("expected normal command sequence after 'normal'".into());
    };

    let seq = tk.span.as_str().to_string();
    ExR::success(ExNdRule::Normal { seq })
  }
  fn parse_stash(&mut self) -> ExR<ExNdRule> {
    let arg_names = ["pop", "drop", "apply", "insert", "swap", "list"];
    let arg = self.tokens.next().map(|tk| VarStr::from(tk.span.as_str()));
    if arg.is_none() {
      return ExR::success(ExNdRule::Stash(StashArgs::Push(None)));
    } else if !arg_names
      .iter()
      .any(|name| name.starts_with(arg.as_ref().unwrap().as_str()))
    {
      return ExR::success(ExNdRule::Stash(StashArgs::Push(arg)));
    }

    let name = self.tokens.next().map(|tk| VarStr::from(tk.span.as_str()));
    let arg = arg.unwrap();
    // Inner matches use the same prefix direction as the outer gate:
    // `<name>.starts_with(arg)` — so abbreviations like `:stash p` or
    // `:stash ap` resolve to `pop` / `apply` etc. The subcommand names
    // have no shared prefixes, so this is unambiguous.
    match arg.as_str() {
      _ if "pop".starts_with(arg.as_str()) => ExR::success(ExNdRule::Stash(StashArgs::Pop(name))),
      _ if "drop".starts_with(arg.as_str()) => ExR::success(ExNdRule::Stash(StashArgs::Drop(name))),
      _ if "apply".starts_with(arg.as_str()) => {
        ExR::success(ExNdRule::Stash(StashArgs::Apply(name)))
      }
      _ if "insert".starts_with(arg.as_str()) => {
        ExR::success(ExNdRule::Stash(StashArgs::Insert(name)))
      }
      _ if "swap".starts_with(arg.as_str()) => ExR::success(ExNdRule::Stash(StashArgs::Swap(name))),
      _ if "list".starts_with(arg.as_str()) => {
        let target = name
          .map(|n| match n.as_str() {
            _ if "stack".starts_with(n.trim()) => Ok(Some(StashListArg::Stack)),
            _ if "named".starts_with(n.trim()) => Ok(Some(StashListArg::Named)),
            _ => Err(ExR::error(format!("invalid stash list argument: {n}"))),
          })
          .transpose();
        let target = match target {
          Ok(t) => t,
          Err(e) => return e,
        };

        ExR::success(ExNdRule::Stash(StashArgs::List(target.flatten())))
      }
      _ => ExR::error(format!("invalid stash argument: {arg}")),
    }
  }
  fn parse_substitute(&mut self) -> ExR<ExNdRule> {
    let Some(pat) = self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::Address(ExLineAddr::Pattern)))
    else {
      // Bare `:s` with no arguments — repeat the last substitute
      return ExR::success(ExNdRule::RepeatSubstitute);
    };
    let Some(repl) = self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::Address(ExLineAddr::Pattern)))
    else {
      return ExR::error("expected replacement argument for substitute command".into());
    };
    let flags_tk = self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::Argument));
    let pat = pat.span.as_str().to_string();
    let repl = repl.span.as_str().to_string();

    let mut flags = SubFlags::empty();
    if let Some(flags_tk) = flags_tk {
      let flags_str = flags_tk.span.as_str();
      for ch in flags_str.chars() {
        match ch {
          'g' => flags |= SubFlags::GLOBAL,
          'c' => flags |= SubFlags::CONFIRM,
          'i' => flags |= SubFlags::IGNORE_CASE,
          'I' => flags |= SubFlags::NO_IGNORE_CASE,
          'n' => flags |= SubFlags::SHOW_COUNT,
          'p' => flags |= SubFlags::PRINT_RESULT,
          '#' => flags |= SubFlags::PRINT_NUMBERED,
          'l' => flags |= SubFlags::PRINT_LEFT_ALIGN,
          _ => return ExR::error(format!("invalid substitute flag: {ch}")),
        }
      }
    }

    ExR::success(ExNdRule::Substitute { pat, repl, flags })
  }
  fn parse_global(&mut self) -> ExR<ExNdRule> {
    let Some(pat) = self
      .tokens
      .peeking_next(|tk| matches!(tk.class, ExTkRule::Address(ExLineAddr::Pattern)))
    else {
      // Bare `:g` with no arguments — repeat the last global
      return ExR::success(ExNdRule::RepeatGlobal);
    };

    // drain tokens into this
    let mut rest = vec![];
    while let Some(tk) = self.tokens.next() {
      rest.push(tk);
    }

    let nested_parser = ExParser::new(rest);
    let sub_node = match nested_parser.parse() {
      ExP::Success(cmd) => cmd,
      ExP::Error(msg) => return ExR::error(msg),
    };

    ExR::success(ExNdRule::Global {
      pat: pat.span.as_str().to_string(),
      nested: Box::new(sub_node),
    })
  }
  fn parse_edit(&mut self) -> ExR<ExNdRule> {
    let mut args = vec![];
    while let Some(arg) = self.tokens.next() {
      args.push(expand_path_arg(arg.span.as_str()));
    }
    let args = args.into_boxed_slice();

    ExR::success(ExNdRule::Edit(args))
  }
  fn parse_read_write(&mut self, rule: &ExTkRule) -> ExR<ExNdRule> {
    let is_read = matches!(rule, ExTkRule::Command(ExCommand::Read));
    let is_shell_arg = self.bang;

    if is_shell_arg {
      let mut args = vec![];
      for tk in &mut self.tokens {
        args.push(Tk::new(lex::TkRule::Str, tk.span.clone()));
      }
      let args_raw = args
        .get_span() // extract total span of arg tokens
        .map(|s| s.as_str().into())
        .unwrap_or_default();

      if is_read {
        ExR::success(ExNdRule::Read(ReadSrc::Cmd(args_raw)))
      } else {
        ExR::success(ExNdRule::Write(WriteDest::Cmd(args_raw)))
      }
    } else {
      let mut arg = self.tokens.next();
      let is_append = arg
        .as_ref()
        .is_some_and(|a| matches!(a.class, ExTkRule::Append));
      if is_append {
        let Some(next) = self.tokens.next() else {
          return ExR::error(format!(
            "expected {} argument for append write",
            if is_shell_arg { "shell" } else { "file" }
          ));
        };
        arg = Some(next);
      }

      let path = arg.map(|a| expand_path_arg(a.span.as_str()));

      if is_read {
        let Some(path) = path else {
          return ExR::error(format!(
            "expected {} argument for read command",
            if is_shell_arg { "shell" } else { "file" }
          ));
        };
        ExR::success(ExNdRule::Read(ReadSrc::File(path)))
      } else if is_append {
        ExR::success(ExNdRule::Write(WriteDest::FileAppend(path)))
      } else {
        ExR::success(ExNdRule::Write(WriteDest::File(path)))
      }
    }
  }
}

/// Run an ex-command path argument through the shell's word-expansion
fn expand_path_arg(arg: &str) -> PathBuf {
  let expanded = crate::expand::Expander::from_raw(arg, TkFlags::empty())
    .no_glob()
    .no_split()
    .expand_no_split()
    .unwrap_or_else(|_| arg.into());
  PathBuf::from(expanded)
}

#[cfg(test)]
mod parse_stash_tests {
  use super::*;

  /// Parse a stash command and return the resulting `StashArgs`, or panic
  /// if it didn't parse as a stash.
  fn parse_stash(input: &str) -> StashArgs {
    let node = match parse_ex_input(input) {
      ExP::Success(node) => node,
      ExP::Error(msg) => panic!("parse error for {input:?}: {msg}"),
    };
    match node.kind {
      ExNdRule::Stash(args) => args,
      other => panic!("expected Stash for {input:?}, got {other:?}"),
    }
  }

  fn parse_stash_err(input: &str) -> String {
    match parse_ex_input(input) {
      ExP::Success(node) => panic!("expected error for {input:?}, got success: {node:?}"),
      ExP::Error(msg) => msg,
    }
  }

  // ─── push variants ────────────────────────────────────────────────

  #[test]
  fn bare_stash_is_push_none() {
    assert_eq!(parse_stash("stash"), StashArgs::Push(None));
  }

  #[test]
  fn stash_with_non_subcmd_arg_is_push_some() {
    // Arg that isn't a prefix of any known subcommand name → treated
    // as a stash message.
    assert_eq!(
      parse_stash("stash hello_message"),
      StashArgs::Push(Some("hello_message".into()))
    );
  }

  // ─── subcommands (full name) ─────────────────────────────────────

  #[test]
  fn stash_pop_with_no_name() {
    assert_eq!(parse_stash("stash pop"), StashArgs::Pop(None));
  }

  #[test]
  fn stash_pop_with_name() {
    assert_eq!(
      parse_stash("stash pop wip"),
      StashArgs::Pop(Some("wip".into()))
    );
  }

  #[test]
  fn stash_drop_with_name() {
    assert_eq!(
      parse_stash("stash drop wip"),
      StashArgs::Drop(Some("wip".into()))
    );
  }

  #[test]
  fn stash_apply_with_name() {
    assert_eq!(
      parse_stash("stash apply wip"),
      StashArgs::Apply(Some("wip".into()))
    );
  }

  #[test]
  fn stash_insert_with_name() {
    assert_eq!(
      parse_stash("stash insert wip"),
      StashArgs::Insert(Some("wip".into()))
    );
  }

  #[test]
  fn stash_swap_with_name() {
    assert_eq!(
      parse_stash("stash swap wip"),
      StashArgs::Swap(Some("wip".into()))
    );
  }

  // ─── list ────────────────────────────────────────────────────────

  #[test]
  fn stash_list_no_target_is_none() {
    assert_eq!(parse_stash("stash list"), StashArgs::List(None));
  }

  #[test]
  fn stash_list_stack_target() {
    assert_eq!(
      parse_stash("stash list stack"),
      StashArgs::List(Some(StashListArg::Stack))
    );
  }

  #[test]
  fn stash_list_named_target() {
    assert_eq!(
      parse_stash("stash list named"),
      StashArgs::List(Some(StashListArg::Named))
    );
  }

  #[test]
  fn stash_list_invalid_target_errors() {
    let msg = parse_stash_err("stash list bogus");
    assert!(msg.contains("invalid stash list argument"), "got: {msg:?}");
  }

  // ─── abbreviation handling ──────────────────────────────────────
  // Each subcommand name has no shared prefixes with any other, so
  // each starting char (and any prefix shorter than the full name)
  // unambiguously identifies one subcommand.

  #[test]
  fn stash_p_abbrev_is_pop() {
    assert_eq!(parse_stash("stash p"), StashArgs::Pop(None));
    assert_eq!(parse_stash("stash po"), StashArgs::Pop(None));
  }

  #[test]
  fn stash_d_abbrev_is_drop() {
    assert_eq!(parse_stash("stash d"), StashArgs::Drop(None));
    assert_eq!(parse_stash("stash dr"), StashArgs::Drop(None));
  }

  #[test]
  fn stash_a_abbrev_is_apply() {
    assert_eq!(parse_stash("stash a"), StashArgs::Apply(None));
    assert_eq!(parse_stash("stash ap"), StashArgs::Apply(None));
  }

  #[test]
  fn stash_i_abbrev_is_insert() {
    assert_eq!(parse_stash("stash i"), StashArgs::Insert(None));
    assert_eq!(parse_stash("stash in"), StashArgs::Insert(None));
  }

  #[test]
  fn stash_s_abbrev_is_swap() {
    assert_eq!(parse_stash("stash s"), StashArgs::Swap(None));
    assert_eq!(parse_stash("stash sw"), StashArgs::Swap(None));
  }

  #[test]
  fn stash_l_abbrev_is_list() {
    assert_eq!(parse_stash("stash l"), StashArgs::List(None));
    assert_eq!(parse_stash("stash li"), StashArgs::List(None));
  }

  #[test]
  fn stash_abbrev_with_name_arg() {
    // The name arg is still picked up correctly when using an abbrev.
    assert_eq!(
      parse_stash("stash p wip"),
      StashArgs::Pop(Some("wip".into()))
    );
  }

  #[test]
  fn stash_arg_longer_than_subcmd_name_treated_as_push() {
    // "pops" isn't a prefix of any subcommand name (the outer check
    // also tests in the prefix-of-name direction), so it falls through
    // to Push.
    assert_eq!(
      parse_stash("stash pops"),
      StashArgs::Push(Some("pops".into()))
    );
  }
}

#[cfg(test)]
mod parse_read_write_tests {
  use super::*;
  use std::path::PathBuf;

  fn parse_ok(input: &str) -> ExNdRule {
    match parse_ex_input(input) {
      ExP::Success(node) => node.kind,
      ExP::Error(msg) => panic!("parse error for {input:?}: {msg}"),
    }
  }

  fn parse_err(input: &str) -> String {
    match parse_ex_input(input) {
      ExP::Success(_) => panic!("expected error for {input:?}"),
      ExP::Error(msg) => msg,
    }
  }

  // ─── :read with file arg ─────────────────────────────────────────

  #[test]
  fn read_with_file_arg() {
    let kind = parse_ok("read foo.txt");
    assert_eq!(
      kind,
      ExNdRule::Read(ReadSrc::File(PathBuf::from("foo.txt")))
    );
  }

  #[test]
  fn read_no_args_errors() {
    let msg = parse_err("read");
    assert!(msg.contains("expected"), "got: {msg:?}");
  }

  // ─── :read! <cmd> shell source ──────────────────────────────────

  #[test]
  fn read_bang_treats_arg_as_shell_command() {
    let kind = parse_ok("read! ls -la");
    match kind {
      ExNdRule::Read(ReadSrc::Cmd(s)) => {
        // Whatever the lexer gives us, the raw span should contain
        // 'ls' and '-la' joined as one string.
        assert!(s.contains("ls"), "got: {s:?}");
      }
      other => panic!("expected Read(Cmd), got {other:?}"),
    }
  }

  // ─── :write with file arg ───────────────────────────────────────

  #[test]
  fn write_with_file_arg() {
    let kind = parse_ok("write out.txt");
    assert_eq!(
      kind,
      ExNdRule::Write(WriteDest::File(Some(PathBuf::from("out.txt"))))
    );
  }

  #[test]
  fn write_with_append_redirect() {
    // `:w >> file` → FileAppend
    let kind = parse_ok("write >> out.txt");
    assert_eq!(
      kind,
      ExNdRule::Write(WriteDest::FileAppend(Some(PathBuf::from("out.txt"))))
    );
  }

  #[test]
  fn write_no_args_parses_as_active_file_target() {
    // `:w` with no path parses successfully; the active-file check
    // happens at execution time (errors there if no file is set).
    let kind = parse_ok("write");
    assert_eq!(kind, ExNdRule::Write(WriteDest::File(None)));
  }

  // ─── :write! <cmd> shell dest ───────────────────────────────────

  #[test]
  fn write_bang_treats_arg_as_shell_command() {
    let kind = parse_ok("write! cat");
    match kind {
      ExNdRule::Write(WriteDest::Cmd(s)) => assert!(s.contains("cat"), "got: {s:?}"),
      other => panic!("expected Write(Cmd), got {other:?}"),
    }
  }
}

#[cfg(test)]
mod parse_one_addr_tests {
  use super::*;

  /// Run only the lexer over `input` and return the resulting tokens.
  fn lex(input: &str) -> Vec<ExTk> {
    ExLexer::new(input).lex()
  }

  /// Extract the (single) address token and return its rule and lexeme.
  /// Panics if no address token was produced. Useful for verifying that
  /// `parse_one_addr` classified the opening character correctly.
  fn first_token(input: &str) -> (ExTkRule, String) {
    let tokens = lex(input);
    let tk = tokens
      .into_iter()
      .next()
      .unwrap_or_else(|| panic!("no tokens produced for {input:?}"));
    let text = tk.span.as_str().to_string();
    (tk.class, text)
  }

  #[test]
  fn dot_is_dot_token() {
    let (rule, text) = first_token(".");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::Dot)));
    assert_eq!(text, ".");
  }

  #[test]
  fn dollar_is_dollar_token() {
    let (rule, text) = first_token("$");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::Dollar)));
    assert_eq!(text, "$");
  }

  #[test]
  fn single_digit_is_number_token() {
    let (rule, text) = first_token("5");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::Number)));
    assert_eq!(text, "5");
  }

  #[test]
  fn multi_digit_is_number_token() {
    let (rule, text) = first_token("123");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::Number)));
    assert_eq!(text, "123");
  }

  #[test]
  fn plus_offset() {
    let (rule, text) = first_token("+3");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::Offset)));
    assert_eq!(text, "+3");
  }

  #[test]
  fn minus_offset() {
    let (rule, text) = first_token("-7");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::Offset)));
    assert_eq!(text, "-7");
  }

  #[test]
  fn bare_plus_is_offset_with_no_digits() {
    let (rule, _) = first_token("+");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::Offset)));
  }

  #[test]
  fn mark_consumes_following_char() {
    let (rule, text) = first_token("'a");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::Mark)));
    assert_eq!(text, "'a");
  }

  #[test]
  fn unterminated_mark_produces_no_token() {
    // A lone quote → next() consumes None, lexer flagged unfinished,
    // parse_one_addr returns false → no token recorded.
    let tokens = lex("'");
    assert!(tokens.is_empty(), "got: {tokens:?}");
  }

  #[test]
  fn forward_pattern_strips_delimiters() {
    let (rule, text) = first_token("/foo/");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::Pattern)));
    // get_pattern_token strips the surrounding delimiter chars.
    assert_eq!(text, "foo");
  }

  #[test]
  fn reverse_pattern_keeps_delimiters() {
    // The Pattern arm uses get_pattern_token (strips delims), but
    // PatternRev uses get_token (no stripping). Pin that distinction.
    let (rule, text) = first_token("?bar?");
    assert!(matches!(rule, ExTkRule::Address(ExLineAddr::PatternRev)));
    assert_eq!(text, "?bar?");
  }
}

#[cfg(test)]
mod parse_one_address_tests {
  //! `ExParser::parse_one_address` is static + private, but it's invoked
  //! by `parse_address` whenever the lexer hands the parser an address
  //! token. We can verify each branch by feeding `<addr>d` and asserting
  //! on the resulting ExNode.address.

  use super::*;
  use crate::readline::editcmd::LineAddr;

  fn parse_addr(input: &str) -> AddressRange {
    let node = match parse_ex_input(input) {
      ExP::Success(node) => node,
      ExP::Error(msg) => panic!("parse error for {input:?}: {msg}"),
    };
    node
      .address
      .unwrap_or_else(|| panic!("no address parsed for {input:?}"))
  }

  fn single(addr: AddressRange) -> LineAddr {
    match addr {
      AddressRange::Single(a) => a,
      AddressRange::Range(_, _) => {
        panic!("expected Single, got Range: {addr:?}")
      }
    }
  }

  #[test]
  fn number_becomes_line_number() {
    assert_eq!(single(parse_addr("5d")), LineAddr::Number(5));
  }

  #[test]
  fn dot_becomes_current() {
    assert_eq!(single(parse_addr(".d")), LineAddr::Current);
  }

  #[test]
  fn dollar_becomes_last() {
    assert_eq!(single(parse_addr("$d")), LineAddr::Last);
  }

  #[test]
  fn percent_becomes_full_range_all_lines() {
    // Percent is the only "Full" variant — it short-circuits to the
    // canonical 1..$ range.
    assert_eq!(parse_addr("%d"), AddressRange::all_lines());
  }

  #[test]
  fn plus_offset_strips_plus_prefix() {
    assert_eq!(single(parse_addr("+3d")), LineAddr::Offset(3));
  }

  #[test]
  fn minus_offset_keeps_sign() {
    assert_eq!(single(parse_addr("-2d")), LineAddr::Offset(-2));
  }

  #[test]
  fn mark_addr_uses_char_after_quote() {
    assert_eq!(single(parse_addr("'ad")), LineAddr::Mark('a'));
  }

  #[test]
  fn forward_pattern_becomes_pattern_addr() {
    // Pattern stores the delimiter-stripped text from the lexer.
    assert_eq!(
      single(parse_addr("/foo/d")),
      LineAddr::Pattern("foo".into())
    );
  }

  #[test]
  fn range_parses_both_endpoints() {
    let addr = parse_addr("1,5d");
    match addr {
      AddressRange::Range(s, e) => {
        assert_eq!(s, LineAddr::Number(1));
        assert_eq!(e, LineAddr::Number(5));
      }
      other @ AddressRange::Single(_) => panic!("expected Range, got {other:?}"),
    }
  }

  #[test]
  fn reverse_pattern_becomes_pattern_rev_addr() {
    // PatternRev arm uses get_token (no delimiter stripping), so the
    // stored string includes the surrounding '?' delimiters.
    assert_eq!(
      single(parse_addr("?bar?d")),
      LineAddr::PatternRev("?bar?".into())
    );
  }
}

#[cfg(test)]
mod parse_command_tests {
  //! `parse_command` dispatches each `ExCommand` variant to a sub-parser.
  //! We verify the simple ones (Delete, Yank, Put, Quit), the Unknown
  //! arm, the bang side-effect, and the `NoMatch` path.

  use super::*;
  use crate::readline::editcmd::Anchor;

  fn parse_ok(input: &str) -> ExNode {
    match parse_ex_input(input) {
      ExP::Success(node) => node,
      ExP::Error(msg) => panic!("parse error for {input:?}: {msg}"),
    }
  }

  fn parse_err(input: &str) -> String {
    match parse_ex_input(input) {
      ExP::Success(node) => panic!("expected error for {input:?}, got {node:?}"),
      ExP::Error(msg) => msg,
    }
  }

  #[test]
  fn bare_delete_produces_delete_node() {
    let node = parse_ok("delete");
    assert_eq!(node.kind, ExNdRule::Delete);
    assert!(!node.bang);
    assert!(node.address.is_none());
  }

  #[test]
  fn bare_yank_produces_yank_node() {
    let node = parse_ok("yank");
    assert_eq!(node.kind, ExNdRule::Yank);
  }

  #[test]
  fn bare_put_produces_put_after_node() {
    let node = parse_ok("put");
    assert_eq!(node.kind, ExNdRule::Put(Anchor::After));
  }

  #[test]
  fn bare_quit_produces_quit_node() {
    let node = parse_ok("quit");
    assert_eq!(node.kind, ExNdRule::Quit);
  }

  #[test]
  fn prefix_match_picks_first_matching_command() {
    // classify_cmd uses prefix matching against the COMMANDS table.
    // "d" → Delete (first entry whose name starts with "d").
    let node = parse_ok("d");
    assert_eq!(node.kind, ExNdRule::Delete);
  }

  #[test]
  fn unknown_command_name_errors() {
    // "zzz" is not a prefix of any command → classify_cmd returns
    // Unknown → parse_command emits "not an editor command".
    let msg = parse_err("zzz");
    assert!(
      msg.contains("not an editor command") || msg.contains("zzz"),
      "got: {msg:?}"
    );
  }

  #[test]
  fn bang_after_command_sets_bang_flag() {
    let node = parse_ok("quit!");
    assert_eq!(node.kind, ExNdRule::Quit);
    assert!(node.bang, "expected bang=true, got: {node:?}");
  }

  #[test]
  fn no_command_at_all_errors_expected_command() {
    // An address with no command → parse_command returns NoMatch →
    // parse() emits "expected command".
    let msg = parse_err("5");
    assert!(msg.contains("expected command"), "got: {msg:?}");
  }

  #[test]
  fn leading_bang_parses_as_shell_command() {
    // `:!cmd` form — Bang as the command-starter routes to parse_shell.
    // Regression guard: the ExTkRule refactor split Bang out of the
    // is_command predicate and silently broke this path.
    let node = parse_ok("!echo foo");
    match node.kind {
      ExNdRule::Shell(s) => assert!(s.contains("echo"), "got: {s:?}"),
      other => panic!("expected Shell, got {other:?}"),
    }
  }
}
