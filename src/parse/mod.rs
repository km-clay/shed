use std::str::FromStr;

use bitflags::bitflags;
use fmt::Display;
use lex::{LexFlags, LexStream, Span, Tk, TkFlags, TkRule};

use crate::{
  libsh::{
    error::{Note, ShErr, ShErrKind, ShResult},
    utils::TkVecUtils,
  },
  prelude::*,
  procio::IoMode,
};

pub mod execute;
pub mod lex;

pub const TEST_UNARY_OPS: [&str; 21] = [
  "-a", "-b", "-c", "-d", "-e", "-f", "-g", "-h", "-L", "-k", "-p", "-r", "-s", "-S", "-t", "-u",
  "-w", "-x", "-O", "-G", "-N",
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
/// Uses Arc<String> instead of &str because the reference has to stay alive
/// while errors are propagated upwards The string also has to stay alive in the
/// case of pre-parsed shell function nodes, which live in the logic table Using
/// &str for this use-case dramatically overcomplicates the code
#[derive(Clone, Debug)]
pub struct ParsedSrc {
  pub src: Arc<String>,
  pub ast: Ast,
  pub lex_flags: LexFlags,
}

impl ParsedSrc {
  pub fn new(src: Arc<String>) -> Self {
    Self {
      src,
      ast: Ast::new(vec![]),
      lex_flags: LexFlags::empty(),
    }
  }
  pub fn with_lex_flags(mut self, flags: LexFlags) -> Self {
    self.lex_flags = flags;
    self
  }
  pub fn parse_src(&mut self) -> Result<(), Vec<ShErr>> {
    let mut tokens = vec![];
    for lex_result in LexStream::new(self.src.clone(), self.lex_flags) {
      match lex_result {
        Ok(token) => tokens.push(token),
        Err(error) => return Err(vec![error]),
      }
    }

    let mut errors = vec![];
    let mut nodes = vec![];
    for parse_result in ParseStream::new(tokens) {
      match parse_result {
        Ok(node) => nodes.push(node),
        Err(error) => errors.push(error),
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

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
pub struct Node {
  pub class: NdRule,
  pub flags: NdFlags,
  pub redirs: Vec<Redir>,
  pub tokens: Vec<Tk>,
}

impl Node {
  pub fn get_command(&self) -> Option<&Tk> {
    let NdRule::Command {
      assignments: _,
      argv,
    } = &self.class
    else {
      return None;
    };
    let command = argv.iter().find(|tk| tk.flags.contains(TkFlags::IS_CMD))?;
    Some(command)
  }
  pub fn get_span(&self) -> Span {
    let Some(first_tk) = self.tokens.first() else {
      unreachable!()
    };
    let Some(last_tk) = self.tokens.last() else {
      unreachable!()
    };

    Span::new(
      first_tk.span.start..last_tk.span.end,
      first_tk.span.get_source(),
    )
  }
}

bitflags! {
#[derive(Clone,Copy,Debug)]
  pub struct NdFlags: u32 {
    const BACKGROUND = 0b000001;
  }
}

#[derive(Clone, Debug)]
pub struct Redir {
  pub io_mode: IoMode,
  pub class: RedirType,
}

impl Redir {
  pub fn new(io_mode: IoMode, class: RedirType) -> Self {
    Self { io_mode, class }
  }
}

#[derive(Default, Debug)]
pub struct RedirBldr {
  pub io_mode: Option<IoMode>,
  pub class: Option<RedirType>,
  pub tgt_fd: Option<RawFd>,
}

impl RedirBldr {
  pub fn new() -> Self {
    Default::default()
  }
  pub fn with_io_mode(self, io_mode: IoMode) -> Self {
    let Self {
      io_mode: _,
      class,
      tgt_fd,
    } = self;
    Self {
      io_mode: Some(io_mode),
      class,
      tgt_fd,
    }
  }
  pub fn with_class(self, class: RedirType) -> Self {
    let Self {
      io_mode,
      class: _,
      tgt_fd,
    } = self;
    Self {
      io_mode,
      class: Some(class),
      tgt_fd,
    }
  }
  pub fn with_tgt(self, tgt_fd: RawFd) -> Self {
    let Self {
      io_mode,
      class,
      tgt_fd: _,
    } = self;
    Self {
      io_mode,
      class,
      tgt_fd: Some(tgt_fd),
    }
  }
  pub fn build(self) -> Redir {
    Redir::new(self.io_mode.unwrap(), self.class.unwrap())
  }
}

impl FromStr for RedirBldr {
  type Err = ();
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let mut chars = s.chars().peekable();
    let mut src_fd = String::new();
    let mut tgt_fd = String::new();
    let mut redir = RedirBldr::new();

    while let Some(ch) = chars.next() {
      match ch {
        '>' => {
          redir = redir.with_class(RedirType::Output);
          if let Some('>') = chars.peek() {
            chars.next();
            redir = redir.with_class(RedirType::Append);
          }
        }
        '<' => {
          redir = redir.with_class(RedirType::Input);
          let mut count = 0;

          while count < 2 && matches!(chars.peek(), Some('<')) {
            chars.next();
            count += 1;
          }

          redir = match count {
            1 => redir.with_class(RedirType::HereDoc),
            2 => redir.with_class(RedirType::HereString),
            _ => redir, // Default case remains RedirType::Input
          };
        }
        '&' => {
          while let Some(next_ch) = chars.next() {
            if next_ch.is_ascii_digit() {
              src_fd.push(next_ch)
            } else {
              break;
            }
          }
          if src_fd.is_empty() {
            return Err(());
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
        _ => return Err(()),
      }
    }

    // FIXME: I am 99.999999999% sure that tgt_fd and src_fd are backwards here
    let tgt_fd = tgt_fd
      .parse::<i32>()
      .unwrap_or_else(|_| match redir.class.unwrap() {
        RedirType::Input | RedirType::HereDoc | RedirType::HereString => 0,
        _ => 1,
      });
    redir = redir.with_tgt(tgt_fd);
    if let Ok(src_fd) = src_fd.parse::<i32>() {
      let io_mode = IoMode::fd(tgt_fd, src_fd);
      redir = redir.with_io_mode(io_mode);
    }
    Ok(redir)
  }
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum RedirType {
  Null,       // Default
  Pipe,       // |
  PipeAnd,    // |&, redirs stderr and stdout
  Input,      // <
  Output,     // >
  Append,     // >>
  HereDoc,    // <<
  HereString, // <<<
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

impl FromStr for LoopKind {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "while" => Ok(Self::While),
      "until" => Ok(Self::Until),
      _ => Err(ShErr::simple(
        ShErrKind::ParseErr,
        format!("Invalid loop kind: {s}"),
      )),
    }
  }
}

impl Display for LoopKind {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    match self {
      LoopKind::While => write!(f, "while"),
      LoopKind::Until => write!(f, "until"),
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
    pipe_err: bool,
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

#[derive(Debug)]
pub struct ParseStream {
  pub tokens: Vec<Tk>,
}

impl ParseStream {
  pub fn new(tokens: Vec<Tk>) -> Self {
    Self { tokens }
  }
  fn next_tk_class(&self) -> &TkRule {
    if let Some(tk) = self.tokens.first() {
      &tk.class
    } else {
      &TkRule::Null
    }
  }
  fn peek_tk(&self) -> Option<&Tk> {
    self.tokens.first()
  }
  fn next_tk(&mut self) -> Option<Tk> {
    if !self.tokens.is_empty() {
      if *self.next_tk_class() == TkRule::EOI {
        return None;
      }
      Some(self.tokens.remove(0))
    } else {
      None
    }
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
      _ => Err(ShErr::simple(
        ShErrKind::ParseErr,
        "Expected a semicolon or newline here",
      )),
    }
  }
  fn next_tk_is_some(&self) -> bool {
    self
      .tokens
      .first()
      .is_some_and(|tk| tk.class != TkRule::EOI)
  }
  fn check_case_pattern(&self) -> bool {
    self
      .tokens
      .first()
      .is_some_and(|tk| tk.class == TkRule::CasePattern)
  }
  fn check_keyword(&self, kw: &str) -> bool {
    self.tokens.first().is_some_and(|tk| {
      if kw == "in" {
        tk.span.as_str() == "in"
      } else {
        tk.flags.contains(TkFlags::KEYWORD) && tk.span.as_str() == kw
      }
    })
  }
  fn check_redir(&self) -> bool {
    self
      .tokens
      .first()
      .is_some_and(|tk| tk.class == TkRule::Redir)
  }
  /// Slice off consumed tokens
  fn commit(&mut self, num_consumed: usize) {
    assert!(num_consumed <= self.tokens.len());
    self.tokens = self.tokens[num_consumed..].to_vec();
  }
  /// This tries to match on different stuff that can appear in a command
  /// position Matches shell commands like if-then-fi, pipelines, etc.
  /// Ordered from specialized to general, with more generally matchable stuff
  /// appearing at the bottom The check_pipelines parameter is used to prevent
  /// left-recursion issues in self.parse_pipeln()
  fn parse_block(&mut self, check_pipelines: bool) -> ShResult<Option<Node>> {
    try_match!(self.parse_func_def()?);
    try_match!(self.parse_brc_grp(false /* from_func_def */)?);
    try_match!(self.parse_case()?);
    try_match!(self.parse_loop()?);
    try_match!(self.parse_for()?);
    try_match!(self.parse_if()?);
    try_match!(self.parse_test()?);
    if check_pipelines {
      try_match!(self.parse_pipeln()?);
    } else {
      try_match!(self.parse_cmd()?);
    }
    Ok(None)
  }
  fn parse_cmd_list(&mut self) -> ShResult<Option<Node>> {
    let mut elements = vec![];
    let mut node_tks = vec![];

    while let Some(block) = self.parse_block(true)? {
      node_tks.append(&mut block.tokens.clone());
      let conjunct_op = match self.next_tk_class() {
        TkRule::And => ConjunctOp::And,
        TkRule::Or => ConjunctOp::Or,
        _ => ConjunctOp::Null,
      };
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
        tokens: node_tks,
      }))
    }
  }
  fn parse_func_def(&mut self) -> ShResult<Option<Node>> {
    let mut node_tks: Vec<Tk> = vec![];
    let body;

    if !is_func_name(self.peek_tk()) {
      return Ok(None);
    }
    let name_tk = self.next_tk().unwrap();
    node_tks.push(name_tk.clone());
    let name = name_tk;

    let Some(brc_grp) = self.parse_brc_grp(true /* from_func_def */)? else {
      return Err(parse_err_full(
        "Expected a brace group after function name",
        &node_tks.get_span().unwrap(),
      ));
    };
    body = Box::new(brc_grp);

    let node = Node {
      class: NdRule::FuncDef { name, body },
      flags: NdFlags::empty(),
      redirs: vec![],
      tokens: node_tks,
    };

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
      }
      self.catch_separator(&mut node_tks);
      if !self.next_tk_is_some() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          "Expected a closing brace for this brace group",
          &node_tks.get_span().unwrap(),
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
      tokens: node_tks,
    };
    Ok(Some(node))
  }
  fn parse_redir(&mut self, redirs: &mut Vec<Redir>, node_tks: &mut Vec<Tk>) -> ShResult<()> {
    while self.check_redir() {
      let tk = self.next_tk().unwrap();
      node_tks.push(tk.clone());
      let redir_bldr = tk.span.as_str().parse::<RedirBldr>().unwrap();
      if redir_bldr.io_mode.is_none() {
        let path_tk = self.next_tk();

        if path_tk.clone().is_none_or(|tk| tk.class == TkRule::EOI) {
          return Err(ShErr::full(
            ShErrKind::ParseErr,
            "Expected a filename after this redirection",
            tk.span.clone(),
          ));
        };

        let path_tk = path_tk.unwrap();
        node_tks.push(path_tk.clone());
        let redir_class = redir_bldr.class.unwrap();
        let pathbuf = PathBuf::from(path_tk.span.as_str());

        let io_mode = IoMode::file(redir_bldr.tgt_fd.unwrap(), pathbuf, redir_class);
        let redir_bldr = redir_bldr.with_io_mode(io_mode);
        let redir = redir_bldr.build();
        redirs.push(redir);
      } else {
        // io_mode is already set (e.g., for fd redirections like 2>&1)
        let redir = redir_bldr.build();
        redirs.push(redir);
      }
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
    )
    .with_note(
      Note::new("Patterns can be raw text, or anything that gets substituted with raw text")
        .with_sub_notes(vec![
          "This includes variables like '$foo' or command substitutions like '$(echo foo)'",
        ]),
    );

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
        ));
      }
      let case_pat_tk = self.next_tk().unwrap();
      node_tks.push(case_pat_tk.clone());
      self.catch_separator(&mut node_tks);

      let mut nodes = vec![];
      while let Some(node) = self.parse_block(true /* check_pipelines */)? {
        node_tks.extend(node.tokens.clone());
        let sep = node.tokens.last().unwrap();
        if sep.has_double_semi() {
          nodes.push(node);
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
      let prefix_keywrd = if cond_nodes.is_empty() { "if" } else { "elif" };
      let Some(cond) = self.parse_cmd_list()? else {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          &format!("Expected an expression after '{prefix_keywrd}'"),
          &node_tks.get_span().unwrap(),
        ));
      };
      node_tks.extend(cond.tokens.clone());

      if !self.check_keyword("then") || !self.next_tk_is_some() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          &format!("Expected 'then' after '{prefix_keywrd}' condition"),
          &node_tks.get_span().unwrap(),
        ));
      }
      node_tks.push(self.next_tk().unwrap());
      self.catch_separator(&mut node_tks);

      let mut body_blocks = vec![];
      while let Some(body_block) = self.parse_block(true)? {
        node_tks.extend(body_block.tokens.clone());
        body_blocks.push(body_block);
      }
      if body_blocks.is_empty() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          "Expected an expression after 'then'",
          &node_tks.get_span().unwrap(),
        ));
      };
      let cond_node = CondNode {
        cond: Box::new(cond),
        body: body_blocks,
      };
      cond_nodes.push(cond_node);

      if !self.check_keyword("elif") || !self.next_tk_is_some() {
        break;
      } else {
        node_tks.push(self.next_tk().unwrap());
        self.catch_separator(&mut node_tks);
      }
    }

    if self.check_keyword("else") {
      node_tks.push(self.next_tk().unwrap());
      self.catch_separator(&mut node_tks);
      while let Some(block) = self.parse_block(true)? {
        else_block.push(block)
      }
      if else_block.is_empty() {
        self.panic_mode(&mut node_tks);
        return Err(parse_err_full(
          "Expected an expression after 'else'",
          &node_tks.get_span().unwrap(),
        ));
      }
    }

    if !self.check_keyword("fi") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Expected 'fi' after if statement",
        &node_tks.get_span().unwrap(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());

    self.parse_redir(&mut redirs, &mut node_tks)?;

    self.assert_separator(&mut node_tks)?;

    let node = Node {
      class: NdRule::IfNode {
        cond_nodes,
        else_block,
      },
      flags: NdFlags::empty(),
      redirs,
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
      ));
    }
    if arr.is_empty() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "This for loop is missing an array",
        &node_tks.get_span().unwrap(),
      ));
    }
    if !self.check_keyword("do") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Missing a 'do' for this for loop",
        &node_tks.get_span().unwrap(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());
    self.catch_separator(&mut node_tks);

    while let Some(node) = self.parse_block(true)? {
      body.push(node)
    }

    if !self.check_keyword("done") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Missing a 'done' after this for loop",
        &node_tks.get_span().unwrap(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());

    self.parse_redir(&mut redirs, &mut node_tks)?;

    let node = Node {
      class: NdRule::ForNode { vars, arr, body },
      flags: NdFlags::empty(),
      redirs,
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

    let Some(cond) = self.parse_cmd_list()? else {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        &format!("Expected an expression after '{loop_kind}'"), // It also implements Display
        &node_tks.get_span().unwrap(),
      ));
    };
    node_tks.extend(cond.tokens.clone());

    if !self.check_keyword("do") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Expected 'do' after loop condition",
        &node_tks.get_span().unwrap(),
      ));
    }
    node_tks.push(self.next_tk().unwrap());
    self.catch_separator(&mut node_tks);

    let mut body = vec![];
    while let Some(block) = self.parse_block(true)? {
      node_tks.extend(block.tokens.clone());
      body.push(block);
    }
    if body.is_empty() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Expected an expression after 'do'",
        &node_tks.get_span().unwrap(),
      ));
    };

    if !self.check_keyword("done") || !self.next_tk_is_some() {
      self.panic_mode(&mut node_tks);
      return Err(parse_err_full(
        "Expected 'done' after loop body",
        &node_tks.get_span().unwrap(),
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
      tokens: node_tks,
    };
    Ok(Some(loop_node))
  }
  fn parse_pipeln(&mut self) -> ShResult<Option<Node>> {
    let mut cmds = vec![];
    let mut node_tks = vec![];
    while let Some(cmd) = self.parse_block(false)? {
      let is_punctuated = node_is_punctuated(&cmd.tokens);
      node_tks.append(&mut cmd.tokens.clone());
      cmds.push(cmd);
      if *self.next_tk_class() != TkRule::Pipe || is_punctuated {
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
        class: NdRule::Pipeline {
          cmds,
          pipe_err: false,
        },
        flags: NdFlags::empty(),
        redirs: vec![],
        tokens: node_tks,
      }))
    }
  }
  fn parse_cmd(&mut self) -> ShResult<Option<Node>> {
    let tk_slice = self.tokens.clone();
    let mut tk_iter = tk_slice.iter();
    let mut node_tks = vec![];
    let mut redirs = vec![];
    let mut argv = vec![];
    let mut assignments = vec![];

    while let Some(prefix_tk) = tk_iter.next() {
      if let TkRule::CasePattern = prefix_tk.class {
        return Err(parse_err_full(
          "Found case pattern in command",
          &prefix_tk.span,
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
				return Ok(None);
			} else {
				// If we have assignments but no command word,
				// return the assignment-only command without parsing more tokens
				self.commit(node_tks.len());
				return Ok(Some(Node {
					class: NdRule::Command { assignments, argv },
					tokens: node_tks,
					flags: NdFlags::empty(),
					redirs,
				}));
			}
    }

    while let Some(tk) = tk_iter.next() {
      match tk.class {
        TkRule::EOI | TkRule::Pipe | TkRule::And | TkRule::BraceGrpEnd | TkRule::Or => break,
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
          let redir_bldr = tk.span.as_str().parse::<RedirBldr>().unwrap();
          if redir_bldr.io_mode.is_none() {
            let path_tk = tk_iter.next();

            if path_tk.is_none_or(|tk| tk.class == TkRule::EOI) {
              return Err(ShErr::full(
                ShErrKind::ParseErr,
                "Expected a filename after this redirection",
                tk.span.clone(),
              ));
            };

            let path_tk = path_tk.unwrap();
            node_tks.push(path_tk.clone());
            let redir_class = redir_bldr.class.unwrap();
            let pathbuf = PathBuf::from(path_tk.span.as_str());

            let io_mode = IoMode::file(redir_bldr.tgt_fd.unwrap(), pathbuf, redir_class);
            let redir_bldr = redir_bldr.with_io_mode(io_mode);
            let redir = redir_bldr.build();
            redirs.push(redir);
          } else {
            // io_mode is already set (e.g., for fd redirections like 2>&1)
            let redir = redir_bldr.build();
            redirs.push(redir);
          }
        }
        _ => unimplemented!("Unexpected token rule `{:?}` in parse_cmd()", tk.class),
      }
    }
    self.commit(node_tks.len());

    Ok(Some(Node {
      class: NdRule::Command { assignments, argv },
      tokens: node_tks,
      flags: NdFlags::empty(),
      redirs,
    }))
  }
  fn parse_assignment(&self, token: &Tk) -> Option<Node> {
    let mut chars = token.span.as_str().chars();
    let mut var_name = String::new();
    let mut name_range = token.span.start..token.span.start;
    let mut var_val = String::new();
    let mut val_range = token.span.end..token.span.end;
    let mut assign_kind = None;
    let mut pos = token.span.start;

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
    if assign_kind.is_none() || var_name.is_empty() {
      None
    } else {
      let var = Tk::new(TkRule::Str, Span::new(name_range, token.source()));
      let val = Tk::new(TkRule::Str, Span::new(val_range, token.source()));
      Some(Node {
        class: NdRule::Assignment {
          kind: assign_kind.unwrap(),
          var,
          val,
        },
        tokens: vec![token.clone()],
        flags: NdFlags::empty(),
        redirs: vec![],
      })
    }
  }
}

