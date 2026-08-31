use super::{
  lex::{self, KEYWORDS, Span, Tk, TkFlags},
  parse::{
    self,
    ast::{Ast, NodeId},
  },
};
use crate::builtin::{self, BUILTIN_NAMES};
use crate::{
  eval::parse::{NdFlags, NdRule, ParsedSrc},
  expand::{alias, arithmetic},
  lifecycle, procio, sherr, shopt, signal,
  state::{
    Shed,
    jobs::{ChildProc, JobStack},
    logic::TrapTarget,
    meta::{CmdTimer, MetaTab},
    terminal::Terminal,
    vars::{ShellParam, VarFlags, VarKind, VarStr},
  },
  util::{
    self,
    error::{LabelBuilder, ShErr, ShErrKind, ShResult, ShResultExt},
    guards,
  },
};

use bstr::ByteSlice;
use std::{ffi::CString, os::fd::RawFd, rc::Rc};

use nix::unistd::{self, ForkResult, Pid};

mod assign;
pub(crate) mod classify;
mod command;
mod control;
mod function;
mod pipeline;

#[cfg(test)]
mod tests;

// re-exports
pub(crate) use assign::AssignBehavior;
pub(crate) use control::dispatch_deferred_cmd;

thread_local! {
  // Last expanded word of the most recently expanded argv, the raw material for
  // `$_`. Written by `prepare_argv_with`, consumed by `commit_underscore`.
  static LAST_ARG: std::cell::RefCell<Option<VarStr>> = const { std::cell::RefCell::new(None) };
  static SUPPRESS_UNDERSCORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn prepare_argv(argv: &[Tk]) -> ShResult<Vec<(VarStr, Span)>> {
  prepare_argv_with(argv, false)
}

/// Same as `prepare_argv`, but with control over word-splitting per token.
/// `no_split` is set by `parse_cmd` for `[[`/`]]` commands so operands like
/// `$unset` survive expansion as the empty string instead of vanishing
/// from argv (bash `[[ ]]` semantics).
pub(crate) fn prepare_argv_with(argv: &[Tk], no_split: bool) -> ShResult<Vec<(VarStr, Span)>> {
  let mut out = Vec::with_capacity(argv.len());

  for arg in argv {
    let span = arg.span.clone();
    if no_split {
      // this is the bash regex thing, not a tilde expansion
      if arg.span.as_bytes() == b"=~" {
        out.push(("=~".into(), span));
        continue;
      }
      let word = arg.expand_no_split()?;
      out.push((word, span));
    } else {
      for exp in arg.expand_to_words()?.iter() {
        out.push((exp.clone(), span.clone()));
      }
    }
  }

  record_last_arg(out.last().map(|(s, _)| s.clone()));

  Ok(out)
}

/// Run a closure, catching and handling `ShErrKind::CleanExit(code)` if it is returned.
pub(crate) fn catch_exit<F: FnMut() -> ShResult<()>, E: FnMut(i32)>(mut f: F, mut on_exit: E) {
  if let Err(e) = f() {
    if let ShErrKind::CleanExit(code) = e.kind() {
      on_exit(*code);
    } else {
      e.print_error();
    }
  }
}

pub(crate) fn exit_with(code: i32) {
  lifecycle::exit_shed(true, code);
}

/// Stash the last expanded word so a later `commit_underscore` can promote it to `$_`.
pub(crate) fn record_last_arg(arg: Option<VarStr>) {
  LAST_ARG.with(|l| *l.borrow_mut() = arg);
}

/// Promote the most recently expanded last word to `$_`, unless we're mid-pipeline.
fn commit_underscore() {
  if SUPPRESS_UNDERSCORE.with(std::cell::Cell::get) {
    return;
  }
  let last = LAST_ARG.with(|l| l.borrow_mut().take());
  if let Some(arg) = last {
    Shed::vars_mut(|v| v.set_var("_", VarKind::string(arg), VarFlags::EXPORT)).ok();
  }
}

/// Suppress `$_` updates for the duration of the returned guard, restoring the
/// prior state on drop so nested pipelines compose.
fn suppress_underscore_guard() -> impl Drop {
  let prev = SUPPRESS_UNDERSCORE.with(|s| s.replace(true));
  guards::guard((), move |()| SUPPRESS_UNDERSCORE.with(|s| s.set(prev)))
}

/// Dispatch commands registered by the `defer` keyword.
pub(crate) fn dispatch_deferred_cmds() {
  let mut deferred = Shed::vars_mut(|v| v.cur_scope_mut().take_deferred_cmds());

  while let Some(cmd) = deferred.pop() {
    if !dispatch_deferred_cmd(cmd) {
      break;
    }
  }
}

/// Arguments to the execvpe function
pub(crate) struct ExecArgs {
  pub cmd: (CString, Span),
  pub argv: Rc<[CString]>,
  pub envp: Rc<[CString]>,
}

impl ExecArgs {
  pub(crate) fn from_expanded(argv: Vec<(VarStr, Span)>) -> Self {
    let cmd = Self::get_cmd(&argv);
    let argv = Self::get_argv(argv);
    let envp = Self::get_envp();
    Self { cmd, argv, envp }
  }
  pub(crate) fn get_cmd(argv: &[(VarStr, Span)]) -> (CString, Span) {
    let cmd = argv[0].0.as_bytes();
    let span = argv[0].1.clone();
    (CString::new(cmd).unwrap(), span)
  }
  pub(crate) fn get_argv(argv: Vec<(VarStr, Span)>) -> Rc<[CString]> {
    argv
      .into_iter()
      .map(|s| CString::new(s.0).unwrap())
      .collect::<Vec<CString>>()
      .into()
  }
  pub(crate) fn get_envp() -> Rc<[CString]> {
    Shed::meta_mut(MetaTab::get_envp)
  }
}

/// Execute a `-c` command string, optimizing single simple commands to exec
/// directly without forking. This avoids process group issues where grandchild
/// processes (e.g. nvim spawning opencode) lose their controlling terminal.
pub(crate) fn exec_dash_c(input: &str, args: Vec<String>) -> ShResult<()> {
  fn single_command_id(ast: &Ast, root: NodeId) -> Option<NodeId> {
    let mut id = root;
    loop {
      match &ast[id].class {
        NdRule::Command { .. } => return Some(id),
        NdRule::Pipeline { cmds } if cmds.len() == 1 => id = cmds.get(0),
        NdRule::Conjunction { elements } if elements.len() == 1 => id = ast[elements.get(0)].cmd,
        _ => return None,
      }
    }
  }

  let stdin = procio::stdin_fileno();
  let is_tty = unistd::isatty(stdin).unwrap_or(false);
  let _guard = Shed::term_mut(|t| t.interactive_guard(is_tty));
  let name = args
    .first()
    .cloned()
    .map_or("<shed -c>".into(), VarStr::from);

  Shed::vars_mut(|v| {
    v.set_param(ShellParam::ShellName, &name.to_str_lossy()); // $0
    let scope = v.cur_scope_mut();
    scope.sh_argv_mut().clear();
    // bpush_arg (vs raw push_back) runs update_arg_params, keeping
    // $#, $@, $* in sync with sh_argv.
    scope.bpush_arg(name.clone());
    for (i, arg) in args.into_iter().enumerate() {
      if i == 0 {
        continue;
      }
      scope.bpush_arg(arg.into());
    }
  });

  let expanded = alias::expand_aliases(input);
  let mut parser = ParsedSrc::new(expanded.into())
    .with_lex_flags(super::lex::LexFlags::empty())
    .with_name(name.clone());

  if let Err(errors) = parser.parse_src() {
    for error in errors {
      error.print_error();
    }
    Shed::set_status(2);
    return Ok(());
  }

  let mut ast = parser.into_ast();

  let mut dispatcher = Dispatcher::new(name);
  // exec_cmd expects a job on the stack (normally set up by exec_pipeline).
  // For the NO_FORK exec-in-place path, create one so it doesn't panic.
  dispatcher.job_stack.new_job();

  // Single simple command: exec directly without forking.
  // The parser wraps single commands as Conjunction -> Pipeline -> Command.
  // Walk down to the inner Command, mark it NO_FORK, and dispatch it directly
  // (bypassing pipeline setup).
  let single_root = match ast.roots() {
    [only] => Some(*only),
    _ => None,
  };
  if let Some(cmd_id) = single_root.and_then(|root| single_command_id(&ast, root)) {
    ast[cmd_id].flags |= NdFlags::NO_FORK;
    let blame = ast.span_for(cmd_id);
    return dispatcher.dispatch_node(&ast, cmd_id).try_blame(blame);
  }

  dispatcher.begin_dispatch(&ast)
}

/// Execute interactively.
///
/// Used in the main loop and other places that are guaranteed to be interacting with a tty somehow.
/// This controls whether or not the shell passes terminal control to child processes.
pub(crate) fn exec_int(input: VarStr, source_name: Option<VarStr>) -> ShResult<()> {
  let _guard = Shed::term_mut(|t| t.interactive_guard(true));
  exec_input(input, source_name)
}

/// Execute non-interactively
pub(crate) fn exec_nonint(input: VarStr, source_name: Option<VarStr>) -> ShResult<()> {
  let _guard = Shed::term_mut(|t| t.interactive_guard(false));
  exec_input(input, source_name)
}

/// Execute arbitrary shell input
///
/// This should only be called directly if you wish to inherit
/// the caller's interactive status.
pub(crate) fn exec_input(mut input: VarStr, source_name: Option<VarStr>) -> ShResult<()> {
  let interactive = Shed::term(Terminal::interactive);

  if !interactive || !Shed::shopts(|o| o.prompt.expand_aliases) {
    input = alias::expand_aliases(&input.to_str_lossy()).into();
  }
  let lex_flags = if interactive {
    super::lex::LexFlags::INTERACTIVE
  } else {
    super::lex::LexFlags::empty()
  };
  let source_name = source_name.unwrap_or("<unknown>".into());
  let mut parser = ParsedSrc::new(input)
    .with_lex_flags(lex_flags)
    .with_name(source_name.clone());
  if let Err(errors) = parser.parse_src() {
    for error in errors {
      error.print_error();
    }
    Shed::set_status(2);
    return Ok(());
  }

  let mut dispatcher = Dispatcher::new(source_name.clone());
  dispatcher.begin_dispatch(&parser.ast)
}

pub(crate) struct Dispatcher {
  source_name: VarStr,
  pub job_stack: JobStack,
  timer_stack: Vec<Option<CmdTimer>>,
  fg_job: bool,
  /// A pipe fd a forked builtin/compound segment must close in its child (the
  /// downstream read end it inherited but doesn't exec away). Set per-segment in
  /// `exec_pipeline`, consumed in `run_fork`.
  fork_close_fd: Option<RawFd>,
}

impl Dispatcher {
  pub(crate) fn new(source_name: VarStr) -> Self {
    Self {
      source_name,
      job_stack: JobStack::new(),
      timer_stack: vec![],
      fg_job: true,
      fork_close_fd: None,
    }
  }
  pub(crate) fn begin_dispatch(&mut self, tree: &Ast) -> ShResult<()> {
    for &root in tree.roots() {
      let blame = tree.span_for(root);
      self.dispatch_node(tree, root).try_blame(blame)?;
    }
    Ok(())
  }
  pub(crate) fn dispatch_node(&mut self, tree: &Ast, node: NodeId) -> ShResult<()> {
    stacker::maybe_grow(64 * 1024, 1024 * 1024, || {
      let _guard = Shed::meta_mut(MetaTab::push_procsub_frame);

      while signal::signals_pending() {
        // If we have received SIGINT,
        // this will stop the execution here
        // and propagate back to the functions in main.rs
        signal::check_signals()?;
      }

      // set -n
      if Shed::shopts(|o| o.set.noexec) {
        return Ok(());
      }

      match tree[node].class {
        NdRule::List { .. } => self.exec_list(tree, node),
        NdRule::Conjunction { .. } => self.exec_conjunction(tree, node),
        NdRule::Pipeline { .. } => self.exec_pipeline(tree, node),
        NdRule::IfNode { .. } => self.exec_if(tree, node),
        NdRule::LoopNode { .. } => self.exec_loop(tree, node),
        NdRule::ForNode { .. } => self.exec_for_arr(tree, node),
        NdRule::ForArith { .. } => self.exec_for_arith(tree, node),
        NdRule::CaseNode { .. } => self.exec_case(tree, node),
        NdRule::BraceGrp { .. } => self.exec_brc_grp(tree, node),
        NdRule::Subshell { .. } => self.exec_subsh(tree, node),
        NdRule::Negate { .. } => self.exec_negated(tree, node),
        NdRule::Timed { .. } => self.exec_timed(tree, node),
        NdRule::Command { .. } => self.dispatch_cmd(tree, node),
        NdRule::TryNode { .. } => self.exec_try(tree, node),
        NdRule::DeferNode { .. } => Self::exec_defer(tree, node),

        NdRule::FuncDef { .. } => Self::exec_func_def(tree, node),
        NdRule::Arithmetic { .. } => Self::exec_arith(tree, node),
        NdRule::Assignment { .. } => unreachable!(),
      }
    })
  }
  pub(crate) fn exec_list(&mut self, tree: &Ast, node: NodeId) -> ShResult<()> {
    let NdRule::List { commands } = &tree[node].class else {
      unreachable!()
    };
    for node in &tree[*commands] {
      let blame = tree.span_for(*node);
      self.dispatch_node(tree, *node).try_blame(blame)?;
    }

    Ok(())
  }
  pub(crate) fn dispatch_cmd(&mut self, tree: &Ast, node: NodeId) -> ShResult<()> {
    let (line, _) = tree.span_for(node).clone().line_and_col();
    Shed::vars_mut(|v| v.set_var("LINENO", VarKind::Int((line + 1) as i32), VarFlags::empty()))?;

    let result = self.route_command(tree, node, true);
    commit_underscore();
    result
  }

  /// Route a simple command to its executor: function, builtin, arithmetic,
  /// autocd, or external `exec_cmd`.
  ///
  /// `allow_func` gates function lookup. The `command` builtin passes `false`
  /// so that, per POSIX, it suppresses function lookup while still running
  /// shell builtins and external commands.
  pub(crate) fn route_command(
    &mut self,
    tree: &Ast,
    node: NodeId,
    allow_func: bool,
  ) -> ShResult<()> {
    let Some(cmd) = &tree[node].get_command() else {
      return self.exec_cmd(tree, node); // Argv is empty, probably an assignment
    };
    // We need to expand this token
    // so that a command smuggled inside of a variable is routed correctly,
    // instead of only hitting the exec_cmd path
    let words = tree[*cmd].clone().expand_to_words()?;
    let Some(cmd_word) = words.iter().next().cloned() else {
      if let NdRule::Command {
        assignments,
        argv: _,
      } = &tree[node].class
        && !assignments.is_empty()
      {
        return self.exec_cmd(tree, node);
      }
      return Ok(());
    };

    let cmd = &tree[*cmd];

    if allow_func && classify::is_func(&cmd_word.to_str_lossy()) {
      // function
      self.exec_func(tree, node)
    } else if cmd.flags.contains(TkFlags::BUILTIN) || BUILTIN_NAMES.contains(&cmd_word.as_bytes()) {
      // builtin
      self.exec_builtin(tree, node, cmd_word.as_bytes())
    } else if classify::is_arith(cmd) {
      // arithmetic
      Self::exec_arith(tree, node)
    } else if classify::can_autocd(cmd) {
      // autocd
      let cd_call = [b"cd ", cmd.span.as_bytes()].concat();
      exec_input(cd_call.into(), Some(self.source_name.clone()))
    } else {
      // normal external
      self.exec_cmd(tree, node)
    }
  }
  fn exec_arith(tree: &Ast, arith: NodeId) -> ShResult<()> {
    let NdRule::Arithmetic { body } = &tree[arith].class else {
      unreachable!()
    };
    let result = arithmetic::expand_arithmetic_wrapped(tree[*body].as_bytes())?;
    let val: f64 = result.to_str_lossy().parse().unwrap_or(0.0);
    Shed::set_status_from_bool(val != 0.0);
    Ok(())
  }
  fn exec_builtin(&mut self, tree: &Ast, cmd_id: NodeId, cmd_name: &[u8]) -> ShResult<()> {
    let fork_builtins = Shed::meta_mut(MetaTab::take_fork);

    let Some(builtin) = builtin::lookup_builtin(cmd_name) else {
      sherr!(NotFound @ tree.span_for(cmd_id), "builtin not found: {}", cmd_name.to_str_lossy())
        .print_error();
      return util::with_status(127);
    };

    if fork_builtins {
      self.run_fork(cmd_name, |s| {
        catch_exit(|| builtin.setup_builtin(tree, cmd_id, s), exit_with);
      })?;
      Ok(())
    } else if let Err(e) = builtin.setup_builtin(tree, cmd_id, self) {
      let is_flow_ctl = matches!(
        e.kind(),
        ShErrKind::CleanExit(_)
          | ShErrKind::FuncReturn(_)
          | ShErrKind::LoopBreak(_)
          | ShErrKind::LoopContinue(_)
      );
      if !is_flow_ctl && Shed::get_status() == 0 {
        Shed::set_status(1);
      }
      Err(e)
    } else {
      Ok(())
    }
  }
  fn run_fork(&mut self, name: &[u8], f: impl FnOnce(&mut Self)) -> ShResult<()> {
    let existing_pgid = self.job_stack.curr_job_mut().unwrap().pgid();
    let interactive = Shed::term(Terminal::interactive);
    match unsafe { unistd::fork()? } {
      ForkResult::Child => {
        lifecycle::setup_child();

        // only give a new job its own group under interactive job control.
        // in a script, the child stays in the shell's process group
        // otherwise a backgrounded subshell claims the tty and stops the
        // entire script with SIGTTOU
        if let Some(pgid) = existing_pgid {
          let _ = unistd::setpgid(Pid::from_raw(0), pgid);
        } else if interactive {
          let _ = unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0));
        }
        signal::reset_signals(self.fg_job);

        if let Some(fd) = self.fork_close_fd {
          let _ = nix::unistd::close(fd);
        }
        let _guard = Shed::term_mut(|t| t.interactive_guard(false));
        f(self);

        lifecycle::exit_shed(true, Shed::get_status());
      }
      ForkResult::Parent { child } => {
        let timer = self.take_timer();
        let job = self.job_stack.curr_job_mut().unwrap();
        let child_pgid = if let Some(pgid) = existing_pgid {
          pgid
        } else if interactive {
          job.set_pgid(child);
          child
        } else {
          let pgrp = unistd::getpgrp();
          job.set_pgid(pgrp);
          pgrp
        };
        let child_proc = ChildProc::new(child, Some(name), Some(child_pgid), timer);
        job.push_child(child_proc);
        Ok(())
      }
    }
  }
  pub(crate) fn take_timer(&mut self) -> Option<CmdTimer> {
    self.timer_stack.last_mut().and_then(Option::take)
  }
}

