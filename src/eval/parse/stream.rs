use crate::{sherr, util::strops};

use super::{
  LabelCtx, NdFlags, NdRule, Node, ParseStream, ShErr, ShResult, Span, Tk, TkFlags, TkRule,
  ast::{Ast, NodeId},
};

impl ParseStream {
  /// Slice off consumed tokens
  pub(super) fn commit(&mut self, num_consumed: usize) {
    assert!(self.cursor + num_consumed <= self.tokens.len());
    self.cursor += num_consumed;
  }
  pub(super) fn last_consumed_was_sep(&self) -> bool {
    self
      .tokens
      .get(self.cursor.wrapping_sub(1))
      .is_some_and(|tk| tk.class == TkRule::Sep)
  }
  pub(super) fn next_tk_class(&self) -> &TkRule {
    self.peek_tk().map_or(&TkRule::Null, |tk| &tk.class)
  }
  pub(super) fn peek_tk(&self) -> Option<&Tk> {
    self.tokens.get(self.cursor)
  }
  pub(super) fn next_tk(&mut self) -> Option<Tk> {
    let tk = self
      .tokens
      .get(self.cursor)
      .and_then(|tk| (tk.class != TkRule::Eoi).then_some(tk))
      .cloned()?;
    self.cursor += 1;
    Some(tk)
  }
  pub(super) fn tokens(&self) -> &[Tk] {
    &self.tokens[self.cursor..]
  }
  pub(super) fn is_empty(&self) -> bool {
    self.tokens().is_empty()
  }
  pub(super) fn len(&self) -> usize {
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
  pub(super) fn catch_separator(&mut self, span: &mut Option<Span>) {
    while *self.next_tk_class() == TkRule::Sep {
      let next = self.next_tk().unwrap();
      if let Some(span) = span {
        span.merge_inplace(&next.span);
      } else {
        *span = Some(next.span);
      }
    }
  }
  /// Like [`Self::catch_separator`], but only consumes newline separators
  /// (the POSIX "linebreak"), leaving any `;`-bearing separator in place.
  ///
  /// Used immediately after a binary operator (`&&`, `||`, `|`), where POSIX
  /// permits a following newline but not a `;` — so `cmd &&\n cmd2` continues
  /// while `cmd && ; cmd2` is left to fail as a dangling operator.
  pub(super) fn catch_linebreak(&mut self, span: &mut Option<Span>) {
    while self
      .peek_tk()
      .is_some_and(|tk| tk.class == TkRule::Sep && !tk.as_bytes().contains(&b';'))
    {
      let next = self.next_tk().unwrap();
      extend_span!(*span, next.span);
    }
  }
  pub(super) fn assert_separator(&mut self, node_tks: &mut Option<Span>) -> ShResult<()> {
    let next_class = self.next_tk_class();
    match next_class {
      TkRule::Eoi
      | TkRule::Or
      | TkRule::Bg
      | TkRule::And
      | TkRule::BraceGrpEnd
      | TkRule::SubshEnd
      | TkRule::Pipe => Ok(()),

      TkRule::Sep => {
        if let Some(tk) = self.next_tk() {
          extend_span!(*node_tks, tk.span);
        }
        Ok(())
      }
      _ => Err(sherr!(ParseErr, "Expected a semicolon or newline here",)),
    }
  }
  pub(super) fn next_tk_is_some(&self) -> bool {
    self
      .peek_tk()
      .is_some_and(|tk| !matches!(tk.class, TkRule::Comment | TkRule::Eoi))
  }
  pub(super) fn check_flags(&self, flags: TkFlags) -> bool {
    self.peek_tk().is_some_and(|tk| tk.flags.contains(flags))
  }
  pub(super) fn check_keyword(&self, kw: &[u8]) -> bool {
    self.peek_tk().is_some_and(|tk| {
      if kw == b"in" {
        tk.span.as_bytes() == b"in"
      } else {
        tk.flags.contains(TkFlags::KEYWORD) && tk.as_bytes() == kw
      }
    })
  }
  pub(super) fn check_redir(&self) -> bool {
    self
      .peek_tk()
      .is_some_and(|tk| matches!(tk.class, TkRule::Redir | TkRule::HereDoc { .. }))
  }
}

pub(super) fn split_for_arith_tk(
  tree: &mut Ast,
  tk: &Tk,
) -> ShResult<Option<(NodeId, NodeId, NodeId)>> {
  let span = tk.span.clone();
  let mut tks = strops::split_tk(&tk.strip_arith_header()?, b";").into_iter();

  let Some(init_tk) = tks.next() else {
    return Err(sherr!(ParseErr @ span, "Missing init statement"));
  };
  let span = tree.alloc(init_tk.span.clone());
  let init_tk = tree.alloc(init_tk);
  let init = Node {
    class: NdRule::Arithmetic { body: init_tk },
    flags: NdFlags::empty(),
    redirs: None,
    span,
    context: None,
  };

  let Some(cond_tk) = tks.next() else {
    return Err(sherr!(ParseErr @ tree[span].clone(), "Missing condition statement"));
  };

  let cond_tk_span = tree.alloc(cond_tk.span.clone());
  let cond_tk = tree.alloc(cond_tk);
  let cond = Node {
    class: NdRule::Arithmetic { body: cond_tk },
    flags: NdFlags::empty(),
    redirs: None,
    span: cond_tk_span,
    context: None,
  };

  let Some(step_tk) = tks.next() else {
    return Err(sherr!(ParseErr @ tree[span].clone(), "Missing step statement"));
  };

  let step_tk_span = tree.alloc(step_tk.span.clone());
  let step_tk = tree.alloc(step_tk);

  let step = Node {
    class: NdRule::Arithmetic { body: step_tk },
    flags: NdFlags::empty(),
    redirs: None,
    span: step_tk_span,
    context: None,
  };

  let nodes = (tree.alloc(init), tree.alloc(cond), tree.alloc(step));

  Ok(Some(nodes))
}

pub(super) fn parse_err_full(reason: &str, blame: &Span, context: &LabelCtx) -> ShErr {
  sherr!(ParseErr @ blame.clone(), "{reason}").with_context(context.iter())
}