impl Iterator for ParseStream {
  type Item = ShResult<Node>;
  fn next(&mut self) -> Option<Self::Item> {
    // Empty token vector or only SOI/EOI tokens, nothing to do
    if self.tokens.is_empty() || self.tokens.len() == 1 {
      return None;
    }
    while let Some(tk) = self.tokens.first() {
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
      Err(e) => Some(Err(e)),
    }
  }
}

fn node_is_punctuated(tokens: &[Tk]) -> bool {
  tokens
    .last()
    .is_some_and(|tk| matches!(tk.class, TkRule::Sep))
}

pub fn get_redir_file(class: RedirType, path: PathBuf) -> ShResult<File> {
  let result = match class {
    RedirType::Input => OpenOptions::new().read(true).open(Path::new(&path)),
    RedirType::Output => OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .open(Path::new(&path)),
    RedirType::Append => OpenOptions::new()
      .create(true)
      .append(true)
      .open(Path::new(&path)),
    _ => unimplemented!(),
  };
  Ok(result?)
}

fn parse_err_full(reason: &str, blame: &Span) -> ShErr {
  ShErr::full(ShErrKind::ParseErr, reason, blame.clone())
}

fn is_func_name(tk: Option<&Tk>) -> bool {
  tk.is_some_and(|tk| {
    tk.flags.contains(TkFlags::KEYWORD)
      && (tk.span.as_str().ends_with("()") && !tk.span.as_str().ends_with("\\()"))
  })
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
    NdRule::Pipeline {
      ref mut cmds,
      pipe_err: _,
    } => {
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
    NdRule::Test { cases: _ } => (),
  }
}
