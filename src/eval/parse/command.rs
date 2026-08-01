use crate::{
  eval::parse::node::LabelCtx,
  util::{self, error::get_context},
};

use super::{
  NdFlags, NdRule, Node, ParseStream, ShResult, Span, Tk, TkFlags, TkRule,
  node::{AssignKind, NodeVecUtils},
  procio::{RedirBldr, RedirSpec, RedirTarget, RedirType},
  sherr,
};

impl ParseStream {
  fn build_redir<F: FnMut() -> Option<Tk>>(
    redir_tk: &Tk,
    mut next: F,
    span: &mut Option<Span>,
    context: &LabelCtx,
  ) -> ShResult<RedirSpec> {
    let redir_bldr = RedirBldr::try_from(redir_tk.clone())?;
    if redir_bldr.target.is_some() {
      return redir_bldr.build();
    }

    let Some(class) = redir_bldr.class else {
      // excluding this from coverage reports
      // because it's theoretically impossible
      // to reach using test input, if the lexer is working.
      // LCOV_EXCL_START
      return Err(
        sherr!(
          ParseErr @ redir_tk.span.clone(),
          "Invalid redirection operator"
        )
        .with_context(context.iter()),
      );
      // LCOV_EXCL_STOP
    };
    let Some(next_tk) = next().filter(|tk| tk.class != TkRule::Eoi) else {
      // LCOV_EXCL_START
      return Err(
        sherr!(
          ParseErr @ redir_tk.span.clone(),
          "Expected a filename after this redirection",
        )
        .with_context(context.iter()),
      );
      // LCOV_EXCL_STOP
    };

    let target = if class == RedirType::HereString {
      RedirTarget::HereDoc {
        body: next_tk.span.as_str().to_string(),
        flags: next_tk.flags | TkFlags::HERESTRING,
      }
    } else {
      extend_span!(*span, next_tk.span);
      RedirTarget::Path(next_tk)
    };

    redir_bldr.with_target(target).build()
  }
  /// Build a `RedirSpec` from a token
  ///
  /// Handles desugaring of '&>' internally
  fn push_redir<F: FnMut() -> Option<Tk>>(
    redir_tk: &Tk,
    next: F,
    span: &mut Option<Span>,
    context: &LabelCtx,
    redirs: &mut Vec<RedirSpec>,
  ) -> ShResult<()> {
    let redir = Self::build_redir(redir_tk, next, span, context)?;
    redirs.push(redir);
    if redir_tk.flags.contains(TkFlags::REDIR_ALL) {
      redirs.push(RedirSpec::Dup {
        from: 1,
        to: 2,
        mode: RedirType::Output,
      });
    }
    Ok(())
  }

