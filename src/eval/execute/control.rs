//! Execution of control flow structures
//!
//! This module handles the execution of control flow structures in shell scripts.

use unicode_segmentation::UnicodeSegmentation;

use crate::{
  defer, errln,
  expand::case,
  procio::{RedirResult, RedirSet},
  sherr, shopt, shopt_mut, signal,
  state::{
    Shed,
    meta::{CmdTimer, MetaTab},
    vars::{DeferredAst, VarFlags, VarKind, VarStr, VarStrSliceExt},
  },
  util::{
    error::{ShErr, ShErrKind, ShResult, ShResultExt},
    guards,
  },
};

use super::{
  lex::Tk,
  parse::{
    ast::{Ast, NodeId},
    node::{CaseNode, CondNode, ConjunctNode, ConjunctOp, LoopKind, NdRule},
  },
};

/// Run a command that was registered using the `defer` builtin
///
/// Error handling happens inside the function, so this is infallible.
/// If the function returns false, that means we should stop executing deferred cmds
pub(crate) fn dispatch_deferred_cmd(cmd: DeferredAst) -> bool {
  let DeferredAst { ast, ctx } = cmd;

  let outcome = {
    let _frame = Shed::push_call_frame(vec![ctx]);
    super::Dispatcher::new("defer".into()).begin_dispatch(&ast)
  };

  if let Err(e) = outcome {
    let maybe_flowctl = match e.kind() {
      ShErrKind::ErrInterrupt => {
        // set -e aborted the execution
        // so the error has already technically been handled
        return true;
      }

      ShErrKind::FuncReturn(_) => Some("return"),
      ShErrKind::LoopBreak(_) => Some("break"),
      ShErrKind::LoopContinue(_) => Some("continue"),

      ShErrKind::CleanExit(code) => {
        signal::request_exit(*code);
        return false;
      }

      _ => None,
    };

    if let Some(flowctl) = maybe_flowctl {
      sherr!(
        SyntaxErr,
        "'{flowctl}' cannot be used inside a deferred command"
      )
      .print_error();
    } else {
      e.print_error();
    }
  }
  true
}

