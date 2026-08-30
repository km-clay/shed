use std::{collections::VecDeque, rc::Rc};

use crate::{
  Shed,
  eval::{
    execute::{is_builtin, is_func_node},
    parse::{
      Ast,
      ast::{
        CaseNodeRange, ChildRange, CondNodeId, CondNodeRange, ConjunctRange, LabelId, LabelRange,
        RedirRange, SpanId, TkId, TkRange,
      },
    },
  },
  expand::subshell,
  state::logic::{IsInternal, ShFunc},
  util::error::LabelBuilder,
};

use super::{ast::NodeId, lex::Tk, two_way_display};
use bitflags::bitflags;

#[derive(Clone, Debug, Default)]
pub struct LabelCtx(Rc<VecDeque<LabelBuilder>>);

impl LabelCtx {
  pub fn iter(&self) -> impl Iterator<Item = &LabelBuilder> {
    self.0.iter()
  }
  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
  pub fn into_iter(self) -> impl Iterator<Item = LabelBuilder> {
    Rc::try_unwrap(self.0)
      .unwrap_or_else(|rc| (*rc).clone())
      .into_iter()
  }
}

impl From<VecDeque<LabelBuilder>> for LabelCtx {
  fn from(queue: VecDeque<LabelBuilder>) -> Self {
    LabelCtx(Rc::new(queue))
  }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Node {
  pub class: NdRule,
  pub flags: NdFlags,
  pub redirs: Option<RedirRange>,
  pub span: SpanId,
  pub context: Option<LabelRange>,
}

impl Node {
  pub fn get_command(&self) -> Option<TkId> {
    let NdRule::Command {
      assignments: _,
      argv,
    } = &self.class
    else {
      return None;
    };
    argv.first()
  }
  pub fn redirs_empty(&self) -> bool {
    self.redirs.is_none_or(RedirRange::is_empty)
  }
  /// Mark this node as exempt from `set -e`
  ///
  /// Unless it is already marked as `IS_ERR`, in which case do nothing
  pub fn not_err(&mut self) {
    if !self.flags.contains(NdFlags::IS_ERR) {
      self.flags.insert(NdFlags::NOT_ERR);
    }
  }
  pub fn is_err(&mut self) {
    if !self.flags.contains(NdFlags::NOT_ERR) {
      self.flags.insert(NdFlags::IS_ERR);
    }
  }
  pub fn get_span(&self) -> SpanId {
    self.span
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
    const NO_TRACE      = 1 << 10;// no set -x trace output
  }
}

/// A conditional AST node
///
/// Used in `while`/`until`/`if` conditions
#[derive(Copy, Clone, Debug)]
pub(crate) struct CondNode {
  pub cond: NodeId,
  pub body: NodeId,
}

/// A case block AST node
///
/// Used in `case` statements
#[derive(Clone, Debug)]
pub(crate) struct CaseNode {
  pub patterns: Vec<Tk>,
  pub body: NodeId,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ConjunctOp {
  And,
  Or,
  Null,
}

#[derive(Clone, Debug)]
pub(crate) struct ConjunctNode {
  pub cmd: NodeId,
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

#[derive(Clone, Copy, Debug)]
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
#[derive(Clone, Copy, Debug)]
pub(crate) enum NdRule {
  List {
    commands: ChildRange,
  },
  IfNode {
    cond_nodes: CondNodeRange,
    else_block: Option<NodeId>,
  },
  LoopNode {
    kind: LoopKind,
    cond_node: CondNodeId,
  },
  ForNode {
    vars: TkRange,
    arr: TkRange,
    body: NodeId,
    positional: bool, // true if no "in" keyword is passed to the for loop
  },
  TryNode {
    body: NodeId,
    err: TkRange,
    catch: Option<NodeId>,
    ctx: LabelId,
  },
  DeferNode {
    body: NodeId,
    ctx: LabelId,
  },
  ForArith {
    init: Option<NodeId>,
    cond: Option<NodeId>,
    step: Option<NodeId>,
    body: NodeId,
  },
  Arithmetic {
    body: TkId,
  },
  Negate {
    cmd: NodeId,
  },
  Timed {
    cmd: NodeId,
  },
  CaseNode {
    pattern: TkId,
    case_blocks: CaseNodeRange,
  },
  Command {
    assignments: ChildRange,
    argv: TkRange,
  },
  Pipeline {
    cmds: ChildRange,
  },
  Conjunction {
    elements: ConjunctRange,
  },
  Assignment {
    kind: AssignKind,
    var: TkId,
    val: TkId,
  },
  Subshell {
    body: NodeId,
  },
  BraceGrp {
    body: NodeId,
  },
  FuncDef {
    name: TkId,
    body: NodeId,
    ctx: LabelId,
  },
}

pub(crate) fn node_has_only_builtins(tree: &Ast, node_id: NodeId) -> bool {
  let mut res = None;
  tree.walk_tree(node_id, &mut |id, tree| {
    let node = &tree[id];
    if let Some(false) = res {
      return;
    }

    if node.redirs.is_some_and(|r| !r.is_empty()) {
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
      NdRule::Command { argv, .. } => {
        if argv.is_empty() {
          // assignment-only command (e.g. `a=1`); runs in-process, never forks
          res = Some(true);
          return;
        }
        if !is_func_node(id, tree) {
          res = Some(is_builtin(id, tree));
          return;
        }
        let name = node.get_command().unwrap();

        // Caller is about to execute this anyway (cmd sub, pipeline, etc),
        // so source the autoload now while we have the chance.
        let autoload_src = Shed::logic_mut(|l| {
          if let Some(ShFunc::Autoload(_)) = l.get_func_ref(&tree[name].to_str_lossy()) {
            let func = l.remove_func(&tree[name].to_str_lossy())?;
            if let ShFunc::Autoload(src) = func {
              return Some(src);
            }
          }
          None
        });

        if let Some(src) = autoload_src
          && src.source().is_err()
        {
          res = Some(false);
          return;
        }

        let short_circuit = Shed::logic(|l| {
          let Some(func) = l.get_func_ref(&tree[name].to_str_lossy()) else {
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
        let Some(logic) = Shed::logic_mut(|l| match l.get_func_mut(&tree[name].to_str_lossy()) {
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
        let Some(root) = logic.get_root() else {
          res = Some(false);
          return;
        };

        let body_src = logic.span_for(root);
        let is = subshell::is_internal(&body_src.to_str_lossy());
        let verdict = if is { IsInternal::Yes } else { IsInternal::No };
        Shed::logic_mut(|l| {
          if let Some(func) = l.get_func_mut(&tree[name].to_str_lossy()) {
            func.set_is_internal(verdict).ok();
          }
        });
        res = Some(is);
      }
      NdRule::Subshell { .. } => res = Some(false),
      _ => res = Some(true),
    }
  });

  res.unwrap_or(false)
}

pub(crate) fn nodes_have_only_builtins(tree: &Ast, nodes: impl Iterator<Item = NodeId>) -> bool {
  for node in nodes {
    if !node_has_only_builtins(tree, node) {
      return false;
    }
  }

  true
}