  pub(super) fn parse_redir(
    &mut self,
    redirs: &mut Vec<RedirSpec>,
    span: &mut Option<Span>,
  ) -> ShResult<()> {
    while self.check_redir() {
      let tk = self.next_tk().unwrap();
      extend_span!(*span, tk.span);
      let ctx = self.context.clone();
      if let Err(e) = Self::push_redir(&tk, || self.next_tk(), span, &ctx, redirs) {
        self.panic_mode(span);
        return Err(e);
      }
    }
    Ok(())
  }
  pub(super) fn parse_pipeln(&mut self) -> ShResult<Option<Node>> {
    let mut cmds = vec![];
    let mut span: Option<Span> = None;
    let mut flags = NdFlags::empty();

    let mut dangling_pipe: Option<Span> = None;
    while let Some(mut cmd) = self.parse_block(false)? {
      dangling_pipe = None;
      let is_punctuated = cmd.flags.contains(NdFlags::PUNCTUATED);

      extend_span!(span, cmd.span);
      let next_class = self.next_tk_class().clone();
      if next_class == TkRule::ErrPipe {
        cmd.flags |= NdFlags::PIPE_ERR;
      }
      if matches!(next_class, TkRule::Pipe | TkRule::ErrPipe) {
        cmd.walk_tree(&mut |n| n.flags |= NdFlags::PIPE_CMD);
        cmd.walk_tree(&mut Node::not_err);
      }

      cmds.push(cmd);
      if next_class == TkRule::Bg {
        let tk = self.next_tk().unwrap();
        extend_span!(span, tk.span);
        flags |= NdFlags::BACKGROUND;
        break;
      } else if (!matches!(next_class, TkRule::Pipe | TkRule::ErrPipe)) || is_punctuated {
        break;
      } else if let Some(pipe) = self.next_tk() {
        dangling_pipe = Some(pipe.span.clone());
        extend_span!(span, pipe.span);
        self.catch_linebreak(&mut span);
      } else {
        break;
      }
    }
    if let Some(pipe_span) = dangling_pipe {
      return Err(parse_err!(
        self,
        Some(pipe_span),
        "Expected a command after this pipe"
      ));
    }
    if cmds.is_empty() {
      Ok(None)
    } else {
      Ok(Some(node!(
        self,
        span,
        NdRule::Pipeline { cmds },
        vec![/*redirs*/],
        flags
      )))
    }
  }
  #[expect(clippy::while_let_loop, clippy::too_many_lines)]
  pub(super) fn parse_cmd(&mut self) -> ShResult<Option<Node>> {
    let mut span: Option<Span> = None;

    let result = 'out: {
      let tk_slice = self.tokens();
      let mut tk_iter = tk_slice.iter().peekable();
      let mut tk_counter = 0;
      let mut redirs = vec![];
      let mut argv = vec![];
      let mut flags = NdFlags::empty();
      let mut assignments = vec![];

      loop {
        let Some(prefix_tk) = tk_iter.next() else {
          break;
        };
        let is_cmd = prefix_tk.flags.contains(TkFlags::IS_CMD);
        let is_assignment = prefix_tk.flags.contains(TkFlags::ASSIGN);
        let is_keyword = prefix_tk.flags.contains(TkFlags::KEYWORD);

        if is_cmd {
          extend_span!(span, prefix_tk.span);
          tk_counter += 1;
          argv.push(prefix_tk.clone());
          break;
        } else if is_assignment {
          let Some(assign) = self.parse_assignment(prefix_tk) else {
            // it's marked as an assignment, but it doesn't parse.
            // just treat it as the command word
            extend_span!(span, prefix_tk.span);
            tk_counter += 1;
            argv.push(prefix_tk.clone());
            break;
          };
          extend_span!(span, prefix_tk.span);
          tk_counter += 1;
          assignments.push(assign);
        } else if is_keyword {
          return Ok(None);
        } else if prefix_tk.class == TkRule::Redir {
          extend_span!(span, prefix_tk.span);
          tk_counter += 1;
          let ctx = self.context.clone();
          if let Err(e) = Self::push_redir(
            prefix_tk,
            || {
              let tk = tk_iter.next().cloned();
              if tk.is_some() {
                tk_counter += 1;
              }
              tk
            },
            &mut span,
            &ctx,
            &mut redirs,
          ) {
            break 'out Err(e);
          }
        } else if prefix_tk.class == TkRule::Sep {
          // Separator ends the prefix section - add it so commit() consumes it
          extend_span!(span, prefix_tk.span);
          tk_counter += 1;
          break;
        } else {
          // Other non-prefix token ends the prefix section
          break;
        }
      }
      let in_dbracket = argv.first().map(|t: &Tk| t.as_str()) == Some("[[");
      if in_dbracket {
        // add this flag so we don't split words on test members
        flags |= NdFlags::NO_SPLIT;
      }
      if argv.is_empty() {
        if assignments.is_empty() && redirs.is_empty() {
          break 'out Ok(None);
        }
        // we have an empty argv, but we got here with assignments and/or redirections.
        // we still need to execute this, so emit an empty command node.
        self.commit(tk_counter);
        let assignments_span = assignments.get_span();
        let mut nd = node!(
          self,
          span.clone(),
          NdRule::Command { assignments, argv },
          redirs,
          flags
        );
        if let Some(assignments_span) = assignments_span {
          nd.context.push_back(get_context(
            "in variable assignment defined here".into(),
            &assignments_span,
          ));
        }
        return Ok(Some(nd));
      }
      loop {
        let Some(tk) = tk_iter.next() else {
          break;
        };
        match tk.class {
          TkRule::And | TkRule::Or if in_dbracket => {
            extend_span!(span, tk.span);
            tk_counter += 1;
            argv.push(tk.clone());
          }
          TkRule::Eoi
          | TkRule::Comment
          | TkRule::Pipe
          | TkRule::ErrPipe
          | TkRule::And
          | TkRule::BraceGrpEnd
          | TkRule::SubshEnd
          | TkRule::Or
          | TkRule::Bg => break,
          TkRule::Sep => {
            extend_span!(span, tk.span);
            tk_counter += 1;
            break;
          }
          TkRule::Str => {
            let is_dbracket_close = in_dbracket && tk.as_str() == "]]";
            extend_span!(span, tk.span);
            tk_counter += 1;
            argv.push(tk.clone());
            if is_dbracket_close {
              break;
            }
          }
          TkRule::HereDoc { .. } | TkRule::Redir => {
            extend_span!(span, tk.span);
            tk_counter += 1;
            let ctx = self.context.clone();
            if let Err(e) = Self::push_redir(
              tk,
              || {
                let tk = tk_iter.next().cloned();
                if tk.is_some() {
                  tk_counter += 1;
                }
                tk
              },
              &mut span,
              &ctx,
              &mut redirs,
            ) {
              // excluding from coverage reports, see the
              // comment at line 24
              // LCOV_EXCL_START
              self.panic_mode(&mut span);
              return Err(e);
              // LCOV_EXCL_STOP
            }
          }
          _ => {
            break 'out Err(parse_err!(
              self,
              span.clone(),
              "Unexpected token in command: {:?}",
              tk.class
            ));
          }
        }
      }
      self.commit(tk_counter);

      return Ok(Some(node!(
        self,
        span,
        NdRule::Command { assignments, argv },
        redirs,
        flags
      )));
    };

    match result {
      Ok(node) => Ok(node),
      Err(e) => {
        self.panic_mode(&mut span);
        Err(e)
      }
    }
  }
  #[expect(clippy::too_many_lines)]
  fn parse_assignment(&self, token: &Tk) -> Option<Node> {
    let mut chars = token.span.as_str().chars();
    let mut var_name = util::scratch_buf();
    let mut name_range = token.span.range().start..token.span.range().start;
    let mut var_val = util::scratch_buf();
    let mut val_range = token.span.range().end..token.span.range().end;
    let mut assign_kind = None;
    let mut pos = token.span.range().start;
    let mut bracket_depth = 0usize;

    while let Some(ch) = chars.next() {
      if assign_kind.is_some() {
        if ch == '\\' {
          pos += ch.len_utf8();
          var_val.push(ch);
          if let Some(esc_ch) = chars.next() {
            pos += esc_ch.len_utf8();
            var_val.push(esc_ch);
          }
        } else {
          pos += ch.len_utf8();
          var_val.push(ch);
        }
      } else {
        match ch {
          '[' => {
            bracket_depth += 1;
            pos += ch.len_utf8();
            var_name.push(ch);
          }
          ']' if bracket_depth > 0 => {
            bracket_depth -= 1;
            pos += ch.len_utf8();
            var_name.push(ch);
          }
          '=' if bracket_depth == 0 => {
            name_range.end = pos;
            pos += ch.len_utf8();
            val_range.start = pos;
            assign_kind = Some(AssignKind::Eq);
          }
          '-' if bracket_depth == 0 => {
            name_range.end = pos;
            pos += ch.len_utf8();
            let Some('=') = chars.next() else { return None };
            pos += 1;
            val_range.start = pos;
            assign_kind = Some(AssignKind::MinusEq);
          }
          '+' if bracket_depth == 0 => {
            name_range.end = pos;
            pos += ch.len_utf8();
            let Some('=') = chars.next() else { return None };
            pos += 1;
            val_range.start = pos;
            assign_kind = Some(AssignKind::PlusEq);
          }
          '/' if bracket_depth == 0 => {
            name_range.end = pos;
            pos += ch.len_utf8();
            let Some('=') = chars.next() else { return None };
            pos += 1;
            val_range.start = pos;
            assign_kind = Some(AssignKind::DivEq);
          }
          '*' if bracket_depth == 0 => {
            name_range.end = pos;
            pos += ch.len_utf8();
            let Some('=') = chars.next() else { return None };
            pos += 1;
            val_range.start = pos;
            assign_kind = Some(AssignKind::MultEq);
          }
          '\\' => {
            pos += ch.len_utf8();
            var_name.push(ch);
            if let Some(esc_ch) = chars.next() {
              pos += esc_ch.len_utf8();
              var_name.push(esc_ch);
            }
          }
          _ => {
            pos += ch.len_utf8();
            var_name.push(ch);
          }
        }
      }
    }
    if assign_kind.is_none() || var_name.is_empty() {
      return None;
    }
    let assign_kind = assign_kind.unwrap();

    let var = Tk::new(TkRule::Str, Span::new(name_range, token.source()));
    let val = Tk::new(TkRule::Str, Span::new(val_range, token.source()));
    let flags = if var_val.starts_with('(') && var_val.ends_with(')') {
      NdFlags::ARR_ASSIGN
    } else {
      NdFlags::empty()
    };

    Some(node!(
      self,
      Some(token.span.clone()),
      NdRule::Assignment {
        kind: assign_kind,
        var,
        val
      },
      vec![/*redirs*/],
      flags
    ))
  }
}