impl super::Dispatcher {
  pub(super) fn exec_conjunction(&mut self, tree: &Ast, conj: NodeId) -> ShResult<()> {
    let span = tree.span_for(conj);
    let NdRule::Conjunction { elements } = &tree[conj].class else {
      unreachable!()
    };

    if Shed::shopts(|o| o.set.verbose) {
      let command = span.to_str_lossy();
      errln!("{command}");
    }

    let mut elem_iter = elements.ids();
    let mut skip = false;
    while let Some(element) = elem_iter.next() {
      let ConjunctNode { cmd, operator } = &tree[element];
      if !skip {
        self.dispatch_node(tree, *cmd)?;
      }

      let status = Shed::get_status();
      skip = match operator {
        ConjunctOp::And => status != 0,
        ConjunctOp::Or => status == 0,
        ConjunctOp::Null => break,
      };
    }
    Ok(())
  }
  pub(super) fn exec_defer(tree: &Ast, node: NodeId) -> ShResult<()> {
    let NdRule::DeferNode { body, ctx } = &tree[node].class else {
      unreachable!()
    };
    let mut body = tree.break_off(*body);
    let Some(root) = body.get_root() else {
      return Err(sherr!(
          InternalErr @ tree.span_for(node),
          "defer: No root node found",
      ));
    };

    // something like `defer shopt core.nullglob=$(shopt core.nullglob)`
    // needs to expand at registration time, and not at
    // execution time.
    let mut err: Option<ShErr> = None;
    body.walk_tree_mut(root, &mut |id, tree| {
      if err.is_some() {
        return;
      }
      if let Err(e) = tree.eager_expand(id) {
        err = Some(e);
      }
    });
    if let Some(e) = err {
      return Err(e);
    }

    Shed::vars_mut(|v| v.cur_scope_mut().defer_cmd(body, tree[*ctx].clone()));
    Ok(())
  }
  pub(super) fn exec_try(&mut self, tree: &Ast, node: NodeId) -> ShResult<()> {
    let try_blame = tree.span_for(node);
    let NdRule::TryNode {
      body,
      err,
      catch,
      ctx,
    } = &tree[node].class
    else {
      unreachable!()
    };
    let mut trace = tree.context_for(node).to_vec();
    trace.push(tree[*ctx].clone());

    // enable set -e -o pipefail temporarily
    let errexit = shopt!(set.errexit);
    let pipefail = shopt!(set.pipefail);
    shopt_mut!(set.errexit = true);
    shopt_mut!(set.pipefail = true);
    defer!(shopt_mut!(set.errexit = errexit));
    defer!(shopt_mut!(set.pipefail = pipefail));

    let outcome = {
      let _frame = Shed::push_call_frame(vec![tree[*ctx].clone()]);
      self.dispatch_node(tree, *body)
    };

    match outcome {
      Ok(()) => Ok(()),
      Err(e) => {
        if e.is_flow_control() {
          return Err(e);
        }

        let blame = e.src_span().cloned().unwrap_or(try_blame);

        if !err.is_empty() {
          let mut msg_parts = Vec::with_capacity(err.len());
          for tk in err.ids() {
            msg_parts.push(tree[tk].expand_no_split()?);
          }
          let msg = msg_parts.join_with(" ");

          ShErr::at(ShErrKind::TryFailed, blame, msg)
            .with_context(trace.iter())
            .print_error();
        }

        if let Some(catch) = catch
          && let Err(e) = self.dispatch_node(tree, *catch)
        {
          if e.is_flow_control() {
            return Err(e);
          }
          e.print_error();
        }
        Shed::set_status(0);

        Ok(())
      }
    }
  }
  pub(super) fn exec_negated(&mut self, tree: &Ast, node: NodeId) -> ShResult<()> {
    let NdRule::Negate { cmd } = &tree[node].class else {
      unreachable!()
    };
    self.dispatch_node(tree, *cmd)?;
    let status = Shed::get_status();
    Shed::set_status_from_bool(status != 0);

    Ok(())
  }
  pub(super) fn exec_timed(&mut self, tree: &Ast, node: NodeId) -> ShResult<()> {
    let NdRule::Timed { cmd } = &tree[node].class else {
      unreachable!();
    };

    self.timer_stack.push(Some(CmdTimer::new()?));
    let res = self.dispatch_node(tree, *cmd);
    self.timer_stack.pop();
    res
  }
  /// Run a compound command.
  ///
  /// Handles all of the necessary I/O plumbing and fork dispatch.
  pub(super) fn run_compound<F>(
    &mut self,
    name: &str,
    node: NodeId,
    tree: &Ast,
    mut logic: F,
  ) -> ShResult<()>
  where
    F: FnMut(&mut Self, &Ast) -> ShResult<()>,
  {
    let fork_builtins = Shed::meta_mut(MetaTab::take_fork);
    let blame = tree.span_for(node);
    let node = &tree[node];
    let redirs = &tree[node.redirs];

    let redirs = RedirSet::from(redirs);
    let guard = match redirs.try_apply(false) {
      RedirResult::Applied(guard) => Some(guard),
      RedirResult::NoRedirs => None,
      RedirResult::Skipped => return Ok(()),
      RedirResult::Error(e) => return Err(e),
    };

    if fork_builtins {
      log::trace!("Forking compound command: {name}");
      self.run_fork(name.as_bytes(), |s| {
        super::catch_exit(|| logic(s, tree), super::exit_with);
      })?;
      Ok(())
    } else {
      logic(self, tree)
        .try_blame(blame)
        .map_err(|e| e.with_redirs(guard))
    }
  }
  pub(super) fn exec_brc_grp(&mut self, tree: &Ast, brc_grp_id: NodeId) -> ShResult<()> {
    let brc_grp = &tree[brc_grp_id];
    let NdRule::BraceGrp { body } = &brc_grp.class else {
      unreachable!()
    };

    let _timer = self.take_timer();
    let brc_grp_logic = |s: &mut Self, tree: &Ast| -> ShResult<()> {
      let _guard = guards::shared_scope_guard();
      s.dispatch_node(tree, *body)?;

      Ok(())
    };

    self.run_compound("brace_group", brc_grp_id, tree, brc_grp_logic)
  }
  pub(super) fn exec_subsh(&mut self, tree: &Ast, subsh_id: NodeId) -> ShResult<()> {
    let subsh = &tree[subsh_id];
    let NdRule::Subshell { body } = &subsh.class else {
      unreachable!()
    };
    let span = tree.span_for(*body);

    let redirs = RedirSet::from(&tree[subsh.redirs]);
    let _guard = match redirs.try_apply(false) {
      RedirResult::Applied(guard) => Some(guard),
      RedirResult::NoRedirs => None,
      RedirResult::Skipped => return Ok(()),
      RedirResult::Error(e) => return Err(e),
    };

    let body_raw = span.to_str_lossy();
    let body_display = body_raw.graphemes(true).take(70).collect::<String>();
    let name = format!("( {body_display} )");

    self.run_fork(name.as_bytes(), |s| {
      super::catch_exit(|| s.dispatch_node(tree, *body), super::exit_with);
    })?;

    Ok(())
  }
  pub(super) fn exec_case(&mut self, tree: &Ast, case_stmt_id: NodeId) -> ShResult<()> {
    let case_stmt = &tree[case_stmt_id];
    let NdRule::CaseNode {
      pattern,
      case_blocks,
    } = &case_stmt.class
    else {
      unreachable!()
    };

    let case_logic = |s: &mut Self, tree: &Ast| -> ShResult<()> {
      let exp_pattern = tree[*pattern].clone().expand()?;
      let pattern_raw = exp_pattern
        .get_words()
        .first()
        .map(ToString::to_string)
        .unwrap_or_default();

      Shed::set_status(0);
      'outer: for block in case_blocks.ids() {
        let CaseNode { patterns, body } = &tree[block];

        for pattern in patterns {
          let pattern_exp = case::expand_case_pattern(pattern.span.as_bytes())?;
          if pattern_exp.is_empty() {
            if pattern_raw.is_empty() {
              let _guard = guards::shared_scope_guard();
              s.dispatch_node(tree, *body)?;
              break 'outer;
            }
          } else {
            let pattern = Shed::meta_mut(|m| m.get_glob(pattern_exp.as_bytes()));
            if pattern.is_match(pattern_raw.as_bytes()) {
              let _guard = guards::shared_scope_guard();
              s.dispatch_node(tree, *body)?;
              break 'outer;
            }
          }
        }
      }

      Ok(())
    };

