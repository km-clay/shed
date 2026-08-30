//! The internal representation of `shed` scripts
//!
//! Contains the [`Ast`], which contains "flat trees" of [`Node`]s, which are the actual statements to execute.
//! [`Node`]s themselves are just bags of indices used to reach other nodes, tokens, and redirections in the [`Ast`].

use std::{
  ops::{Index, IndexMut},
  sync::atomic::{AtomicU32, Ordering},
};

use ariadne::Span as _;
use smallvec::SmallVec;

use crate::{
  ShResult,
  eval::{
    CaseNode, CondNode, ConjunctNode, NdRule, Node,
    lex::{Span, Tk},
  },
  procio::RedirSpec,
  util::error::LabelBuilder,
};

/// Used to identify instances of `Ast`.
///
/// `Ast` is indexed by `NodeId`, which also carries this identifier.
/// `Ast` being indexed by a mismatched `NodeId` is a panic.
static AST_GENERATION: AtomicU32 = AtomicU32::new(0);

/// Abstract Syntax Tree
///
/// The internal representation of a `shed` script. Contains "flat trees" of [`Node`]s, which are the actual statements to execute. Each [`Node`] is just a bag of indices used to reach other nodes, tokens, and redirections in the [`Ast`]. Each of these arenas has a corresponding `Id` type, which is used to index into the arena.
///
/// The [`Ast`] implements [`Index`] for each of these `Id` types. Indexing automatically dispatches to the correct arena, and checks that the `Id`'s `ast_id` matches the `Ast`'s `id`.

#[derive(Clone, Debug, Default)]
pub(crate) struct Ast {
  nodes: Vec<Node>,             // all ast nodes
  roots: Vec<NodeId>,           // top-level statements (entry points)
  tokens: Vec<Tk>,              // every token in the tree
  redirs: Vec<RedirSpec>,       // every node's list of redirections
  child_nodes: Vec<NodeId>,     // lists of NodeIds, like for pipelines and stuff
  case_nodes: Vec<CaseNode>,    // every case node in the tree, used by `case`
  cond_nodes: Vec<CondNode>,    // every conditional node in the tree, used by `if`/`while`
  conjuncts: Vec<ConjunctNode>, // every conjunction node in the tree, used by `&&`/`||`

  spans: Vec<Span>,
  labels: Vec<LabelBuilder>,

  id: u32,
}

