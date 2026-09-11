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

use std::sync::Arc;

use nix::{
  libc::STDIN_FILENO,
  unistd::{self, Pid},
};

use crate::{
  builtin::ForkBehavior,
  eval::{
    lex::Span,
    parse::{NdFlags, Node, node},
  },
  procio::{RedirSet, Sink, Sinks},
  shopt,
  state::{
    Shed,
    jobs::{self},
    shopt::PipeStyle,
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

    let cmds: &[NodeId] = &tree[*cmds];

    let is_bg = pipeline_flags.contains(NdFlags::BACKGROUND);
    let num_cmds = cmds.len();

    // closure that tells us if a pipeline segment should fork
    let should_fork_segment =
      |cmd: &Node| -> bool { is_bg && num_cmds == 1 && !classify::will_fork(cmd, tree) };

    if cmds.len() == 1 && !is_bg && classify::runs_inline(&tree[cmds[0]], tree) {
      // it's a single command. skip the I/O setup
      return self.exec_one(tree, cmds[0], should_fork_segment, pipeline_flags);
    }

    let interactive = Shed::term(Terminal::interactive);
    let mut tty_attached = false;

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

    let (in_rdrs, out_rdrs) = redirs.split_by_channel();
    let mut result = Ok(());

    let mut spans = vec![];

    let lastpipe = shopt!(core.lastpipe);
    let pipe_style = shopt!(core.pipeline_style);

    // Per-stage statuses of the in-process tail, captured for the PIPESTATUS
    // splice and pipefail blame after the forked prefix is waited on.
    let mut tail_status: Option<(i32, Span)> = None;
    let mut cmd_iter = cmds.iter().enumerate().peekable();

    let mut prev_read: Option<Arc<dyn Sink>> = None;
    while let Some((i, cmd)) = cmd_iter.next() {
      let mut guard = Sinks::redir_scope();

      let cmd_name = tree
        .command_for(*cmd)
        .map(|s| s.slice())
        .unwrap_or_default();

      // now we decide if we are threading this pipeline stage or not
      // builtins get a thread instead of a fork
      let cmd_node = &tree[*cmd];
      let thread_this_stage = num_cmds > 1
        && !interactive
        && !is_bg
        && !should_fork_segment(cmd_node)
        && node::node_fork_behavior(tree, *cmd) == Some(ForkBehavior::Never)
        && !matches!(pipe_style, PipeStyle::Fork);

      // if the next stage is also threaded, we can use our threaded in-process pipes
      // instead of using a syscall to create os pipes
      let use_thread_pipes = if let Some((_, n_cmd)) = cmd_iter.peek() {
        let next_node = &tree[**n_cmd];
        thread_this_stage
          && !should_fork_segment(next_node)
          && node::node_fork_behavior(tree, **n_cmd) == Some(ForkBehavior::Never)
      } else {
        false
      };

      let run_in_shell = lastpipe
        && i == num_cmds - 1
        && node::node_fork_behavior(tree, *cmd) == Some(ForkBehavior::Never);
      let will_fork = (num_cmds > 1 || is_bg) && !thread_this_stage && !run_in_shell;
      let _fork = Shed::meta_mut(|m| m.enter_fork(will_fork));

      if run_in_shell {
        if let Some(read) = prev_read.take() {
          guard.apply_sink(STDIN_FILENO, read)?;
        }
        guard.apply_set(&out_rdrs)?;
        if is_bg {
          let name = tree
            .command_for(cmds[i])
            .map(|tk| tk.slice())
            .unwrap_or_default();
          result = self.run_fork(name.as_bytes(), move |s| {
            super::catch_exit(
              || s.exec_internal_segment(tree, cmds[i]).map(|_| ()),
              super::exit_with,
            );
          });
          break;
        }
        if Shed::term(Terminal::interactive) {
          Shed::term_mut(|t| t.attach(unistd::getpgrp())).ok();
        }
        result = match self.exec_internal_segment(tree, cmds[i]) {
          Ok(status) => {
            tail_status = Some(status);
            Ok(())
          }
          Err(e) => Err(e),
        };
        break;
      }

      match (i, prev_read.take()) {
        (0, _) => guard.apply_set(&in_rdrs)?,
        (_, Some(read)) => guard.apply_sink(0, read)?,
        _ => {}
      }

      if i + 1 < num_cmds {
        // middle segment, get pipes
        let (read, write) = if use_thread_pipes {
          Sinks::thread_pipes()
        } else {
          Sinks::os_pipes()?
        };
        guard.apply_sink(1, write)?;
        prev_read = Some(read);
      } else {
        // last segment, apply output redirs
        guard.apply_set(&out_rdrs)?;
      }

      let cmd_node = &tree[*cmd];

      spans.push(tree.span_for(*cmd));

      result = if thread_this_stage {
        let stage_sinks = Shed::sinks(|s| s.clone());
        let handle = self.spawn_stage(tree, *cmd, stage_sinks);
        self
          .job_stack
          .curr_job_mut()
          .unwrap()
          .push_member(jobs::JobMember::Thread(handle));
        Ok(())
      } else if should_fork_segment(cmd_node) {
        self.run_fork(&cmd_name, |s| {
          super::catch_exit(|| s.dispatch_node(tree, *cmd), super::exit_with);
        })
      } else {
        self.dispatch_node(tree, *cmd)
      };

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
    if let Some((status, span)) = tail_status {
      // The forked prefix's per-stage codes: the wait only fills PIPESTATUS for
      // a multi-stage job (`Job::pipe_status` bails at len <= 1), so a lone
      // prefix stage's code is just `$?`.
      let mut codes: Vec<i32> = match num_cmds - 1 {
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
      codes.push(status);

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
      spans.push(span);
    }

    result?;
    dispatch_result?;

    let blame_span = if shopt!(set.pipefail) {
      super::pipefail_span(&spans).or(Some(tree[pipeline_span]))
    } else {
      Some(tree[pipeline_span])
    };

    super::check_err(pipeline_flags, None, blame_span, &tree[pipeline_context])?;
    Ok(())
  }

  pub(super) fn exec_internal_segment(&mut self, tree: &Ast, cmd: NodeId) -> ShResult<(i32, Span)> {
    let result = match &tree[cmd].class {
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
      _ => self.dispatch_node(tree, cmd),
    };

    let status = (Shed::get_status(), tree.span_for(cmd));

    match result {
      Ok(()) => Ok(status),
      Err(e) => Err(e),
    }
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
        .map(|tk| tree[tk].slice())
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
    super::check_err(flags, None, Some(tree[span]), &tree[context])?;
    res
  }
}
