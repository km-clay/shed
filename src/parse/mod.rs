use std::{collections::VecDeque, fmt::Debug, rc::Rc, str::FromStr};

use ariadne::{Fmt, Label, Span as AriadneSpan};
use bitflags::bitflags;
use fmt::Display;
use lex::{LexFlags, LexStream, Span, SpanSource, Tk, TkFlags, TkRule};

use crate::{
  libsh::{
    error::{ShErr, ShErrKind, ShResult, last_color, next_color},
    utils::{NodeVecUtils, TkVecUtils},
  },
  match_loop,
  parse::lex::clean_input,
  prelude::*,
  procio::IoMode,
  sherr,
  state::read_shopts,
};

pub mod execute;
pub mod lex;

pub const TEST_UNARY_OPS: [&str; 23] = [
  "-a", "-b", "-c", "-d", "-e", "-f", "-g", "-h", "-L", "-k", "-n", "-p", "-r", "-s", "-S", "-t",
  "-u", "-w", "-x", "-z", "-O", "-G", "-N",
];

/// Try to match a specific parsing rule
///
/// # Notes
/// * If the match fails, execution continues.
/// * If the match succeeds, the matched node is returned.
macro_rules! try_match {
  ($expr:expr) => {
    if let Some(node) = $expr {
      return Ok(Some(node));
    }
  };
}

/// The parsed AST along with the source input it parsed
///
/// Uses Rc<String> instead of &str because the reference has to stay alive
/// while errors are propagated upwards The string also has to stay alive in the
/// case of pre-parsed shell function nodes, which live in the logic table Using
/// &str for this use-case dramatically overcomplicates the code
#[derive(Clone, Debug)]
pub struct ParsedSrc {
  pub src: Rc<str>,
  pub name: Rc<str>,
  pub ast: Ast,
  pub lex_flags: LexFlags,
  pub parse_flags: ParseFlags,
  pub context: LabelCtx,

  /// Not used internally, used mainly for auto-indent in the line editor. Mirrors the field on ParseStream
  pub block_depth: usize,
}

impl ParsedSrc {
  pub fn new(src: Rc<str>) -> Self {
    let src = if src.contains("\\\n") || src.contains('\r') {
      clean_input(&src).as_str().into()
    } else {
      src
    };
    Self {
      src,
      name: "<stdin>".into(),
      ast: Ast::new(vec![]),
      lex_flags: LexFlags::empty(),
      parse_flags: ParseFlags::empty(),
      context: VecDeque::new(),
      block_depth: 0,
    }
  }
  pub fn with_name(mut self, name: Rc<str>) -> Self {
    self.name = name;
    self
  }
  pub fn with_lex_flags(mut self, flags: LexFlags) -> Self {
    self.lex_flags = flags;
    self
  }
  pub fn with_parse_flags(mut self, flags: ParseFlags) -> Self {
    self.parse_flags = flags;
    self
  }
  pub fn with_context(mut self, ctx: LabelCtx) -> Self {
    self.context = ctx;
    self
  }
  pub fn parse_src(&mut self) -> Result<(), Vec<ShErr>> {
    let mut tokens = vec![];
    for lex_result in LexStream::new(self.src.clone(), self.lex_flags).with_name(self.name.clone())
    {
      match lex_result {
        Ok(token) => tokens.push(token),
        Err(error) => return Err(vec![error]),
      }
    }

    let mut errors = vec![];
    let mut nodes = vec![];
    let parser =
      ParseStream::with_context(tokens, self.context.clone()).with_flags(self.parse_flags);
    for parse_result in parser {
      match parse_result {
        Ok(node) => {
          self.block_depth = 0;
          nodes.push(node)
        }
        Err((depth, error)) => {
          self.block_depth = depth;
          if self.parse_flags.contains(ParseFlags::ERR_RETURN) {
            return Err(vec![error]);
          } else {
            errors.push(error);
          }
        }
      }
    }

    if !errors.is_empty() {
      return Err(errors);
    }

    *self.ast.tree_mut() = nodes;
    Ok(())
  }
  pub fn extract_nodes(&mut self) -> Vec<Node> {
    mem::take(self.ast.tree_mut())
  }
}

#[derive(Default, Clone, Debug)]
pub struct Ast(Vec<Node>);

impl Ast {
  pub fn new(tree: Vec<Node>) -> Self {
    Self(tree)
  }
  pub fn into_inner(self) -> Vec<Node> {
    self.0
  }
  pub fn tree_mut(&mut self) -> &mut Vec<Node> {
    &mut self.0
  }
}

pub type LabelCtx = VecDeque<(SpanSource, Label<Span>)>;

#[derive(Clone, Debug)]
pub struct Node {
  pub class: NdRule,
  pub flags: NdFlags,
  pub redirs: Vec<Redir>,
  pub tokens: Vec<Tk>,
  pub context: LabelCtx,
}

impl Node {
  pub fn get_command(&self) -> Option<&Tk> {
    if let NdRule::Command {
      assignments: _,
      argv,
    } = &self.class
    {
      argv.iter().next()
    } else {
      None
    }
  }
  pub fn get_context(&self, msg: String) -> (SpanSource, Label<Span>) {
    let color = last_color();
    let span = self.get_span().clone();
    (
      span.clone().source().clone(),
      Label::new(span).with_color(color).with_message(msg),
    )
  }
  pub fn walk_tree<F: FnMut(&mut Node)>(&mut self, f: &mut F) {
    f(self);

    match self.class {
      NdRule::IfNode {
        ref mut cond_nodes,
        ref mut else_block,
      } => {
        for node in cond_nodes {
          let CondNode { cond, body } = node;
          cond.walk_tree(f);
          for body_node in body {
            body_node.walk_tree(f);
          }
        }

        for else_node in else_block {
          else_node.walk_tree(f);
        }
      }
      NdRule::LoopNode {
        kind: _,
        ref mut cond_node,
      } => {
        let CondNode { cond, body } = cond_node;
        cond.walk_tree(f);
        for body_node in body {
          body_node.walk_tree(f);
        }
      }
      NdRule::ForNode {
        vars: _,
        arr: _,
        ref mut body,
      } => {
        for body_node in body {
          body_node.walk_tree(f);
        }
      }
      NdRule::CaseNode {
        pattern: _,
        ref mut case_blocks,
      } => {
        for block in case_blocks {
          let CaseNode { pattern: _, body } = block;
          for body_node in body {
            body_node.walk_tree(f);
          }
        }
      }
      NdRule::Command {
        ref mut assignments,
        argv: _,
      } => {
        for assign_node in assignments {
          assign_node.walk_tree(f);
        }
      }
      NdRule::Pipeline { ref mut cmds } => {
        for cmd_node in cmds {
          cmd_node.walk_tree(f);
        }
      }
      NdRule::Conjunction { ref mut elements } => {
        for node in elements.iter_mut() {
          let ConjunctNode { cmd, operator: _ } = node;
          cmd.walk_tree(f);
        }
      }
      NdRule::Assignment {
        kind: _,
        var: _,
        val: _,
      } => (), // No nodes to check
      NdRule::BraceGrp { ref mut body } => {
        for body_node in body {
          body_node.walk_tree(f);
        }
      }
      NdRule::FuncDef {
        name: _,
        ref mut body,
      } => {
        body.walk_tree(f);
      }
      NdRule::Negate { ref mut cmd } => {
        cmd.walk_tree(f);
      }
      NdRule::Test { cases: _ } => (),
    }
  }
  pub fn propagate_context(&mut self, ctx: (SpanSource, Label<Span>)) {
    self.walk_tree(&mut |nd| nd.context.push_back(ctx.clone()));
  }
  pub fn get_span(&self) -> Span {
    let Some(first_tk) = self.tokens.first() else {
      unreachable!()
    };
    let Some(last_tk) = self.tokens.last() else {
      unreachable!()
    };

    Span::from_span_source(
      first_tk.span.range().start..last_tk.span.range().end,
      first_tk.span.span_source().clone(),
    )
  }
}

bitflags! {
#[derive(Clone,Copy,Debug)]
  pub struct NdFlags: u32 {
    const BACKGROUND    = 1 << 0;
    const FORK_BUILTINS = 1 << 1;
    const NO_FORK       = 1 << 2;
    const ARR_ASSIGN    = 1 << 3;
    const PIPE_ERR      = 1 << 4; // whether to include stderr in a pipe
    const NOT_ERR       = 1 << 5; // whether an error triggers ERR traps and set -e
    const PIPE_CMD      = 1 << 6; // is not the last command in a pipeline
    const REPORT_TIME   = 1 << 7; // whether this node should be reported by the time keyword
  }
}

#[derive(Clone, Debug)]
pub struct Redir {
  pub io_mode: IoMode,
  pub class: RedirType,
  pub span: Option<Span>,
}

impl Redir {
  pub fn new(io_mode: IoMode, class: RedirType) -> Self {
    Self {
      io_mode,
      class,
      span: None,
    }
  }
  pub fn with_span(mut self, span: Span) -> Self {
    self.span = Some(span);
    self
  }
}

#[derive(Default, Debug)]
pub struct RedirBldr {
  pub io_mode: Option<IoMode>,
  pub class: Option<RedirType>,
  pub tgt_fd: Option<RawFd>,
  pub span: Option<Span>,
}