impl Ast {
  pub fn new() -> Self {
    Self {
      id: AST_GENERATION.fetch_add(1, Ordering::SeqCst),
      ..Default::default()
    }
  }
  pub(super) fn reserve(&mut self, n: usize) {
    // based on testing, these ratios seem to be
    // the upper bound of AST memory usage.
    // 'n' is equal to the number of input tokens.

    self.tokens.reserve(n);
    self.nodes.reserve(2 * n);
    self.spans.reserve(2 * n);
    self.child_nodes.reserve(3 * n / 2);
    self.conjuncts.reserve(n / 2);
    self.labels.reserve(n / 2);
    self.redirs.reserve(n / 2);
    self.case_nodes.reserve(n / 4);
    self.cond_nodes.reserve(n / 4);
  }
  pub fn with_capacity(size: usize) -> Self {
    let mut new = Self::new();
    new.reserve(size);
    new
  }
  pub fn alloc<T: ArenaMember>(&mut self, value: T) -> T::Id {
    value.alloc_in(self)
  }
  #[cfg(test)]
  pub(crate) fn arena_lens(&self) -> Vec<(&'static str, usize)> {
    vec![
      ("nodes", self.nodes.len()),
      ("tokens", self.tokens.len()),
      ("spans", self.spans.len()),
      ("child_nodes", self.child_nodes.len()),
      ("redirs", self.redirs.len()),
      ("case_nodes", self.case_nodes.len()),
      ("cond_nodes", self.cond_nodes.len()),
      ("conjuncts", self.conjuncts.len()),
      ("labels", self.labels.len()),
    ]
  }
  pub fn mark_root(&mut self, id: NodeId) {
    if !self.roots.contains(&id) {
      self.roots.push(id);
    }
  }
  pub fn span_for_range(&self, range: NodeRange) -> Span {
    let first = self[range.first().unwrap()].get_span();
    let last = self[range.last().unwrap()].get_span();
    let start = self[first].range().start;
    let end = self[last].range().end;
    Span::new(start..end, self[first].source().content())
  }
  pub fn span_for(&self, node: NodeId) -> Span {
    self[self[node].get_span()].clone()
  }
  pub fn command_for(&self, node: NodeId) -> Option<&Tk> {
    self[node].get_command().map(|tk| &self[tk])
  }
  pub fn context_for(&self, node: NodeId) -> &[LabelBuilder] {
    &self[self[node].context]
  }
  pub fn roots(&self) -> &[NodeId] {
    &self.roots
  }
  pub fn get_root(&self) -> Option<NodeId> {
    self.roots.first().copied()
  }
  pub fn break_off(&self, id: NodeId) -> Self {
    let mut new = Self::new();
    let root = self.copy_into(id, &mut new);
    new.mark_root(root);
    new
  }
  fn copy_into(&self, id: NodeId, dst: &mut Self) -> NodeId {
    let Node {
      class,
      flags,
      redirs,
      span,
      context,
    } = self[id];

    let class = self.copy_class(class, dst);
    let span = dst.alloc(self[span].clone());
    let redirs = redirs.map(|r| dst.alloc_redirs(self[r].to_vec()));
    let context = context.map(|r| dst.alloc_labels(self[r].to_vec()));

    dst.alloc(Node {
      class,
      flags,
      redirs,
      span,
      context,
    })
  }
  fn copy_class(&self, class: NdRule, dst: &mut Self) -> NdRule {
    match class {
      NdRule::List { commands } => NdRule::List {
        commands: self.copy_node_range(commands, dst),
      },
      NdRule::IfNode {
        cond_nodes,
        else_block,
      } => NdRule::IfNode {
        cond_nodes: self.copy_cond_range(cond_nodes, dst),
        else_block: else_block.map(|n| self.copy_into(n, dst)),
      },
      NdRule::LoopNode { kind, cond_node } => NdRule::LoopNode {
        kind,
        cond_node: self.copy_cond(cond_node, dst),
      },
      NdRule::ForNode {
        vars,
        arr,
        body,
        positional,
      } => NdRule::ForNode {
        vars: self.copy_tk_range(vars, dst),
        arr: self.copy_tk_range(arr, dst),
        body: self.copy_into(body, dst),
        positional,
      },
      NdRule::TryNode {
        body,
        err,
        catch,
        ctx,
      } => NdRule::TryNode {
        body: self.copy_into(body, dst),
        err: self.copy_tk_range(err, dst),
        catch: catch.map(|n| self.copy_into(n, dst)),
        ctx: dst.alloc(self[ctx].clone()),
      },
      NdRule::DeferNode { body, ctx } => NdRule::DeferNode {
        body: self.copy_into(body, dst),
        ctx: dst.alloc(self[ctx].clone()),
      },
      NdRule::ForArith {
        init,
        cond,
        step,
        body,
      } => NdRule::ForArith {
        init: init.map(|n| self.copy_into(n, dst)),
        cond: cond.map(|n| self.copy_into(n, dst)),
        step: step.map(|n| self.copy_into(n, dst)),
        body: self.copy_into(body, dst),
      },
      NdRule::Arithmetic { body } => NdRule::Arithmetic {
        body: dst.alloc(self[body].clone()),
      },
      NdRule::Negate { cmd } => NdRule::Negate {
        cmd: self.copy_into(cmd, dst),
      },
      NdRule::Timed { cmd } => NdRule::Timed {
        cmd: self.copy_into(cmd, dst),
      },
      NdRule::CaseNode {
        pattern,
        case_blocks,
      } => NdRule::CaseNode {
        pattern: dst.alloc(self[pattern].clone()),
        case_blocks: self.copy_case_range(case_blocks, dst),
      },
      NdRule::Command { assignments, argv } => NdRule::Command {
        assignments: self.copy_node_range(assignments, dst),
        argv: self.copy_tk_range(argv, dst),
      },
      NdRule::Pipeline { cmds } => NdRule::Pipeline {
        cmds: self.copy_node_range(cmds, dst),
      },
      NdRule::Conjunction { elements } => NdRule::Conjunction {
        elements: self.copy_conjunct_range(elements, dst),
      },
      NdRule::Assignment { kind, var, val } => NdRule::Assignment {
        kind,
        var: dst.alloc(self[var].clone()),
        val: dst.alloc(self[val].clone()),
      },
      NdRule::Subshell { body } => NdRule::Subshell {
        body: self.copy_into(body, dst),
      },
      NdRule::BraceGrp { body } => NdRule::BraceGrp {
        body: self.copy_into(body, dst),
      },
      NdRule::FuncDef { name, body, ctx } => NdRule::FuncDef {
        name: dst.alloc(self[name].clone()),
        body: self.copy_into(body, dst),
        ctx: dst.alloc(self[ctx].clone()),
      },
    }
  }
  fn copy_node_range(&self, r: ChildRange, dst: &mut Self) -> ChildRange {
    let ids = self[r].to_vec();
    let mapped: Vec<NodeId> = ids.into_iter().map(|n| self.copy_into(n, dst)).collect();
    dst.alloc_children(mapped)
  }
  fn copy_tk_range(&self, r: TkRange, dst: &mut Self) -> TkRange {
    dst.alloc_tokens(self[r].to_vec())
  }
  fn copy_cond(&self, c: CondNodeId, dst: &mut Self) -> CondNodeId {
    let CondNode { cond, body } = self[c];
    let mapped = CondNode {
      cond: self.copy_into(cond, dst),
      body: self.copy_into(body, dst),
    };
    dst.alloc(mapped)
  }
  fn copy_cond_range(&self, r: CondNodeRange, dst: &mut Self) -> CondNodeRange {
    let mapped: Vec<CondNode> = self[r]
      .iter()
      .copied()
      .map(|CondNode { cond, body }| CondNode {
        cond: self.copy_into(cond, dst),
        body: self.copy_into(body, dst),
      })
      .collect();
    dst.alloc_conds(mapped)
  }
  fn copy_case_range(&self, r: CaseNodeRange, dst: &mut Self) -> CaseNodeRange {
    let mapped: Vec<CaseNode> = self[r]
      .iter()
      .cloned()
      .map(|c| CaseNode {
        patterns: c.patterns,
        body: self.copy_into(c.body, dst),
      })
      .collect();
    dst.alloc_cases(mapped)
  }
  fn copy_conjunct_range(&self, r: ConjunctRange, dst: &mut Self) -> ConjunctRange {
    let mapped: Vec<ConjunctNode> = self[r]
      .iter()
      .map(|c| ConjunctNode {
        cmd: self.copy_into(c.cmd, dst),
        operator: c.operator,
      })
      .collect();
    dst.alloc_conjuncts(mapped)
  }
  pub fn child_ids(&self, id: NodeId) -> SmallVec<[NodeId; 4]> {
    let mut out: SmallVec<[NodeId; 4]> = SmallVec::new();
    match &self[id].class {
      NdRule::List { commands } => out.extend_from_slice(&self[*commands]),
      NdRule::Pipeline { cmds } => out.extend_from_slice(&self[*cmds]),
      NdRule::IfNode {
        cond_nodes,
        else_block,
      } => {
        for c in &self[*cond_nodes] {
          out.push(c.cond);
          out.push(c.body);
        }
        out.extend(*else_block);
      }
      NdRule::LoopNode { cond_node, .. } => {
        let c = self[*cond_node];
        out.push(c.cond);
        out.push(c.body);
      }
      NdRule::Conjunction { elements } => out.extend(self[*elements].iter().map(|e| e.cmd)),
      NdRule::CaseNode { case_blocks, .. } => {
        out.extend(self[*case_blocks].iter().map(|b| b.body));
      }
      NdRule::Command { assignments, .. } => out.extend_from_slice(&self[*assignments]),
      NdRule::Negate { cmd } | NdRule::Timed { cmd } => out.push(*cmd),
      NdRule::Subshell { body }
      | NdRule::BraceGrp { body }
      | NdRule::ForNode { body, .. }
      | NdRule::DeferNode { body, .. }
      | NdRule::FuncDef { body, .. } => out.push(*body),
      NdRule::TryNode { body, catch, .. } => {
        out.push(*body);
        out.extend(*catch);
      }
      NdRule::ForArith {
        init,
        cond,
        step,
        body,
      } => {
        out.extend([*init, *cond, *step].into_iter().flatten());
        out.push(*body);
      }
      NdRule::Arithmetic { .. } | NdRule::Assignment { .. } => {}
    }
    out
  }
  pub fn walk_tree<F: FnMut(NodeId, &Self)>(&self, id: NodeId, f: &mut F) {
    f(id, self);
    let children = self.child_ids(id);
    for child in children {
      self.walk_tree(child, f);
    }
  }
  pub fn walk_tree_mut<F: FnMut(NodeId, &mut Self)>(&mut self, id: NodeId, f: &mut F) {
    f(id, self);
    let children = self.child_ids(id);
    for child in children {
      self.walk_tree_mut(child, f);
    }
  }
  pub fn eager_expand(&mut self, id: NodeId) -> ShResult<()> {
    let tk_ids: SmallVec<[TkId; 4]> = match &self[id].class {
      NdRule::Command { argv: tks, .. } | NdRule::ForNode { arr: tks, .. } => tks.ids().collect(),
      NdRule::Assignment { val: tk, .. }
      | NdRule::CaseNode { pattern: tk, .. }
      | NdRule::Arithmetic { body: tk } => [*tk].into_iter().collect(),
      _ => return Ok(()),
    };
    for tk in tk_ids {
      let expanded = std::mem::take(&mut self[tk]).expand()?;
      self[tk] = expanded;
    }
    Ok(())
  }
}

