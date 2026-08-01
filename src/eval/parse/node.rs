use std::{collections::VecDeque, rc::Rc, string::ToString};

use crate::{
  ShResult, Shed,
  eval::execute::{is_builtin, is_func_node},
  state::logic::{AutoloadKind, IsInternal, ShFunc},
  util::error::LabelBuilder,
};

use super::{
  lex::{Span, Tk},
  procio::RedirSpec,
  two_way_display,
};
use ariadne::Span as AriadneSpan;
use bitflags::bitflags;

pub(crate) trait NodeVecUtils<Node> {
  fn get_span(&self) -> Option<Span>;
}

impl NodeVecUtils<Node> for Vec<Node> {
  fn get_span(&self) -> Option<Span> {
    if let Some(first_nd) = self.first()
      && let Some(last_nd) = self.last()
    {
      let first_start = first_nd.get_span().range().start;
      let last_end = last_nd.get_span().range().end;
      if first_start <= last_end {
        return Some(Span::new(
          first_start..last_end,
          first_nd.get_span().source().content(),
        ));
      }
    }
    None
  }
}

#[derive(Clone, Debug, Default)]
pub struct LabelCtx(Rc<VecDeque<LabelBuilder>>);

impl LabelCtx {
  pub fn push_back(&mut self, label: LabelBuilder) {
    Rc::make_mut(&mut self.0).push_back(label);
  }

  pub fn iter(&self) -> impl Iterator<Item = &LabelBuilder> {
    self.0.iter()
  }
}