impl RedirBldr {
  pub fn new() -> Self {
    Default::default()
  }
  pub fn with_io_mode(self, io_mode: IoMode) -> Self {
    Self {
      io_mode: Some(io_mode),
      ..self
    }
  }
  pub fn with_class(self, class: RedirType) -> Self {
    Self {
      class: Some(class),
      ..self
    }
  }
  pub fn with_tgt(self, tgt_fd: RawFd) -> Self {
    Self {
      tgt_fd: Some(tgt_fd),
      ..self
    }
  }
  pub fn with_span(self, span: Span) -> Self {
    Self {
      span: Some(span),
      ..self
    }
  }
  pub fn build(self) -> Redir {
    let new = Redir::new(self.io_mode.unwrap(), self.class.unwrap());
    if let Some(span) = self.span {
      new.with_span(span)
    } else {
      new
    }
  }
}

impl FromStr for RedirBldr {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let mut chars = s.chars().peekable();
    let mut src_fd = String::new();
    let mut tgt_fd = String::new();
    let mut redir = RedirBldr::new();

    match_loop!(chars.next() => ch, {
      '>' => {
        redir = redir.with_class(RedirType::Output);
        if let Some('>') = chars.peek() {
          chars.next();
          redir = redir.with_class(RedirType::Append);
        } else if let Some('|') = chars.peek() {
          chars.next();
          redir = redir.with_class(RedirType::OutputForce);
        }
      }
      '<' => {
        redir = redir.with_class(RedirType::Input);
        let mut count = 0;

        if chars.peek() == Some(&'>') {
          chars.next(); // consume the '>'
          redir = redir.with_class(RedirType::ReadWrite);
        } else {
          while count < 2 && matches!(chars.peek(), Some('<')) {
            chars.next();
            count += 1;
          }
        }

        redir = match count {
          1 => redir.with_class(RedirType::HereDoc),
          2 => redir.with_class(RedirType::HereString),
          _ => redir, // Default case remains RedirType::Input
        };
      }
      '&' => {
        if chars.peek() == Some(&'-') {
          chars.next();
          src_fd.push('-');
        } else {
          while let Some(next_ch) = chars.next() {
            if next_ch.is_ascii_digit() {
              src_fd.push(next_ch)
            } else {
              break;
            }
          }
        }
        if src_fd.is_empty() {
          return Err(sherr!(
              ParseErr,
              "Invalid character '{}' in redirection operator",
              ch,
          ));
        }
      }
      _ if ch.is_ascii_digit() && tgt_fd.is_empty() => {
        tgt_fd.push(ch);
        while let Some(next_ch) = chars.peek() {
          if next_ch.is_ascii_digit() {
            let next_ch = chars.next().unwrap();
            tgt_fd.push(next_ch);
          } else {
            break;
          }
        }
      }
      _ => {
        return Err(sherr!(
            ParseErr,
            "Invalid character '{}' in redirection operator",
            ch,
        ));
      }
    });

    let tgt_fd = tgt_fd
      .parse::<i32>()
      .unwrap_or_else(|_| match redir.class.unwrap() {
        RedirType::Input | RedirType::ReadWrite | RedirType::HereDoc | RedirType::HereString => 0,
        _ => 1,
      });
    redir = redir.with_tgt(tgt_fd);
    if src_fd.as_str() == "-" {
      let io_mode = IoMode::Close { tgt_fd };
      redir = redir.with_io_mode(io_mode);
    } else if let Ok(src_fd) = src_fd.parse::<i32>() {
      let io_mode = IoMode::fd(tgt_fd, src_fd);
      redir = redir.with_io_mode(io_mode);
    }
    Ok(redir)
  }
}

impl TryFrom<Tk> for RedirBldr {
  type Error = ShErr;
  fn try_from(tk: Tk) -> Result<Self, Self::Error> {
    let span = tk.span.clone();
    if tk.flags.contains(TkFlags::IS_HEREDOC) {
      let flags = tk.flags;

      Ok(RedirBldr {
        io_mode: Some(IoMode::buffer(0, tk.to_string(), flags)?),
        class: Some(RedirType::HereDoc),
        tgt_fd: Some(0),
        span: Some(span),
      })
    } else {
      match Self::from_str(tk.as_str()) {
        Ok(bldr) => Ok(bldr.with_span(span)),
        Err(e) => Err(e.promote(span)),
      }
    }
  }
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum RedirType {
  Null,          // Default
  Pipe,          // |
  PipeAnd,       // |&, redirs stderr and stdout
  Input,         // <
  Output,        // >
  OutputForce,   // >|
  Append,        // >>
  HereDoc,       // <<
  IndentHereDoc, // <<-, strips leading tabs
  HereString,    // <<<
  ReadWrite,     // <>, fd is opened for reading and writing
}

#[derive(Clone, Debug)]
pub struct CondNode {
  pub cond: Box<Node>,
  pub body: Vec<Node>,
}

#[derive(Clone, Debug)]
pub struct CaseNode {
  pub pattern: Tk,
  pub body: Vec<Node>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ConjunctOp {
  And,
  Or,
  Null,
}

#[derive(Clone, Debug)]
pub struct ConjunctNode {
  pub cmd: Box<Node>,
  pub operator: ConjunctOp,
}

#[derive(Clone, Copy, Debug)]
pub enum LoopKind {
  While,
  Until,
}

crate::two_way_display!(LoopKind,
  While <=> "while";
  Until <=> "until";
);

#[derive(Clone, Debug)]
pub enum TestCase {
  Unary {
    operator: Tk,
    operand: Tk,
    conjunct: Option<ConjunctOp>,
  },
  Binary {
    lhs: Tk,
    operator: Tk,
    rhs: Tk,
    conjunct: Option<ConjunctOp>,
  },
}

#[derive(Default, Clone, Debug)]
pub struct TestCaseBuilder {
  lhs: Option<Tk>,
  operator: Option<Tk>,
  rhs: Option<Tk>,
  conjunct: Option<ConjunctOp>,
}

impl TestCaseBuilder {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn is_empty(&self) -> bool {
    self.lhs.is_none() && self.operator.is_none() && self.rhs.is_none() && self.conjunct.is_none()
  }
  pub fn with_lhs(self, lhs: Tk) -> Self {
    let Self {
      lhs: _,
      operator,
      rhs,
      conjunct,
    } = self;
    Self {
      lhs: Some(lhs),
      operator,
      rhs,
      conjunct,
    }
  }
  pub fn with_rhs(self, rhs: Tk) -> Self {
    let Self {
      lhs,
      operator,
      rhs: _,
      conjunct,
    } = self;
    Self {
      lhs,
      operator,
      rhs: Some(rhs),
      conjunct,
    }
  }
  pub fn with_operator(self, operator: Tk) -> Self {
    let Self {
      lhs,
      operator: _,
      rhs,
      conjunct,
    } = self;
    Self {
      lhs,
      operator: Some(operator),
      rhs,
      conjunct,
    }
  }
  pub fn with_conjunction(self, conjunction: ConjunctOp) -> Self {
    let Self {
      lhs,
      operator,
      rhs,
      conjunct: _,
    } = self;
    Self {
      lhs,
      operator,
      rhs,
      conjunct: Some(conjunction),
    }
  }
  pub fn can_build(&self) -> bool {
    self.operator.is_some() && self.rhs.is_some()
  }
  pub fn build(self) -> TestCase {
    let Self {
      lhs,
      operator,
      rhs,
      conjunct,
    } = self;
    if let Some(lhs) = lhs {
      TestCase::Binary {
        lhs,
        operator: operator.unwrap(),
        rhs: rhs.unwrap(),
        conjunct,
      }
    } else {
      TestCase::Unary {
        operator: operator.unwrap(),
        operand: rhs.unwrap(),
        conjunct,
      }
    }
  }
  pub fn build_and_take(&mut self) -> TestCase {
    if self.lhs.is_some() {
      TestCase::Binary {
        lhs: self.lhs.take().unwrap(),
        operator: self.operator.take().unwrap(),
        rhs: self.rhs.take().unwrap(),
        conjunct: self.conjunct.take(),
      }
    } else {
      TestCase::Unary {
        operator: self.operator.take().unwrap(),
        operand: self.rhs.take().unwrap(),
        conjunct: self.conjunct.take(),
      }
    }
  }
}

#[derive(Clone, Debug)]
pub enum AssignKind {
  Eq,
  PlusEq,
  MinusEq,
  MultEq,
  DivEq,
}

#[derive(Clone, Debug, PartialEq)]
/// Flat NdRule names used mainly for debugging
pub enum NdKind {
  IfNode,
  LoopNode,
  ForNode,
  CaseNode,
  Command,
  Pipeline,
  Conjunction,
  Assignment,
  BraceGrp,
  Negate,
  Test,
  FuncDef,
}

impl crate::parse::NdRule {
  pub fn as_nd_kind(&self) -> NdKind {
    match self {
      Self::Negate { .. } => NdKind::Negate,
      Self::IfNode { .. } => NdKind::IfNode,
      Self::LoopNode { .. } => NdKind::LoopNode,
      Self::ForNode { .. } => NdKind::ForNode,
      Self::CaseNode { .. } => NdKind::CaseNode,
      Self::Command { .. } => NdKind::Command,
      Self::Pipeline { .. } => NdKind::Pipeline,
      Self::Conjunction { .. } => NdKind::Conjunction,
      Self::Assignment { .. } => NdKind::Assignment,
      Self::BraceGrp { .. } => NdKind::BraceGrp,
      Self::Test { .. } => NdKind::Test,
      Self::FuncDef { .. } => NdKind::FuncDef,
    }
  }
}

#[derive(Clone, Debug)]
pub enum NdRule {
  IfNode {
    cond_nodes: Vec<CondNode>,
    else_block: Vec<Node>,
  },
  LoopNode {
    kind: LoopKind,
    cond_node: CondNode,
  },
  ForNode {
    vars: Vec<Tk>,
    arr: Vec<Tk>,
    body: Vec<Node>,
  },
  Negate {
    cmd: Box<Node>,
  },
  CaseNode {
    pattern: Tk,
    case_blocks: Vec<CaseNode>,
  },
  Command {
    assignments: Vec<Node>,
    argv: Vec<Tk>,
  },
  Pipeline {
    cmds: Vec<Node>,
  },
  Conjunction {
    elements: Vec<ConjunctNode>,
  },
  Assignment {
    kind: AssignKind,
    var: Tk,
    val: Tk,
  },
  BraceGrp {
    body: Vec<Node>,
  },
  Test {
    cases: Vec<TestCase>,
  },
  FuncDef {
    name: Tk,
    body: Box<Node>,
  },
}

bitflags! {
  #[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,PartialOrd,Ord)]
  pub struct ParseFlags: u32 {
    const INCOMPLETE = 1 << 0; // Whether to error
    const ERR_RETURN = 1 << 1; // Return on first error instead of continuing
  }
}

pub struct ParseStream {
  pub tokens: Vec<Tk>,
  pub cursor: usize,
  pub context: LabelCtx,
  pub flags: ParseFlags,

