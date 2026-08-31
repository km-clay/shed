use bitflags::bitflags;
use std::{
  collections::VecDeque,
  fmt::{self, Debug},
};

pub(crate) mod node;
pub(crate) use node::{
  AssignKind, CaseNode, CondNode, ConjunctNode, ConjunctOp, LoopKind, NdFlags, NdRule, Node,
};
pub(crate) mod ast;

#[cfg(test)]
pub(crate) use node::NdKind;

#[macro_use]
mod macros;
mod command;
mod compound;
mod stream;

#[cfg(test)]
pub(crate) mod tests;

use crate::{
  eval::parse::{
    ast::{Ast, NodeId},
    node::LabelCtx,
  },
  match_loop, procio, sherr,
  state::vars::VarStr,
  two_way_display,
  util::error::{ShErr, ShResult},
};

use super::lex::{self, LexFlags, LexStream, Span, Tk, TkFlags, TkRule, clean_input};

/// The parsed AST along with the source input it parsed
///
/// Uses Rc<str> instead of &str because the reference has to stay alive
/// while errors are propagated upwards The string also has to stay alive in the
/// case of pre-parsed shell function nodes, which live in the logic table Using
/// &str for this use-case dramatically overcomplicates the code
#[derive(Clone, Debug)]
pub(crate) struct ParsedSrc {
  pub src: VarStr,
  pub name: VarStr,
  pub ast: Ast,
  pub lex_flags: LexFlags,
  pub parse_flags: ParseFlags,
  pub context: LabelCtx,
}

impl ParsedSrc {
  pub(crate) fn new(src: VarStr) -> Self {
    let src = if src.contains_slice(b"\\\n") || src.contains(&b'\r') {
      clean_input(&src)
    } else {
      src
    };
    Self {
      src,
      name: "<stdin>".into(),
      ast: Ast::new(),
      lex_flags: LexFlags::empty(),
      parse_flags: ParseFlags::empty(),
      context: VecDeque::new().into(),
    }
  }
  pub(crate) fn with_name(mut self, name: VarStr) -> Self {
    self.name = name;
    self
  }
  pub(crate) fn with_lex_flags(mut self, flags: LexFlags) -> Self {
    self.lex_flags = flags;
    self
  }
  pub(crate) fn with_parse_flags(mut self, flags: ParseFlags) -> Self {
    self.parse_flags = flags;
    self
  }
  pub(crate) fn parse_src(&mut self) -> Result<(), Vec<ShErr>> {
    let mut tokens = vec![];
    let mut errors = vec![];
    let mut stream = LexStream::new(&self.src, self.lex_flags).with_name(self.name.clone());

    while let Some(lex_result) = stream.next() {
      // inline what the previous .filter() did
      if lex_result
        .as_ref()
        .is_ok_and(|tk| matches!(tk.class, TkRule::Comment))
      {
        continue;
      }
      match lex_result {
        Ok(token) => tokens.push(token),
        Err(error) => {
          if self.lex_flags.contains(LexFlags::LEX_UNFINISHED) {
            errors.push(error);
          } else {
            return Err(vec![error]);
          }
        }
      }
    }

    let mut parser = ParseStream::new(tokens, self.context.clone()).with_flags(self.parse_flags);

    match_loop!(parser.next() => parse_result, {
      Ok(root) => parser.tree.mark_root(root),
      Err(error) => {
        if self.parse_flags.contains(ParseFlags::ERR_RETURN) {
          return Err(vec![error]);
        }
        errors.push(error);
      }
    });

    if !errors.is_empty() {
      return Err(errors);
    }

    self.ast = parser.tree;
    Ok(())
  }
  pub(crate) fn into_ast(self) -> Ast {
    self.ast
  }
}

bitflags! {
  #[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,PartialOrd,Ord)]
  pub(crate) struct ParseFlags: u32 {
    const INCOMPLETE = 1 << 0; // Whether to error
    const ERR_RETURN = 1 << 1; // Return on first error instead of continuing
  }
}

struct ParseStream {
  pub tokens: Vec<Tk>,
  pub tree: Ast,
  pub cursor: usize,
  pub context: LabelCtx,
  pub flags: ParseFlags,
}

impl Debug for ParseStream {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ParseStream")
      .field("tokens", &self.tokens)
      .field("cursor", &self.cursor)
      .finish_non_exhaustive()
  }
}

