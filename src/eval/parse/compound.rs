use shed_macros::styled_format;

use crate::{
  eval::parse::node::NodeId,
  state::vars::VarStr,
  util::{error::get_context, parse_bytes},
};

use super::{
  CaseNode, CondNode, LoopKind, NdRule, Node, ParseStream, ShResult, Tk, TkFlags, TkRule,
  lex::Span, util::split_for_arith_tk,
};

impl ParseStream {
  pub(super) fn parse_func_def(&mut self) -> ShResult<Option<NodeId>> {
    let mut span: Option<Span> = None;
    let has_func_kw = self.check_keyword(b"function");

    if has_func_kw {
      extend_span!(span, self.next_tk().unwrap().span);
    }

    if !self.check_flags(TkFlags::FUNCNAME) {
      if has_func_kw {
        bail!(
          self,
          span,
          "Expected function name after 'function' keyword"
        );
      } else {
        return Ok(None);
      }
    }
    let name_tk = self.next_tk().unwrap();
    extend_span!(span, name_tk.span);

    let name = name_tk.clone();

    self.catch_separator(&mut span);

    let Some(body) = self.parse_compound()? else {
      bail!(
        self,
        span,
        "Expected a compound command after function name"
      );
    };

    extend_span!(span, self.tree[body].get_span());

    let ctx = get_context(
      VarStr::from(styled_format!(
        "in function '{}' defined here",
        name.to_str_lossy()
      )),
      &span.clone().unwrap_or_default(),
    );

    let mut redirs = vec![];
    self.parse_redir(&mut redirs, &mut span)?;
    self.tree[body].redirs.append(&mut redirs);

    let node = node!(self, span, NdRule::FuncDef { name, body, ctx });

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_subsh(&mut self) -> ShResult<Option<NodeId>> {
    if *self.next_tk_class() != TkRule::SubshStart {
      return Ok(None);
    }

    let mut span: Option<Span> = None;
    let mut body_span: Option<Span> = None;

    let mut body = vec![];
    let mut redirs = vec![];

    extend_span!(span, self.next_tk().unwrap().span);
    self.catch_separator(&mut span);

    loop {
      if *self.next_tk_class() == TkRule::SubshEnd {
        extend_span!(span, self.next_tk().unwrap().span);
        break;
      }
      if let Some(node) = self.parse_conjunction()? {
        extend_span!(span, self.tree[node].get_span());
        extend_span!(body_span, self.tree[node].get_span());
        body.push(node);
      } else if *self.next_tk_class() != TkRule::SubshEnd {
        let next = self.peek_tk().cloned();
        let err = match next {
          Some(tk) => Err(parse_err!(
            self,
            span.clone(),
            "Unexpected token '{}' in subshell body",
            tk.to_str_lossy()
          )),
          None => Err(parse_err!(
            self,
            span.clone(),
            "Unexpected end of input while parsing subshell body"
          )),
        };
        self.panic_mode(&mut span);
        return err;
      }
      self.catch_separator(&mut span);
      if !self.next_tk_is_some() {
        bail!(
          self,
          span,
          "Expected a closing parenthesis for this subshell"
        );
      }
    }

    let node = node!(self, body_span, NdRule::List { commands: body }, vec![]);

    let body = self.tree.insert_node(node);

    self.parse_redir(&mut redirs, &mut span)?;

    let node = node!(self, span, NdRule::Subshell { body }, redirs);

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_brc_grp(&mut self, from_func_def: bool) -> ShResult<Option<NodeId>> {
    if *self.next_tk_class() != TkRule::BraceGrpStart {
      return Ok(None);
    }

    let mut span: Option<Span> = None;
    let mut body_span: Option<Span> = None;

    let mut body = vec![];
    let mut redirs = vec![];

    extend_span!(span, self.next_tk().unwrap().span);

    self.catch_separator(&mut span);

    loop {
      if *self.next_tk_class() == TkRule::BraceGrpEnd {
        extend_span!(span, self.next_tk().unwrap().span);
        break;
      }
      if let Some(node) = self.parse_conjunction()? {
        extend_span!(span, self.tree[node].get_span());
        extend_span!(body_span, self.tree[node].get_span());
        body.push(node);
      } else if *self.next_tk_class() != TkRule::BraceGrpEnd {
        let next = self.peek_tk().cloned();
        let err = match next {
          Some(tk) => Err(parse_err!(
            self,
            span.clone(),
            "Unexpected token '{}' in brace group body",
            tk.to_str_lossy()
          )),
          None => Err(parse_err!(
            self,
            span.clone(),
            "Unexpected end of input while parsing brace group body"
          )),
        };
        self.panic_mode(&mut span);
        return err;
      }
      self.catch_separator(&mut span);
      if !self.next_tk_is_some() {
        bail!(self, span, "Expected a closing brace for this brace group");
      }
    }

    let node = node!(self, body_span, NdRule::List { commands: body }, vec![]);
    let body = self.tree.insert_node(node);

    if !from_func_def {
      self.parse_redir(&mut redirs, &mut span)?;
    }

    let node = node!(self, span, NdRule::BraceGrp { body }, redirs);

    Ok(Some(self.tree.insert_node(node)))
  }
  #[expect(clippy::too_many_lines)]
  pub(super) fn parse_case(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_keyword(b"case") {
      return Ok(None);
    }

    let mut span: Option<Span> = None;

    let mut case_blocks: Vec<CaseNode> = vec![];
    let mut redirs = vec![];

    extend_span!(span, self.next_tk().unwrap().span);

    let pat_err = parse_err!(
      self,
      span.clone(),
      "Expected a pattern after 'case' keyword"
    )
    .with_note("Patterns can be raw text, or anything that gets substituted with raw text".into());

    let Some(pat_tk) = self.next_tk() else {
      self.panic_mode(&mut span);
      return Err(pat_err);
    };

    if matches!(pat_tk.class, TkRule::Sep) || pat_tk.span.as_bytes() == b"in" {
      return Err(pat_err);
    }

    let pattern: Tk = pat_tk;

    extend_span!(span, pattern.clone().span);

    if !self.check_keyword(b"in") {
      bail!(self, span, "Expected 'in' after case variable name");
    }
    extend_span!(span, self.next_tk().unwrap().span);

    self.catch_separator(&mut span);

    loop {
      if self.check_keyword(b"esac") {
        extend_span!(span, self.next_tk().unwrap().span);
        self.parse_redir(&mut redirs, &mut span)?;
        self.assert_separator(&mut span)?;
        break;
      }

      let leading_paren = matches!(self.next_tk_class(), TkRule::SubshStart);
      if leading_paren {
        // optional leading paren, push and continue
        extend_span!(span, self.next_tk().unwrap().span);
      }

      let mut patterns: Vec<Tk> = vec![];
      loop {
        let Some(word) = self.next_tk() else {
          bail!(self, span, "Expected a case pattern here");
        };
        if matches!(word.class, TkRule::SubshEnd | TkRule::Sep | TkRule::Eoi)
          || word.flags.contains(TkFlags::KEYWORD)
        {
          self.panic_mode(&mut span);
          return Err(parse_err!(
            self,
            Some(word.span),
            "Expected a case pattern here"
          ));
        }
        extend_span!(span, word.clone().span);
        patterns.push(word);

        match self.next_tk_class() {
          TkRule::Pipe => {
            extend_span!(span, self.next_tk().unwrap().span); // consume '|'
            // loop back for next alternative
          }
          TkRule::SubshEnd => break,
          _ => {
            bail!(self, span, "Expected '|' or ')' after case pattern");
          }
        }
      }

      // Consume the closing ')'.
      extend_span!(span, self.next_tk().unwrap().span);

      let mut found_end = false;
      while *self.next_tk_class() == TkRule::Sep {
        let sep = self.peek_tk().unwrap();
        if sep.has_double_semi() {
          extend_span!(span, self.next_tk().unwrap().span);
          found_end = true;
          break;
        }
        extend_span!(span, self.next_tk().unwrap().span);
      }
      let mut arm_commands = vec![];
      let mut arm_span: Option<Span> = None;

      while !found_end {
        let Some(conj) = self.parse_conjunction()? else {
          break;
        };
        extend_span!(arm_span, self.tree[conj].get_span());

        let trailing_dbl_semi = self
          .tokens
          .get(self.cursor.wrapping_sub(1))
          .is_some_and(Tk::has_double_semi);

        arm_commands.push(conj);

        if trailing_dbl_semi {
          found_end = true;
        }
      }

      let arm_body = node!(
        self,
        arm_span,
        NdRule::List {
          commands: arm_commands
        }
      );

      let case_node = CaseNode {
        patterns,
        body: self.tree.insert_node(arm_body),
      };
      case_blocks.push(case_node);

      self.catch_separator(&mut span);

      if self.check_keyword(b"esac") {
        extend_span!(span, self.next_tk().unwrap().span);
        self.parse_redir(&mut redirs, &mut span)?;
        self.assert_separator(&mut span)?;
        break;
      }

      if !self.next_tk_is_some() {
        bail!(self, span, "Expected 'esac' to close this case statement");
      }
    }

    let node = node!(
      self,
      span,
      NdRule::CaseNode {
        pattern,
        case_blocks
      },
      redirs
    );

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_time(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_keyword(b"time") {
      return Ok(None);
    }

    let mut span: Option<Span> = None;

    extend_span!(span, self.next_tk().unwrap().span);

    let Some(cmd) = self.parse_block(true)? else {
      bail!(self, span, "Expected a command after 'time'");
    };

    self.tree.walk_tree_mut(cmd, &mut Node::not_err);

    extend_span!(span, self.tree[cmd].get_span());
    self.catch_separator(&mut span);

    let node = node!(self, span, NdRule::Timed { cmd });

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_func_keyword(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_keyword(b"function") {
      return Ok(None);
    }
    self.parse_func_def()
  }
  pub(super) fn parse_arith(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_flags(TkFlags::IS_ARITH) {
      return Ok(None);
    }

    let mut span: Option<Span> = None;
    let mut redirs = vec![];

    let arith_tk = self.next_tk().unwrap();
    extend_span!(span, arith_tk.clone().span);

    self.parse_redir(&mut redirs, &mut span)?;

    if matches!(self.next_tk_class(), TkRule::Str) {
      bail!(self, span, "Unexpected argument after arithmetic command");
    }

    let node = node!(self, span, NdRule::Arithmetic { body: arith_tk }, redirs);

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_negate(&mut self) -> ShResult<Option<NodeId>> {
    if (!self.check_keyword(b"not") && !self.check_keyword(b"!")) || !self.next_tk_is_some() {
      return Ok(None);
    }
    let display = if self.check_keyword(b"!") { "!" } else { "not" };

    let mut span: Option<Span> = None;

    extend_span!(span, self.next_tk().unwrap().span);

    let Some(cmd) = self.parse_block(true)? else {
      bail!(self, span, "Expected a command after '{display}'");
    };
    self.tree.walk_tree_mut(cmd, &mut Node::not_err); // disable set -e for negated commands

    extend_span!(span, self.tree[cmd].get_span());
    self.catch_separator(&mut span);

    let node = node!(self, span, NdRule::Negate { cmd });

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_if(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_keyword(b"if") {
      return Ok(None);
    }

    let mut span: Option<Span> = None;
    let mut cond_nodes: Vec<CondNode> = vec![];
    let mut else_block: Option<NodeId> = None;
    let mut redirs = vec![];

    extend_span!(span, self.next_tk().unwrap().span);

    loop {
      let prefix_keywrd = if cond_nodes.is_empty() { "if" } else { "elif" };
      let Some(cond) = self.parse_cmd_list()? else {
        bail!(self, span, "Expected a command after '{prefix_keywrd}'");
      };
      extend_span!(span, self.tree[cond].get_span());
      self.tree.walk_tree_mut(cond, &mut Node::not_err); // disable set -e for condition commands

      if !self.check_keyword(b"then") {
        bail!(
          self,
          span,
          "Expected 'then' after '{prefix_keywrd}' condition"
        );
      }
      extend_span!(span, self.next_tk().unwrap().span);
      self.catch_separator(&mut span);

      let Some(body) = self.parse_cmd_list()? else {
        bail!(self, span, "Expected a command after 'then'");
      };
      extend_span!(span, self.tree[body].get_span());

      let cond_node = CondNode { cond, body };
      cond_nodes.push(cond_node);

      self.catch_separator(&mut span);
      if self.check_keyword(b"elif") {
        extend_span!(span, self.next_tk().unwrap().span);
        self.catch_separator(&mut span);
      } else {
        break;
      }
    }

    self.catch_separator(&mut span);
    if self.check_keyword(b"else") {
      extend_span!(span, self.next_tk().unwrap().span);
      self.catch_separator(&mut span);

      let Some(body) = self.parse_cmd_list()? else {
        bail!(self, span, "Expected a command after 'else'");
      };
      else_block = Some(body);
    }

    self.catch_separator(&mut span);
    if !self.check_keyword(b"fi") {
      bail!(self, span, "Expected 'fi' after if statement");
    }
    extend_span!(span, self.next_tk().unwrap().span);

    self.parse_redir(&mut redirs, &mut span)?;

    self.assert_separator(&mut span)?;

    let node = node!(
      self,
      span,
      NdRule::IfNode {
        cond_nodes,
        else_block
      },
      redirs
    );

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_for_arith(&mut self, span: &mut Option<Span>) -> ShResult<Option<NodeId>> {
    let mut redirs = vec![];

    let arith_tk = self.next_tk().unwrap(); // we checked already
    extend_span!(*span, arith_tk.clone().span);
    let (init, cond, step) = match split_for_arith_tk(&mut self.tree, &arith_tk)? {
      None => (None, None, None),
      Some((init, cond, step)) => (Some(init), Some(cond), Some(step)),
    };
    self.catch_separator(span);

    if !self.check_keyword(b"do") {
      bail!(
        self,
        span.clone(),
        "Expected 'do' after for loop arithmetic expression"
      );
    }
    extend_span!(*span, self.next_tk().unwrap().span);
    self.catch_separator(span);

    let Some(body) = self.parse_cmd_list()? else {
      bail!(
        self,
        span.clone(),
        "Expected a command after 'do' in this loop"
      );
    };

    self.catch_separator(span);
    if !self.check_keyword(b"done") {
      bail!(self, span.clone(), "Expected 'done' after for loop body");
    }
    extend_span!(*span, self.next_tk().unwrap().span);

    self.parse_redir(&mut redirs, span)?;

    let node = node!(
      self,
      span.clone(),
      NdRule::ForArith {
        init,
        cond,
        step,
        body
      },
      redirs
    );

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_for_arr(&mut self, span: &mut Option<Span>) -> ShResult<Option<NodeId>> {
    let mut vars: Vec<Tk> = vec![];
    let mut arr: Vec<Tk> = vec![];
    let mut redirs = vec![];

    // Read variable names, stopping at "in", a separator, or "do". Whether an
    // "in" clause is present decides the iteration list: "for x in words"
    // iterates "words" (possibly empty, zero iterations), while "for x" with
    // no "in" iterates the positional parameters ("$@").
    let mut positional = true;
    let array_checks = |this: &Self| {
      this.peek_tk().map(|tk| {
        let is_in = tk.as_bytes() == b"in";
        let is_sep = tk.class == TkRule::Sep;
        let is_do = tk.as_bytes() == b"do";
        (is_in, is_sep, is_do)
      })
    };

    while let Some((is_in, is_sep, is_do)) = array_checks(self) {
      if is_in {
        // we have been given an explicit array
        extend_span!(*span, self.next_tk().unwrap().span);
        positional = false;
        break;
      }
      if is_sep || is_do {
        // we are done here
        break;
      }
      let tk = self.next_tk().unwrap();
      extend_span!(*span, tk.span);
      vars.push(tk);
    }

    // Read the explicit word list only when an `in` clause was given.
    if !positional {
      while self.peek_tk().is_some_and(|tk| tk.class != TkRule::Sep) {
        let tk = self.next_tk().unwrap();
        extend_span!(*span, tk.span);
        arr.push(tk);
      }
    }

    // Consume the separator(s) between the header and `do`.
    self.catch_separator(span);

    if vars.is_empty() {
      bail!(
        self,
        span.clone(),
        "Expected a variable name for this for loop"
      );
    }
    if self.peek_tk().is_none_or(|tk| tk.as_bytes() != b"do") {
      bail!(
        self,
        span.clone(),
        "Expected 'do' after for loop variable and array"
      );
    }
    extend_span!(*span, self.next_tk().unwrap().span);
    self.catch_separator(span);

    let Some(body) = self.parse_cmd_list()? else {
      bail!(
        self,
        span.clone(),
        "Expected a command after 'do' in this loop"
      );
    };

    self.catch_separator(span);
    if !self.check_keyword(b"done") {
      bail!(self, span.clone(), "Expected 'done' after for loop body");
    }
    extend_span!(*span, self.next_tk().unwrap().span);

    self.parse_redir(&mut redirs, span)?;

    self.assert_separator(span)?;

    let node = node!(
      self,
      span.clone(),
      NdRule::ForNode {
        vars,
        arr,
        body,
        positional
      },
      redirs
    );

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_for(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_keyword(b"for") {
      return Ok(None);
    }

    let mut span: Option<Span> = None;
    extend_span!(span, self.next_tk().unwrap().span);

    if self.check_flags(TkFlags::IS_ARITH) {
      self.parse_for_arith(&mut span)
    } else {
      self.parse_for_arr(&mut span)
    }
  }
  pub(super) fn parse_loop(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_keyword(b"while") && !self.check_keyword(b"until") {
      return Ok(None);
    }

    let mut span: Option<Span> = None;
    let mut redirs = vec![];

    let loop_tk = self.next_tk().unwrap();
    let loop_kind: LoopKind = parse_bytes(loop_tk.as_bytes()) // LoopKind implements FromStr
      .unwrap();

    extend_span!(span, loop_tk.span);
    self.catch_separator(&mut span);

    let Some(cond) = self.parse_cmd_list()? else {
      bail!(self, span, "Expected a command after '{loop_kind}'");
    };
    extend_span!(span, self.tree[cond].get_span());
    self.tree.walk_tree_mut(cond, &mut Node::not_err); // disable set -e for condition commands

    if !self.check_keyword(b"do") {
      bail!(self, span, "Expected 'do' after '{loop_kind}' condition");
    }
    extend_span!(span, self.next_tk().unwrap().span);
    self.catch_separator(&mut span);

    let Some(body) = self.parse_cmd_list()? else {
      bail!(self, span, "Expected a command after 'do' in this loop");
    };

    self.catch_separator(&mut span);
    if !self.check_keyword(b"done") {
      bail!(self, span, "Expected 'done' after loop body");
    }
    extend_span!(span, self.next_tk().unwrap().span);

    self.parse_redir(&mut redirs, &mut span)?;

    self.assert_separator(&mut span)?;

    let cond_node = CondNode { cond, body };

    let node = node!(
      self,
      span,
      NdRule::LoopNode {
        kind: loop_kind,
        cond_node
      },
      redirs
    );

    Ok(Some(self.tree.insert_node(node)))
  }
  #[expect(clippy::too_many_lines)]
  pub(super) fn parse_try(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_keyword(b"try") {
      return Ok(None);
    }

    let mut span: Option<Span> = None;
    let mut redirs = vec![];

    let try_tk = self.next_tk().unwrap();
    let try_tk_span = try_tk.span.clone();

    extend_span!(span, try_tk.span);
    self.catch_separator(&mut span);

    let mut body = vec![];
    let mut body_span: Option<Span> = None;

    loop {
      if self.check_keyword(b"catch") {
        if body.is_empty() {
          bail!(
            self,
            span,
            "Expected a command before '{}' clause in '{}' block",
            "catch",
            "try"
          );
        }
        break;
      }

      if let Some(node) = self.parse_conjunction()? {
        extend_span!(span, self.tree[node].get_span());
        extend_span!(body_span, self.tree[node].get_span());
        body.push(node);
      } else {
        bail!(
          self,
          span,
          "Expected a command or '{}' clause after '{}'",
          "catch",
          "try"
        );
      }

      self.catch_separator(&mut span);
      if !self.next_tk_is_some() {
        bail!(
          self,
          span,
          "Unexpected end of input while parsing '{}' block",
          "try"
        );
      }
    }

    let body = self.tree.insert_node(node!(
      self,
      body_span,
      NdRule::List { commands: body },
      vec![]
    ));
    self.tree.walk_tree_mut(body, &mut Node::is_err);

    let try_span = self.tree[body].get_span().merge_with(&try_tk_span).unwrap();
    let try_span = if try_span.as_bytes().contains(&b'\n') {
      try_span
    } else {
      try_tk_span
    };
    let ctx = get_context(
      VarStr::from(styled_format!("in '{}' block defined here", "try")),
      &try_span,
    );

    extend_span!(span, self.next_tk().unwrap().span); // consume 'catch'

    let mut err = vec![];

    while let Some(tk) = self.peek_tk() {
      let is_sep = tk.class == TkRule::Sep;
      let is_done = tk.flags.contains(TkFlags::KEYWORD) && tk.as_bytes() == b"done";
      let is_terminator = matches!(tk.class, TkRule::Eoi | TkRule::Comment);
      if is_sep || is_done || is_terminator {
        break;
      }
      let tk = self.next_tk().unwrap();
      extend_span!(span, tk.clone().span);
      err.push(tk);
    }

    self.catch_separator(&mut span);

    if !self.check_keyword(b"do") {
      self.parse_redir(&mut redirs, &mut span)?;

      let node = node!(
        self,
        span,
        NdRule::TryNode {
          body,
          err,
          catch: None,
          ctx
        },
        redirs
      );

      return Ok(Some(self.tree.insert_node(node)));
    }

    extend_span!(span, self.next_tk().unwrap().span); // consume 'do'

    self.catch_separator(&mut span);

    let Some(catch_body) = self.parse_cmd_list()? else {
      bail!(
        self,
        span,
        "Expected a command after '{}' in this '{}' clause",
        "do",
        "catch"
      );
    };
    extend_span!(span, self.tree[catch_body].get_span());

    self.tree.walk_tree_mut(catch_body, &mut |n| n.not_err());

    if !self.check_keyword(b"done") {
      bail!(
        self,
        span,
        "Expected '{}' after '{}' clause in '{}' statement",
        "done",
        "catch",
        "try"
      );
    }
    extend_span!(span, self.next_tk().unwrap().span);

    self.parse_redir(&mut redirs, &mut span)?;
    let catch = Some(catch_body);

    let node = node!(
      self,
      span,
      NdRule::TryNode {
        body,
        err,
        catch,
        ctx
      },
      redirs
    );

    Ok(Some(self.tree.insert_node(node)))
  }
  pub(super) fn parse_defer(&mut self) -> ShResult<Option<NodeId>> {
    if !self.check_keyword(b"defer") {
      return Ok(None);
    }
    let mut span: Option<Span> = None;

    let defer_tk = self.next_tk().unwrap();
    let defer_tk_span = defer_tk.span.clone();

    extend_span!(span, defer_tk.span);

    self.catch_separator(&mut span);

    let Some(body) = self.parse_block(true)? else {
      bail!(self, span, "Expected a command after '{}' keyword", "defer");
    };

    let body_span = self.tree[body].get_span();
    let defer_span = if body_span.as_bytes().contains(&b'\n') {
      body_span.merge_with(&defer_tk_span).unwrap()
    } else {
      defer_tk_span
    };

    extend_span!(span, self.tree[body].get_span());

    let ctx = get_context(
      VarStr::from(styled_format!("in '{}' block defined here", "defer")),
      &defer_span,
    );

    self.catch_separator(&mut span);

    let node = node!(self, span, NdRule::DeferNode { body, ctx });

    Ok(Some(self.tree.insert_node(node)))
  }
}

#[cfg(test)]
mod parse_for_arith_tests {
  //! End-to-end tests for C-style arithmetic `for` loops, which take
  //! the `parse_for_arith` branch of compound parsing.

  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  #[test]
  fn basic_arith_for_loop_runs_n_iterations() {
    let g = TestGuard::new();
    test_input("for (( i=0; i<3; i=i+1 )); do echo $i; done").unwrap();
    let out = g.read_output();
    assert_eq!(out, "0\n1\n2\n", "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn arith_for_loop_with_empty_body_runs() {
    let _g = TestGuard::new();
    test_input("for (( i=0; i<3; i=i+1 )); do :; done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn arith_for_loop_with_falsy_initial_cond_skips_body() {
    let g = TestGuard::new();
    test_input("for (( i=5; i<5; i=i+1 )); do echo hit; done").unwrap();
    let out = g.read_output();
    assert_eq!(out, "");
  }

  // Note: missing `do` / `done` cause parse errors that print but
  // don't propagate to `$?` (exec_input's parser branch prints errors
  // then returns Ok). They're observable via stderr but not status,
  // so we don't include them as direct branch tests.

  #[test]
  fn arith_for_loop_with_arithmetic_in_body() {
    let g = TestGuard::new();
    test_input("total=0; for (( i=1; i<=3; i=i+1 )); do total=$((total+i)); done; echo $total")
      .unwrap();
    let out = g.read_output();
    assert!(out.contains('6'), "expected 1+2+3=6, got: {out:?}");
  }

  #[test]
  fn arith_for_loop_with_grouped_subexpr_in_clause() {
    // A clause ending in `)` (grouping) must survive header stripping: the
    // `((`/`))` delimiters come off but the inner `(1+1)` group stays.
    let g = TestGuard::new();
    test_input("for (( i=0; i<6; i=i+(1+1) )); do printf '%s ' $i; done").unwrap();
    let out = g.read_output();
    assert_eq!(out, "0 2 4 ", "got: {out:?}");
  }

  #[test]
  fn arith_for_loop_empty_clauses_still_rejected() {
    // Pinned behavior: `for ((;;))` (empty clauses) is not supported and parses
    // to an error today. Header stripping must not silently change that.
    let _g = TestGuard::new();
    // A parse error prints but leaves $? untouched, so assert no infinite loop
    // and that the body never ran (no output) rather than a status code.
    let g2 = TestGuard::new();
    test_input("for ((;;)); do echo ran; break; done").ok();
    assert_eq!(
      g2.read_output(),
      "",
      "empty-clause for-loop should not run its body"
    );
  }

  #[test]
  fn arith_for_loop_with_redirect_on_done() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    test_input(format!(
      "for (( i=0; i<2; i=i+1 )); do echo $i; done > {}",
      path.display()
    ))
    .unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "0\n1\n");
  }
}

#[cfg(test)]
mod compound_parse_error_tests {
  //! Targets uncovered error paths in compound parsing. We use
  //! `get_ast` which returns Err when parsing fails; for happy-path
  //! coverage of normally-reached-but-missed branches, we run the
  //! input end-to-end via `test_input`.

  use crate::tests::testutil::{TestGuard, get_ast, test_input};

  // ─── subshell body errors ──────────────────────────────────────────

  #[test]
  fn subshell_with_leading_pipe_errors() {
    // `parse_conjunction` returns None when next is an operator that
    // can't start a command. The else-branch error fires.
    let _g = TestGuard::new();
    assert!(get_ast("( | echo foo )").is_err());
  }

  #[test]
  fn unclosed_subshell_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("(echo foo").is_err());
  }

  // ─── brace group body errors ───────────────────────────────────────

  #[test]
  fn brace_group_with_leading_pipe_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("{ | echo foo; }").is_err());
  }

  #[test]
  fn unclosed_brace_group_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("{ echo foo").is_err());
  }

  // ─── case parsing errors ───────────────────────────────────────────

  #[test]
  fn bare_case_with_no_pattern_token_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("case").is_err());
  }

  #[test]
  fn case_immediately_followed_by_in_keyword_errors() {
    // Hits the explicit `pat_tk.span.as_str() == "in"` check.
    let _g = TestGuard::new();
    assert!(get_ast("case in foo) ;; esac").is_err());
  }

  #[test]
  fn case_missing_in_after_variable_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("case x foo) ;; esac").is_err());
  }

  #[test]
  fn empty_case_with_no_arms_parses() {
    // POSIX: `case x in esac` is valid (no pattern arms).
    let _g = TestGuard::new();
    assert!(get_ast("case x in esac").is_ok());
  }

  #[test]
  fn empty_case_runs_with_status_zero() {
    let _g = TestGuard::new();
    test_input("case x in esac").unwrap();
    assert_eq!(crate::state::Shed::get_status(), 0);
  }

  #[test]
  fn esac_as_argument_does_not_corrupt_later_arm() {
    // Regression: `esac` used as an ordinary argument inside an arm body
    // wrongly decremented case_depth, breaking the *next* arm's pattern.
    let g = TestGuard::new();
    test_input("case x in x) echo has esac word ;; y) echo second ;; esac").unwrap();
    assert_eq!(g.read_output().trim(), "has esac word");
  }

  #[test]
  fn esac_as_argument_parses() {
    let _g = TestGuard::new();
    assert!(get_ast("case x in x) echo has esac word ;; y) echo second ;; esac").is_ok());
  }

  // ─── case double-semi happy path (the *normal* break) ──────────────

  #[test]
  fn case_with_empty_arm_takes_double_semi_break() {
    // `;;` immediately after the pattern — the `if sep.has_double_semi()`
    // branch in the inner separator-scan loop fires.
    let g = TestGuard::new();
    test_input("case foo in foo) ;; esac").unwrap();
    let out = g.read_output();
    assert_eq!(out, "");
  }

  // ─── parse_time happy path ─────────────────────────────────────────

  #[test]
  fn time_wraps_a_command() {
    // Whole function body — `time` keyword consumed, inner command
    // parsed via parse_block(true), flags walked.
    let g = TestGuard::new();
    test_input("time echo hello_from_time").unwrap();
    assert!(g.read_output().contains("hello_from_time"));
  }

  #[test]
  fn time_with_no_following_command_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("time").is_err());
  }

  // ─── parse_arith happy path ────────────────────────────────────────

  #[test]
  fn standalone_arithmetic_command_parses() {
    // (( expr )) as a standalone command — exercises parse_arith
    // from check_flags(IS_ARITH) through to the Arithmetic node.
    let _g = TestGuard::new();
    test_input("(( 1 + 2 ))").unwrap();
  }

  #[test]
  fn arithmetic_command_with_trailing_arg_errors() {
    // The `matches!(self.next_tk_class(), TkRule::Str)` check after
    // parse_redir fires.
    let _g = TestGuard::new();
    assert!(get_ast("(( 1 + 2 )) extra_arg").is_err());
  }

  // ─── negation error ────────────────────────────────────────────────

  #[test]
  fn bare_bang_with_no_command_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("!").is_err());
  }

  // ─── if-then missing ───────────────────────────────────────────────

  #[test]
  fn if_without_then_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("if echo foo; echo bar; fi").is_err());
  }

  // ─── C-style for: missing do / commands / done ─────────────────────

  #[test]
  fn arith_for_without_do_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("for (( i=0; i<3; i=i+1 )); echo $i; done").is_err());
  }

  #[test]
  fn arith_for_with_empty_body_errors() {
    // parse_cmd_list returns None when next is the `done` keyword.
    let _g = TestGuard::new();
    assert!(get_ast("for (( i=0; i<3; i=i+1 )); do done").is_err());
  }

  #[test]
  fn arith_for_without_done_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("for (( i=0; i<3; i=i+1 )); do echo $i;").is_err());
  }

  // ─── array-style for: missing var / do / done / commands ───────────

  #[test]
  fn for_with_in_but_no_variable_errors() {
    // `for in 1 2 3` — first token after `for` is `in`, so vars stays
    // empty and the early bail at 637 fires.
    let _g = TestGuard::new();
    assert!(get_ast("for in 1 2 3; do echo $x; done").is_err());
  }

  #[test]
  fn for_arr_without_do_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("for x in 1 2 3; echo $x; done").is_err());
  }

  #[test]
  fn for_arr_with_empty_body_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("for x in 1 2 3; do done").is_err());
  }

  #[test]
  fn for_arr_without_done_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("for x in 1 2 3; do echo $x;").is_err());
  }

  // ─── while/until: missing command after keyword / missing do ───────

  #[test]
  fn while_with_no_condition_command_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("while ; do echo; done").is_err());
  }

  #[test]
  fn until_with_no_condition_command_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("until ; do echo; done").is_err());
  }

  #[test]
  fn while_without_do_after_condition_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("while true; echo; done").is_err());
  }

  #[test]
  fn until_without_do_after_condition_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("until false; echo; done").is_err());
  }

  #[test]
  fn for_empty_array_succeeds() {
    let _g = TestGuard::new();
    assert!(get_ast("for i in; do true; done").is_ok());
  }
}