  /// Not used internally, used mainly for auto-indent in the line editor
  pub block_depth: usize,
}

impl Debug for ParseStream {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ParseStream")
      .field("tokens", &self.tokens)
      .field("cursor", &self.cursor)
      .finish()
  }
}

impl ParseStream {
  pub fn new(tokens: Vec<Tk>) -> Self {
    let tokens = tokens
      .into_iter()
      .filter(|tk| tk.class != TkRule::Comment)
      .collect();
    Self {
      tokens,
      cursor: 0,
      context: VecDeque::new(),
      block_depth: 0,
      flags: ParseFlags::empty(),
    }
  }
  pub fn with_context(tokens: Vec<Tk>, context: LabelCtx) -> Self {
    Self {
      tokens,
      cursor: 0,
      context,
      block_depth: 0,
      flags: ParseFlags::empty(),
    }
  }
  pub fn with_flags(mut self, flags: ParseFlags) -> Self {
    self.flags = flags;
    self
  }
  /// Slice off consumed tokens
  fn commit(&mut self, num_consumed: usize) {
    assert!(self.cursor + num_consumed <= self.tokens.len());
    self.cursor += num_consumed;
  }
  fn next_tk_class(&self) -> &TkRule {
    self.peek_tk().map(|tk| &tk.class).unwrap_or(&TkRule::Null)
  }
  fn peek_tk(&self) -> Option<&Tk> {
    self.tokens.get(self.cursor)
  }
  fn next_tk(&mut self) -> Option<Tk> {
    let tk = self
      .tokens
      .get(self.cursor)
      .and_then(|tk| (tk.class != TkRule::EOI).then_some(tk))
      .cloned()?;
    self.cursor += 1;
    Some(tk)
  }
  fn tokens(&self) -> &[Tk] {
    &self.tokens[self.cursor..]
  }
  fn is_empty(&self) -> bool {
    self.tokens().is_empty()
  }
  fn len(&self) -> usize {
    self.tokens().len()
  }
  /// Catches a Sep token in cases where separators are optional
  ///
  /// e.g. both `if foo; then bar; fi` and
  /// ```bash
  /// if foo; then
  /// 	bar
  /// fi
  /// ```
  /// are valid syntax
  fn catch_separator(&mut self, node_tks: &mut Vec<Tk>) {
    if *self.next_tk_class() == TkRule::Sep {
      node_tks.push(self.next_tk().unwrap());
    }
  }
  fn check_separator(&self) -> bool {
    matches!(
      self.next_tk_class(),
      TkRule::Or | TkRule::Bg | TkRule::And | TkRule::BraceGrpEnd | TkRule::Pipe | TkRule::Sep
    )
  }
  fn assert_separator(&mut self, node_tks: &mut Vec<Tk>) -> ShResult<()> {
    let next_class = self.next_tk_class();
    match next_class {
      TkRule::EOI | TkRule::Or | TkRule::Bg | TkRule::And | TkRule::BraceGrpEnd | TkRule::Pipe => {
        Ok(())
      }

      TkRule::Sep => {
        if let Some(tk) = self.next_tk() {
          node_tks.push(tk);
        }
        Ok(())
      }
      _ => Err(sherr!(ParseErr, "Expected a semicolon or newline here",)),
    }
  }
  fn next_tk_is_some(&self) -> bool {
    self.peek_tk().is_some_and(|tk| tk.class != TkRule::EOI)
  }
  fn check_case_pattern(&self) -> bool {
    self
      .peek_tk()
      .is_some_and(|tk| tk.class == TkRule::CasePattern)
  }
  fn check_keyword(&self, kw: &str) -> bool {
    self.peek_tk().is_some_and(|tk| {
      if kw == "in" {
        tk.span.as_str() == "in"
      } else {
        tk.flags.contains(TkFlags::KEYWORD) && tk.span.as_str() == kw
      }
    })
  }
  fn check_redir(&self) -> bool {
    self.peek_tk().is_some_and(|tk| tk.class == TkRule::Redir)
  }
  /// This tries to match on different stuff that can appear in a command
  /// position Matches shell commands like if-then-fi, pipelines, etc.
  /// Ordered from specialized to general, with more generally matchable stuff
  /// appearing at the bottom The check_pipelines parameter is used to prevent
  /// left-recursion issues in self.parse_pipeln()
  fn parse_block(&mut self, check_pipelines: bool) -> ShResult<Option<Node>> {
    if !check_pipelines {
      self.block_depth += 1;
    }

    // You will live to see man made horrors beyond your comprehension
    let result = || -> ShResult<Option<Node>> {
      if check_pipelines {
        try_match!(self.parse_pipeln()?);
        Ok(None)
      } else {
        try_match!(self.parse_func_def()?);
        try_match!(self.parse_brc_grp(false /* from_func_def */)?);
        try_match!(self.parse_case()?);
        try_match!(self.parse_loop()?);
        try_match!(self.parse_for()?);
        try_match!(self.parse_test()?);

        // these aren't nested contexts
        // so we decrement the depth and descend into
        // yet another immediately-invoked closure
        // please stabilize the try block my rust developers!!
        self.block_depth -= 1;
        let r = || -> ShResult<Option<Node>> {
          try_match!(self.parse_if()?);
          try_match!(self.parse_negate()?);
          try_match!(self.parse_time()?);
          try_match!(self.parse_cmd()?);
          Ok(None)
        }()?;
        self.block_depth += 1;

        Ok(r)
      }
    }()?;

    if !check_pipelines {
      self.block_depth -= 1;
    }

    Ok(result)
  }
  fn parse_cmd_list(&mut self) -> ShResult<Option<Node>> {
    let mut elements = vec![];
    let mut node_tks = vec![];

    while let Some(mut block) = self.parse_block(true)? {
      node_tks.append(&mut block.tokens.clone());
      let conjunct_op = match self.next_tk_class() {
        TkRule::And => ConjunctOp::And,
        TkRule::Or => ConjunctOp::Or,
        _ => ConjunctOp::Null,
      };
      if conjunct_op != ConjunctOp::Null {
        block.walk_tree(&mut |nd| nd.flags |= NdFlags::NOT_ERR);
      }
      let conjunction = ConjunctNode {
        cmd: Box::new(block),
        operator: conjunct_op,
      };
      elements.push(conjunction);
      if conjunct_op != ConjunctOp::Null {
        let Some(tk) = self.next_tk() else { break };
        node_tks.push(tk);
      }
      if conjunct_op == ConjunctOp::Null {
        break;
      }
    }
    if elements.is_empty() {
      Ok(None)
    } else {
      Ok(Some(Node {
        class: NdRule::Conjunction { elements },
        flags: NdFlags::empty(),
        redirs: vec![],
        context: self.context.clone(),
        tokens: node_tks,
      }))
    }
  }
  fn parse_func_def(&mut self) -> ShResult<Option<Node>> {
    let mut node_tks: Vec<Tk> = vec![];
    let body;

    // Two forms: "name()" as one token, or "name" followed by "()" as separate tokens
    // Kind of a hack but I forgot you can have the spaces when I wrote the lexer :D
    let spaced_form = !is_func_name(self.peek_tk())
      && self
        .peek_tk()
        .is_some_and(|tk| tk.flags.contains(TkFlags::IS_CMD))
      && is_func_parens(self.tokens().get(1));

    if !is_func_name(self.peek_tk()) && !spaced_form {
      return Ok(None);
    }

    let name_tk = self.next_tk().unwrap();
    node_tks.push(name_tk.clone());
    let name = name_tk.clone();
    let name_raw: Rc<str> = if spaced_form {
      // Consume the "()" token
      let parens_tk = self.next_tk().unwrap();
      node_tks.push(parens_tk);
      name.as_str().into()
    } else {
      name.as_str().into()
    };
    let mut src = name_tk.span.span_source().clone();
    src.rename(name_raw.clone());
    let color = next_color();
    // Push a placeholder context so child nodes inherit it
    self.context.push_back((
      src.clone(),
      Label::new(name_tk.span.clone().with_name(name_raw.clone()))
        .with_message(format!(
          "in function '{}' defined here",
          name_raw.clone().fg(color)
        ))
        .with_color(color),
    ));

    let Some(brc_grp) = self.parse_brc_grp(true /* from_func_def */)? else {
      self.context.pop_back();
      return Err(parse_err_full(
        "Expected a brace group after function name",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    };
    body = Box::new(brc_grp);
    // Replace placeholder with full-span label
    self.context.pop_back();

    let node = Node {
      class: NdRule::FuncDef { name, body },
      flags: NdFlags::empty(),
      redirs: vec![],
      tokens: node_tks,
      context: self.context.clone(),
    };

    self.context.pop_back();
    Ok(Some(node))
  }
  fn panic_mode(&mut self, node_tks: &mut Vec<Tk>) {
    while let Some(tk) = self.next_tk() {
      node_tks.push(tk.clone());
      if tk.class == TkRule::Sep {
        break;
      }
    }
  }
  fn parse_test(&mut self) -> ShResult<Option<Node>> {
    let mut node_tks: Vec<Tk> = vec![];
    let mut cases: Vec<TestCase> = vec![];
    if !self.check_keyword("[[") || !self.next_tk_is_some() {
      return Ok(None);
    }
    node_tks.push(self.next_tk().unwrap());
    let mut case_builder = TestCaseBuilder::new();
    while let Some(tk) = self.next_tk() {
      node_tks.push(tk.clone());
      if tk.as_str() == "]]" {
        if case_builder.can_build() {
          let case = case_builder.build_and_take();
          cases.push(case);
          break;
        } else if cases.is_empty() {
          return Err(parse_err_full(
            "Malformed test call",
            &node_tks.get_span().unwrap(),
            self.context.clone(),
          ));
        } else {
          break;
        }
      }
      if case_builder.is_empty() {
        match tk.as_str() {
          _ if TEST_UNARY_OPS.contains(&tk.as_str()) => {
            case_builder = case_builder.with_operator(tk.clone())
          }
          _ => case_builder = case_builder.with_lhs(tk.clone()),
        }
        continue;
      } else if case_builder.operator.is_some() && case_builder.rhs.is_none() {
        case_builder = case_builder.with_rhs(tk.clone());
        continue;
      } else if case_builder.lhs.is_some() && case_builder.operator.is_none() {
        // we got lhs, then rhs → treat it as operator maybe?
        case_builder = case_builder.with_operator(tk.clone());
        continue;
      } else if let TkRule::And | TkRule::Or = tk.class {
        if case_builder.can_build() {
          if case_builder.conjunct.is_some() {
            return Err(parse_err_full(
              "Invalid placement for logical operator in test",
              &node_tks.get_span().unwrap(),
              self.context.clone(),
            ));
          }
          let op = match tk.class {
            TkRule::And => ConjunctOp::And,
            TkRule::Or => ConjunctOp::Or,
            _ => unreachable!(),
          };
          case_builder = case_builder.with_conjunction(op);
          let case = case_builder.build_and_take();
          cases.push(case);
          continue;
        } else {
          return Err(parse_err_full(
            "Invalid placement for logical operator in test",
            &node_tks.get_span().unwrap(),
            self.context.clone(),
          ));
        }
      }
      if case_builder.can_build() {
        let case = case_builder.build_and_take();
        cases.push(case);
      }
    }
    self.catch_separator(&mut node_tks);
    let node: Node = Node {
      class: NdRule::Test { cases },
      flags: NdFlags::empty(),
      redirs: vec![],
      context: self.context.clone(),
      tokens: node_tks,
    };
    Ok(Some(node))
  }
  fn parse_brc_grp(&mut self, from_func_def: bool) -> ShResult<Option<Node>> {
    let mut node_tks: Vec<Tk> = vec![];
    let mut body: Vec<Node> = vec![];
    let mut redirs: Vec<Redir> = vec![];

    if *self.next_tk_class() != TkRule::BraceGrpStart {
      return Ok(None);
    }
    node_tks.push(self.next_tk().unwrap());

    self.catch_separator(&mut node_tks);

    loop {
      if *self.next_tk_class() == TkRule::BraceGrpEnd {
        node_tks.push(self.next_tk().unwrap());
        break;
      }
      if let Some(node) = self.parse_cmd_list()? {
        node_tks.extend(node.tokens.clone());
        body.push(node);
      } else if *self.next_tk_class() != TkRule::BraceGrpEnd {
        let next = self.peek_tk().cloned();
        let err = match next {
          Some(tk) => Err(parse_err_full(
            &format!("Unexpected token '{}' in brace group body", tk.as_str()),
            &tk.span,
            self.context.clone(),
          )),
          None => Err(parse_err_full(
            "Unexpected end of input while parsing brace group body",
            &node_tks.get_span().unwrap(),
            self.context.clone(),
          )),
        };
        self.panic_mode(&mut node_tks);
        return err;
      }
      self.catch_separator(&mut node_tks);
      if !self.next_tk_is_some() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          "Expected a closing brace for this brace group",
          &node_tks.get_span().unwrap(),
          self.context.clone(),
        ));
      }
    }

