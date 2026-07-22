use std::collections::VecDeque;

use super::{
  LabelCtx, NdFlags, NdRule, Node, ParseStream, ShErr, ShResult, Span, Tk, TkFlags, TkRule,
  crate_util::split_tk, sherr,
};

impl ParseStream {
  /// Slice off consumed tokens
  pub(super) fn commit(&mut self, num_consumed: usize) {
    assert!(self.cursor + num_consumed <= self.tokens.len());
    self.cursor += num_consumed;
  }
  pub fn last_consumed_was_sep(&self) -> bool {
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
      .is_some_and(|tk| tk.class == TkRule::Sep && !tk.span.as_str().contains(';'))
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
  pub(super) fn check_keyword(&self, kw: &str) -> bool {
    self.peek_tk().is_some_and(|tk| {
      if kw == "in" {
        tk.span.as_str() == "in"
      } else {
        tk.flags.contains(TkFlags::KEYWORD) && tk.span.as_str() == kw
      }
    })
  }
  pub(super) fn check_redir(&self) -> bool {
    self
      .peek_tk()
      .is_some_and(|tk| matches!(tk.class, TkRule::Redir | TkRule::HereDoc { .. }))
  }
}

#[expect(clippy::type_complexity)]
pub(super) fn split_for_arith_tk(
  tk: &Tk,
) -> ShResult<(Option<Box<Node>>, Option<Box<Node>>, Option<Box<Node>>)> {
  let span = tk.span.clone();
  let mut tks = split_tk(tk, ";").into_iter();

  let Some(init_tk) = tks.next() else {
    return Err(sherr!(ParseErr @ span, "Missing init statement"));
  };
  let init = Some(Box::new(Node {
    class: NdRule::Arithmetic {
      body: init_tk.clone(),
    },
    flags: NdFlags::empty(),
    redirs: vec![],
    span: init_tk.span,
    context: VecDeque::default().into(),
  }));

  let Some(cond_tk) = tks.next() else {
    return Err(sherr!(ParseErr @ span, "Missing condition statement"));
  };
  let cond = Some(Box::new(Node {
    class: NdRule::Arithmetic {
      body: cond_tk.clone(),
    },
    flags: NdFlags::empty(),
    redirs: vec![],
    span: cond_tk.span,
    context: VecDeque::default().into(),
  }));

  let Some(step_tk) = tks.next() else {
    return Err(sherr!(ParseErr @ span, "Missing step statement"));
  };
  let step = Some(Box::new(Node {
    class: NdRule::Arithmetic {
      body: step_tk.clone(),
    },
    flags: NdFlags::empty(),
    redirs: vec![],
    span: step_tk.span,
    context: VecDeque::default().into(),
  }));

  Ok((init, cond, step))
}

pub(super) fn parse_err_full(reason: &str, blame: &Span, context: LabelCtx) -> ShErr {
  sherr!(ParseErr @ blame.clone(), "{reason}").with_context(context.iter())
}