impl From<VecDeque<LabelBuilder>> for LabelCtx {
  fn from(queue: VecDeque<LabelBuilder>) -> Self {
    LabelCtx(Rc::new(queue))
  }
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
  pub class: NdRule,
  pub flags: NdFlags,
  pub redirs: Vec<RedirSpec>,
  pub span: Span,
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
  pub fn walk_tree<F: FnMut(&mut Node)>(&mut self, f: &mut F) {
    f(self);

    match self.class {
      NdRule::List { ref mut commands } => {
        for cmd in commands {
          cmd.walk_tree(f);
        }
      }
      NdRule::IfNode {
        ref mut cond_nodes,
        ref mut else_block,
      } => {
        for node in cond_nodes {
          let CondNode { cond, body } = node;
          cond.walk_tree(f);
          body.walk_tree(f);
        }

        if let Some(block) = else_block {
          block.walk_tree(f);
        }
      }
      NdRule::LoopNode {
        kind: _,
        ref mut cond_node,
      } => {
        let CondNode { cond, body } = cond_node;
        cond.walk_tree(f);
        body.walk_tree(f);
      }
      NdRule::ForNode { ref mut body, .. }
      | NdRule::TryNode { ref mut body, .. }
      | NdRule::FuncDef { ref mut body, .. }
      | NdRule::DeferNode { ref mut body }
      | NdRule::Subshell { ref mut body }
      | NdRule::BraceGrp { ref mut body } => {
        body.walk_tree(f);
      }
      NdRule::ForArith {
        ref mut init,
        ref mut cond,
        ref mut step,
        ref mut body,
      } => {
        if let Some(init) = init {
          init.walk_tree(f);
        }
        if let Some(cond) = cond {
          cond.walk_tree(f);
        }
        if let Some(step) = step {
          step.walk_tree(f);
        }
        body.walk_tree(f);
      }
      NdRule::CaseNode {
        pattern: _,
        ref mut case_blocks,
      } => {
        for block in case_blocks {
          let CaseNode { patterns: _, body } = block;
          body.walk_tree(f);
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
      NdRule::Timed { ref mut cmd } | NdRule::Negate { ref mut cmd } => {
        cmd.walk_tree(f);
      }
      NdRule::Arithmetic { .. } | NdRule::Assignment { .. } => (), // No nodes to check
    }
  }
  pub fn eager_expand(&mut self) -> ShResult<()> {
    let expand_tk = |tk: &mut Tk| -> ShResult<()> {
      *tk = std::mem::take(tk).expand()?;
      Ok(())
    };

    match &mut self.class {
      NdRule::Command { argv: tks, .. } | NdRule::ForNode { arr: tks, .. } => {
        for tk in tks {
          expand_tk(tk)?;
        }
      }
      NdRule::Assignment { val: tk, .. }
      | NdRule::CaseNode { pattern: tk, .. }
      | NdRule::Arithmetic { body: tk } => {
        expand_tk(tk)?;
      }

      _ => {}
    }

    Ok(())
  }
  /// Mark this node as exempt from `set -e`
  ///
  /// Unless it is already marked as `IS_ERR`, in which case do nothing
  pub fn not_err(&mut self) {
    if !self.flags.contains(NdFlags::IS_ERR) {
      self.flags.insert(NdFlags::NOT_ERR);
    }
  }
  /// Mark this node as exempt from `set -e` exemptions.
  ///
  /// Unless it is already marked as `NOT_ERR`, in which case do nothing
  ///
  /// This is used for `try` blocks to force `set -e` to propagate their errors
  /// even when `try` is used in a context that is exempt from them, like a `catch` block.
  pub fn is_err(&mut self) {
    if !self.flags.contains(NdFlags::NOT_ERR) {
      self.flags.insert(NdFlags::IS_ERR);
    }
  }
  pub fn propagate_context(&mut self, ctx: &LabelBuilder) {
    self.walk_tree(&mut |nd| {
      nd.context.push_back(ctx.clone());
    });
  }
  pub fn get_span(&self) -> Span {
    self.span.clone()
  }
}

bitflags! {
  /// Bitfield containing miscellaneous info about a node
  ///
  /// This info is consumed by the parser and dispatcher
  #[derive(Clone,Copy,Debug)]
  pub struct NdFlags: u32 {
    const BACKGROUND    = 1 << 0;
    const FORK_BUILTINS = 1 << 1;
    const NO_FORK       = 1 << 2;
    const ARR_ASSIGN    = 1 << 3;
    const PIPE_ERR      = 1 << 4; // whether to include stderr in a pipe
    const NOT_ERR       = 1 << 5; // don't trigger ERR traps and set -e
    const IS_ERR        = 1 << 6; // force trigger ERR traps and set -e
    const PIPE_CMD      = 1 << 7; // is not the last command in a pipeline
    const NO_SPLIT      = 1 << 8; // don't split words, used in double bracket tests ('[[')
    const PUNCTUATED    = 1 << 9; // ends with a separator
  }
}

/// A conditional AST node
///
/// Used in `while`/`until`/`if` conditions
#[derive(Clone, Debug)]
pub(crate) struct CondNode {
  pub cond: Box<Node>,
  pub body: Box<Node>,
}

/// A case block AST node
///
/// Used in `case` statements
#[derive(Clone, Debug)]
pub(crate) struct CaseNode {
  pub patterns: Vec<Tk>,
  pub body: Box<Node>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ConjunctOp {
  And,
  Or,
  Null,
}

#[derive(Clone, Debug)]
pub(crate) struct ConjunctNode {
  pub cmd: Box<Node>,
  pub operator: ConjunctOp,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LoopKind {
  While,
  Until,
}

two_way_display!(LoopKind,
  While <=> "while";
  Until <=> "until";
);

#[derive(Clone, Debug)]
pub(crate) enum AssignKind {
  Eq,
  PlusEq,
  MinusEq,
  MultEq,
  DivEq,
}

/// Flat `NdRule` names used mainly for debugging
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NdKind {
  List,
  IfNode,
  LoopNode,
  ForNode,
  ForArith,
  Arithmetic,
  CaseNode,
  TryNode,
  DeferNode,
  Command,
  Pipeline,
  Conjunction,
  Assignment,
  BraceGrp,
  Subsh,
  Negate,
  Timed,
  FuncDef,
}

impl NdRule {
  pub fn as_nd_kind(&self) -> NdKind {
    match self {
      Self::List { .. } => NdKind::List,
      Self::Negate { .. } => NdKind::Negate,
      Self::IfNode { .. } => NdKind::IfNode,
      Self::LoopNode { .. } => NdKind::LoopNode,
      Self::ForNode { .. } => NdKind::ForNode,
      Self::TryNode { .. } => NdKind::TryNode,
      Self::DeferNode { .. } => NdKind::DeferNode,
      Self::ForArith { .. } => NdKind::ForArith,
      Self::Arithmetic { .. } => NdKind::Arithmetic,
      Self::CaseNode { .. } => NdKind::CaseNode,
      Self::Command { .. } => NdKind::Command,
      Self::Pipeline { .. } => NdKind::Pipeline,
      Self::Conjunction { .. } => NdKind::Conjunction,
      Self::Assignment { .. } => NdKind::Assignment,
      Self::Timed { .. } => NdKind::Timed,
      Self::BraceGrp { .. } => NdKind::BraceGrp,
      Self::FuncDef { .. } => NdKind::FuncDef,
      Self::Subshell { .. } => NdKind::Subsh,
    }
  }
}

/// The various types of AST nodes
///
/// A load-bearing component of `shed`'s execution logic.
/// Each member contains all of the data required to execute the associated type of statement
#[derive(Clone, Debug)]
pub(crate) enum NdRule {
  List {
    commands: Vec<Node>,
  },
  IfNode {
    cond_nodes: Vec<CondNode>,
    else_block: Option<Box<Node>>,
  },
  LoopNode {
    kind: LoopKind,
    cond_node: CondNode,
  },
  ForNode {
    vars: Vec<Tk>,
    arr: Vec<Tk>,
    body: Box<Node>,
    positional: bool, // true if no "in" keyword is passed to the for loop
  },
  TryNode {
    body: Box<Node>,
    err: Vec<Tk>,
    catch: Option<Box<Node>>,
  },
  DeferNode {
    body: Box<Node>,
  },
  ForArith {
    init: Option<Box<Node>>,
    cond: Option<Box<Node>>,
    step: Option<Box<Node>>,
    body: Box<Node>,
  },
  Arithmetic {
    body: Tk,
  },
  Negate {
    cmd: Box<Node>,
  },
  Timed {
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
  Subshell {
    body: Box<Node>,
  },
  BraceGrp {
    body: Box<Node>,
  },
  FuncDef {
    name: Tk,
    body: Box<Node>,
  },
}

pub(crate) fn node_has_only_builtins(node: &mut Node) -> bool {
  let mut res = None;
  node.walk_tree(&mut |node| {
    if let Some(false) = res {
      return;
    }

    if !node.redirs.is_empty() {
      res = Some(false);
      return;
    }

    if node
      .flags
      .contains(NdFlags::BACKGROUND | NdFlags::FORK_BUILTINS)
    {
      res = Some(false);
      return;
    }

    match &node.class {
      NdRule::Command { .. } => {
        if !is_func_node(node) {
          res = Some(is_builtin(node));
          return;
        }
        let name = node.get_command().map(ToString::to_string).unwrap();

        // Caller is about to execute this anyway (cmd sub, pipeline, etc),
        // so source the autoload now while we have the chance.
        let autoload_src = Shed::logic_mut(|l| {
          if let Some(ShFunc::Autoload(_)) = l.get_func_ref(&name) {
            let func = l.remove_func(&name)?;
            if let ShFunc::Autoload(src) = func {
              return Some(src);
            }
          }
          None
        });

        if let Some(src) = autoload_src
          && src.source(AutoloadKind::Function).is_err()
        {
          res = Some(false);
          return;
        }

        let short_circuit = Shed::logic(|l| {
          let Some(func) = l.get_func_ref(&name) else {
            return Some(false);
          };

          match func {
            ShFunc::Defined { is_internal, .. } => match is_internal {
              Some(IsInternal::No) => Some(false),
              Some(IsInternal::Yes | IsInternal::Checking) => Some(true),
              None => None,
            },
            ShFunc::Autoload(_) => Some(false),
          }
        });

        if let Some(verdict) = short_circuit {
          res = Some(verdict);
          return;
        }

        // Cache miss: function exists, is Defined, is_internal is None.
        // Mark Checking and clone the body in a single borrow.
        let Some(mut logic) = Shed::logic_mut(|l| match l.get_func_mut(&name) {
          Some(ShFunc::Defined {
            logic, is_internal, ..
          }) => {
            *is_internal = Some(IsInternal::Checking);
            Some(logic.clone())
          }
          _ => None,
        }) else {
          return;
        };

        let is = node_has_only_builtins(&mut logic);
        let verdict = if is { IsInternal::Yes } else { IsInternal::No };
        Shed::logic_mut(|l| {
          if let Some(func) = l.get_func_mut(&name) {
            func.set_is_internal(verdict).ok();
          }
        });
        res = Some(is);
      }
      _ => res = Some(true),
    }
  });

  res.unwrap_or(false)
}

pub(crate) fn nodes_have_only_builtins<'a>(nodes: impl Iterator<Item = &'a mut Node>) -> bool {
  for node in nodes {
    if !node_has_only_builtins(node) {
      return false;
    }
  }

  true
}
