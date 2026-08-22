use std::{
  collections::VecDeque,
  ops::{Index, IndexMut},
  rc::Rc,
};

use crate::{
  ShResult, Shed,
  eval::{
    execute::{is_builtin, is_func_node},
    parse::Ast,
  },
  expand::subshell,
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
use smallvec::SmallVec;

pub(crate) trait NodeVecUtils<Node> {
  fn get_span(&self, tree: &Ast) -> Option<Span>;
}

impl NodeVecUtils<Node> for Vec<NodeId> {
  fn get_span(&self, tree: &Ast) -> Option<Span> {
    if let Some(first_nd) = self.first()
      && let Some(last_nd) = self.last()
    {
      let first_start = tree[*first_nd].get_span().range().start;
      let last_end = tree[*last_nd].get_span().range().end;
      if first_start <= last_end {
        return Some(Span::new(
          first_start..last_end,
          tree[*first_nd].get_span().source().content(),
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

/// An index into an instance of `Ast`.
///
/// Contains the actual id of the `Node` itself, and the id of the `Ast` that it refers to.
/// Attempting to index any instance of `Ast` with a `NodeId` that does not match its own id will panic.
///
/// `NodeId` cannot be constructed outside of this module, and cannot be mutated once created. Because `Ast` is also immutable, this means that using a `NodeId` to index an `Ast` is guaranteed to be safe, as long as the `NodeId` was
/// created by the same `Ast` instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NodeId {
  node_id: u32,
  ast_id: u32,
}

impl NodeId {
  pub(super) fn new(node_id: u32, ast_id: u32) -> Self {
    NodeId { node_id, ast_id }
  }
}

impl Index<NodeId> for super::Ast {
  type Output = Node;
  fn index(&self, index: NodeId) -> &Self::Output {
    assert_eq!(index.ast_id, self.id, "NodeId does not match Ast");
    &self.arena[index.node_id as usize]
  }
}

impl IndexMut<NodeId> for super::Ast {
  fn index_mut(&mut self, index: NodeId) -> &mut Self::Output {
    assert_eq!(index.ast_id, self.id, "NodeId does not match Ast");
    &mut self.arena[index.node_id as usize]
  }
}

impl Ast {
  pub fn walk_tree<F: FnMut(&Node)>(&self, id: NodeId, f: &mut F) {
    f(&self[id]);
    let children = self[id].child_ids();
    for child in children {
      self.walk_tree(child, f);
    }
  }
  pub fn walk_tree_mut<F: FnMut(&mut Node)>(&mut self, id: NodeId, f: &mut F) {
    f(&mut self[id]);
    let children = self[id].child_ids();
    for child in children {
      self.walk_tree_mut(child, f);
    }
  }
  pub fn propagate_context(&mut self, id: NodeId, ctx: &LabelBuilder) {
    self.walk_tree_mut(id, &mut |nd| {
      nd.context.push_back(ctx.clone());
    });
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
  /// Collect this node's direct child ids into an owned buffer, so callers can
  /// drop the borrow on the arena before recursing.
  pub fn child_ids(&self) -> SmallVec<[NodeId; 4]> {
    let mut ids: SmallVec<[NodeId; 4]> = SmallVec::new();
    match &self.class {
      NdRule::List { commands } => ids.extend_from_slice(commands),
      NdRule::IfNode {
        cond_nodes,
        else_block,
      } => {
        for CondNode { cond, body } in cond_nodes {
          ids.push(*cond);
          ids.push(*body);
        }
        if let Some(block) = else_block {
          ids.push(*block);
        }
      }
      NdRule::LoopNode {
        cond_node: CondNode { cond, body },
        ..
      } => {
        ids.push(*cond);
        ids.push(*body);
      }
      NdRule::ForNode { body, .. }
      | NdRule::DeferNode { body }
      | NdRule::Subshell { body }
      | NdRule::BraceGrp { body }
      | NdRule::FuncDef { body, .. } => ids.push(*body),
      NdRule::TryNode { body, catch, .. } => {
        ids.push(*body);
        if let Some(catch) = catch {
          ids.push(*catch);
        }
      }
      NdRule::ForArith {
        init,
        cond,
        step,
        body,
      } => {
        if let Some(init) = init {
          ids.push(*init);
        }
        if let Some(cond) = cond {
          ids.push(*cond);
        }
        if let Some(step) = step {
          ids.push(*step);
        }
        ids.push(*body);
      }
      NdRule::CaseNode { case_blocks, .. } => {
        for CaseNode { body, .. } in case_blocks {
          ids.push(*body);
        }
      }
      NdRule::Command { assignments, .. } => ids.extend_from_slice(assignments),
      NdRule::Pipeline { cmds } => ids.extend_from_slice(cmds),
      NdRule::Conjunction { elements } => {
        for ConjunctNode { cmd, .. } in elements {
          ids.push(*cmd);
        }
      }
      NdRule::Timed { cmd } | NdRule::Negate { cmd } => ids.push(*cmd),
      NdRule::Arithmetic { .. } | NdRule::Assignment { .. } => (),
    }
    ids
  }
  /// Like [`Node::child_ids`] but yields mutable references, so a caller holding
  /// an owned clone of this node can remap its child ids in place (see
  /// `Ast::extract_subtree`).
  pub fn child_ids_mut(&mut self) -> SmallVec<[&mut NodeId; 4]> {
    let mut ids: SmallVec<[&mut NodeId; 4]> = SmallVec::new();
    match &mut self.class {
      NdRule::List { commands } => ids.extend(commands.iter_mut()),
      NdRule::IfNode {
        cond_nodes,
        else_block,
      } => {
        for CondNode { cond, body } in cond_nodes.iter_mut() {
          ids.push(cond);
          ids.push(body);
        }
        if let Some(block) = else_block {
          ids.push(block);
        }
      }
      NdRule::LoopNode {
        cond_node: CondNode { cond, body },
        ..
      } => {
        ids.push(cond);
        ids.push(body);
      }
      NdRule::ForNode { body, .. }
      | NdRule::DeferNode { body }
      | NdRule::Subshell { body }
      | NdRule::BraceGrp { body }
      | NdRule::FuncDef { body, .. } => ids.push(body),
      NdRule::TryNode { body, catch, .. } => {
        ids.push(body);
        if let Some(catch) = catch {
          ids.push(catch);
        }
      }
      NdRule::ForArith {
        init,
        cond,
        step,
        body,
      } => {
        if let Some(init) = init {
          ids.push(init);
        }
        if let Some(cond) = cond {
          ids.push(cond);
        }
        if let Some(step) = step {
          ids.push(step);
        }
        ids.push(body);
      }
      NdRule::CaseNode { case_blocks, .. } => {
        for CaseNode { body, .. } in case_blocks.iter_mut() {
          ids.push(body);
        }
      }
      NdRule::Command { assignments, .. } => ids.extend(assignments.iter_mut()),
      NdRule::Pipeline { cmds } => ids.extend(cmds.iter_mut()),
      NdRule::Conjunction { elements } => {
        for ConjunctNode { cmd, .. } in elements.iter_mut() {
          ids.push(cmd);
        }
      }
      NdRule::Timed { cmd } | NdRule::Negate { cmd } => ids.push(cmd),
      NdRule::Arithmetic { .. } | NdRule::Assignment { .. } => (),
    }
    ids
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
    const NO_TRACE      = 1 << 10;// no set -x trace output
  }
}

/// A conditional AST node
///
/// Used in `while`/`until`/`if` conditions
#[derive(Clone, Debug)]
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
    commands: Vec<NodeId>,
  },
  IfNode {
    cond_nodes: Vec<CondNode>,
    else_block: Option<NodeId>,
  },
  LoopNode {
    kind: LoopKind,
    cond_node: CondNode,
  },
  ForNode {
    vars: Vec<Tk>,
    arr: Vec<Tk>,
    body: NodeId,
    positional: bool, // true if no "in" keyword is passed to the for loop
  },
  TryNode {
    body: NodeId,
    err: Vec<Tk>,
    catch: Option<NodeId>,
  },
  DeferNode {
    body: NodeId,
  },
  ForArith {
    init: Option<NodeId>,
    cond: Option<NodeId>,
    step: Option<NodeId>,
    body: NodeId,
  },
  Arithmetic {
    body: Tk,
  },
  Negate {
    cmd: NodeId,
  },
  Timed {
    cmd: NodeId,
  },
  CaseNode {
    pattern: Tk,
    case_blocks: Vec<CaseNode>,
  },
  Command {
    assignments: Vec<NodeId>,
    argv: Vec<Tk>,
  },
  Pipeline {
    cmds: Vec<NodeId>,
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
    body: NodeId,
  },
  BraceGrp {
    body: NodeId,
  },
  FuncDef {
    name: Tk,
    body: NodeId,
  },
}

pub(crate) fn node_has_only_builtins(tree: &Ast, node: NodeId) -> bool {
  let mut res = None;
  tree.walk_tree(node, &mut |node| {
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
        let name = node.get_command().unwrap();

        // Caller is about to execute this anyway (cmd sub, pipeline, etc),
        // so source the autoload now while we have the chance.
        let autoload_src = Shed::logic_mut(|l| {
          if let Some(ShFunc::Autoload(_)) = l.get_func_ref(&name.to_str_lossy()) {
            let func = l.remove_func(&name.to_str_lossy())?;
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
          let Some(func) = l.get_func_ref(&name.to_str_lossy()) else {
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
        let Some(logic) = Shed::logic_mut(|l| match l.get_func_mut(&name.to_str_lossy()) {
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

        let body_src = logic[root].get_span();
        let is = subshell::is_internal(&body_src.to_str_lossy());
        let verdict = if is { IsInternal::Yes } else { IsInternal::No };
        Shed::logic_mut(|l| {
          if let Some(func) = l.get_func_mut(&name.to_str_lossy()) {
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