    if !from_func_def {
      self.parse_redir(&mut redirs, &mut node_tks)?;
    }

    let node = Node {
      class: NdRule::BraceGrp { body },
      flags: NdFlags::empty(),
      redirs,
      context: self.context.clone(),
      tokens: node_tks,
    };
    Ok(Some(node))
  }
  fn build_redir<F: FnMut() -> Option<Tk>>(
    redir_tk: &Tk,
    mut next: F,
    node_tks: &mut Vec<Tk>,
    context: LabelCtx,
  ) -> ShResult<Redir> {
    let redir_bldr = RedirBldr::try_from(redir_tk.clone()).unwrap();
    let next_tk = if redir_bldr.io_mode.is_none() {
      next()
    } else {
      None
    };
    if redir_bldr.io_mode.is_some() {
      return Ok(redir_bldr.build());
    }
    let Some(redir_type) = redir_bldr.class else {
      return Err(parse_err_full(
        "Malformed redirection operator",
        &redir_tk.span,
        context.clone(),
      ));
    };
    match redir_type {
      RedirType::HereString => {
        if next_tk.as_ref().is_none_or(|tk| tk.class == TkRule::EOI) {
          return Err(sherr!(
            ParseErr @ next_tk.unwrap_or(redir_tk.clone()).span.clone(),
            "Expected a string after this redirection",
          ));
        }
        let mut string = next_tk.unwrap().expand()?.get_words().join(" ");
        string.push('\n');
        let io_mode = IoMode::buffer(redir_bldr.tgt_fd.unwrap_or(0), string, redir_tk.flags)?;
        Ok(redir_bldr.with_io_mode(io_mode).build())
      }
      _ => {
        if next_tk.as_ref().is_none_or(|tk| tk.class == TkRule::EOI) {
          return Err(sherr!(
            ParseErr @ redir_tk.span.clone(),
            "Expected a filename after this redirection",
          ));
        }
        let path_tk = next_tk.unwrap();
        node_tks.push(path_tk.clone());
        let pathbuf = PathBuf::from(path_tk.span.as_str());
        let io_mode = IoMode::file(redir_bldr.tgt_fd.unwrap(), pathbuf, redir_type);
        Ok(redir_bldr.with_io_mode(io_mode).build())
      }
    }
  }
  fn parse_redir(&mut self, redirs: &mut Vec<Redir>, node_tks: &mut Vec<Tk>) -> ShResult<()> {
    while self.check_redir() {
      let tk = self.next_tk().unwrap();
      node_tks.push(tk.clone());
      let ctx = self.context.clone();
      let redir = Self::build_redir(&tk, || self.next_tk(), node_tks, ctx)?;
      redirs.push(redir);
    }
    Ok(())
  }
  fn parse_case(&mut self) -> ShResult<Option<Node>> {
    // Needs a pattern token
    // Followed by any number of CaseNodes
    let mut node_tks: Vec<Tk> = vec![];

    let mut case_blocks: Vec<CaseNode> = vec![];
    let redirs: Vec<Redir> = vec![];

    if !self.check_keyword("case") || !self.next_tk_is_some() {
      return Ok(None);
    }
    node_tks.push(self.next_tk().unwrap());

    let pat_err = parse_err_full(
      "Expected a pattern after 'case' keyword",
      &node_tks.get_span().unwrap(),
      self.context.clone(),
    )
    .with_note("Patterns can be raw text, or anything that gets substituted with raw text");

    let Some(pat_tk) = self.next_tk() else {
      self.panic_mode(&mut node_tks);
      return Err(pat_err);
    };

    if pat_tk.span.as_str() == "in" {
      return Err(pat_err);
    }

    let pattern: Tk = pat_tk;

    node_tks.push(pattern.clone());

    if !self.check_keyword("in") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Expected 'in' after case variable name",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());

    self.catch_separator(&mut node_tks);

    loop {
      if !self.check_case_pattern() || !self.next_tk_is_some() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          "Expected a case pattern here",
          &node_tks.get_span().unwrap(),
          self.context.clone(),
        ));
      }
      let case_pat_tk = self.next_tk().unwrap();
      node_tks.push(case_pat_tk.clone());
      self.catch_separator(&mut node_tks);
      self.block_depth += 1;

      let mut nodes = vec![];
      while let Some(node) = self.parse_cmd_list()? {
        node_tks.extend(node.tokens.clone());
        let sep = node.tokens.last().unwrap();
        if sep.has_double_semi() {
          nodes.push(node);
          self.block_depth -= 1;
          break;
        } else {
          nodes.push(node);
        }
      }

      let case_node = CaseNode {
        pattern: case_pat_tk,
        body: nodes,
      };
      case_blocks.push(case_node);

      self.catch_separator(&mut node_tks);

      if self.check_keyword("esac") {
        node_tks.push(self.next_tk().unwrap());
        self.assert_separator(&mut node_tks)?;
        break;
      }

      if !self.next_tk_is_some() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          "Expected 'esac' after case block",
          &node_tks.get_span().unwrap(),
          self.context.clone(),
        ));
      }
    }

    let node = Node {
      class: NdRule::CaseNode {
        pattern,
        case_blocks,
      },
      flags: NdFlags::empty(),
      redirs,
      context: self.context.clone(),
      tokens: node_tks,
    };
    Ok(Some(node))
  }
  fn make_err(&self, span: lex::Span, label: Label<lex::Span>) -> ShErr {
    let src = span.span_source().clone();
    ShErr::new(ShErrKind::ParseErr, span)
      .with_label(src, label)
      .with_context(self.context.clone())
  }
  fn parse_time(&mut self) -> ShResult<Option<Node>> {
    let mut node_tks: Vec<Tk> = vec![];

    if !self.check_keyword("time") || !self.next_tk_is_some() {
      return Ok(None);
    }
    node_tks.push(self.next_tk().unwrap());

    let Some(mut cmd) = self.parse_block(true)? else {
      self.panic_mode(&mut node_tks);
      let span = node_tks.get_span().unwrap();
      let color = next_color();
      return Err(
        self.make_err(
          span.clone(),
          Label::new(span)
            .with_message("Expected a command after 'time'")
            .with_color(color),
        ),
      );
    };
    // the 'time' keyword does not have it's own NdRule. This is because it does not alter execution in any meaningful way.
    // All it does here is set the REPORT_TIME flag on the node it wraps. Then we just return the node itself.
    // Also, this is an unchecked context for the purpose of 'set -e' errors.
    cmd.walk_tree(&mut |n| n.flags |= NdFlags::NOT_ERR | NdFlags::REPORT_TIME);

    node_tks.extend(cmd.tokens.clone());
    self.catch_separator(&mut node_tks);
    Ok(Some(cmd))
  }
  fn parse_negate(&mut self) -> ShResult<Option<Node>> {
    let mut node_tks: Vec<Tk> = vec![];

    if !self.check_keyword("!") || !self.next_tk_is_some() {
      return Ok(None);
    }
    node_tks.push(self.next_tk().unwrap());

    let Some(mut cmd) = self.parse_block(true)? else {
      self.panic_mode(&mut node_tks);
      let span = node_tks.get_span().unwrap();
      let color = next_color();
      return Err(
        self.make_err(
          span.clone(),
          Label::new(span)
            .with_message("Expected a command after '!'")
            .with_color(color),
        ),
      );
    };
    cmd.walk_tree(&mut |n| n.flags |= NdFlags::NOT_ERR); // disable set -e for negated commands

    node_tks.extend(cmd.tokens.clone());
    self.catch_separator(&mut node_tks);

    let node = Node {
      class: NdRule::Negate { cmd: Box::new(cmd) },
      flags: NdFlags::empty(),
      redirs: vec![],
      context: self.context.clone(),
      tokens: node_tks,
    };
    Ok(Some(node))
  }
  fn parse_if(&mut self) -> ShResult<Option<Node>> {
    // Needs at last one 'if-then',
    // Any number of 'elif-then',
    // Zero or one 'else'
    let mut node_tks: Vec<Tk> = vec![];
    let mut cond_nodes: Vec<CondNode> = vec![];
    let mut else_block: Vec<Node> = vec![];
    let mut redirs: Vec<Redir> = vec![];

    if !self.check_keyword("if") || !self.next_tk_is_some() {
      return Ok(None);
    }
    node_tks.push(self.next_tk().unwrap());

    loop {
      self.block_depth += 1;
      let prefix_keywrd = if cond_nodes.is_empty() { "if" } else { "elif" };
      let Some(mut cond) = self.parse_cmd_list()? else {
        if prefix_keywrd == "elif" {
          self.block_depth -= 1;
        }
        self.panic_mode(&mut node_tks);
        let span = node_tks.get_span().unwrap();
        let color = next_color();
        return Err(
          self.make_err(
            span.clone(),
            Label::new(span)
              .with_message(format!(
                "Expected an expression after '{}'",
                prefix_keywrd.fg(color)
              ))
              .with_color(color),
          ),
        );
      };
      node_tks.extend(cond.tokens.clone());
      cond.walk_tree(&mut |n| n.flags |= NdFlags::NOT_ERR); // disable set -e for condition commands

      if !self.check_keyword("then") || !self.next_tk_is_some() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          &format!("Expected 'then' after '{prefix_keywrd}' condition"),
          &node_tks.get_span().unwrap(),
          self.context.clone(),
        ));
      }
      node_tks.push(self.next_tk().unwrap());
      self.catch_separator(&mut node_tks);

      let mut body_blocks = vec![];
      while let Some(body_block) = self.parse_cmd_list()? {
        node_tks.extend(body_block.tokens.clone());
        body_blocks.push(body_block);
      }

      if body_blocks.is_empty() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          "Expected an expression after 'then'",
          &node_tks.get_span().unwrap(),
          self.context.clone(),
        ));
      };
      let cond_node = CondNode {
        cond: Box::new(cond),
        body: body_blocks,
      };
      cond_nodes.push(cond_node);

      self.catch_separator(&mut node_tks);
      if !self.check_keyword("elif") || !self.next_tk_is_some() {
        break;
      } else {
        self.block_depth -= 1;
        node_tks.push(self.next_tk().unwrap());
        self.catch_separator(&mut node_tks);
      }
    }

    self.catch_separator(&mut node_tks);
    if self.check_keyword("else") {
      self.block_depth -= 1;
      node_tks.push(self.next_tk().unwrap());
      let mut already_added = false;

      if self.check_separator() || self.next_tk_is_some() {
        already_added = true;
        self.block_depth += 1;
      }

      let has_sep = self.check_separator();
      log::debug!("has_sep = {has_sep}");

      self.catch_separator(&mut node_tks);

      while let Some(block) = self.parse_cmd_list()? {
        else_block.push(block)
      }

      if else_block.is_empty() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          "Expected an expression after 'else'",
          &node_tks.get_span().unwrap(),
          self.context.clone(),
        ));
      }

      if !already_added {
        self.block_depth += 1;
      }
    }

    self.catch_separator(&mut node_tks);
    if !self.check_keyword("fi") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Expected 'fi' after if statement",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());
    self.block_depth -= 1;

    self.parse_redir(&mut redirs, &mut node_tks)?;

    self.assert_separator(&mut node_tks)?;

    let node = Node {
      class: NdRule::IfNode {
        cond_nodes,
        else_block,
      },
      flags: NdFlags::empty(),
      redirs,
      context: self.context.clone(),
      tokens: node_tks,
    };
    Ok(Some(node))
  }
  fn parse_for(&mut self) -> ShResult<Option<Node>> {
    let mut node_tks: Vec<Tk> = vec![];
    let mut vars: Vec<Tk> = vec![];
    let mut arr: Vec<Tk> = vec![];
    let mut body: Vec<Node> = vec![];
    let mut redirs: Vec<Redir> = vec![];

    if !self.check_keyword("for") || !self.next_tk_is_some() {
      return Ok(None);
    }
    node_tks.push(self.next_tk().unwrap());

    while let Some(tk) = self.next_tk() {
      node_tks.push(tk.clone());
      if tk.as_str() == "in" {
        break;
      } else {
        vars.push(tk.clone());
      }
    }

    while let Some(tk) = self.next_tk() {
      node_tks.push(tk.clone());
      if tk.class == TkRule::Sep {
        break;
      } else {
        arr.push(tk.clone());
      }
    }

    if vars.is_empty() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "This for loop is missing a variable",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    }
    if arr.is_empty() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "This for loop is missing an array",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    }
    if !self.check_keyword("do") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Missing a 'do' for this for loop",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());
    self.catch_separator(&mut node_tks);

    while let Some(node) = self.parse_cmd_list()? {
      body.push(node)
    }

    self.catch_separator(&mut node_tks);
    if !self.check_keyword("done") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Missing a 'done' after this for loop",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());

    self.parse_redir(&mut redirs, &mut node_tks)?;

    let node = Node {
      class: NdRule::ForNode { vars, arr, body },
      flags: NdFlags::empty(),
      redirs,
      context: self.context.clone(),
      tokens: node_tks,
    };
    Ok(Some(node))
  }
  fn parse_loop(&mut self) -> ShResult<Option<Node>> {
    // Requires a single CondNode and a LoopKind

    let cond_node: CondNode;
    let mut node_tks = vec![];
    let mut redirs = vec![];

    if (!self.check_keyword("while") && !self.check_keyword("until")) || !self.next_tk_is_some() {
      return Ok(None);
    }
    let loop_tk = self.next_tk().unwrap();
    let loop_kind: LoopKind = loop_tk
      .span
      .as_str()
      .parse() // LoopKind implements FromStr
      .unwrap();

    node_tks.push(loop_tk);
    self.catch_separator(&mut node_tks);

    let Some(mut cond) = self.parse_cmd_list()? else {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        &format!("Expected an expression after '{loop_kind}'"), // It also implements Display
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    };
    node_tks.extend(cond.tokens.clone());
    cond.walk_tree(&mut |n| n.flags |= NdFlags::NOT_ERR); // disable set -e for condition commands

    if !self.check_keyword("do") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Expected 'do' after loop condition",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());
    self.catch_separator(&mut node_tks);

    let mut body = vec![];
    while let Some(block) = self.parse_cmd_list()? {
      node_tks.extend(block.tokens.clone());
      body.push(block);
    }
    if body.is_empty() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Expected an expression after 'do'",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    };

    self.catch_separator(&mut node_tks);
    if !self.check_keyword("done") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Expected 'done' after loop body",
        &node_tks.get_span().unwrap(),
        self.context.clone(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());

    self.parse_redir(&mut redirs, &mut node_tks)?;

    self.assert_separator(&mut node_tks)?;

    cond_node = CondNode {
      cond: Box::new(cond),
      body,
    };
    let loop_node = Node {
      class: NdRule::LoopNode {
        kind: loop_kind,
        cond_node,
      },
      flags: NdFlags::empty(),
      redirs,
      context: self.context.clone(),
      tokens: node_tks,
    };
    Ok(Some(loop_node))
  }
  fn parse_pipeln(&mut self) -> ShResult<Option<Node>> {
    let mut cmds = vec![];
    let mut node_tks = vec![];
    let mut flags = NdFlags::empty();

    while let Some(mut cmd) = self.parse_block(false)? {
      let is_punctuated = node_is_punctuated(&cmd.tokens);
      node_tks.append(&mut cmd.tokens.clone());
      let next_class = self.next_tk_class();
      if *next_class == TkRule::ErrPipe {
        cmd.flags |= NdFlags::PIPE_ERR;
      }
      if matches!(*next_class, TkRule::Pipe | TkRule::ErrPipe) {
        cmd.walk_tree(&mut |n| n.flags |= NdFlags::PIPE_CMD | NdFlags::NOT_ERR);
      }

      cmds.push(cmd);
      if *next_class == TkRule::Bg {
        let tk = self.next_tk().unwrap();
        node_tks.push(tk.clone());
        flags |= NdFlags::BACKGROUND;
        break;
      } else if (!matches!(*next_class, TkRule::Pipe | TkRule::ErrPipe)) || is_punctuated {
        break;
      } else if let Some(pipe) = self.next_tk() {
        node_tks.push(pipe)
      } else {
        break;
      }
    }
    if cmds.is_empty() {
      Ok(None)
    } else {
      Ok(Some(Node {
        // TODO: implement pipe_err support
        class: NdRule::Pipeline { cmds },
        flags,
        redirs: vec![],
        context: self.context.clone(),
        tokens: node_tks,
      }))
    }
  }
  fn parse_cmd(&mut self) -> ShResult<Option<Node>> {
    let mut node_tks = vec![];

    let result = 'out: {
      let tk_slice = self.tokens();
      let mut tk_iter = tk_slice.iter().peekable();
      let mut redirs = vec![];
      let mut argv = vec![];
      let flags = NdFlags::empty();
      let mut assignments = vec![];

      loop {
        let Some(prefix_tk) = tk_iter.next() else {
          break;
        };
        if let TkRule::CasePattern = prefix_tk.class {
          break 'out Err(parse_err_full(
            "Found case pattern in command",
            &prefix_tk.span,
            self.context.clone(),
          ));
        }
        let is_cmd = prefix_tk.flags.contains(TkFlags::IS_CMD);
        let is_assignment = prefix_tk.flags.contains(TkFlags::ASSIGN);
        let is_keyword = prefix_tk.flags.contains(TkFlags::KEYWORD);

        if is_cmd {
          node_tks.push(prefix_tk.clone());
          argv.push(prefix_tk.clone());
          break;
        } else if is_assignment {
          let Some(assign) = self.parse_assignment(prefix_tk) else {
            break;
          };
          node_tks.push(prefix_tk.clone());
          assignments.push(assign)
        } else if is_keyword {
          return Ok(None);
        } else if prefix_tk.class == TkRule::Redir {
          let ctx = self.context.clone();
          let redir = Self::build_redir(prefix_tk, || tk_iter.next().cloned(), &mut node_tks, ctx)?;
          redirs.push(redir);
        } else if prefix_tk.class == TkRule::Sep {
          // Separator ends the prefix section - add it so commit() consumes it
          node_tks.push(prefix_tk.clone());
          break;
        } else {
          // Other non-prefix token ends the prefix section
          break;
        }
      }
      if argv.is_empty() {
        if assignments.is_empty() {
          break 'out Ok(None);
        } else {
          // If we have assignments but no command word,
          // return the assignment-only command without parsing more tokens
          self.commit(node_tks.len());
          let mut context = self.context.clone();
          let assignments_span = assignments.get_span().unwrap();
          context.push_back((
            assignments_span.source().clone(),
            Label::new(assignments_span)
              .with_message("in variable assignment defined here".to_string())
              .with_color(next_color()),
          ));
          return Ok(Some(Node {
            class: NdRule::Command { assignments, argv },
            tokens: node_tks,
            flags,
            redirs,
            context: self.context.clone(),
          }));
        }
      }
      loop {
        let Some(tk) = tk_iter.next() else {
          break;
        };
        match tk.class {
          _ if tk_iter.peek().is_some_and(|tk| tk.class == TkRule::Bg) => break,
          TkRule::Comment => break,

          TkRule::EOI
          | TkRule::Pipe
          | TkRule::ErrPipe
          | TkRule::And
          | TkRule::BraceGrpEnd
          | TkRule::Or
          | TkRule::Bg => break,
          TkRule::Sep => {
            node_tks.push(tk.clone());
            break;
          }
          TkRule::Str => {
            argv.push(tk.clone());
            node_tks.push(tk.clone());
          }
          TkRule::Redir => {
            node_tks.push(tk.clone());
            let ctx = self.context.clone();
            let redir = match Self::build_redir(tk, || tk_iter.next().cloned(), &mut node_tks, ctx)
            {
              Ok(r) => r,
              Err(e) => {
                self.panic_mode(&mut node_tks);
                return Err(e);
              }
            };
            redirs.push(redir);
          }
          _ => {
            return Err(parse_err_full(
              &format!("Unexpected token in command: {:?}", tk.class),
              &tk.span,
              self.context.clone(),
            ));
          }
        };
      }
      self.commit(node_tks.len());

      return Ok(Some(Node {
        class: NdRule::Command { assignments, argv },
        tokens: node_tks,
        flags,
        redirs,
        context: self.context.clone(),
      }));
    };

    match result {
      Ok(node) => Ok(node),
      Err(e) => {
        self.panic_mode(&mut node_tks);
        Err(e)
      }
    }
  }
  fn parse_assignment(&self, token: &Tk) -> Option<Node> {
    let mut chars = token.span.as_str().chars();
    let mut var_name = String::new();
    let mut name_range = token.span.range().start..token.span.range().start;
    let mut var_val = String::new();
    let mut val_range = token.span.range().end..token.span.range().end;
    let mut assign_kind = None;
    let mut pos = token.span.range().start;

    while let Some(ch) = chars.next() {
      if assign_kind.is_some() {
        match ch {
          '\\' => {
            pos += ch.len_utf8();
            var_val.push(ch);
            if let Some(esc_ch) = chars.next() {
              pos += esc_ch.len_utf8();
              var_val.push(esc_ch);
            }
          }
          _ => {
            pos += ch.len_utf8();
            var_val.push(ch);
          }
        }
      } else {
        match ch {
          '=' => {
            name_range.end = pos;
            pos += ch.len_utf8();
            val_range.start = pos;
            assign_kind = Some(AssignKind::Eq);
          }
          '-' => {
            name_range.end = pos;
            pos += ch.len_utf8();
            let Some('=') = chars.next() else { return None };
            pos += '='.len_utf8();
            val_range.start = pos;
            assign_kind = Some(AssignKind::MinusEq);
          }
          '+' => {
            name_range.end = pos;
            pos += ch.len_utf8();
            let Some('=') = chars.next() else { return None };
            pos += '='.len_utf8();
            val_range.start = pos;
            assign_kind = Some(AssignKind::PlusEq);
          }
          '/' => {
            name_range.end = pos;
            pos += ch.len_utf8();
            let Some('=') = chars.next() else { return None };
            pos += '='.len_utf8();
            val_range.start = pos;
            assign_kind = Some(AssignKind::DivEq);
          }
          '*' => {
            name_range.end = pos;
            pos += ch.len_utf8();
            let Some('=') = chars.next() else { return None };
            pos += '='.len_utf8();
            val_range.start = pos;
            assign_kind = Some(AssignKind::MultEq);
          }
          '\\' => {
            pos += ch.len_utf8();
            var_name.push(ch);
            if let Some(esc_ch) = chars.next() {
              pos += esc_ch.len_utf8();
              var_name.push(esc_ch);
            }
          }
          _ => {
            pos += ch.len_utf8();
            var_name.push(ch)
          }
        }
      }
    }
    if let Some(assign_kind) = assign_kind
      && !var_name.is_empty()
    {
      let var = Tk::new(TkRule::Str, Span::new(name_range, token.source()));
      let val = Tk::new(TkRule::Str, Span::new(val_range, token.source()));
      let flags = if var_val.starts_with('(') && var_val.ends_with(')') {
        NdFlags::ARR_ASSIGN
      } else {
        NdFlags::empty()
      };

      Some(Node {
        class: NdRule::Assignment {
          kind: assign_kind,
          var,
          val,
        },
        tokens: vec![token.clone()],
        flags,
        redirs: vec![],
        context: self.context.clone(),
      })
    } else {
      None
    }
  }
}