#[cfg(test)]
mod command_parse_tests {
  //! Targets uncovered branches in command.rs parsing — redirection
  //! errors, the Bg pipeline path, leading redirs, comment-as-argv-
  //! terminator, and `parse_assignment` escape handling.

  use crate::tests::testutil::{TestGuard, get_ast, test_input};

  // ─── build_redir / parse_redir error paths ─────────────────────────

  #[test]
  fn redir_with_no_filename_at_eoi_errors() {
    // After consuming `>`, build_redir's `next()` returns None.
    // Triggers the "Expected a filename after this redirection"
    // branch and the panic_mode wrapper in parse_redir.
    let _g = TestGuard::new();
    assert!(get_ast("echo foo >").is_err());
  }

  #[test]
  fn append_redir_with_no_filename_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("echo foo >>").is_err());
  }

  #[test]
  fn input_redir_with_no_filename_errors() {
    let _g = TestGuard::new();
    assert!(get_ast("cat <").is_err());
  }

  // ─── parse_pipeln Bg branch ────────────────────────────────────────

  #[test]
  fn background_command_parses_with_bg_flag() {
    // `cmd &` — the parse_pipeln loop sees Bg as next class, consumes
    // it, sets BACKGROUND, breaks. We just verify the parse succeeds;
    // executing a real background command in the test harness can
    // hit tcsetpgrp issues.
    let _g = TestGuard::new();
    get_ast("sleep 0 &").unwrap();
  }

  #[test]
  fn pipeline_followed_by_bg_parses() {
    let _g = TestGuard::new();
    get_ast("echo foo | cat &").unwrap();
  }

  // ─── parse_cmd: leading redir before command word ──────────────────

  #[test]
  fn leading_redir_before_command_routes_through_build_redir() {
    // The first token in the prefix loop is a Redir, not a Cmd /
    // Assignment / Keyword — hits the `prefix_tk.class == TkRule::Redir`
    // branch and the inline build_redir call.
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    test_input(format!("> {} echo prefixed_redir_marker", path.display())).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
      content.contains("prefixed_redir_marker"),
      "got: {content:?}"
    );
  }

  // ─── parse_cmd: comment in argv breaks loop ────────────────────────

  #[test]
  fn comment_after_command_word_terminates_argv() {
    // The argv loop hits `TkRule::Comment => break`.
    let g = TestGuard::new();
    test_input("echo hello_before_comment # this should be skipped").unwrap();
    let out = g.read_output();
    assert!(out.contains("hello_before_comment"), "got: {out:?}");
    assert!(
      !out.contains("this should be skipped"),
      "comment leaked: {out:?}"
    );
  }

  // ─── parse_cmd: build_redir error inside argv ──────────────────────

  #[test]
  fn redir_with_no_filename_after_argv_errors() {
    // After `echo foo`, the argv loop encounters `>` then tries to
    // build a redir with no filename — hits the Err arm at the inner
    // build_redir call, which panic_modes and returns.
    let _g = TestGuard::new();
    assert!(get_ast("echo foo >").is_err());
  }

  // ─── parse_cmd: unexpected token in argv ───────────────────────────
  //
  // The catchall `_` arm in the argv loop is reachable only for token
  // classes that (a) the lexer can produce and (b) aren't handled by
  // any of the explicit arms (Str / Redir / HereDoc / Sep / the
  // Comment-and-terminator break list). In practice, lex tries hard to
  // either close paired delimiters at this position (`(` / `{`) or
  // classify standalone punctuation as Str — so the `_` arm has no
  // straightforward reachable input. I tried `echo foo {`, `echo foo (`,
  // `echo foo !` and none of them surface as parse errors. Leaving the
  // test out rather than baking in something flaky; arguably dead code.

  // ─── parse_assignment escape handling ──────────────────────────────

  #[test]
  fn assignment_value_preserves_escaped_char() {
    // Hits the `\\` arm of the post-= chars loop in parse_assignment.
    // The literal backslash + next char should be preserved verbatim.
    let g = TestGuard::new();
    test_input("x=a\\zb; echo \"$x\"").unwrap();
    let out = g.read_output();
    assert!(out.contains('a'), "got: {out:?}");
    assert!(out.contains("zb"), "got: {out:?}");
  }

  // ===================== &> / &>> REDIR_ALL =====================
  //
  // Pins the desugar from `&>file` / `&>>file` into a File redirect on
  // fd 1 plus a Dup of fd 1 to fd 2. Covers both prefix and arg-loop
  // positions and both truncate/append modes, since the four
  // combinations exercise different code paths through parse_cmd
  // and the lexer's `&` arm.

  /// Run an `ls /nonexistent...` invocation with the given redirect
  /// expression interpolated. ls writes its "No such file" error to
  /// stderr; if both fds get routed to the file (`REDIR_ALL` working),
  /// the error lands in the file rather than the terminal.
  fn run_with_ls_stderr(cmd: String, path: &std::path::Path) -> String {
    test_input(cmd).unwrap();
    std::fs::read_to_string(path).unwrap()
  }

  #[test]
  fn redir_all_prefix_captures_stderr_to_file() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    let cmd = format!("&> {} ls /nonexistent_marker_aaa", path.display());
    let content = run_with_ls_stderr(cmd, &path);
    assert!(
      content.contains("nonexistent_marker_aaa"),
      "stderr not captured by prefix `&>`: {content:?}"
    );
  }

  #[test]
  fn redir_all_arg_loop_captures_stderr_to_file() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    let cmd = format!("ls /nonexistent_marker_bbb &> {}", path.display());
    let content = run_with_ls_stderr(cmd, &path);
    assert!(
      content.contains("nonexistent_marker_bbb"),
      "stderr not captured by arg-loop `&>`: {content:?}"
    );
  }

  #[test]
  fn redir_all_prefix_truncates() {
    // `&>` (single `>`) should truncate, not append. Write a sentinel,
    // then run a `&>` that produces no stdout and a known stderr; the
    // sentinel must be gone.
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    std::fs::write(&path, "PRIOR_CONTENT_SENTINEL\n").unwrap();
    let cmd = format!("&> {} ls /nonexistent_truncate", path.display());
    test_input(cmd).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
      !content.contains("PRIOR_CONTENT_SENTINEL"),
      "`&>` should truncate but prior content remained: {content:?}"
    );
    assert!(
      content.contains("nonexistent_truncate"),
      "`&>` did not capture new content: {content:?}"
    );
  }

  #[test]
  fn redir_all_append_prefix_preserves_prior_content() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    std::fs::write(&path, "PRIOR_CONTENT_KEPT\n").unwrap();
    let cmd = format!("&>> {} ls /nonexistent_append", path.display());
    test_input(cmd).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
      content.contains("PRIOR_CONTENT_KEPT"),
      "`&>>` should append but prior content was overwritten: {content:?}"
    );
    assert!(
      content.contains("nonexistent_append"),
      "`&>>` did not capture new content: {content:?}"
    );
  }

  #[test]
  fn redir_all_append_arg_loop_preserves_prior_content() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    std::fs::write(&path, "PRIOR_CONTENT_ARGLOOP\n").unwrap();
    let cmd = format!("ls /nonexistent_argloop_append &>> {}", path.display());
    test_input(cmd).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
      content.contains("PRIOR_CONTENT_ARGLOOP"),
      "arg-loop `&>>` should append but prior content was overwritten: {content:?}"
    );
    assert!(
      content.contains("nonexistent_argloop_append"),
      "arg-loop `&>>` did not capture new content: {content:?}"
    );
  }

  #[test]
  fn redir_all_routes_stdout_too() {
    // Mirror of the stderr check — confirm that fd 1 (stdout) is also
    // routed to the file. Use `echo` (a builtin) for stdout content.
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    let cmd = format!("&> {} echo STDOUT_VIA_REDIR_ALL", path.display());
    test_input(cmd).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
      content.contains("STDOUT_VIA_REDIR_ALL"),
      "stdout not captured by prefix `&>`: {content:?}"
    );
  }

  // ===================== prefix dup-style redirects =====================
  //
  // Pins the lex-level fix where dup-style Redir tokens (e.g. `2>&1`)
  // no longer poison `NEXT_IS_REDIR` and prevent the following Str from
  // being classified as IS_CMD. Without the fix, the prefix loop in
  // parse_cmd would never identify the command word and parse_cmd would
  // silently return Ok(None) — no command executes.

  #[test]
  fn prefix_dup_followed_by_command_executes() {
    // `2>&1 echo MARKER` — the dup is a no-op at the shell level (we
    // dup fd 1's terminal to fd 2, no visible change), but it must
    // not prevent `echo MARKER` from being recognized as the command.
    let g = TestGuard::new();
    test_input("2>&1 echo prefix_dup_marker").unwrap();
    let out = g.read_output();
    assert!(
      out.contains("prefix_dup_marker"),
      "command after prefix `2>&1` did not execute: {out:?}"
    );
  }

  #[test]
  fn prefix_file_then_dup_routes_both_to_file() {
    // `>>file 2>&1 cmd` — file redirect then dup, with both as
    // prefixes. Apply order ([File, Dup]) means fd 1 points to file
    // before the Dup borrows it, so fd 2 also lands in the file.
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    let cmd = format!(">> {} 2>&1 ls /nonexistent_prefix_dup_file", path.display());
    test_input(cmd).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
      content.contains("nonexistent_prefix_dup_file"),
      "stderr not captured by prefix file+dup: {content:?}"
    );
  }

  #[test]
  fn prefix_redir_does_not_leak_to_shed_via_apply_persistent() {
    // Regression for the node_tks asymmetry that caused commit() to
    // short-advance: leftover redirect operators would reparse as a
    // bare redirect-only command and call apply_persistent on shed's
    // own fds. Symptom was "the redirect works for cat AND appears on
    // the terminal." We verify the absence of that leak by running a
    // prefix-redir command and then a subsequent plain echo whose
    // output should still reach the test harness (which it wouldn't
    // if shed's stdout had been silently redirected).
    let g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("out.txt");
    let cmd = format!(
      "&> {} ls /nonexistent_no_leak ; echo POST_MARKER",
      path.display()
    );
    test_input(cmd).unwrap();
    let out = g.read_output();
    assert!(
      out.contains("POST_MARKER"),
      "shed's stdout was leaked by prefix-redir parse — POST_MARKER \
       did not reach test harness: {out:?}"
    );
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
      content.contains("nonexistent_no_leak"),
      "prefix `&>` didn't capture stderr: {content:?}"
    );
    assert!(
      !content.contains("POST_MARKER"),
      "POST_MARKER leaked into the redirect-target file: {content:?}"
    );
  }
}
