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
  readline::{NestedSub, nested_subs},
  state::{
    ForkBlame, Shed,
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

fn contains_sub_intro(src: &[u8]) -> bool {
  src.contains(&b'`')
    || src
      .windows(2)
      .any(|w| w[1] == b'(' && matches!(w[0], b'$' | b'<' | b'>'))
}

enum ForkReport {
  Simple(bool),
  Detailed(Vec<ForkBlame>),
}

pub(crate) fn node_forks(tree: &Ast, node_id: NodeId) -> bool {
  let ForkReport::Simple(forks) = node_fork_behavior(tree, node_id, true) else {
    unreachable!()
  };

  forks
}

pub(crate) fn node_fork_report(tree: &Ast, node_id: NodeId) -> Vec<ForkBlame> {
  let ForkReport::Detailed(rep) = node_fork_behavior(tree, node_id, false) else {
    unreachable!()
  };

  rep
}

fn node_fork_behavior(tree: &Ast, node_id: NodeId, simple: bool) -> ForkReport {
  let src = tree.span_for(node_id).slice();
  let mut acc = vec![];
  let mut has_fork = false;

  macro_rules! blame {
    ($blame:expr) => {{
      if let ForkBehavior::Always = $blame.behavior() {
        has_fork = true;
      }

      if simple && has_fork {
        return;
      }
      if !simple {
        acc.push($blame)
      };
    }};
  }

  if contains_sub_intro(src.as_bytes()) {
    for sub in nested_subs(src.as_bytes()) {
      match sub {
        NestedSub::Proc(span) => {
          if simple {
            return ForkReport::Simple(true);
          }
          acc.push(ForkBlame::proc_sub(span, ForkBehavior::Always));
        }
        NestedSub::Cmd(span, body) => {
          let is_internal = subshell::is_internal(body.as_bytes());

          if simple && !is_internal {
            return ForkReport::Simple(true);
          }

          let behavior = if is_internal {
            ForkBehavior::Never
          } else {
            ForkBehavior::Always
          };

          acc.push(ForkBlame::command_sub(span, behavior));
        }
      }
    }
  }

  tree.walk_tree(node_id, &mut |id, tree| {
    let node = &tree[id];
    let span = tree.span_for(id);
    if simple && has_fork {
      return;
    }

    if node
      .flags
      .contains(NdFlags::BACKGROUND | NdFlags::FORK_BUILTINS)
    {
      acc.push(ForkBlame::external(span));
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
              .and_then(|name| fork_behavior_for(tree[name].slice().as_bytes()))
              .unwrap_or(ForkBehavior::Never);

            let behavior = match behavior {
              ForkBehavior::Subshell if node.flags.contains(NdFlags::PIPE_CMD) => {
                ForkBehavior::Always
              }
              ForkBehavior::Subshell => ForkBehavior::Never,
              other => other,
            };

            blame!(ForkBlame::builtin(span, behavior));
          } else {
            // external or something? assume we fork for it
            blame!(ForkBlame::external(span));
          }
          return;
        }

        // if we are here, we are dealing with a function (the complicated case)
        // now we have to traverse the AST of the function and check all of its nodes,
        // even the stuff in command subs
        let name = node.get_command().unwrap();
        let func_name = tree[name].slice();

        // Caller is about to execute this anyway (cmd sub, pipeline, etc),
        // so source the autoload now while we have the chance.
        let autoload_src = Shed::logic_mut(|l| {
          if let Some(ShFunc::Autoload(_)) = l.get_func_ref(&func_name.to_str_lossy()) {
            let func = l.remove_func(&func_name.to_str_lossy())?;
            if let ShFunc::Autoload(src) = func {
              return Some(src);
            }
          }
          None
        });

        if let Some(src) = autoload_src
          && src.source().is_err()
        {
          // failed to source; we read this as a command
          blame!(ForkBlame::function(span, ForkBehavior::Always));
          return;
        }

        // Cached verdict: outer None means cache miss (compute below); outer
        // Some(inner) short-circuits, with inner None = not internal.
        let cached = Shed::logic(|l| {
          let Some(func) = l.get_func_ref(&func_name.to_str_lossy()) else {
            return Some(None);
          };

          match func {
            ShFunc::Defined { is_internal, .. } => match is_internal {
              Some(IsInternal::Resolved(b)) => Some(Some(*b)),
              Some(IsInternal::Checking) => Some(Some(ForkBehavior::Never)),
              None => None,
            },
            ShFunc::Autoload(_) => Some(None),
          }
        });

        if let Some(verdict) = cached {
          if let Some(b) = verdict {
            blame!(ForkBlame::command(span, b));
          } else {
            blame!(ForkBlame::external(span));
          }
          return;
        }

        // Cache miss: function exists, is Defined, is_internal is None.
        // Mark Checking and clone the body in a single borrow.
        let Some(logic) = Shed::logic_mut(|l| match l.get_func_mut(&func_name.to_str_lossy()) {
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
          // this should basically always return something. if not
          // we can just return. There's nothing to check
          return;
        };

        let body_src = logic.span_for(root).slice();
        let behavior = if subshell::is_internal(body_src.as_bytes()) {
          ForkBehavior::Never
        } else {
          ForkBehavior::Always
        };
        Shed::logic_mut(|l| {
          if let Some(func) = l.get_func_mut(&func_name.to_str_lossy()) {
            func.set_is_internal(IsInternal::Resolved(behavior)).ok();
          }
        });
        blame!(ForkBlame::command(span, behavior));
      }
      NdRule::Subshell { .. } => blame!(ForkBlame::subshell(span, ForkBehavior::Always)),
      _ => {}
    }
  });

  if simple {
    ForkReport::Simple(has_fork)
  } else {
    ForkReport::Detailed(acc)
  }
}