impl Iterator for ParseStream {
  type Item = Result<Node, (usize, ShErr)>; // (block_depth and error)
  fn next(&mut self) -> Option<Self::Item> {
    // Empty token vector or only SOI/EOI tokens, nothing to do
    if self.is_empty() || self.len() == 1 {
      return None;
    }
    while let Some(tk) = self.tokens().first() {
      if let TkRule::EOI = tk.class {
        return None;
      }
      if let TkRule::SOI | TkRule::Sep = tk.class {
        self.next_tk();
      } else {
        break;
      }
    }
    let result = self.parse_cmd_list();
    match result {
      Ok(Some(node)) => Some(Ok(node)),
      Ok(None) => None,
      Err(e) => {
        let block_depth = std::mem::take(&mut self.block_depth);
        Some(Err((block_depth, e)))
      }
    }
  }
}

fn node_is_punctuated(tokens: &[Tk]) -> bool {
  tokens
    .last()
    .is_some_and(|tk| matches!(tk.class, TkRule::Sep))
}

pub fn get_redir_file<P: AsRef<Path>>(class: RedirType, path: P) -> ShResult<File> {
  let path = path.as_ref();
  let result = match class {
    RedirType::Input => OpenOptions::new().read(true).open(Path::new(&path)),
    RedirType::Output => {
      if read_shopts(|o| o.set.noclobber) && path.is_file() {
        return Err(sherr!(
          ExecFail,
          "shopt core.noclobber is set, refusing to overwrite existing file `{}`",
          path.display()
        ));
      }
      OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
    }
    RedirType::ReadWrite => OpenOptions::new()
      .write(true)
      .read(true)
      .create(true)
      .truncate(false)
      .open(path),
    RedirType::OutputForce => OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .open(path),
    RedirType::Append => OpenOptions::new().create(true).append(true).open(path),
    _ => unimplemented!("Unimplemented redir type: {:?}", class),
  };
  Ok(result?)
}