impl ParseStream {
  pub(crate) fn new(mut tokens: Vec<Tk>, context: LabelCtx) -> Self {
    tokens.retain(|tk| tk.class != TkRule::Comment);

    let tree = Ast::with_capacity(tokens.len());
    Self {
      tokens,
      tree,
      cursor: 0,
      context,
      flags: ParseFlags::empty(),
    }
  }
  pub(crate) fn with_flags(mut self, flags: ParseFlags) -> Self {
    self.flags = flags;
    self
  }
  fn parse_cmd_list(&mut self) -> ShResult<Option<NodeId>> {
    let mut commands = vec![];
    let mut span: Option<Span> = None;
    while let Some(cmd) = self.parse_conjunction()? {
      extend_span!(span, self.tree.span_for(cmd));
      commands.push(cmd);
    }

    let node = (!commands.is_empty()).then(|| {
      let commands = self.tree.alloc_children(commands);
      let span = self.tree.alloc(span.unwrap_or_default());
      let node = node!(self, span, NdRule::List { commands });
      self.tree.alloc(node)
    });

    Ok(node)
  }
  fn parse_conjunction(&mut self) -> ShResult<Option<NodeId>> {
    let mut elements = vec![];
    let mut span: Option<Span> = None;

    let mut dangling_op: Option<Span> = None;
    while let Some(block) = self.parse_block(true)? {
      dangling_op = None;
      extend_span!(span, self.tree.span_for(block));
      self.catch_separator(&mut span);

      let conjunct_op = match self.next_tk_class() {
        TkRule::And => ConjunctOp::And,
        TkRule::Or => ConjunctOp::Or,
        _ => ConjunctOp::Null,
      };

      // A `&&`/`||` may only directly follow a pipeline, not a separator:
      // `echo a ; && echo b` is a syntax error, not `echo a && echo b`.
      if conjunct_op != ConjunctOp::Null && self.last_consumed_was_sep() {
        return Err(parse_err!(
          self,
          span,
          "Unexpected binary operator after a separator"
        ));
      }

      if conjunct_op != ConjunctOp::Null {
        self
          .tree
          .walk_tree_mut(block, &mut |id, tree| tree[id].not_err());
      }

      let conjunction = ConjunctNode {
        cmd: block,
        operator: conjunct_op,
      };

      elements.push(conjunction);

      if conjunct_op != ConjunctOp::Null {
        let Some(tk) = self.next_tk() else { break };
        dangling_op = Some(tk.span.clone());
        extend_span!(span, tk.span);
        // Only a newline (not `;`) may follow the operator.
        self.catch_linebreak(&mut span);
      }

      if conjunct_op == ConjunctOp::Null {
        break;
      }
    }

    if let Some(op_span) = dangling_op {
      return Err(parse_err!(
        self,
        Some(op_span),
        "Expected a command after this operator"
      ));
    }

    if elements.is_empty() {
      Ok(None)
    } else {
      let elements = self.tree.alloc_conjuncts(elements);
      let span = self.tree.alloc(span.unwrap_or_default());
      let node = node!(self, span, NdRule::Conjunction { elements });
      Ok(Some(self.tree.alloc(node)))
    }
  }
  /// This tries to match on different stuff that can appear in a command
  /// position Matches shell commands like if-then-fi, pipelines, etc.
  /// Ordered from specialized to general, with more generally matchable stuff
  /// appearing at the bottom The `check_pipelines` parameter is used to prevent
  /// left-recursion issues in `self.parse_pipeln()`
  fn parse_block(&mut self, check_pipelines: bool) -> ShResult<Option<NodeId>> {
    stacker::maybe_grow(64 * 1024, 1024 * 1024, || {
      if check_pipelines {
        try_match!(self.parse_pipeln()?);
        return Ok(None);
      }

      try_match!(self.parse_func_def()?);
      try_match!(self.parse_brc_grp(false /* from_func_def */)?);
      try_match!(self.parse_subsh()?);
      try_match!(self.parse_case()?);
      try_match!(self.parse_loop()?);
      try_match!(self.parse_for()?);
      try_match!(self.parse_if()?);
      try_match!(self.parse_negate()?);
      try_match!(self.parse_time()?);
      try_match!(self.parse_defer()?);
      try_match!(self.parse_try()?);
      try_match!(self.parse_func_keyword()?);
      try_match!(self.parse_arith()?);
      try_match!(self.parse_cmd()?);
      Ok(None)
    })
  }
  fn parse_compound(&mut self) -> ShResult<Option<NodeId>> {
    // parse only a compound command, used by function definitions since any
    // compound command is a valid function body.
    let result = || -> ShResult<Option<NodeId>> {
      try_match!(self.parse_brc_grp(true /* from_func_def */)?);
      try_match!(self.parse_subsh()?);
      try_match!(self.parse_case()?);
      try_match!(self.parse_loop()?);
      try_match!(self.parse_for()?);
      try_match!(self.parse_try()?);
      try_match!(self.parse_defer()?);
      try_match!(self.parse_if()?);

      Ok(None)
    }()?;

    Ok(result)
  }
  fn panic_mode(&mut self, span: &mut Option<Span>) {
    while let Some(tk) = self.next_tk() {
      if tk.class == TkRule::Sep {
        break;
      }
      extend_span!(*span, tk.span);
    }
  }
}

impl Iterator for ParseStream {
  type Item = Result<NodeId, ShErr>;
  fn next(&mut self) -> Option<Self::Item> {
    // Empty token vector or only Soi/Eoi tokens, nothing to do
    if self.is_empty() && self.len() == 1 && self.tokens().last().unwrap().class == TkRule::Eoi {
      return None;
    }
    while let Some(tk) = self.tokens().first() {
      if let TkRule::Eoi = tk.class {
        return None;
      }
      if let TkRule::Soi | TkRule::Sep = tk.class {
        self.next_tk();
      } else {
        break;
      }
    }
    let result = self.parse_cmd_list();
    match result {
      Ok(Some(node)) => Some(Ok(node)),
      Ok(None) => match self.peek_tk() {
        None => None,
        Some(tk) if tk.class == TkRule::Eoi => None,
        Some(tk) => {
          let class = tk.class.clone();
          let mut span = Some(tk.span.clone());
          self.panic_mode(&mut span);
          Some(Err(parse_err!(self, span, "Unexpected token: {class:?}")))
        }
      },
      Err(e) => Some(Err(e)),
    }
  }
}