pub(crate) fn pipefail_span(spans: &[Span]) -> Option<Span> {
  let pipestatus = Shed::vars(|v| v.try_get_arr_elems("PIPESTATUS")).ok()?;
  for (i, status) in pipestatus.into_iter().enumerate().rev() {
    let status = status.to_str_lossy().parse::<usize>().ok()?;
    if status != 0 {
      return spans.get(i).cloned();
    }
  }
  None
}

pub(crate) fn check_err(
  flags: NdFlags,
  err: Option<ShErr>,
  span: Option<Span>,
  context: &[LabelBuilder],
) -> ShResult<()> {
  if Shed::get_status() == 0 || flags.contains(NdFlags::NOT_ERR) {
    return Ok(());
  }

  if let Some(trap) = Shed::logic(|l| l.get_trap(TrapTarget::Error)) {
    util::with_saved_status(|| exec_nonint(trap, Some("trap ERR".into())))?;
  }

  if !shopt!(set.errexit) {
    return Ok(());
  }

  if let Some(mut e) = err {
    e.set_kind(ShErrKind::ErrInterrupt);
    e.persist_redirs();
    Err(e.with_context(context.iter()))
  } else if let Some(span) = span {
    Err(
      sherr!(ErrInterrupt @ span, "Command returned non-zero exit status")
        .with_context(context.iter()),
    )
  } else {
    Err(sherr!(ErrInterrupt, "Command returned non-zero exit status",).with_context(context.iter()))
  }
}