fn parse_err_full(reason: &str, blame: &Span, context: LabelCtx) -> ShErr {
  sherr!(ParseErr @ blame.clone(), "{reason}").with_context(context)
}

fn is_func_name(tk: Option<&Tk>) -> bool {
  tk.is_some_and(|tk| {
    tk.flags.contains(TkFlags::KEYWORD)
      && (tk.span.as_str().ends_with("()") && !tk.span.as_str().ends_with("\\()"))
  })
}

fn is_func_parens(tk: Option<&Tk>) -> bool {
  tk.is_some_and(|tk| tk.flags.contains(TkFlags::KEYWORD) && tk.span.as_str() == "()")
}

/// Perform an operation on the child nodes of a given node
///
/// # Parameters
/// node: A mutable reference to a node to be operated on
/// filter: A closure or function which checks an attribute of a child node and
/// returns a boolean operation: The closure or function to apply to a child
/// node which matches on the filter
///
/// Very useful for testing, i.e. needing to extract specific types of nodes
/// from the AST to inspect values
pub fn node_operation<F1, F2>(node: &mut Node, filter: &F1, operation: &mut F2)
where
  F1: Fn(&Node) -> bool,
  F2: FnMut(&mut Node),
{
  let check_node = |node: &mut Node, filter: &F1, operation: &mut F2| {
    if filter(node) {
      operation(node);
    } else {
      node_operation::<F1, F2>(node, filter, operation);
    }
  };

  if filter(node) {
    operation(node);
  }

  match node.class {
    NdRule::IfNode {
      ref mut cond_nodes,
      ref mut else_block,
    } => {
      for node in cond_nodes {
        let CondNode { cond, body } = node;
        check_node(cond, filter, operation);
        for body_node in body {
          check_node(body_node, filter, operation);
        }
      }

      for else_node in else_block {
        check_node(else_node, filter, operation);
      }
    }
    NdRule::LoopNode {
      kind: _,
      ref mut cond_node,
    } => {
      let CondNode { cond, body } = cond_node;
      check_node(cond, filter, operation);
      for body_node in body {
        check_node(body_node, filter, operation);
      }
    }
    NdRule::ForNode {
      vars: _,
      arr: _,
      ref mut body,
    } => {
      for body_node in body {
        check_node(body_node, filter, operation);
      }
    }
    NdRule::CaseNode {
      pattern: _,
      ref mut case_blocks,
    } => {
      for block in case_blocks {
        let CaseNode { pattern: _, body } = block;
        for body_node in body {
          check_node(body_node, filter, operation);
        }
      }
    }
    NdRule::Command {
      ref mut assignments,
      argv: _,
    } => {
      for assign_node in assignments {
        check_node(assign_node, filter, operation);
      }
    }
    NdRule::Pipeline { ref mut cmds } => {
      for cmd_node in cmds {
        check_node(cmd_node, filter, operation);
      }
    }
    NdRule::Conjunction { ref mut elements } => {
      for node in elements.iter_mut() {
        let ConjunctNode { cmd, operator: _ } = node;
        check_node(cmd, filter, operation);
      }
    }
    NdRule::Assignment {
      kind: _,
      var: _,
      val: _,
    } => (), // No nodes to check
    NdRule::BraceGrp { ref mut body } => {
      for body_node in body {
        check_node(body_node, filter, operation);
      }
    }
    NdRule::FuncDef {
      name: _,
      ref mut body,
    } => check_node(body, filter, operation),

    NdRule::Negate { ref mut cmd } => check_node(cmd, filter, operation),
    NdRule::Test { cases: _ } => (),
  }
}