    self.run_compound("case", case_stmt_id, tree, case_logic)
  }
  pub(super) fn exec_loop(&mut self, tree: &Ast, loop_stmt_id: NodeId) -> ShResult<()> {
    let loop_stmt = &tree[loop_stmt_id];
    let NdRule::LoopNode { kind, cond_node } = &loop_stmt.class else {
      unreachable!();
    };

    let loop_logic = |s: &mut Self, tree: &Ast| -> ShResult<()> {
      let keep_going = |kind: LoopKind, status: i32| -> bool {
        match kind {
          LoopKind::While => status == 0,
          LoopKind::Until => status != 0,
        }
      };
      let CondNode { cond, body } = tree[*cond_node];
      let mut last_body_status = 0;
      'outer: loop {
        {
          // condition scope
          let _guard = guards::shared_scope_guard();
          if let Err(mut e) = s.dispatch_node(tree, cond) {
            match e.kind_mut() {
              ShErrKind::LoopBreak(count) => {
                if *count == 1 || Shed::meta(MetaTab::loop_depth) <= 1 {
                  Shed::set_status(0);
                  break 'outer;
                }
                *count -= 1;
                return Err(e);
              }
              ShErrKind::LoopContinue(count) => {
                if *count > 1 && Shed::meta(MetaTab::loop_depth) > 1 {
                  *count -= 1;
                  return Err(e);
                }
                Shed::set_status(0);
                continue 'outer;
              }
              _ => {
                Shed::set_status(1);
                return Err(e);
              }
            }
          }
        }

        let status = Shed::get_status();

        {
          // body scope
          let _guard = guards::shared_scope_guard();
          if !keep_going(*kind, status) {
            Shed::set_status(last_body_status);
            break;
          }
          if let Err(mut e) = s.dispatch_node(tree, body) {
            match e.kind_mut() {
              ShErrKind::LoopBreak(count) => {
                if *count == 1 || Shed::meta(MetaTab::loop_depth) <= 1 {
                  Shed::set_status(0);
                  break 'outer;
                }
                *count -= 1;
                return Err(e);
              }
              ShErrKind::LoopContinue(count) => {
                if *count > 1 && Shed::meta(MetaTab::loop_depth) > 1 {
                  *count -= 1;
                  return Err(e);
                }
                Shed::set_status(0);
              }
              _ => return Err(e),
            }
          }
          last_body_status = Shed::get_status();
        }
      }

      Ok(())
    };

    let _loop_guard = Shed::meta_mut(MetaTab::enter_loop);
    self.run_compound("loop", loop_stmt_id, tree, loop_logic)
  }
  pub(super) fn exec_for_arith(&mut self, tree: &Ast, for_stmt_id: NodeId) -> ShResult<()> {
    let for_stmt = &tree[for_stmt_id];
    let NdRule::ForArith {
      init,
      cond,
      step,
      body,
    } = &for_stmt.class
    else {
      unreachable!();
    };
    let for_logic = |s: &mut Self, tree: &Ast| -> ShResult<()> {
      if let Some(init_node) = init {
        s.dispatch_node(tree, *init_node)?;
      }

      let mut last_body_status = 0;
      'outer: loop {
        if let Some(cond_node) = cond {
          if let Err(e) = s.dispatch_node(tree, *cond_node) {
            Shed::set_status(1);
            return Err(e);
          }
          let status = Shed::get_status();
          if status != 0 {
            Shed::set_status(last_body_status);
            break;
          }
        }
        let _guard = guards::shared_scope_guard();

        if let Err(mut e) = s.dispatch_node(tree, *body) {
          match e.kind_mut() {
            ShErrKind::LoopBreak(count) => {
              if *count == 1 || Shed::meta(MetaTab::loop_depth) <= 1 {
                Shed::set_status(0);
                break 'outer;
              }
              *count -= 1;
              return Err(e);
            }
            ShErrKind::LoopContinue(count) => {
              if *count > 1 && Shed::meta(MetaTab::loop_depth) > 1 {
                *count -= 1;
                return Err(e);
              }
              Shed::set_status(0);
            }
            _ => return Err(e),
          }
        }
        last_body_status = Shed::get_status();

        if let Some(step_node) = step
          && let Err(e) = s.dispatch_node(tree, *step_node)
        {
          Shed::set_status(1);
          return Err(e);
        }
      }

      Ok(())
    };

    let _loop_guard = Shed::meta_mut(MetaTab::enter_loop);
    self.run_compound("c_for", for_stmt_id, tree, for_logic)
  }
  pub(super) fn exec_for_arr(&mut self, tree: &Ast, for_stmt_id: NodeId) -> ShResult<()> {
    let for_stmt = &tree[for_stmt_id];
    let NdRule::ForNode {
      vars,
      arr,
      body,
      positional,
    } = &for_stmt.class
    else {
      unreachable!();
    };

    let for_logic = |s: &mut Self, tree: &Ast| -> ShResult<()> {
      let to_expanded_strings = |tks: &[Tk]| -> ShResult<Vec<VarStr>> {
        let mut out = vec![];
        for tk in tks {
          out.extend(tk.expand_to_words()?.iter().cloned());
        }

        Ok(out)
      };

      let arr: Vec<VarStr> = if *positional {
        // the for loop was written with no 'in' keyword
        // so we use the positional parameters instead
        Shed::vars(|v| v.sh_argv().iter().skip(1).cloned().collect())
      } else {
        to_expanded_strings(&tree[*arr])?
      };
      let vars: Vec<VarStr> = to_expanded_strings(&tree[*vars])?;

      'outer: for chunk in arr.chunks(vars.len()) {
        let empty = VarStr::default();
        let chunk_iter = vars
          .iter()
          .zip(chunk.iter().chain(std::iter::repeat(&empty)));

        for (var, val) in chunk_iter {
          Shed::vars_mut(|v| {
            v.set_var(
              &var.to_str_lossy(),
              VarKind::string(val.clone()),
              VarFlags::empty(),
            )
          })?;
        }

        let _guard = guards::shared_scope_guard();

        if let Err(mut e) = s.dispatch_node(tree, *body) {
          match e.kind_mut() {
            ShErrKind::LoopBreak(count) => {
              if *count == 1 || Shed::meta(MetaTab::loop_depth) <= 1 {
                Shed::set_status(0);
                break 'outer;
              }
              *count -= 1;
              return Err(e);
            }
            ShErrKind::LoopContinue(count) => {
              if *count > 1 && Shed::meta(MetaTab::loop_depth) > 1 {
                *count -= 1;
                return Err(e);
              }
              Shed::set_status(0);
            }
            _ => return Err(e),
          }
        }
      }

      Ok(())
    };

    let _loop_guard = Shed::meta_mut(MetaTab::enter_loop);
    self.run_compound("for", for_stmt_id, tree, for_logic)
  }
  pub(super) fn exec_if(&mut self, tree: &Ast, if_stmt_id: NodeId) -> ShResult<()> {
    let if_stmt = &tree[if_stmt_id];
    let NdRule::IfNode {
      cond_nodes,
      else_block,
    } = &if_stmt.class
    else {
      unreachable!();
    };

    let if_logic = |s: &mut Self, tree: &Ast| -> ShResult<()> {
      for node in cond_nodes.ids() {
        let CondNode { cond, body } = &tree[node];

        {
          // condition scope
          let _guard = guards::shared_scope_guard();
          if let Err(e) = s.dispatch_node(tree, *cond) {
            Shed::set_status(1);
            return Err(e);
          }
        }

        {
          // body scope
          if Shed::get_status() == 0 {
            let _guard = guards::shared_scope_guard();
            return s.dispatch_node(tree, *body);
          }
        }
      }

      if let Some(body) = else_block {
        let _guard = guards::shared_scope_guard();
        s.dispatch_node(tree, *body)?;
      } else {
        Shed::set_status(0);
      }

      Ok(())
    };

    self.run_compound("if", if_stmt_id, tree, if_logic)
  }
}
