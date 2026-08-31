//! Execution of command pipelines
//!
//! This module contains the logic for executing command pipelines, which are sequences of commands connected by pipes.
//! It handles the setup of pipes, redirections, and the execution of each command in the pipeline, either in the current
//! process or by forking new processes as needed.
//!
//! The `core.pipeline_style` shopt controls the forking behavior of pipelines:
//! * `tail`: the longest tail sequence of builtins executes in-process
//! * `last`: the last command executes in-process if it is a builtin
//! * `all`: every command forks, no matter what.

use nix::unistd::{self, Pid};

use crate::{
  errln,
  eval::{
    lex::{Span, Tk},
    parse::{NdFlags, Node, node},
  },
  procio::{self, OutputSink, PipeGenerator, RedirGuard, RedirSet, SinkScope, StdinScope},
  shopt,
  state::{
    Shed,
    jobs::{self},
    shopt,
    terminal::Terminal,
    vars::{VarFlags, VarKind},
  },
  util::{
    error::{ShErrKind, ShResult},
    guards,
  },
};

use super::{Ast, NdRule, NodeId, classify};
impl super::Dispatcher {
  pub(super) fn exec_pipeline(&mut self, tree: &Ast, pipeline: NodeId) -> ShResult<()> {
    let pipeline = &tree[pipeline];
    let pipeline_span = pipeline.get_span();
    let pipeline_flags = pipeline.flags;
    let pipeline_context = pipeline.context;
    let NdRule::Pipeline { cmds } = &pipeline.class else {
      unreachable!()
    };

    let mut cmds: Vec<NodeId> = tree[*cmds].to_vec();

    let has_redirs = !pipeline.redirs_empty();
    let is_bg = pipeline_flags.contains(NdFlags::BACKGROUND);
    let interactive = Shed::term(Terminal::interactive);
    let num_cmds = cmds.len();
    let last = num_cmds.saturating_sub(1);
    let mut tty_attached = false;

    // closure that tells us if a pipeline segment should fork
    let should_fork_segment =
      |cmd: &Node| -> bool { is_bg && num_cmds == 1 && !classify::will_fork(cmd, tree) };

    if cmds.len() == 1 && !is_bg && classify::runs_inline(&tree[cmds[0]], tree) {
      // it's a single command. skip the I/O setup
      return self.exec_one(tree, cmds[0], should_fork_segment, pipeline_flags);
    }

    let _underscore_guard = (num_cmds > 1).then(super::suppress_underscore_guard);

    let _cooked_guard = (!is_bg && interactive).then(|| Shed::term_mut(Terminal::prepare_for_exec));

    // closure that gets the pgid we need if the child wants the tty
    let tty_controller = |s: &mut Self| -> Option<Pid> {
      (!is_bg && Shed::term(Terminal::interactive))
        .then(|| s.job_stack.curr_job_mut().unwrap().pgid())
        .flatten()
    };

    self.job_stack.new_job();
    self.fg_job = !is_bg && Shed::term(Terminal::interactive);

    let redirs = RedirSet::from(&tree[pipeline.redirs]);

    let (mut in_rdrs, mut out_rdrs) = redirs.split_by_channel();
    let mut result = Ok(());

    let mut spans = vec![];

    // calculate when we should stop forking, based on `core.pipeline_style`
    // tail -> the longest tail sequence of builtins executes in-process
    // last -> the last command executes in-process if it is a builtin
    // all -> every command forks, no matter what
    let tail_start = match shopt!(core.pipeline_style) {
      shopt::PipeStyle::All => num_cmds,
      style @ (shopt::PipeStyle::Last | shopt::PipeStyle::Tail) => {
        // start of the trailing run of builtin-only stages
        let builtin_tail = match cmds
          .iter_mut()
          .rev()
          .position(|n| !node::node_has_only_builtins(tree, *n))
        {
          Some(pos) => num_cmds - pos,
          None => 0,
        };

        if matches!(style, shopt::PipeStyle::Last) {
          // keep only the last stage in-process (or all-fork if it isn't a builtin)
          builtin_tail.max(num_cmds - 1)
        } else {
          builtin_tail
        }
      }
    };

    let pipes = PipeGenerator::new((tail_start + 1).min(num_cmds));
    let cmds_and_pipes = cmds.iter().enumerate().zip(pipes);

    // Per-stage statuses of the in-process tail, captured for the PIPESTATUS
    // splice and pipefail blame after the forked prefix is waited on.
    let mut tail_statuses: Vec<(i32, Span)> = vec![];

    for ((i, cmd), (r, w, downstream_read)) in cmds_and_pipes {
      let has_redirs = has_redirs || (r.is_some() || w.is_some());

      // builtins must fork in the middle of multi-command pipelines
      let fork_builtins = num_cmds > 1 && i != tail_start;
      let _fork = Shed::meta_mut(|m| m.enter_fork(fork_builtins));

      let _guard = (has_redirs).then(RedirGuard::stdio);

      if i == 0 {
        std::mem::take(&mut in_rdrs).apply_persistent().ok();
      }

      if let Some(mut r) = r {
        r.apply()?;
      }

      if i == tail_start {
        // the rest of these are non-forking builtins
        std::mem::take(&mut out_rdrs).apply_persistent().ok();
        if is_bg {
          let tail: Vec<NodeId> = cmds[i..].to_vec();
          let name = tail
            .first()
            .and_then(|id| tree.command_for(*id))
            .map(Tk::to_str_lossy)
            .unwrap_or_default();
          result = self.run_fork(name.as_bytes(), move |s| {
            super::catch_exit(
              || s.exec_internal_pipeline(tree, &tail).map(|_| ()),
              super::exit_with,
            );
          });
          break;
        }
        if tail_start > 0 && Shed::term(Terminal::interactive) {
          Shed::term_mut(|t| t.attach(unistd::getpgrp())).ok();
        }
        result = match self.exec_internal_pipeline(tree, &cmds[i..]) {
          Ok(statuses) => {
            tail_statuses = statuses;
            Ok(())
          }
          Err(e) => Err(e),
        };
        break;
      }

      if let Some(mut w) = w {
        w.apply()?;
      }

      if i == last {
        std::mem::take(&mut out_rdrs).apply_persistent().ok();
      }

      let cmd_node = &tree[*cmd];

      spans.push(tree.span_for(*cmd));

      self.fork_close_fd = downstream_read;
      result = if should_fork_segment(cmd_node) {
        let name = tree
          .command_for(*cmd)
          .map(Tk::to_str_lossy)
          .unwrap_or_default();

        self.run_fork(name.as_bytes(), |s| {
          super::catch_exit(|| s.dispatch_node(tree, *cmd), super::exit_with);
        })
      } else {
        self.dispatch_node(tree, *cmd)
      };
      self.fork_close_fd = None;

      if !tty_attached && let Some(pgid) = tty_controller(self) {
        Shed::term_mut(|t| t.attach(pgid)).ok();
        tty_attached = true;
      }

      if result.is_err() {
        break;
      }
    }

    let job = self.job_stack.finalize_job().unwrap();
    let dispatch_result = jobs::dispatch_job(job, is_bg, Shed::term(Terminal::interactive));

    // The in-process tail ran inline, so its statuses never reached the wait.
    // Splice them onto the forked prefix's (which the wait left in PIPESTATUS)
    // and recompute $? across the whole pipeline.
    if !tail_statuses.is_empty() {
      // The forked prefix's per-stage codes: the wait only fills PIPESTATUS for
      // a multi-stage job (`Job::pipe_status` bails at len <= 1), so a lone
      // prefix stage's code is just `$?`.
      let mut codes: Vec<i32> = match tail_start {
        0 => vec![],
        1 => vec![Shed::get_status()],
        _ => Shed::vars(|v| v.try_get_arr_elems("PIPESTATUS"))
          .map(|elems| {
            elems
              .iter()
              .filter_map(|s| s.to_string().parse().ok())
              .collect()
          })
          .unwrap_or_default(),
      };
      codes.extend(tail_statuses.iter().map(|(code, _)| *code));

      let status = if shopt!(set.pipefail) {
        codes.iter().rev().find(|c| **c != 0).copied()
      } else {
        codes.last().copied()
      }
      .unwrap_or(0);

      Shed::vars_mut(|v| {
        v.set_var(
          "PIPESTATUS",
          VarKind::arr(codes.iter().map(|c| c.to_string().into())),
          VarFlags::empty(),
        )
      })
      .ok();
      Shed::set_status(status);

      // keep `spans` aligned with PIPESTATUS so pipefail blame indexes correctly
      spans.extend(tail_statuses.iter().map(|(_, span)| span.clone()));
    }

    result?;
    dispatch_result?;

    let blame_span = if shopt!(set.pipefail) {
      super::pipefail_span(&spans).or(Some(tree[pipeline_span].clone()))
    } else {
      Some(tree[pipeline_span].clone())
    };

    super::check_err(pipeline_flags, None, blame_span, &tree[pipeline_context])?;
    Ok(())
  }
  /// Run a contiguous run of in-process builtins, wiring them together with
  /// string sinks instead of pipes. Returns each stage's exit status and span
  /// so the caller can fold them into PIPESTATUS and the pipefail blame; the
  /// first hard error short-circuits the run.
  pub(super) fn exec_internal_pipeline(
    &mut self,
    tree: &Ast,
    cmds: &[NodeId],
  ) -> ShResult<Vec<(i32, Span)>> {
    let mut prev: Option<OutputSink> = None;
    let num_cmds = cmds.len();
    let last = num_cmds.saturating_sub(1);
    let mut statuses = Vec::with_capacity(num_cmds);

    for (i, cmd) in cmds.iter().enumerate() {
      let is_last = i == last;
      let out_scope = (!is_last).then(SinkScope::new);
      let _in_scope = prev.take().map(StdinScope::push);

      let result = match &tree[*cmd].class {
        NdRule::Subshell { body } => {
          let _ceiling = guards::isolation_guard(None);

          match self.dispatch_node(tree, *body) {
            Err(e) => {
              if let ShErrKind::CleanExit(code) = e.kind() {
                Shed::set_status(*code);
                Ok(())
              } else {
                Err(e)
              }
            }
            res => res,
          }
        }
        _ => self.dispatch_node(tree, *cmd),
      };

      statuses.push((Shed::get_status(), tree.span_for(*cmd)));

      if let Some(scope) = out_scope {
        let scope = scope.take();
        if scope.was_truncated() {
          Shed::set_status(procio::SINK_TRUNCATED_STATUS);
          let size = scope.limit();

          errln!("shed: pipeline output truncated (exceeded {size})");
        }
        prev = Some(scope);
      }

      result?;
    }

    Ok(statuses)
  }

  pub(super) fn exec_one(
    &mut self,
    tree: &Ast,
    cmd_id: NodeId,
    should_fork: impl Fn(&Node) -> bool,
    flags: NdFlags,
  ) -> ShResult<()> {
    let cmd = &tree[cmd_id];
    let span = cmd.get_span();
    let context = cmd.context;
    // it's a single command
    // just thread it through dispatch_node directly.
    // this avoids the stdio setup that follows this
    self.job_stack.new_job();
    let res = if should_fork(cmd) {
      let name = cmd
        .get_command()
        .map(|tk| tree[tk].to_str_lossy())
        .unwrap_or_default();

      self.run_fork(name.as_bytes(), |s| {
        if let Err(e) = s.dispatch_node(tree, cmd_id) {
          e.print_error();
        }
      })
    } else {
      self.dispatch_node(tree, cmd_id)
    };

    if let Some(job) = self.job_stack.finalize_job() {
      // just in case this somehow forked a child
      // let's handle it here. Shouldn't happen in practice
      // but you never know
      jobs::dispatch_job(job, false, Shed::term(Terminal::interactive))?;
    }
    super::check_err(flags, None, Some(tree[span].clone()), &tree[context])?;
    res
  }
}