/// A trait for types that can be allocated in an [`Ast`].
///
/// This trait is implemented for all types that can be stored in an [`Ast`]'s arenas. It provides a method to allocate the value in the given [`Ast`] and return its corresponding `Id` type.
pub trait ArenaMember {
  type Id;
  fn alloc_in(self, ast: &mut Ast) -> Self::Id;
}

/// A macro to generate arena types and their corresponding `Id` types.
/// There are seven of these we have to generate, so this saves a lot of boilerplate
macro_rules! arenas {
  ($($ty:ident => $field:ident : $out:ty,)*) => {
    $(
      #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
      pub(crate) struct $ty {
        id: u32,
        ast_id: u32,
      }
      impl $ty {
        pub(super) fn new(id: u32, ast_id: u32) -> Self {
          Self { id, ast_id }
        }
      }
      impl Index<$ty> for super::Ast {
        type Output = $out;
        fn index(&self, index: $ty) -> &Self::Output {
          assert_eq!(index.ast_id, self.id, "Id does not match Ast");
          &self.$field[index.id as usize]
        }
      }
      impl IndexMut<$ty> for super::Ast {
        fn index_mut(&mut self, index: $ty) -> &mut Self::Output {
          assert_eq!(index.ast_id, self.id, "Id does not match Ast");
          &mut self.$field[index.id as usize]
        }
      }
      impl ArenaMember for $out {
        type Id = $ty;
        fn alloc_in(self, ast: &mut Ast) -> $ty {
          let id = ast.$field.len() as u32;
          ast.$field.push(self);
          $ty::new(id, ast.id)
        }
      }
    )*
  };
}