#[cfg(test)]
pub mod tests {
  use super::NdKind;
  use crate::testutil::get_ast;

  #[test]
  fn parse_hello_world() {
    let input = "echo hello world";
    let expected = &mut [NdKind::Conjunction, NdKind::Pipeline, NdKind::Command].into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_if_statement() {
    let input = "if echo foo; then echo bar; fi";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::IfNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_pipeline() {
    let input = "ls | grep foo | wc -l";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Command,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_conjunction_and() {
    let input = "echo foo && echo bar";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_while_loop() {
    let input = "while true; do echo hello; done";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::LoopNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_for_loop() {
    let input = "for i in a b c; do echo $i; done";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::ForNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_case_statement() {
    let input = "case foo in bar) echo bar;; baz) echo baz;; esac";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::CaseNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_func_def() {
    let input = "foo() { echo hello; }";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::FuncDef,
      NdKind::BraceGrp,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_assignment() {
    let input = "FOO=bar";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Assignment,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_assignment_with_command() {
    let input = "FOO=bar echo hello";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Assignment,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_if_elif_else() {
    let input = "if true; then echo a; elif false; then echo b; else echo c; fi";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::IfNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_brace_group() {
    let input = "{ echo hello; echo world; }";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::BraceGrp,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_nested_if_in_while() {
    let input = "while true; do if false; then echo no; fi; done";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::LoopNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::IfNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_test_bracket() {
    let input = "[[ -n hello ]]";
    let expected = &mut [NdKind::Conjunction, NdKind::Pipeline, NdKind::Test].into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_nested_func_with_if_and_loop() {
    let input = "setup() {
			for f in a b c; do
				if [[ -n $f ]]; then
					echo $f
				fi
			done
		}";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::FuncDef,
      NdKind::BraceGrp,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::ForNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::IfNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Test,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_pipeline_with_brace_groups() {
    let input = "{ echo foo; echo bar; } | { grep foo; wc -l; }";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::BraceGrp,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::BraceGrp,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_deeply_nested_if() {
    let input = "if true; then
			if false; then
				if true; then
					echo deep
				fi
			fi
		fi";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::IfNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::IfNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::IfNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_case_with_multiple_commands() {
    let input = "case $1 in
			start)
				echo starting
				run_server
			;;
			stop)
				echo stopping
				kill_server
			;;
			*)
				echo unknown
			;;
		esac";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::CaseNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_func_with_case_and_conjunction() {
    let input = "dispatch() {
			case $1 in
				build)
					make clean && make all
				;;
				test)
					make test || echo failed
				;;
			esac
		}";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::FuncDef,
      NdKind::BraceGrp,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::CaseNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_while_with_pipeline_and_assignment() {
    let input = "while read line; do
			FOO=bar echo $line | grep pattern | wc -l
		done";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::LoopNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Assignment,
      NdKind::Command,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_nested_loops() {
    let input = "for i in 1 2 3; do
			for j in a b c; do
				while true; do
					echo $i $j
				done
			done
		done";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::ForNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::ForNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::LoopNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_complex_conjunction_chain() {
    let input = "mkdir -p dir && cd dir && touch file || echo failed && echo done";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_func_defining_inner_func() {
    let input = "outer() {
			inner() {
				echo hello from inner
			}
			inner
		}";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::FuncDef,
      NdKind::BraceGrp,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::FuncDef,
      NdKind::BraceGrp,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_multiline_if_elif_with_pipelines() {
    let input = "if cat /etc/passwd | grep root; then
			echo found root
		elif ls /tmp | wc -l; then
			echo tmp has files
		else
			echo fallback | tee log.txt
		fi";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::IfNode,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_cursed_input() {
    let input = "if if while if if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; then if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; elif while while :; do :; done; do until :; do :; done; done; then while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; elif until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; then until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; else case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; fi; do while case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; done; then until while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; do until while :; do :; done; do until :; do :; done; done; done; elif until until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; do case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; done; then case foo in; foo) case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac;; bar) if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi;; biz) if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi;; esac; elif case foo in; foo) while while :; do :; done; do until :; do :; done; done;; bar) while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done;; biz) until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done;; esac; then if until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; then case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; elif case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; then if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; elif if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; then while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; else while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; fi; else if until while :; do :; done; do until :; do :; done; done; then until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; elif case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; then case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; elif if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; then if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; else while while :; do :; done; do until :; do :; done; done; fi; fi; then while while while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; do until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; done; do while until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; do case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; done; done; elif until until case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; do if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; done; do until if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; do while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; done; done; then case foo in; foo) case foo in; foo) while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done;; bar) until while :; do :; done; do until :; do :; done; done;; biz) until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done;; esac;; bar) case foo in; foo) case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac;; bar) case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac;; biz) if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi;; esac;; biz) if if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; then while while :; do :; done; do until :; do :; done; done; elif while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; then until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; elif until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; then case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; else case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; fi;; esac; elif if if if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; then if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; elif while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; then while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; elif until while :; do :; done; do until :; do :; done; done; then until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; else case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; fi; then while case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; do if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; done; elif while if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; do while while :; do :; done; do until :; do :; done; done; done; then until while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; do until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; done; elif until until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; do case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; done; then case foo in; foo) case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac;; bar) if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi;; biz) if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi;; esac; else case foo in; foo) while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done;; bar) while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done;; biz) until while :; do :; done; do until :; do :; done; done;; esac; fi; then if if until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; then case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; elif case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; then if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; elif if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; then while while :; do :; done; do until :; do :; done; done; else while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; fi; then if until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; then until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; elif case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; then case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; elif if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; then if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; else while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; fi; elif while while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; do until while :; do :; done; do until :; do :; done; done; done; then while until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; do case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; done; elif until case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; do if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; done; then until if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; do while while :; do :; done; do until :; do :; done; done; done; else case foo in; foo) while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done;; bar) until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done;; biz) until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done;; esac; fi; else while case foo in; foo) case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac;; bar) case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac;; biz) if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi;; esac; do if if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; then while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; elif while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; then until while :; do :; done; do until :; do :; done; done; elif until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; then case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; else case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; fi; done; fi";
    assert!(get_ast(input).is_ok()); // lets spare our sanity and just say that "ok" means "it parsed correctly"
  }
  #[test]
  fn parse_stray_keyword_in_brace_group() {
    let input = "{ echo bar case foo in bar) echo fizz ;; buzz) echo buzz ;; esac }";
    assert!(get_ast(input).is_err());
  }

  // ===================== Heredocs =====================

  #[test]
  fn parse_basic_heredoc() {
    let input = "cat <<EOF\nhello world\nEOF";
    let expected = &mut [NdKind::Conjunction, NdKind::Pipeline, NdKind::Command].into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_heredoc_with_tab_strip() {
    let input = "cat <<-EOF\n\t\thello\n\t\tworld\nEOF";
    let expected = &mut [NdKind::Conjunction, NdKind::Pipeline, NdKind::Command].into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_literal_heredoc() {
    let input = "cat <<'EOF'\nhello $world\nEOF";
    let expected = &mut [NdKind::Conjunction, NdKind::Pipeline, NdKind::Command].into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_herestring() {
    let input = "cat <<< \"hello world\"";
    let expected = &mut [NdKind::Conjunction, NdKind::Pipeline, NdKind::Command].into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_heredoc_in_pipeline() {
    let input = "cat <<EOF | grep hello\nhello world\ngoodbye world\nEOF";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_heredoc_in_conjunction() {
    let input = "cat <<EOF && echo done\nhello\nEOF";
    let expected = &mut [
      NdKind::Conjunction,
      NdKind::Pipeline,
      NdKind::Command,
      NdKind::Pipeline,
      NdKind::Command,
    ]
    .into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_heredoc_double_quoted_delimiter() {
    let input = "cat <<\"EOF\"\nhello $world\nEOF";
    let expected = &mut [NdKind::Conjunction, NdKind::Pipeline, NdKind::Command].into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_heredoc_empty_body() {
    let input = "cat <<EOF\nEOF";
    let expected = &mut [NdKind::Conjunction, NdKind::Pipeline, NdKind::Command].into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_heredoc_multiword_delimiter() {
    // delimiter should only be the first word
    let input = "cat <<DELIM\nsome content\nDELIM";
    let expected = &mut [NdKind::Conjunction, NdKind::Pipeline, NdKind::Command].into_iter();
    let ast = get_ast(input).unwrap();
    let mut node = ast[0].clone();
    if let Err(e) = node.assert_structure(expected) {
      panic!("{}", e);
    }
  }

  #[test]
  fn parse_two_heredocs_on_one_line() {
    let input = "cat <<A; cat <<B\nfoo\nA\nbar\nB";
    let ast = get_ast(input).unwrap();
    assert_eq!(ast.len(), 2);
  }

  // ===================== Heredoc Execution =====================

  use crate::state::{VarFlags, VarKind, write_vars};
  use crate::testutil::{TestGuard, test_input};

  #[test]
  fn heredoc_basic_output() {
    let guard = TestGuard::new();
    test_input("cat <<EOF\nhello world\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello world\n");
  }

  #[test]
  fn heredoc_multiline_output() {
    let guard = TestGuard::new();
    test_input("cat <<EOF\nline one\nline two\nline three\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "line one\nline two\nline three\n");
  }

  #[test]
  fn heredoc_variable_expansion() {
    let guard = TestGuard::new();
    write_vars(|v| v.set_var("NAME", VarKind::Str("world".into()), VarFlags::NONE)).unwrap();
    test_input("cat <<EOF\nhello $NAME\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello world\n");
  }

  #[test]
  fn heredoc_literal_no_expansion() {
    let guard = TestGuard::new();
    write_vars(|v| v.set_var("NAME", VarKind::Str("world".into()), VarFlags::NONE)).unwrap();
    test_input("cat <<'EOF'\nhello $NAME\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello $NAME\n");
  }

  #[test]
  fn heredoc_tab_stripping() {
    let guard = TestGuard::new();
    test_input("cat <<-EOF\n\t\thello\n\t\tworld\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\nworld\n");
  }

  #[test]
  fn heredoc_tab_stripping_uneven() {
    let guard = TestGuard::new();
    test_input("cat <<-EOF\n\t\t\thello\n\tworld\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "\t\thello\nworld\n");
  }

  #[test]
  fn heredoc_empty_body() {
    let guard = TestGuard::new();
    test_input("cat <<EOF\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "");
  }

  #[test]
  fn heredoc_in_pipeline() {
    let guard = TestGuard::new();
    test_input("cat <<EOF | grep hello\nhello world\ngoodbye world\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello world\n");
  }

  #[test]
  fn herestring_basic() {
    let guard = TestGuard::new();
    test_input("cat <<< \"hello world\"".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello world\n");
  }

  #[test]
  fn herestring_variable_expansion() {
    let guard = TestGuard::new();
    write_vars(|v| v.set_var("MSG", VarKind::Str("hi there".into()), VarFlags::NONE)).unwrap();
    test_input("cat <<< $MSG".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hi there\n");
  }

  #[test]
  fn heredoc_double_quoted_delimiter_is_literal() {
    let guard = TestGuard::new();
    write_vars(|v| v.set_var("X", VarKind::Str("val".into()), VarFlags::NONE)).unwrap();
    test_input("cat <<\"EOF\"\nhello $X\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello $X\n");
  }

  #[test]
  fn heredoc_preserves_blank_lines() {
    let guard = TestGuard::new();
    test_input("cat <<EOF\nfirst\n\nsecond\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "first\n\nsecond\n");
  }

  #[test]
  fn heredoc_tab_strip_preserves_empty_lines() {
    let guard = TestGuard::new();
    test_input("cat <<-EOF\n\thello\n\n\tworld\nEOF".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\n\nworld\n");
  }

  #[test]
  fn heredoc_two_on_one_line() {
    let guard = TestGuard::new();
    test_input("cat <<A; cat <<B\nfoo\nA\nbar\nB".to_string()).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "foo\nbar\n");
  }
}
