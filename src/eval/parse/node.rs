//! AST nodes and associated data structures
//!
//! This module contains the definitions of the various AST node types used in `shed`'s execution logic, as well as
//! associated data structures and utility functions.
//!
//! The [`Node`] type itself is just a bag of ids/indices used to index the instance of [`Ast`](super::ast::Ast) that it
//! lives inside of. This means that individual [`Node`]s are basically free to clone and pass around, since they don't
//! contain any actual data themselves. The actual data is stored in the [`Ast`] instance, which contains flat vectors of all
//! of the data types used in the AST.

use std::{collections::VecDeque, rc::Rc};

use super::{
  super::execute::classify,
  ast::{
    Ast, CaseNodeRange, ChildRange, CondNodeId, CondNodeRange, ConjunctRange, LabelId, LabelRange,
    RedirRange, SpanId, TkId, TkRange,
  },
};

use crate::{
  builtin::{ForkBehavior, fork_behavior_for},
  expand::subshell,
  state::{
    Shed,
    logic::{IsInternal, ShFunc},
  },
  util::error::LabelBuilder,
};

use super::{ast::NodeId, lex::Tk, two_way_display};
use bitflags::bitflags;

#[derive(Clone, Debug, Default)]
pub(crate) struct LabelCtx(Rc<VecDeque<LabelBuilder>>);

impl LabelCtx {
  pub(crate) fn iter(&self) -> impl Iterator<Item = &LabelBuilder> {
    self.0.iter()
  }
  pub(crate) fn is_empty(&self) -> bool {
    self.0.is_empty()
  }
  pub(crate) fn into_iter(self) -> impl Iterator<Item = LabelBuilder> {
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
  pub(crate) fn get_command(&self) -> Option<TkId> {
    let NdRule::Command {
      assignments: _,
      argv,
    } = &self.class
    else {
      return None;
    };
    argv.first()
  }
  /// Mark this node as exempt from `set -e`
  ///
  /// Unless it is already marked as `IS_ERR`, in which case do nothing
  pub(crate) fn not_err(&mut self) {
    if !self.flags.contains(NdFlags::IS_ERR) {
      self.flags.insert(NdFlags::NOT_ERR);
    }
  }
  pub(crate) fn is_err(&mut self) {
    if !self.flags.contains(NdFlags::NOT_ERR) {
      self.flags.insert(NdFlags::IS_ERR);
    }
  }
  pub(crate) fn get_span(&self) -> SpanId {
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
  pub(crate) fn as_nd_kind(&self) -> NdKind {
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

pub(crate) fn node_fork_behavior(tree: &Ast, node_id: NodeId) -> Option<ForkBehavior> {
  let mut acc: Option<ForkBehavior> = Some(ForkBehavior::Never);
  tree.walk_tree(node_id, &mut |id, tree| {
    let node = &tree[id];
    if acc.is_none() {
      return;
    }

    if node.redirs.is_some_and(|r| !r.is_empty()) {
      acc = None;
      return;
    }

    if node
      .flags
      .contains(NdFlags::BACKGROUND | NdFlags::FORK_BUILTINS)
    {
      acc = None;
      return;
    }

    match &node.class {
      NdRule::Command { argv, .. } => {
        if argv.is_empty() {
          // assignment-only command (e.g. `a=1`); runs in-process, never forks
          return;
        }
        if !classify::is_func_node(id, tree) {
          if classify::is_builtin(id, tree) {
            let behavior = node
              .get_command()
              .and_then(|name| fork_behavior_for(tree[name].as_bytes()))
              .unwrap_or(ForkBehavior::Never);
            acc = acc.map(|cur| cur.max(behavior));
          } else {
            acc = None;
          }
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
          acc = None;
          return;
        }

        // Cached verdict: outer None means cache miss (compute below); outer
        // Some(inner) short-circuits, with inner None = not internal.
        let cached = Shed::logic(|l| {
          let Some(func) = l.get_func_ref(&tree[name].to_str_lossy()) else {
            return Some(None);
          };

          match func {
            ShFunc::Defined { is_internal, .. } => match is_internal {
              Some(IsInternal::No) => Some(None),
              Some(IsInternal::Yes(b)) => Some(Some(*b)),
              Some(IsInternal::Checking) => Some(Some(ForkBehavior::Never)),
              None => None,
            },
            ShFunc::Autoload(_) => Some(None),
          }
        });

        if let Some(verdict) = cached {
          match verdict {
            Some(b) => acc = acc.map(|cur| cur.max(b)),
            None => acc = None,
          }
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
          acc = None;
          return;
        };

        let body_src = logic.span_for(root);
        let behavior = subshell::is_internal(&body_src.to_str_lossy());
        let verdict = match behavior {
          Some(b) => IsInternal::Yes(b),
          None => IsInternal::No,
        };
        Shed::logic_mut(|l| {
          if let Some(func) = l.get_func_mut(&tree[name].to_str_lossy()) {
            func.set_is_internal(verdict).ok();
          }
        });
        match behavior {
          Some(b) => acc = acc.map(|cur| cur.max(b)),
          None => acc = None,
        }
      }
      NdRule::Subshell { .. } => acc = None,
      _ => {}
    }
  });

  acc
}

pub(crate) fn node_has_only_builtins(tree: &Ast, node_id: NodeId) -> bool {
  node_fork_behavior(tree, node_id).is_some()
}