/// A macro to generate range types for arenas and their corresponding allocation methods in [`Ast`].
/// The range types are used to represent a contiguous range of `Id`s in an arena, and the allocation methods allow for allocating multiple items at once and returning a range of their `Id`s.
///
/// Ranges are basically the equivalent of storing a [`Vec`] directly on the [`Node`].
macro_rules! arena_ranges {
  ($($range:ident, $alloc:ident, $id:ident => $field:ident : $out:ty,)*) => {
    $(
      #[derive(Clone, Copy, Debug)]
      pub(crate) struct $range { start: $id, end: $id }

      impl $range {
        fn ast_id(self) -> u32 {
          self.start.ast_id
        }
        #[allow(dead_code)]
        pub fn is_empty(self) -> bool {
          self.start.id == self.end.id
        }
        #[allow(dead_code)]
        pub fn len(self) -> usize {
          (self.end.id - self.start.id) as usize
        }
        #[allow(dead_code)]
        pub fn first(self) -> Option<$id> {
          if self.is_empty() {
            None
          } else {
            Some(self.start)
          }
        }
        #[allow(dead_code)]
        pub fn last(self) -> Option<$id> {
          if self.is_empty() {
            None
          } else {
            Some($id::new(self.end.id - 1, self.ast_id()))
          }
        }
        #[allow(dead_code)]
        fn in_bounds(self, idx: usize) -> bool {
          let id = self.start.id + idx as u32;
          id < self.end.id
        }
        #[allow(dead_code)]
        pub fn get(self, idx: usize) -> $id {
          let id = self.start.id + idx as u32;
          assert!(self.in_bounds(idx), "Index out of bounds");
          $id::new(id, self.ast_id())
        }
        #[allow(dead_code)]
        pub fn ids(self) -> impl Iterator<Item = $id> {
          (self.start.id..self.end.id).map(move |id| $id::new(id, self.ast_id()))
        }
      }

      impl From<$id> for $range {
        fn from(id: $id) -> Self {
          let end = $id::new(id.id + 1, id.ast_id);
          $range { start: id, end }
        }
      }

      impl Index<$range> for Ast {
        type Output = [$out];
        fn index(&self, r: $range) -> &[$out] {
          debug_assert_eq!(r.ast_id(), self.id, "Id does not match Ast");
          &self.$field[r.start.id as usize..r.end.id as usize]
        }
      }

      impl Ast {
        #[allow(dead_code)]
        pub(crate) fn $alloc(&mut self, items: impl IntoIterator<Item = $out>) -> $range {
          let start = self.$field.len() as u32;
          self.$field.extend(items);
          let end = self.$field.len() as u32;
          $range { start: $id::new(start, self.id), end: $id::new(end, self.id) }
        }
      }
    )*
  };
}

arena_ranges!(
  NodeRange,     alloc_nodes,     NodeId     => nodes      : Node,
  ChildRange,    alloc_children,  NodeId     => child_nodes: NodeId,
  TkRange,       alloc_tokens,    TkId       => tokens     : Tk,
  RedirRange,    alloc_redirs,    RedirId    => redirs     : RedirSpec,
  CondNodeRange, alloc_conds,     CondNodeId => cond_nodes : CondNode,
  LabelRange,    alloc_labels,    LabelId    => labels     : LabelBuilder,
  ConjunctRange, alloc_conjuncts, ConjunctId => conjuncts  : ConjunctNode,
  CaseNodeRange, alloc_cases,     CaseNodeId => case_nodes : CaseNode,
  SpanRange,     alloc_spans,     SpanId     => spans      : Span,
);

arenas!(
  NodeId     => nodes     : Node,
  TkId       => tokens    : Tk,
  RedirId    => redirs    : RedirSpec,
  CondNodeId => cond_nodes: CondNode,
  LabelId    => labels    : LabelBuilder,
  ConjunctId => conjuncts : ConjunctNode,
  CaseNodeId => case_nodes: CaseNode,
  SpanId     => spans     : Span,
);

impl From<NodeRange> for ChildRange {
  fn from(r: NodeRange) -> Self {
    let NodeRange { start, end } = r;
    ChildRange { start, end }
  }
}

impl From<ChildRange> for NodeRange {
  fn from(r: ChildRange) -> Self {
    let ChildRange { start, end } = r;
    NodeRange { start, end }
  }
}

// Node's `redirs` field contains an `Option<RedirRange>`
// with this, we can index directly using the Option instead of
// pattern matching at every single callsite.
impl Index<Option<RedirRange>> for Ast {
  type Output = [RedirSpec];
  fn index(&self, r: Option<RedirRange>) -> &[RedirSpec] {
    match r {
      Some(r) => &self[r],
      None => &[],
    }
  }
}

// same for this
impl Index<Option<LabelRange>> for Ast {
  type Output = [LabelBuilder];
  fn index(&self, r: Option<LabelRange>) -> &[LabelBuilder] {
    match r {
      Some(r) => &self[r],
      None => &[],
    }
  }
}
