use crate::{
  autocmd,
  eval::parse::node::{LabelCtx, node_has_only_builtins},
  expand, lifecycle,
  procio::{OutputSink, SinkScope, StdinScope},
  shopt_mut, socket,
  state::{
    Shed,
    logic::AutoloadKind,
    vars::{VarStr, VarStrSliceExt},
  },
  util::isolation_guard,
  varstr,
};
use std::{
  collections::VecDeque,
  ffi::CString,
  os::{fd::RawFd, unix::fs::PermissionsExt},
  path::Path,
  rc::Rc,
  string::ToString,
};

use crate::state::util::with_vars;
use crate::util::posix_extension::execvpe;

use itertools::Itertools;
use nix::{
  errno::Errno,
  unistd::{ForkResult, Pid, execve, fork, getpgrp, isatty, setpgid},
};
use scopeguard::defer;
use shed_macros::styled_format;
use unicode_segmentation::UnicodeSegmentation;

use super::{
  super::state::{meta::MetaTab, terminal::Terminal},
  AssignKind, CaseNode, CondNode, ConjunctNode, ConjunctOp, LoopKind, NdFlags, NdRule, Node,
  ParsedSrc,
  builtin::{BUILTIN_NAMES, lookup_builtin},
  errln,
  expand::{expand_aliases, expand_arithmetic, expand_arithmetic_wrapped, expand_case_pattern},
  jobs::{ChildProc, JobStack, dispatch_job},
  lex::{KEYWORDS, Span, Tk, TkFlags},
  procio::{self, PipeGenerator, RedirGuard, RedirResult, RedirSet, RedirSpec},
  sherr, shopt,
  signal::{check_signals, signals_pending},
  state::{
    self,
    logic::{ShFunc, TrapTarget},
    meta::CmdTimer,
    shopt::{xtrace_line, xtrace_print},
    vars::{ShellParam, Var, VarFlags, VarKind, VarKindTag},
  },
  try_var,
  util::{
    self, ShErr, ShErrKind, ShResult, ShResultExt, scope_guard, shared_scope_guard, var_ctx_guard,
    with_status,
  },
  var,
};

pub fn in_cd_path(name: &Tk) -> bool {
  let Ok(expanded) = name.expand_no_side_effects() else {
    return false;
  };
  let Some(name) = expanded.get_first_word() else {
    return false;
  };
  if Path::new(&name).is_dir() {
    return true;
  }
  let cd_path = var!("CDPATH");
  let entries = cd_path.split(':');
  for entry in entries {
    let full_path = Path::new(entry).join(&name);
    if full_path.is_dir() {
      return true;
    }
  }
  false
}

pub fn is_in_path(name: &Tk) -> bool {
  let Ok(expanded) = name.expand_no_side_effects() else {
    return false;
  };
  let Some(name) = expanded.get_first_word() else {
    return false;
  };
  if name.starts_with("./") || name.starts_with("../") || name.starts_with('/') {
    let path = Path::new(&name);
    if path.exists() && path.is_file() && !path.is_dir() {
      let Ok(meta) = path.metadata() else {
        return false;
      };

      if meta.permissions().mode() & 0o111 != 0 {
        return true;
      }
    }
    false
  } else {
    let Some(path) = try_var!("PATH") else {
      return false;
    };
    let paths = path.split(':');
    for path in paths {
      let full_path = Path::new(path).join(&name);
      if full_path.exists() && full_path.is_file() && !full_path.is_dir() {
        let Ok(meta) = full_path.metadata() else {
          continue;
        };

        if meta.permissions().mode() & 0o111 != 0 {
          return true;
        }
      }
    }
    false
  }
}

#[derive(Debug, Clone, Copy)]
pub enum AssignBehavior {
  Export,
  Set,
}

thread_local! {
  // Last expanded word of the most recently expanded argv, the raw material for
  // `$_`. Written by `prepare_argv_with`, consumed by `commit_underscore`.
  static LAST_ARG: std::cell::RefCell<Option<VarStr>> = const { std::cell::RefCell::new(None) };
  static SUPPRESS_UNDERSCORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
  scopeguard::guard((), move |()| SUPPRESS_UNDERSCORE.with(|s| s.set(prev)))
}

/// Arguments to the execvpe function
pub struct ExecArgs {
  pub cmd: (CString, Span),
  pub argv: Rc<[CString]>,
  pub envp: Rc<[CString]>,
}

impl ExecArgs {
  pub fn from_expanded(argv: Vec<(VarStr, Span)>) -> Self {
    let cmd = Self::get_cmd(&argv);
    let argv = Self::get_argv(argv);
    let envp = Self::get_envp();
    Self { cmd, argv, envp }
  }
  pub fn get_cmd(argv: &[(VarStr, Span)]) -> (CString, Span) {
    let cmd = argv[0].0.as_str();
    let span = argv[0].1.clone();
    (CString::new(cmd).unwrap(), span)
  }
  pub fn get_argv(argv: Vec<(VarStr, Span)>) -> Rc<[CString]> {
    argv
      .into_iter()
      .map(|s| CString::new(s.0).unwrap())
      .collect::<Vec<CString>>()
      .into()
  }
  pub fn get_envp() -> Rc<[CString]> {
    Shed::meta_mut(MetaTab::get_envp)
  }
}

/// Execute a `-c` command string, optimizing single simple commands to exec
/// directly without forking. This avoids process group issues where grandchild
/// processes (e.g. nvim spawning opencode) lose their controlling terminal.
pub fn exec_dash_c(input: &str, args: Vec<String>) -> ShResult<()> {
  let stdin = procio::stdin_fileno();
  let is_tty = isatty(stdin).unwrap_or(false);
  let _guard = Shed::term_mut(|t| t.interactive_guard(is_tty));
  let name = args
    .first()
    .cloned()
    .map_or("<shed -c>".into(), VarStr::from);

  Shed::vars_mut(|v| {
    v.set_param(ShellParam::ShellName, &name); // $0
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

  let expanded = expand_aliases(input);
  let source_name: Rc<str> = name.into();
  let mut parser = ParsedSrc::new(expanded.into())
    .with_lex_flags(super::lex::LexFlags::empty())
    .with_name(source_name.clone());

  if let Err(errors) = parser.parse_src() {
    for error in errors {
      error.print_error();
    }
    Shed::set_status(2);
    return Ok(());
  }

  let mut nodes = parser.extract_nodes();

  // Single simple command: exec directly without forking.
  // The parser wraps single commands as Conjunction -> Pipeline -> Command.
  // Unwrap all layers to check, then set NO_FORK on the inner Command.
  if nodes.len() == 1 {
    let is_single_cmd = match &nodes[0].class {
      NdRule::Command { .. } => true,
      NdRule::Pipeline { cmds } => {
        cmds.len() == 1 && matches!(cmds[0].class, NdRule::Command { .. })
      }
      NdRule::Conjunction { elements } => {
        elements.len() == 1
          && match &elements[0].cmd.class {
            NdRule::Pipeline { cmds } => {
              cmds.len() == 1 && matches!(cmds[0].class, NdRule::Command { .. })
            }
            NdRule::Command { .. } => true,
            _ => false,
          }
      }
      _ => false,
    };
    if is_single_cmd {
      // Unwrap to the inner Command node
      let mut node = nodes.remove(0);
      loop {
        match node.class {
          NdRule::Conjunction { mut elements } => {
            node = *elements.remove(0).cmd;
          }
          NdRule::Pipeline { mut cmds } => {
            node = cmds.remove(0);
          }
          _ => break,
        }
      }
      node.flags |= NdFlags::NO_FORK;
      nodes.push(node);
    }
  }

  let mut dispatcher = Dispatcher::new(nodes, source_name);
  // exec_cmd expects a job on the stack (normally set up by exec_pipeline).
  // For the NO_FORK exec-in-place path, create one so it doesn't panic.
  dispatcher.job_stack.new_job();
  dispatcher.begin_dispatch()
}

/// Execute interactively.
///
/// Used in the main loop and other places that are guaranteed to be interacting with a tty somehow.
/// This controls whether or not the shell passes terminal control to child processes.
pub fn exec_int(input: VarStr, source_name: Option<Rc<str>>) -> ShResult<()> {
  let _guard = Shed::term_mut(|t| t.interactive_guard(true));
  exec_input(input, source_name)
}

/// Execute non-interactively
pub fn exec_nonint(input: VarStr, source_name: Option<Rc<str>>) -> ShResult<()> {
  let _guard = Shed::term_mut(|t| t.interactive_guard(false));
  exec_input(input, source_name)
}

/// Execute arbitrary shell input
///
/// This should only be called directly if you wish to inherit
/// the caller's interactive status.
pub fn exec_input(mut input: VarStr, source_name: Option<Rc<str>>) -> ShResult<()> {
  let interactive = Shed::term(Terminal::interactive);

  if !interactive || !Shed::shopts(|o| o.prompt.expand_aliases) {
    input = expand_aliases(&input).into();
  }
  let lex_flags = if interactive {
    super::lex::LexFlags::INTERACTIVE
  } else {
    super::lex::LexFlags::empty()
  };
  let source_name = source_name.unwrap_or("<unknown>".into());
  let mut parser = ParsedSrc::new(input.into())
    .with_lex_flags(lex_flags)
    .with_name(source_name.clone());
  if let Err(errors) = parser.parse_src() {
    for error in errors {
      error.print_error();
    }
    Shed::set_status(2);
    return Ok(());
  }

  let nodes = parser.extract_nodes();

  let mut dispatcher = Dispatcher::new(nodes, source_name.clone());
  dispatcher.begin_dispatch()
}

pub struct Dispatcher {
  nodes: VecDeque<Node>,
  source_name: Rc<str>,
  pub job_stack: JobStack,
  timer_stack: Vec<Option<CmdTimer>>,
  fg_job: bool,
  /// A pipe fd a forked builtin/compound segment must close in its child (the
  /// downstream read end it inherited but doesn't exec away). Set per-segment in
  /// `exec_pipeline`, consumed in `run_fork`.
  fork_close_fd: Option<RawFd>,
}

impl Dispatcher {
  pub fn new(nodes: Vec<Node>, source_name: Rc<str>) -> Self {
    let nodes = VecDeque::from(nodes);
    Self {
      nodes,
      source_name,
      job_stack: JobStack::new(),
      timer_stack: vec![],
      fg_job: true,
      fork_close_fd: None,
    }
  }
  pub fn begin_dispatch(&mut self) -> ShResult<()> {
    while let Some(node) = self.nodes.pop_front() {
      let blame = node.get_span();
      self.dispatch_node(&node).try_blame(blame)?;
    }
    Ok(())
  }
  pub fn dispatch_node(&mut self, node: &Node) -> ShResult<()> {
    stacker::maybe_grow(64 * 1024, 1024 * 1024, || {
      let _guard = Shed::meta_mut(MetaTab::push_procsub_frame);

      while signals_pending() {
        // If we have received SIGINT,
        // this will stop the execution here
        // and propagate back to the functions in main.rs
        check_signals()?;
      }

      // set -n
      if Shed::shopts(|o| o.set.noexec) {
        return Ok(());
      }

      match node.class {
        NdRule::List { .. } => self.exec_list(node),
        NdRule::Conjunction { .. } => self.exec_conjunction(node),
        NdRule::Pipeline { .. } => self.exec_pipeline(node),
        NdRule::IfNode { .. } => self.exec_if(node),
        NdRule::LoopNode { .. } => self.exec_loop(node),
        NdRule::ForNode { .. } => self.exec_for_arr(node),
        NdRule::ForArith { .. } => self.exec_for_arith(node),
        NdRule::CaseNode { .. } => self.exec_case(node),
        NdRule::BraceGrp { .. } => self.exec_brc_grp(node),
        NdRule::Subshell { .. } => self.exec_subsh(node),
        NdRule::Negate { .. } => self.exec_negated(node),
        NdRule::Timed { .. } => self.exec_timed(node),
        NdRule::Command { .. } => self.dispatch_cmd(node),
        NdRule::TryNode { .. } => self.exec_try(node),
        NdRule::DeferNode { .. } => Self::exec_defer(node),

        NdRule::FuncDef { .. } => Self::exec_func_def(node),
        NdRule::Arithmetic { .. } => Self::exec_arith(node),
        NdRule::Assignment { .. } => unreachable!(),
      }
    })
  }
  pub fn exec_list(&mut self, node: &Node) -> ShResult<()> {
    let NdRule::List { commands } = &node.class else {
      unreachable!()
    };
    for node in commands {
      let blame = node.get_span();
      self.dispatch_node(node).try_blame(blame)?;
    }

    Ok(())
  }
  pub fn dispatch_cmd(&mut self, node: &Node) -> ShResult<()> {
    let (line, _) = node.get_span().clone().line_and_col();
    Shed::vars_mut(|v| v.set_var("LINENO", VarKind::Int((line + 1) as i32), VarFlags::empty()))?;

    let result = self.route_command(node, true);
    commit_underscore();
    result
  }

  /// Route a simple command to its executor: function, builtin, arithmetic,
  /// autocd, or external `exec_cmd`.
  ///
  /// `allow_func` gates function lookup. The `command` builtin passes `false`
  /// so that, per POSIX, it suppresses function lookup while still running
  /// shell builtins and external commands.
  ///
  /// `expand_to_words` is idempotent on already-expanded tokens, so this is
  /// safe to call on the pre-expanded node built by `command`/`builtin`.
  pub(crate) fn route_command(&mut self, node: &Node, allow_func: bool) -> ShResult<()> {
    let Some(cmd) = node.get_command() else {
      return self.exec_cmd(node); // Argv is empty, probably an assignment
    };
    // We need to expand this token
    // so that a command smuggled inside of a variable is routed correctly,
    // instead of only hitting the exec_cmd path
    let words = cmd.clone().expand_to_words()?;
    let Some(cmd_word) = words.iter().next().cloned() else {
      if let NdRule::Command {
        ref assignments,
        argv: _,
      } = node.class
        && !assignments.is_empty()
      {
        return self.exec_cmd(node);
      }
      return Ok(());
    };

    let cmd_tk = node.get_command();

    if allow_func && is_func(&cmd_word) {
      self.exec_func(node)
    } else if cmd.flags.contains(TkFlags::BUILTIN) || BUILTIN_NAMES.contains(&cmd_word.as_str()) {
      self.exec_builtin(node, &cmd_word)
    } else if is_arith(cmd_tk) {
      Self::exec_arith(node)
    } else if can_autocd(cmd) {
      let dir = cmd.span.as_str().to_string();
      exec_input(varstr!("cd {dir}"), Some(self.source_name.clone()))
    } else {
      self.exec_cmd(node)
    }
  }
  pub fn exec_defer(node: &Node) -> ShResult<()> {
    let NdRule::DeferNode { body } = &node.class else {
      unreachable!()
    };
    let mut body = body.clone();

    // something like `defer eval "$(shopt core.nullglob)"`
    // needs to expand at registration time, and not at
    // execution time.
    let mut err: Option<ShErr> = None;
    body.walk_tree(&mut |n| {
      if err.is_some() {
        return;
      }
      if let Err(e) = n.eager_expand() {
        err = Some(e);
      }
    });
    if let Some(e) = err {
      return Err(e);
    }

    Shed::vars_mut(|v| v.cur_scope_mut().defer_cmd(*body.clone()));
    Ok(())
  }
  pub fn exec_try(&mut self, node: &Node) -> ShResult<()> {
    let try_blame = node.get_span();
    let NdRule::TryNode { body, err, catch } = &node.class else {
      unreachable!()
    };
    let context = body.context.clone();

    // enable set -e -o pipefail temporarily
    let errexit = shopt!(set.errexit);
    let pipefail = shopt!(set.pipefail);
    shopt_mut!(set.errexit = true);
    shopt_mut!(set.pipefail = true);
    defer!(shopt_mut!(set.errexit = errexit));
    defer!(shopt_mut!(set.pipefail = pipefail));

    match self.dispatch_node(body) {
      Ok(()) => Ok(()),
      Err(e) => {
        if e.is_flow_control() {
          return Err(e);
        }

        let blame = e.src_span().cloned().unwrap_or(try_blame);

        if !err.is_empty() {
          let mut msg_parts = Vec::with_capacity(err.len());
          for tk in err {
            msg_parts.push(tk.expand_no_split()?);
          }
          let msg = msg_parts.join_with(" ");

          ShErr::at(ShErrKind::TryFailed, blame, msg)
            .with_context(context.iter())
            .print_error();
        }

        if let Some(catch) = catch
          && let Err(e) = self.dispatch_node(catch)
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
  pub fn exec_negated(&mut self, node: &Node) -> ShResult<()> {
    let NdRule::Negate { cmd } = &node.class else {
      unreachable!()
    };
    self.dispatch_node(cmd)?;
    let status = Shed::get_status();
    Shed::set_status_from_bool(status != 0);

    Ok(())
  }
  pub fn exec_timed(&mut self, node: &Node) -> ShResult<()> {
    let NdRule::Timed { cmd } = &node.class else {
      unreachable!();
    };

    self.timer_stack.push(Some(CmdTimer::new()?));
    let res = self.dispatch_node(cmd);
    self.timer_stack.pop();
    res
  }
  pub fn exec_conjunction(&mut self, conjunction: &Node) -> ShResult<()> {
    let span = conjunction.get_span().clone();
    let NdRule::Conjunction { elements } = &conjunction.class else {
      unreachable!()
    };

    if Shed::shopts(|o| o.set.verbose) {
      let command = span.as_str().to_string();
      errln!("{command}");
    }

    let mut elem_iter = elements.iter();
    let mut skip = false;
    while let Some(element) = elem_iter.next() {
      let ConjunctNode { cmd, operator } = element;
      if !skip {
        self.dispatch_node(cmd)?;
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
  fn exec_arith(arith: &Node) -> ShResult<()> {
    let NdRule::Arithmetic { body } = &arith.class else {
      unreachable!()
    };
    let result = expand_arithmetic_wrapped(body.as_str())?;
    let val: f64 = result.parse().unwrap_or(0.0);
    Shed::set_status_from_bool(val != 0.0);
    Ok(())
  }
  pub fn exec_func_def(func_def: &Node) -> ShResult<()> {
    let blame = func_def.get_span();
    let ctx = func_def.context.clone();
    let NdRule::FuncDef { name, body } = &func_def.class else {
      unreachable!()
    };
    let mut body = body.clone();
    for label in ctx.iter() {
      body.context.push_back(label.clone());
    }

    let func_name = name
      .span
      .as_str()
      .strip_suffix("()")
      .unwrap_or(name.span.as_str());

    if KEYWORDS.contains(&func_name) || matches!(func_name, "builtin" | "command") {
      return Err(sherr!(
        SyntaxErr @ name.span.clone(),
        "function: Forbidden function name `{func_name}`",
      ));
    }

    let func = ShFunc::defined(*body, blame);
    Shed::logic_mut(|l| l.insert_func(func_name, func)); // Store the AST
    if Shed::term(Terminal::interactive) {
      Shed::meta_mut(|m| {
        m.set_last_was_func_def(true);
      });
    }

    Shed::set_status(0);
    Ok(())
  }
  fn exec_func(&mut self, func: &Node) -> ShResult<()> {
    if func.flags.contains(NdFlags::FORK_BUILTINS) {
      let name = func
        .get_command()
        .map(ToString::to_string)
        .unwrap_or_default();
      let mut func_call = func.clone();
      func_call.flags.remove(NdFlags::FORK_BUILTINS);
      return self.run_fork(&name, |s| {
        if let Err(e) = s.exec_func(&func_call) {
          e.print_error();
        }
      });
    }

    // need to do this in a new scope so we can borrow func safely
    let (func_name, mut blame) = {
      // borrow func.class to avoid partial move
      let NdRule::Command { argv, .. } = &func.class else {
        unreachable!()
      };

      let Some(func_name) = argv.first() else {
        return Err(sherr!(
            InternalErr @ func.get_span(),
            "Expected function name in command position"
        ));
      };

      let name = func_name
        .clone()
        .expand()?
        .get_first_word()
        .map(Into::<Rc<str>>::into)
        .unwrap_or_default();

      (name, func_name.span.clone())
    };

    let Some(ref mut sh_func) = Shed::logic(|l| l.get_func(&func_name)) else {
      return Err(sherr!(
        InternalErr @ blame,
        "Failed to find function '{func_name}'"
      ));
    };

    let func_body = match sh_func {
      ShFunc::Defined { logic, .. } => logic,
      ShFunc::Autoload(src) => {
        Shed::logic_mut(|l| l.remove_func(&func_name)); // remove autoload from the table
        src.source(AutoloadKind::Function)?;

        // retry, passing func by value
        // the scoped assignment and borrow above are done
        // so that we can pass func untouched to dispatch_cmd()
        return self.dispatch_cmd(func);
      }
    };

    // now we take ownership
    let NdRule::Command { assignments, argv } = &func.class
    // not borrowed
    else {
      unreachable!()
    };

    let func_ctx = util::get_context(
      styled_format!("in call to function '{}'", &func_name).into(),
      &blame,
    );
    let caller_contexts: Vec<_> = func.context.iter().cloned().collect();

    let max_depth = Shed::shopts(|s| s.core.max_recurse_depth);
    let depth = Shed::meta(MetaTab::func_depth);
    if depth > max_depth {
      return Err(sherr!(
        InternalErr @ blame,
        "maximum recursion depth ({max_depth}) exceeded",
      ));
    }

    let env_vars = Self::set_assignments(assignments, AssignBehavior::Export)?;
    let _var_guard = var_ctx_guard(env_vars.into_iter().collect());

    let redirs = RedirSet::from(&func.redirs);
    let _guard = match redirs.try_apply(false) {
      RedirResult::Applied(guard) => Some(guard),
      RedirResult::NoRedirs => None,
      RedirResult::Skipped => return Ok(()),
      RedirResult::Error(e) => return Err(e),
    };

    blame.rename(func_name.clone());

    let argv = prepare_argv(argv).try_blame(blame.clone())?;

    if !func.flags.contains(NdFlags::NO_TRACE) {
      xtrace_print(&argv);
    }

    defer! {
      if let Some(trap) = Shed::logic(|l| l.get_trap(TrapTarget::Return)) {
        util::with_saved_status(|| {
          if let Err(e) = exec_nonint(trap, Some("trap RETURN".into())) {
            e.print_error();
          }
        });
      }
    }

    let _guard = scope_guard(Some(argv));
    let _func_guard = Shed::meta_mut(MetaTab::enter_func);

    // getopts OPTIND variable
    // scoped per-script and per-function call
    Shed::vars_mut(|v| v.set_var("OPTIND", VarKind::Int(1), VarFlags::LOCAL)).ok();

    for ctx in caller_contexts.into_iter().rev() {
      func_body.propagate_context(&ctx);
    }
    func_body.propagate_context(&func_ctx);
    func_body.flags = func.flags;

    let _timer = self.take_timer();
    match self.dispatch_node(&*func_body) {
      Ok(()) => Ok(()),
      Err(e) => match e.kind() {
        ShErrKind::FuncReturn(code) => {
          Shed::set_status(*code);
          Ok(())
        }
        ShErrKind::Raised(_, code) => {
          Shed::set_status(*code);
          if Shed::meta(MetaTab::func_depth) <= 1 {
            // raise builtin has been called
            // set the status, attach the function call span
            return Err(e.collapse_context());
          }

          // nested raise, continue propagating
          Err(e)
        }
        ShErrKind::ErrInterrupt => {
          // set -e caught an error
          Err(e.with_context(func_body.context.iter()))
        }
        _ => Err(e),
      },
    }
  }
  /// Run a compound command.
  ///
  /// Handles all of the necessary I/O plumbing and fork dispatch.
  fn run_compound<F>(
    &mut self,
    name: &str,
    redirs: &[RedirSpec],
    flags: NdFlags,
    blame: Span,
    logic: F,
  ) -> ShResult<()>
  where
    F: FnOnce(&mut Self) -> ShResult<()>,
  {
    let fork_builtins = flags.contains(NdFlags::FORK_BUILTINS);

    let redirs = RedirSet::from(redirs);
    let guard = match redirs.try_apply(false) {
      RedirResult::Applied(guard) => Some(guard),
      RedirResult::NoRedirs => None,
      RedirResult::Skipped => return Ok(()),
      RedirResult::Error(e) => return Err(e),
    };

    if fork_builtins {
      log::trace!("Forking compound command: {name}");
      self.run_fork(name, |s| {
        if let Err(e) = logic(s) {
          e.print_error();
        }
      })?;
      Ok(())
    } else {
      logic(self)
        .try_blame(blame)
        .map_err(|e| e.with_redirs(guard))
    }
  }
  fn exec_brc_grp(&mut self, brc_grp: &Node) -> ShResult<()> {
    let blame = brc_grp.get_span().clone();
    let NdRule::BraceGrp { body } = &brc_grp.class else {
      unreachable!()
    };

    let _timer = self.take_timer();
    let brc_grp_logic = |s: &mut Self| -> ShResult<()> {
      let _guard = shared_scope_guard();
      s.dispatch_node(&*body)?;

      Ok(())
    };

    self.run_compound(
      "brace_group",
      &brc_grp.redirs,
      brc_grp.flags,
      blame,
      brc_grp_logic,
    )
  }
  fn exec_subsh(&mut self, subsh: &Node) -> ShResult<()> {
    let NdRule::Subshell { body } = &subsh.class else {
      unreachable!()
    };
    let span = body.get_span();

    let redirs = RedirSet::from(&subsh.redirs);
    let _guard = match redirs.try_apply(false) {
      RedirResult::Applied(guard) => Some(guard),
      RedirResult::NoRedirs => None,
      RedirResult::Skipped => return Ok(()),
      RedirResult::Error(e) => return Err(e),
    };

    let body_raw = span.as_str();
    let body_display = body_raw.graphemes(true).take(70).collect::<String>();
    let name = format!("( {body_display} )");

    self.run_fork(&name, |s| {
      if let Err(e) = s.dispatch_node(&*body) {
        if let ShErrKind::CleanExit(code) = e.kind() {
          lifecycle::exit_shed(true, *code);
        }
        e.print_error();
      }
    })?;

    Ok(())
  }
  fn exec_case(&mut self, case_stmt: &Node) -> ShResult<()> {
    let blame = case_stmt.get_span().clone();
    let NdRule::CaseNode {
      pattern,
      case_blocks,
    } = &case_stmt.class
    else {
      unreachable!()
    };

    let case_logic = |s: &mut Self| -> ShResult<()> {
      let exp_pattern = pattern.clone().expand()?;
      let pattern_raw = exp_pattern
        .get_words()
        .first()
        .map(ToString::to_string)
        .unwrap_or_default();

      'outer: for block in case_blocks {
        let CaseNode { patterns, body } = block;

        for pattern in patterns {
          let pattern_exp = expand_case_pattern(pattern.span.as_str())?;
          if pattern_exp.is_empty() {
            if pattern_raw.is_empty() {
              let _guard = shared_scope_guard();
              s.dispatch_node(body)?;
              break 'outer;
            }
          } else {
            let pattern = Shed::meta_mut(|m| m.get_glob(&pattern_exp));
            if pattern.is_match(&pattern_raw) {
              let _guard = shared_scope_guard();
              s.dispatch_node(body)?;
              break 'outer;
            }
          }
        }
      }

      Ok(())
    };

    self.run_compound(
      "case",
      &case_stmt.redirs,
      case_stmt.flags,
      blame,
      case_logic,
    )
  }
  fn exec_loop(&mut self, loop_stmt: &Node) -> ShResult<()> {
    let blame = loop_stmt.get_span().clone();
    let NdRule::LoopNode { kind, cond_node } = &loop_stmt.class else {
      unreachable!();
    };

    let loop_logic = |s: &mut Self| -> ShResult<()> {
      let keep_going = |kind: LoopKind, status: i32| -> bool {
        match kind {
          LoopKind::While => status == 0,
          LoopKind::Until => status != 0,
        }
      };
      let CondNode { cond, body } = cond_node;
      let _guard = shared_scope_guard();
      'outer: loop {
        if let Err(e) = s.dispatch_node(cond) {
          Shed::set_status(1);
          return Err(e);
        }

        let status = Shed::get_status();
        if keep_going(*kind, status) {
          if let Err(mut e) = s.dispatch_node(body) {
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
        } else {
          Shed::set_status(0);
          break;
        }
      }

      Ok(())
    };

    let _loop_guard = Shed::meta_mut(MetaTab::enter_loop);
    self.run_compound(
      "loop",
      &loop_stmt.redirs,
      loop_stmt.flags,
      blame,
      loop_logic,
    )
  }
  fn exec_for_arith(&mut self, for_stmt: &Node) -> ShResult<()> {
    let blame = for_stmt.get_span().clone();
    let NdRule::ForArith {
      init,
      cond,
      step,
      body,
    } = &for_stmt.class
    else {
      unreachable!();
    };
    let for_logic = |s: &mut Self| -> ShResult<()> {
      if let Some(init_node) = init {
        s.dispatch_node(init_node)?;
      }

      'outer: loop {
        if let Some(cond_node) = cond {
          if let Err(e) = s.dispatch_node(cond_node) {
            Shed::set_status(1);
            return Err(e);
          }
          let status = Shed::get_status();
          if status != 0 {
            Shed::set_status(0);
            break;
          }
        }
        let _guard = shared_scope_guard();

        if let Err(mut e) = s.dispatch_node(&*body) {
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

        if let Some(step_node) = step
          && let Err(e) = s.dispatch_node(step_node)
        {
          Shed::set_status(1);
          return Err(e);
        }
      }

      Ok(())
    };

    let _loop_guard = Shed::meta_mut(MetaTab::enter_loop);
    self.run_compound("c_for", &for_stmt.redirs, for_stmt.flags, blame, for_logic)
  }
  fn exec_for_arr(&mut self, for_stmt: &Node) -> ShResult<()> {
    let blame = for_stmt.get_span().clone();
    let NdRule::ForNode {
      vars,
      arr,
      body,
      positional,
    } = &for_stmt.class
    else {
      unreachable!();
    };

    let for_logic = |s: &mut Self| -> ShResult<()> {
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
        to_expanded_strings(arr)?
      };
      let vars: Vec<VarStr> = to_expanded_strings(vars)?;

      'outer: for chunk in arr.chunks(vars.len()) {
        let empty = VarStr::new();
        let chunk_iter = vars
          .iter()
          .zip(chunk.iter().chain(std::iter::repeat(&empty)));

        for (var, val) in chunk_iter {
          Shed::vars_mut(|v| v.set_var(var.as_str(), VarKind::string(val), VarFlags::empty()))?;
        }

        let _guard = shared_scope_guard();

        if let Err(mut e) = s.dispatch_node(&*body) {
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
    self.run_compound("for", &for_stmt.redirs, for_stmt.flags, blame, for_logic)
  }
  fn exec_if(&mut self, if_stmt: &Node) -> ShResult<()> {
    let blame = if_stmt.get_span().clone();
    let NdRule::IfNode {
      cond_nodes,
      else_block,
    } = &if_stmt.class
    else {
      unreachable!();
    };

    let if_logic = |s: &mut Self| -> ShResult<()> {
      let mut matched = false;
      for node in cond_nodes {
        let CondNode { cond, body } = node;
        let _guard = shared_scope_guard();

        if let Err(e) = s.dispatch_node(cond) {
          Shed::set_status(1);
          return Err(e);
        }

        if Shed::get_status() == 0 {
          matched = true;
          s.dispatch_node(body)?;
          break; // Don't check remaining elif conditions
        }
      }

      if !matched {
        if let Some(body) = else_block {
          let _guard = shared_scope_guard();
          s.dispatch_node(body)?;
        } else {
          Shed::set_status(0);
        }
      }

      Ok(())
    };

    self.run_compound("if", &if_stmt.redirs, if_stmt.flags, blame, if_logic)
  }

  fn exec_one(
    &mut self,
    cmd: &Node,
    should_fork: impl Fn(&Node) -> bool,
    flags: NdFlags,
  ) -> ShResult<()> {
    let span = cmd.get_span();
    let context = cmd.context.clone();
    // it's a single command
    // just thread it through dispatch_node directly.
    // this avoids the stdio setup that follows this
    self.job_stack.new_job();
    let res = if should_fork(cmd) {
      let name = cmd
        .get_command()
        .map(ToString::to_string)
        .unwrap_or_default();

      self.run_fork(&name, |s| {
        if let Err(e) = s.dispatch_node(cmd) {
          e.print_error();
        }
      })
    } else {
      self.dispatch_node(cmd)
    };

    if let Some(job) = self.job_stack.finalize_job() {
      // just in case this somehow forked a child
      // let's handle it here. Shouldn't happen in practice
      // but you never know
      dispatch_job(job, false, Shed::term(Terminal::interactive))?;
    }
    check_err(flags, None, Some(span), &context)?;
    res
  }

  fn exec_pipeline(&mut self, pipeline: &Node) -> ShResult<()> {
    let pipeline_span = pipeline.get_span().clone();
    let pipeline_flags = pipeline.flags;
    let pipeline_context = pipeline.context.clone();
    let NdRule::Pipeline { cmds } = &pipeline.class else {
      unreachable!()
    };

    let mut cmds = cmds.clone();

    let has_redirs = !pipeline.redirs.is_empty();
    let is_bg = pipeline_flags.contains(NdFlags::BACKGROUND);
    let interactive = Shed::term(Terminal::interactive);
    let num_cmds = cmds.len();
    let last = num_cmds.saturating_sub(1);
    let mut tty_attached = false;

    // closure that tells us if a pipeline segment should fork
    let should_fork_segment = |cmd: &Node| -> bool { is_bg && num_cmds == 1 && !will_fork(cmd) };

    if cmds.len() == 1 && !is_bg && runs_inline(&cmds[0]) {
      // it's a single command. skip the I/O setup
      return self.exec_one(&cmds[0], should_fork_segment, pipeline_flags);
    }

    let _underscore_guard = (num_cmds > 1).then(suppress_underscore_guard);

    let _cooked_guard = (!is_bg && interactive).then(|| Shed::term_mut(Terminal::prepare_for_exec));

    // closure that gets the pgid we need if the child wants the tty
    let tty_controller = |s: &mut Self| -> Option<Pid> {
      (!is_bg && Shed::term(Terminal::interactive))
        .then(|| s.job_stack.curr_job_mut().unwrap().pgid())
        .flatten()
    };

    self.job_stack.new_job();
    self.fg_job = !is_bg && Shed::term(Terminal::interactive);

    let redirs = RedirSet::from(&pipeline.redirs);

    let (mut in_rdrs, mut out_rdrs) = redirs.split_by_channel();
    let mut result = Ok(());

    let mut spans = vec![];

    // trailing builtins that we don't need to fork
    let tail_start = match cmds
      .iter_mut()
      .rev()
      .position(|n| !node_has_only_builtins(n))
    {
      Some(pos) => cmds.len() - pos,
      None => 0,
    };

    let pipes = PipeGenerator::new((tail_start + 1).min(num_cmds));
    let cmds_and_pipes = cmds.iter_mut().enumerate().zip(pipes);

    // Per-stage statuses of the in-process tail, captured for the PIPESTATUS
    // splice and pipefail blame after the forked prefix is waited on.
    let mut tail_statuses: Vec<(i32, Span)> = vec![];

    for ((i, cmd), (r, w, downstream_read)) in cmds_and_pipes {
      let has_redirs = has_redirs || (r.is_some() || w.is_some());

      if num_cmds > 1 && i != tail_start {
        // builtins must fork in the middle of multi-command pipelines
        cmd.flags |= NdFlags::FORK_BUILTINS;
      }

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
          let tail: Vec<Node> = cmds[i..].to_vec();
          let name = tail
            .first()
            .and_then(Node::get_command)
            .map(ToString::to_string)
            .unwrap_or_default();
          result = self.run_fork(&name, move |s| {
            if let Err(e) = s.exec_internal_pipeline(&tail) {
              if let ShErrKind::CleanExit(code) = e.kind() {
                lifecycle::exit_shed(true, *code);
              }
              e.print_error();
            }
          });
          break;
        }
        if tail_start > 0 && Shed::term(Terminal::interactive) {
          Shed::term_mut(|t| t.attach(getpgrp())).ok();
        }
        result = match self.exec_internal_pipeline(&cmds[i..]) {
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

      spans.push(cmd.get_span());

      self.fork_close_fd = downstream_read;
      result = if should_fork_segment(cmd) {
        let name = cmd
          .get_command()
          .map(ToString::to_string)
          .unwrap_or_default();

        self.run_fork(&name, |s| {
          if let Err(e) = s.dispatch_node(cmd) {
            e.print_error();
          }
        })
      } else {
        self.dispatch_node(cmd)
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
    let dispatch_result = dispatch_job(job, is_bg, Shed::term(Terminal::interactive));

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
          VarKind::arr(codes.iter().map(i32::to_string)),
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
      pipefail_span(&spans).or(Some(pipeline_span))
    } else {
      Some(pipeline_span)
    };

    check_err(pipeline_flags, None, blame_span, &pipeline_context)?;
    Ok(())
  }

  /// Run a contiguous run of in-process builtins, wiring them together with
  /// string sinks instead of pipes. Returns each stage's exit status and span
  /// so the caller can fold them into PIPESTATUS and the pipefail blame; the
  /// first hard error short-circuits the run.
  fn exec_internal_pipeline(&mut self, cmds: &[Node]) -> ShResult<Vec<(i32, Span)>> {
    let mut prev: Option<OutputSink> = None;
    let num_cmds = cmds.len();
    let last = num_cmds.saturating_sub(1);
    let mut statuses = Vec::with_capacity(num_cmds);

    for (i, cmd) in cmds.iter().enumerate() {
      let is_last = i == last;
      let out_scope = (!is_last).then(SinkScope::new);
      let _in_scope = prev.take().map(StdinScope::push);

      let result = match &cmd.class {
        NdRule::Subshell { body } => {
          let _ceiling = isolation_guard(None);

          match self.dispatch_node(body) {
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
        _ => self.dispatch_node(cmd),
      };

      statuses.push((Shed::get_status(), cmd.span.clone()));

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

  fn exec_builtin(&mut self, cmd: &Node, cmd_name: &str) -> ShResult<()> {
    let fork_builtins = cmd.flags.contains(NdFlags::FORK_BUILTINS);

    let Some(builtin) = lookup_builtin(cmd_name) else {
      sherr!(NotFound @ cmd.get_span(), "builtin not found: {cmd_name}").print_error();
      return with_status(127);
    };

    if fork_builtins {
      log::trace!("Forking builtin: {cmd_name}");
      self.run_fork(cmd_name, |s| {
        if let Err(e) = builtin.setup_builtin(cmd, s) {
          if let ShErrKind::CleanExit(code) = e.kind() {
            lifecycle::exit_shed(true, *code);
          }
          e.print_error();
        }
      })?;
      Ok(())
    } else if let Err(e) = builtin.setup_builtin(cmd, self) {
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
  #[expect(clippy::too_many_lines)]
  pub fn exec_cmd(&mut self, cmd: &Node) -> ShResult<()> {
    let context = cmd.context.clone();
    let NdRule::Command { assignments, argv } = &cmd.class else {
      unreachable!(
        "found node class '{:?}' in exec_cmd",
        cmd.class.as_nd_kind()
      )
    };
    let assign_behavior = if argv.is_empty() {
      AssignBehavior::Set
    } else {
      AssignBehavior::Export
    };

    if let AssignBehavior::Set = assign_behavior {
      // argv is empty: a command with no command word. Perform any assignments
      // in the current shell, then apply any redirections.
      if !assignments.is_empty() {
        Shed::meta_mut(MetaTab::take_last_cmdsub_status);
        if let Err(e) = Self::set_assignments(assignments, assign_behavior) {
          Shed::set_status(1);
          e.print_error();
          return Ok(());
        }
      }
      match RedirSet::from(&cmd.redirs).try_apply(false) {
        RedirResult::Applied(_) | RedirResult::NoRedirs => {
          // command with only redirections: status 0 unless assignments
          // already produced one.
          if assignments.is_empty() {
            Shed::set_status(0);
          }
        }
        RedirResult::Skipped => {}
        RedirResult::Error(e) => return Err(e),
      }
      return Ok(());
    }
    // argv is not empty. let's set this stuff here.
    let cmd_tk = argv[0].clone();
    let cmd_name = cmd_tk.as_str();
    let exec_path = state::util::lookup_cmd(cmd_name);

    let no_fork = cmd.flags.contains(NdFlags::NO_FORK);

    // POSIX 2.8.1: a redirection failure on an ordinary command is non-fatal —
    // print, set `$?`=1, skip the command, and keep executing the rest of the
    // input. It stays fatal only for a special built-in in a non-interactive
    // shell.
    let fatal = !Shed::term(Terminal::interactive)
      && lookup_builtin(cmd_name).is_some_and(crate::builtin::Builtin::is_special);
    let _guard = match RedirSet::from(&cmd.redirs).try_apply(fatal) {
      RedirResult::Applied(guard) => Some(guard),
      RedirResult::NoRedirs => None,
      RedirResult::Skipped => return Ok(()),
      RedirResult::Error(e) => return Err(e),
    };
    let existing_pgid = self.job_stack.curr_job_mut().unwrap().pgid();

    let fg_job = self.fg_job;
    let interactive = Shed::term(Terminal::interactive);

    let expanded = prepare_argv(argv)?;
    if expanded.is_empty() {
      Shed::set_status(0);
      return Ok(());
    }

    if !cmd.flags.contains(NdFlags::NO_TRACE) {
      xtrace_print(&expanded);
    }

    let child_logic = |pgid: Option<Pid>| -> ! {
      lifecycle::setup_child();

      if let Some(pgid) = pgid {
        let _ = setpgid(Pid::from_raw(0), pgid);
      }
      if let AssignBehavior::Export = assign_behavior
        && !assignments.is_empty()
      {
        Self::set_assignments(assignments, assign_behavior).ok();
      }
      let exec_args = ExecArgs::from_expanded(expanded.clone());

      if interactive || !no_fork {
        crate::signal::reset_signals(fg_job);
      }

      let cmd = &exec_args.cmd.0;
      let span = exec_args.cmd.1;
      let cmd_raw = cmd.to_str().unwrap_or_default();

      let Err(e) = if let Some(path) = exec_path {
        let path_bytes = path.as_os_str().to_str().unwrap_or_default().as_bytes();
        let c_path = CString::new(path_bytes).unwrap_or_default();
        let mut envp = exec_args.envp.to_vec();
        envp.retain(|e| !e.as_bytes().starts_with(b"_="));
        envp.push(unsafe { CString::from_vec_unchecked([b"_=", path_bytes].concat()) });

        execve(&c_path, &exec_args.argv, &envp)
      } else {
        log::warn!("command not found in cache: {cmd_raw}");
        execvpe(cmd, &exec_args.argv, &exec_args.envp)
      };

      // execvpe only returns on error
      let print_error = |err: ShErr| {
        // try to write the error message back to the parent's socket
        let token = &*socket::PRIVATE_TOKEN;
        let response = socket::send_to_socket(&format!("PRIVATE {token} post-error {err}"));
        if response.is_err() {
          // if that failed, just print the error ourselves
          err.print_error();
        }
      };
      match e {
        Errno::ENOENT => {
          let err = sherr!(NotFound @ span, "command not found").with_context(context.iter());
          print_error(err);

          with_vars([("CMD".into(), cmd.to_str().unwrap_or_default())], || {
            autocmd!(OnCommandNotFound);
          });

          unsafe { nix::libc::_exit(127) };
        }
        Errno::EACCES => {
          let err = sherr!(BadPermission @ span, "permission denied").with_context(context.iter());
          print_error(err);

          unsafe { nix::libc::_exit(126) };
        }
        Errno::EISDIR => {
          let err = sherr!(ExecFail @ span, "is a directory").with_context(context.iter());
          print_error(err);
          unsafe { nix::libc::_exit(126) };
        }
        Errno::ENOEXEC => {
          let err = sherr!(ExecFail @ span, "exec format error").with_context(context.iter());
          print_error(err);
          unsafe { nix::libc::_exit(126) };
        }
        _ => {
          let err = sherr!(Errno(e) @ span, "{e}").with_context(context.iter());
          print_error(err);
          unsafe { nix::libc::_exit(e as i32) }
        }
      }
    };

    if no_fork {
      child_logic(existing_pgid);
    }

    match unsafe { fork()? } {
      ForkResult::Child => child_logic(existing_pgid),
      ForkResult::Parent { child } => {
        let timer = self.take_timer();
        let job = self.job_stack.curr_job_mut().unwrap();

        let child_pgid = if let Some(pgid) = existing_pgid {
          pgid
        } else if interactive {
          job.set_pgid(child);
          child
        } else {
          let pgrp = getpgrp();
          job.set_pgid(pgrp);
          pgrp
        };
        let child_proc = ChildProc::new(child, Some(cmd_name), Some(child_pgid), timer);
        job.push_child(child_proc);
      }
    }

    Ok(())
  }
  fn run_fork(&mut self, name: &str, f: impl FnOnce(&mut Self)) -> ShResult<()> {
    let existing_pgid = self.job_stack.curr_job_mut().unwrap().pgid();
    match unsafe { fork()? } {
      ForkResult::Child => {
        lifecycle::setup_child();

        let _ = setpgid(Pid::from_raw(0), existing_pgid.unwrap_or(Pid::from_raw(0)));
        crate::signal::reset_signals(self.fg_job);
        // This segment never execs, so close the downstream pipe read end it
        // inherited — otherwise it holds the reader's side open and a writer
        // that outpaces the pipe buffer deadlocks.
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
        } else {
          job.set_pgid(child);
          child
        };
        let child_proc = ChildProc::new(child, Some(name), Some(child_pgid), timer);
        job.push_child(child_proc);
        Ok(())
      }
    }
  }
  pub fn take_timer(&mut self) -> Option<CmdTimer> {
    self.timer_stack.last_mut().and_then(Option::take)
  }
  #[expect(clippy::too_many_lines)]
  pub fn set_assignments(assigns: &[Node], behavior: AssignBehavior) -> ShResult<Vec<VarStr>> {
    let mut new_env_vars = vec![];
    let mut flags = match behavior {
      AssignBehavior::Export => VarFlags::EXPORT,
      AssignBehavior::Set => VarFlags::empty(),
    };
    if Shed::shopts(|o| o.set.allexport) {
      flags = VarFlags::EXPORT;
    }

    let trace = Shed::shopts(|o| o.set.xtrace);

    for assign in assigns {
      let is_arr = assign.flags.contains(NdFlags::ARR_ASSIGN);
      let span = assign.get_span();
      let NdRule::Assignment { kind, var, val } = &assign.class else {
        unreachable!()
      };
      let old_status = Shed::get_status();
      let var_name = var.span.as_str();
      // For an assignment-only command (Set), the exit status is the last
      // command substitution's status (or 0). Reset `$?` before expanding so
      // a leftover non-zero status from a prior command can't masquerade as an
      // expansion failure; the cmdsub cell tracks whether one actually ran.
      if matches!(behavior, AssignBehavior::Set) {
        Shed::set_status(0);
      }
      let is_integer = !is_arr
        && Shed::vars(|v| v.get_var_flags(var_name)).is_some_and(|f| f.contains(VarFlags::INTEGER));
      let val = if is_arr {
        VarKind::arr_from_tk(val)?
      } else if is_integer {
        let raw = val.expand_no_split()?;
        let n = expand_arithmetic(&raw)
          .ok()
          .and_then(|s| s.parse::<i32>().ok())
          .unwrap_or(0);
        VarKind::Int(n)
      } else {
        VarKind::string(val.expand_no_split()?)
      };
      let param_expansion_status = (Shed::get_status() != 0).then(Shed::get_status);

      // Parse and expand array index BEFORE entering write_vars borrow
      let indexed = state::util::parse_arr_bracket(var_name)
        .map(|(name, idx_raw)| state::util::expand_arr_index(&idx_raw, true).map(|idx| (name, idx)))
        .transpose()?;

      let indexed = if let Some((name, idx)) = indexed {
        let tag = Shed::vars(|v| v.try_get_var_kind_tag(&name)).unwrap_or(VarKindTag::Arr);
        Some((name, idx.resolve_for(tag)?))
      } else {
        None
      };

      if trace {
        let op = match kind {
          AssignKind::Eq => "=",
          AssignKind::PlusEq => "+=",
          AssignKind::MinusEq => "-=",
          AssignKind::MultEq => "*=",
          AssignKind::DivEq => "/=",
        };
        // Arrays render as `(a b c)`, matching bash's trace; scalars/ints use
        // their plain value.
        // bash quotes only the value (`x='a b'`, `a=('c d' e)`), not the
        // `name=` prefix, so quote the value/elements and emit the assembled
        // line directly (xtrace_line doesn't re-quote).
        let rhs = match &val {
          VarKind::Arr(items) => {
            let items = items.iter().map(expand::xtrace_quote).join(" ");
            format!("({items})")
          }
          other => expand::xtrace_quote(other.to_string()),
        };
        xtrace_line(&format!("{var_name}{op}{rhs}"));
      }

      match kind {
        AssignKind::Eq => {
          if let Some((name, idx)) = indexed {
            Shed::vars_mut(|v| v.set_var_indexed(&name, idx, val.to_string(), flags))?;
          } else {
            Shed::vars_mut(|v| v.set_var(var_name, val.clone(), flags))?;
          }
        }
        op
        @ (AssignKind::PlusEq | AssignKind::MinusEq | AssignKind::MultEq | AssignKind::DivEq) => {
          if matches!(op, AssignKind::PlusEq)
            && indexed.is_none()
            && matches!(behavior, AssignBehavior::Set)
          {
            let took_fast = Shed::vars_mut(|v| -> ShResult<bool> {
              let Ok(items) = v.get_arr_mut(var_name) else {
                return Ok(false);
              };
              match &val {
                VarKind::Int(n) => items.push_back(n.to_string().into()),
                VarKind::Str(s) => items.push_back(s.clone()),
                VarKind::Arr(other) => items.extend(other.iter().cloned()),
                VarKind::Magic(n) => {
                  if let Some(s) = n() {
                    items.push_back(s);
                  }
                }
                VarKind::AssocArr(_) => {
                  return Err(sherr!(
                    InvalidAssignment @ span.clone(),
                    "cannot append associative array to indexed array"
                  ));
                }
                VarKind::Unset => {
                  return Err(sherr!(
                    InvalidAssignment @ span.clone(),
                    "cannot append unset value to indexed array"
                  ));
                }
              }
              Ok(true)
            })?;
            if took_fast {
              let status = if matches!(behavior, AssignBehavior::Set) {
                // Assignment-only command: exit status is the last command
                // substitution's status (or 0), not the pre-assignment status.
                param_expansion_status
                  .unwrap_or_else(|| Shed::meta(MetaTab::last_cmdsub_status).unwrap_or(0))
              } else {
                param_expansion_status.unwrap_or(old_status)
              };

              Shed::set_status(status);
              continue;
            }
          }

          let mut var = if let Some((name, idx)) = &indexed {
            Shed::vars(|v| v.index_var(name, idx))?.into()
          } else {
            Shed::vars(|v| v.try_get_var_meta(var_name)).unwrap_or_else(|| {
              let kind = if is_arr {
                VarKind::Arr(VecDeque::new())
              } else {
                VarKind::string(String::new())
              };
              Var::new(kind, VarFlags::empty())
            })
          };

          let op_name = match op {
            AssignKind::PlusEq => "add to",
            AssignKind::MinusEq => "subtract from",
            AssignKind::MultEq => "multiply",
            AssignKind::DivEq => "divide",
            AssignKind::Eq => unreachable!(),
          };

          let parse_rhs = |span: &Span| -> ShResult<i32> {
            val.to_string().parse::<i32>().map_err(
              |_| sherr!(InvalidAssignment @ span.clone(), "cannot {op_name} non-integer value"),
            )
          };

          let check_div_zero = |other: i32, span: &Span| -> ShResult<()> {
            if matches!(op, AssignKind::DivEq) && other == 0 {
              return Err(sherr!(InvalidAssignment @ span.clone(), "division by zero"));
            }
            Ok(())
          };

          // A declared-but-unset variable behaves as the zero value of its type
          // for compound assignment (`local x; x+=foo` -> "foo"; a `-i` var
          // starts from 0), so normalize it before applying the operator.
          if matches!(var.kind(), VarKind::Unset) {
            let zero = if var.flags().contains(VarFlags::INTEGER) {
              VarKind::Int(0)
            } else if is_arr {
              VarKind::Arr(VecDeque::new())
            } else {
              VarKind::string(String::new())
            };
            *var.kind_mut() = zero;
          }

          match var.kind_mut() {
            VarKind::Str(s) => {
              if matches!(op, AssignKind::PlusEq) {
                let other = val.to_string();
                *s = [s.as_str(), &other].join("").into();
              } else {
                let n = s.parse::<i32>().map_err(
                  |_| sherr!(InvalidAssignment @ span.clone(), "cannot {op_name} string variable"),
                )?;
                let other = parse_rhs(&span)?;
                check_div_zero(other, &span)?;
                *s = match op {
                  AssignKind::MinusEq => (n - other).to_string().into(),
                  AssignKind::MultEq => (n * other).to_string().into(),
                  AssignKind::DivEq => (n / other).to_string().into(),
                  _ => unreachable!(),
                };
              }
            }
            VarKind::Int(n) => {
              let other = parse_rhs(&span)?;
              check_div_zero(other, &span)?;
              match op {
                AssignKind::PlusEq => *n += other,
                AssignKind::MinusEq => *n -= other,
                AssignKind::MultEq => *n *= other,
                AssignKind::DivEq => *n /= other,
                AssignKind::Eq => unreachable!(),
              }
            }
            VarKind::Arr(items) => {
              if matches!(op, AssignKind::PlusEq) {
                match &val {
                  VarKind::Int(n) => items.push_back(n.to_string().into()),
                  VarKind::Str(s) => items.push_back(s.clone()),
                  VarKind::Arr(other) => items.extend(other.clone()),
                  VarKind::Magic(n) => {
                    if let Some(s) = n() {
                      items.push_back(s);
                    }
                  }
                  VarKind::AssocArr(_) => {
                    return Err(sherr!(
                      InvalidAssignment @ span,
                      "cannot append associative array to indexed array"
                    ));
                  }
                  VarKind::Unset => {
                    return Err(sherr!(
                      InvalidAssignment @ span,
                      "cannot append unset value to indexed array"
                    ));
                  }
                }
              } else {
                return Err(sherr!(
                  InvalidAssignment @ span,
                  "cannot {op_name} array variable"
                ));
              }
            }
            VarKind::Magic(_) => {
              return Err(sherr!(
                InvalidAssignment @ span,
                "cannot {op_name} magic variable"
              ));
            }
            VarKind::AssocArr(_) => {
              return Err(sherr!(
                InvalidAssignment @ span,
                "cannot {op_name} associative array variable"
              ));
            }
            VarKind::Unset => {
              return Err(sherr!(
                InvalidAssignment @ span,
                "cannot {op_name} unset variable"
              ));
            }
          }

          let indexed = if let Some((name, idx)) = indexed {
            let tag = Shed::vars(|v| v.try_get_var_kind_tag(&name)).unwrap_or(VarKindTag::Arr);
            Some((name, idx.resolve_for(tag)?))
          } else {
            None
          };

          if let Some((name, idx)) = indexed {
            Shed::vars_mut(|v| v.update_var_indexed(&name, idx, var.to_string()))?;
          } else {
            Shed::vars_mut(|v| v.update_var(var_name, var.kind().clone()))?;
          }
        }
      }

      let status = if matches!(behavior, AssignBehavior::Set) {
        // Assignment-only command: exit status is the last command
        // substitution's status (or 0), not the pre-assignment status.
        param_expansion_status
          .unwrap_or_else(|| Shed::meta(MetaTab::last_cmdsub_status).unwrap_or(0))
      } else {
        param_expansion_status.unwrap_or(old_status)
      };
      Shed::set_status(status);

      if matches!(behavior, AssignBehavior::Export) {
        new_env_vars.push(var_name.into());
      }
    }

    Ok(new_env_vars)
  }
}

pub fn prepare_argv(argv: &[Tk]) -> ShResult<Vec<(VarStr, Span)>> {
  prepare_argv_with(argv, false)
}

/// Same as `prepare_argv`, but with control over word-splitting per token.
/// `no_split` is set by `parse_cmd` for `[[`/`]]` commands so operands like
/// `$unset` survive expansion as the empty string instead of vanishing
/// from argv (bash `[[ ]]` semantics).
pub fn prepare_argv_with(argv: &[Tk], no_split: bool) -> ShResult<Vec<(VarStr, Span)>> {
  let mut out = Vec::with_capacity(argv.len());

  for arg in argv {
    let span = arg.span.clone();
    if no_split {
      // `=~` is the bash regex-match operator inside `[[ ]]`. The general
      // expander treats `~` immediately after a word-break `=` as a tilde
      // prefix (which is correct for things like `--arg=~` outside `[[ ]]`),
      // but here it would turn the operator into `=/home/$USER`. Skip
      // expansion for the bare operator token.
      if arg.span.as_str() == "=~" {
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

pub fn is_func_node(cmd: &Node) -> bool {
  cmd
    .get_command()
    .is_some_and(|cmd_word| is_func(cmd_word.as_str()))
}

pub fn is_func(name: &str) -> bool {
  Shed::logic(|l| l.has_command_func(name))
}

pub fn is_arith(tk: Option<&Tk>) -> bool {
  tk.is_some_and(|tk| tk.flags.contains(TkFlags::IS_ARITH))
}

pub fn can_autocd(cmd: &Tk) -> bool {
  shopt!(core.autocd) && in_cd_path(cmd) && !is_in_path(cmd)
}

pub(crate) fn is_builtin(cmd: &Node) -> bool {
  cmd.get_command().is_none_or(|cmd_word| {
    !is_func(cmd_word.as_str())
      && lookup_builtin(cmd_word.as_str()).is_some_and(|b| !b.always_forks())
      && cmd_word.flags.contains(TkFlags::BUILTIN)
  }) // empty argv: assignment-only command
}

/// Checks if a command will fork on its own or not
pub fn runs_inline(cmd: &Node) -> bool {
  match &cmd.class {
    NdRule::Command { argv, .. } => {
      if argv.is_empty() {
        // assignment-only command, will never fork
        return true;
      }
      let cmd_word = cmd.get_command().unwrap();
      is_func(cmd_word.as_str()) || cmd_word.flags.contains(TkFlags::BUILTIN)
    }
    NdRule::List { .. }
    | NdRule::Conjunction { .. }
    | NdRule::IfNode { .. }
    | NdRule::LoopNode { .. }
    | NdRule::ForNode { .. }
    | NdRule::ForArith { .. }
    | NdRule::CaseNode { .. }
    | NdRule::BraceGrp { .. }
    | NdRule::TryNode { .. }
    | NdRule::DeferNode { .. }
    | NdRule::Negate { .. }
    | NdRule::Timed { .. }
    | NdRule::Arithmetic { .. }
    | NdRule::FuncDef { .. } => true,
    NdRule::Subshell { .. } | NdRule::Pipeline { .. } | NdRule::Assignment { .. } => false,
  }
}

pub fn will_fork(cmd: &Node) -> bool {
  match &cmd.class {
    NdRule::Subshell { .. } => true,
    NdRule::Command { argv, .. } if !argv.is_empty() => {
      let cmd_word = cmd.get_command().unwrap();
      !(is_func(cmd_word.as_str()) || cmd_word.flags.contains(TkFlags::BUILTIN))
    }
    _ => false,
  }
}

pub fn pipefail_span(spans: &[Span]) -> Option<Span> {
  let pipestatus = Shed::vars(|v| v.try_get_arr_elems("PIPESTATUS")).ok()?;
  for (i, status) in pipestatus.into_iter().enumerate().rev() {
    let status = status.parse::<usize>().ok()?;
    if status != 0 {
      return spans.get(i).cloned();
    }
  }
  None
}

pub fn check_err(
  flags: NdFlags,
  err: Option<ShErr>,
  span: Option<Span>,
  context: &LabelCtx,
) -> ShResult<()> {
  if Shed::get_status() != 0 && !flags.contains(NdFlags::NOT_ERR) {
    if let Some(trap) = Shed::logic(|l| l.get_trap(TrapTarget::Error)) {
      util::with_saved_status(|| exec_nonint(trap, Some("trap ERR".into())))?;
    }
    if Shed::shopts(|o| o.set.errexit) {
      if let Some(mut e) = err {
        e.set_kind(ShErrKind::ErrInterrupt);
        e.persist_redirs();
        return Err(e.with_context(context.iter()));
      } else if let Some(span) = span {
        return Err(
          sherr!(
              ErrInterrupt @ span,
              "Command returned non-zero exit status",
          )
          .with_context(context.iter()),
        );
      }
      return Err(
        sherr!(ErrInterrupt, "Command returned non-zero exit status",).with_context(context.iter()),
      );
    }
  }
  Ok(())
}
#[cfg(test)]
mod tests {
  use crate::assert_status_eq;
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  // ===================== while/until status =====================

  #[test]
  fn while_loop_status_zero_after_completion() {
    let _g = TestGuard::new();
    test_input("while false; do :; done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn while_loop_status_zero_after_iterations() {
    let _g = TestGuard::new();
    test_input("X=0; while [[ $X -lt 3 ]]; do X=$((X+1)); done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn until_loop_status_zero_after_completion() {
    let _g = TestGuard::new();
    test_input("until true; do :; done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn until_loop_status_zero_after_iterations() {
    let _g = TestGuard::new();
    test_input("X=3; until [[ $X -le 0 ]]; do X=$((X-1)); done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn while_break_preserves_status() {
    let _g = TestGuard::new();
    test_input("while true; do break; done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn while_body_status_propagates() {
    let _g = TestGuard::new();
    test_input("X=0; while [[ $X -lt 1 ]]; do X=$((X+1)); false; done").unwrap();
    // Loop body ended with `false` (status 1), but the loop itself
    // completed normally when the condition failed, so status should be 0
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== pipeline `exit` stage =====================

  #[test]
  fn pipeline_last_stage_exit_sets_status() {
    // A bare `exit N` pipeline stage sets the pipeline status to N (bash), not
    // print the CleanExit box and drop it. `exit` forks (always_forks), so this
    // exercises the forked-builtin CleanExit handling in `run_fork`/`exec_builtin`.
    let _g = TestGuard::new();
    test_input("false | exit 3").unwrap();
    assert_eq!(state::Shed::get_status(), 3);
  }

  #[test]
  fn pipeline_exit_stage_respects_pipefail() {
    let _g = TestGuard::new();
    test_input("set -o pipefail; true | exit 4").unwrap();
    assert_eq!(state::Shed::get_status(), 4);
  }

  #[test]
  fn pipeline_exit_stage_does_not_kill_shell() {
    // `exit` in a stage terminates only that stage's subshell; the parent shell
    // keeps running the rest of the input.
    let g = TestGuard::new();
    test_input("echo hi | exit 0; echo marker").unwrap();
    assert!(
      g.read_output().contains("marker"),
      "shell did not continue after a pipeline `exit` stage"
    );
  }

  // ===================== if/elif/else status =====================

  #[test]
  fn if_true_body_status() {
    let _g = TestGuard::new();
    test_input("if true; then echo ok; fi").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn if_false_no_else_status() {
    let _g = TestGuard::new();
    test_input("if false; then echo ok; fi").unwrap();
    // No branch taken, POSIX says status is 0
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn if_else_branch_status() {
    let _g = TestGuard::new();
    test_input("if false; then true; else false; fi").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  // ===================== for loop status =====================

  #[test]
  fn for_loop_empty_list_status() {
    let _g = TestGuard::new();
    test_input("for x in; do echo $x; done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn for_loop_body_status() {
    let _g = TestGuard::new();
    test_input("for x in a b c; do true; done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn for_loop_empty_array_at_zero_iterations() {
    // Regression: ${arr[@]} on an empty array used to emit one empty
    // word instead of zero words, causing `for i in "${arr[@]}"` to
    // loop once with $i="" instead of looping zero times. Symmetric
    // with "$@" when there are no positional args.
    //
    // Note: bash distinguishes arr=() (zero words) from arr=("")
    // (one empty word). Shed currently collapses both at the downstream
    // expansion layer; only the zero-elements case is fixed. The
    // one-empty-element case is a separate, deeper bug that needs
    // word-splitting changes — not covered here.
    let guard = TestGuard::new();
    test_input("arr=()").unwrap();
    test_input(r#"for i in "${arr[@]}"; do echo loop; done"#).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "");
  }

  #[test]
  fn for_loop_empty_assoc_array_zero_iterations() {
    let guard = TestGuard::new();
    test_input("declare -A am").unwrap();
    test_input(r#"for i in "${am[@]}"; do echo loop; done"#).unwrap();
    let out = guard.read_output();
    assert_eq!(out, "");
  }

  // ===================== case status =====================

  #[test]
  fn case_match_status() {
    let _g = TestGuard::new();
    test_input("case foo in foo) true;; esac").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn case_no_match_status() {
    let _g = TestGuard::new();
    test_input("case foo in bar) true;; esac").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== case pattern whitespace / paren / alternative =====================
  // Regressions for issue #52: POSIX permits whitespace before `)`, an
  // optional leading `(`, and `|` alternatives with arbitrary whitespace.

  #[test]
  fn case_space_before_close_paren() {
    let g = TestGuard::new();
    test_input("case x in * ) echo hit ;; esac").unwrap();
    assert_eq!(g.read_output(), "hit\n");
  }

  #[test]
  fn case_leading_open_paren() {
    let g = TestGuard::new();
    test_input("case x in (*) echo hit ;; esac").unwrap();
    assert_eq!(g.read_output(), "hit\n");
  }

  #[test]
  fn case_leading_paren_with_inner_whitespace() {
    let g = TestGuard::new();
    test_input("case x in ( * ) echo hit ;; esac").unwrap();
    assert_eq!(g.read_output(), "hit\n");
  }

  #[test]
  fn case_pipe_alternatives_no_spaces() {
    let g = TestGuard::new();
    test_input("case b in a|b|c) echo hit ;; esac").unwrap();
    assert_eq!(g.read_output(), "hit\n");
  }

  #[test]
  fn case_pipe_alternatives_with_spaces() {
    let g = TestGuard::new();
    test_input("case b in a | b | c ) echo hit ;; esac").unwrap();
    assert_eq!(g.read_output(), "hit\n");
  }

  #[test]
  fn case_paren_wrapped_alternatives() {
    let g = TestGuard::new();
    test_input("case b in (a | b | c) echo hit ;; esac").unwrap();
    assert_eq!(g.read_output(), "hit\n");
  }

  #[test]
  fn case_quoted_pattern_with_space_is_literal() {
    let g = TestGuard::new();
    test_input("case 'foo bar' in \"foo bar\") echo hit ;; *) echo miss ;; esac").unwrap();
    assert_eq!(g.read_output(), "hit\n");
  }

  #[test]
  fn case_glob_pattern_still_works() {
    let g = TestGuard::new();
    test_input("case hello.txt in *.txt ) echo hit ;; esac").unwrap();
    assert_eq!(g.read_output(), "hit\n");
  }

  #[test]
  fn case_multiple_paren_wrapped_arms() {
    let g = TestGuard::new();
    test_input("case mid in (first) echo a ;; (mid) echo b ;; (*) echo c ;; esac").unwrap();
    assert_eq!(g.read_output(), "b\n");
  }

  // ===================== other stuff =====================

  #[test]
  fn for_loop_var_zip() {
    let g = TestGuard::new();
    test_input("for a b in 1 2 3 4 5 6; do echo $a $b; done").unwrap();
    let out = g.read_output();
    assert_eq!(out, "1 2\n3 4\n5 6\n");
  }

  #[test]
  fn for_loop_unsets_zipped() {
    let g = TestGuard::new();
    test_input("for a b c d in 1 2 3 4 5 6; do echo $a $b $c $d; done").unwrap();
    let out = g.read_output();
    assert_eq!(out, "1 2 3 4\n5 6\n");
  }

  // ===================== set -e + builtin failure =====================

  #[test]
  fn set_e_aborts_on_failing_builtin() {
    // `cd /nonexistent` exits non-zero from a builtin. Under `set -e`
    // the failure should propagate as ErrInterrupt and prevent the
    // following command from running, same as for external commands.
    // Regression guard against builtins being silently exempted from
    // errexit checks.
    let g = TestGuard::new();
    let result = test_input("set -e; cd /__set_e_test_no_such_dir_xyz__; echo SHOULD_NOT_RUN");
    assert!(
      result.is_err(),
      "expected set -e to surface ErrInterrupt for failing builtin"
    );
    let out = g.read_output();
    assert!(
      !out.contains("SHOULD_NOT_RUN"),
      "set -e should have aborted before the second command ran; got: {out:?}"
    );
  }

  #[test]
  fn no_set_e_continues_past_failing_builtin() {
    // Companion to the above: without `set -e`, the failing cd should
    // set $? but execution should continue to the echo. Establishes
    // that the previous test is meaningful (the abort is *because of*
    // set -e, not because builtin errors always abort).
    let g = TestGuard::new();
    test_input("cd /__set_e_test_no_such_dir_xyz__; echo did_continue").unwrap();
    let out = g.read_output();
    assert!(
      out.contains("did_continue"),
      "without set -e, execution should continue past the failed cd; got: {out:?}"
    );
  }

  // ===================== negation (!) status =====================

  #[test]
  fn negate_true() {
    let _g = TestGuard::new();
    test_input("! true").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn negate_false() {
    let _g = TestGuard::new();
    test_input("! false").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn double_negate_true() {
    let _g = TestGuard::new();
    test_input("! ! true").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn double_negate_false() {
    let _g = TestGuard::new();
    test_input("! ! false").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn negate_pipeline_last_cmd() {
    let _g = TestGuard::new();
    // pipeline status = last cmd (false) = 1, negated -> 0
    test_input("! true | false").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn negate_pipeline_last_cmd_true() {
    let _g = TestGuard::new();
    // pipeline status = last cmd (true) = 0, negated -> 1
    test_input("! false | true").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn negate_in_conjunction() {
    let _g = TestGuard::new();
    // ! binds to pipeline, not conjunction: (! (true && false)) && true
    test_input("! (true && false) && true").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn negate_in_if_condition() {
    let g = TestGuard::new();
    test_input("if ! false; then echo yes; fi").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(g.read_output(), "yes\n");
  }

  #[test]
  fn empty_var_in_test() {
    let _g = TestGuard::new();
    // Quoted unset variable expands to an empty string — `[ -n "" ]` is false.
    test_input("[ -n \"$EMPTYVAR_PROBABLY_NOT_SET_TO_ANYTHING\" ]").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
    // POSIX `[`: the unset/unquoted operand vanishes via word-splitting, so
    // argv reaching the builtin is just `[ -n ]`. The arity-1 rule treats the
    // lone `-n` as a literal string to test for non-emptiness (true, status 0).
    test_input("[ -n $EMPTYVAR_PROBABLY_NOT_SET_TO_ANYTHING ]").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== command lists in compound statements =====================
  // POSIX §2.9.4: conditions and bodies of compound statements are command lists,
  // not single commands. Multiple statements separated by `;` or `\n` are valid;
  // the exit status of the last command in the list determines the condition.

  #[test]
  fn if_multi_stmt_condition_last_true() {
    let _g = TestGuard::new();
    test_input("if true; true; then false; fi").unwrap();
    // Condition's last command (true) → enters then-branch → false → status 1
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn if_multi_stmt_condition_last_false() {
    let _g = TestGuard::new();
    test_input("if true; false; then echo a; else echo b; fi").unwrap();
    // Condition's last command (false) → else-branch
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn if_multi_stmt_condition_output() {
    let g = TestGuard::new();
    test_input("if echo a; echo b; then echo c; fi").unwrap();
    // All three commands run; condition's last echo is success
    let out = g.read_output();
    assert_eq!(out, "a\nb\nc\n");
  }

  #[test]
  fn if_multi_stmt_body() {
    let g = TestGuard::new();
    test_input("if true; then echo a; echo b; echo c; fi").unwrap();
    let out = g.read_output();
    assert_eq!(out, "a\nb\nc\n");
  }

  #[test]
  fn if_multi_stmt_body_status_is_last() {
    let _g = TestGuard::new();
    test_input("if true; then true; false; fi").unwrap();
    // Body's last command (false) determines if-statement's status
    assert_eq!(state::Shed::get_status(), 1);
  }

  // ===================== scope sharing between cond and body =====================

  #[test]
  fn if_cond_assignment_visible_in_body() {
    // Assignments in an if-condition need to be visible to the matching
    // body — that's the motivation for sharing a scope between cond and
    // body. Regression for `if foo=$(...); then use $foo; fi`.
    let g = TestGuard::new();
    test_input("if foo=hello; then echo \"got $foo\"; fi").unwrap();
    assert_eq!(g.read_output(), "got hello\n");
  }

  #[test]
  fn while_cond_assignment_visible_in_body() {
    // `while line=$(read_next); do use $line; done` style: cond writes a
    // variable each iteration, body reads it.
    let g = TestGuard::new();
    test_input(
      r#"
        i=0
        while i=$((i + 1)); [ $i -le 3 ]; do
          echo "iter $i"
        done
      "#,
    )
    .unwrap();
    assert_eq!(g.read_output(), "iter 1\niter 2\niter 3\n");
  }

  #[test]
  fn while_cond_mutations_persist_across_iterations() {
    // Regression for the OPTIND-in-getopts case: a while-loop's cond and
    // body share a single scope spanning all iterations, so mutations made
    // in cond on iteration N are visible on iteration N+1. If the scope
    // were per-iteration, this would loop forever (counter resets each
    // time) or exit immediately depending on the variable.
    let g = TestGuard::new();
    test_input(
      r#"
        n=0
        while n=$((n + 1)); [ $n -lt 4 ]; do
          echo "tick"
        done
        echo "final n=$n"
      "#,
    )
    .unwrap();
    assert_eq!(g.read_output(), "tick\ntick\ntick\nfinal n=4\n");
  }

  #[test]
  fn while_multi_stmt_condition_never_enters() {
    let g = TestGuard::new();
    test_input("while echo a; false; do echo b; done").unwrap();
    // Condition's last command (false) → loop never enters body
    let out = g.read_output();
    assert_eq!(out, "a\n");
  }

  #[test]
  fn until_multi_stmt_condition() {
    let g = TestGuard::new();
    test_input("x=0; until echo iter; [ $x -ge 2 ]; do x=$((x+1)); done").unwrap();
    // Condition's last command negated → loops until [ $x -ge 2 ] is true
    let out = g.read_output();
    assert_eq!(out, "iter\niter\niter\n");
  }

  #[test]
  fn brc_grp_multi_stmt_body() {
    let g = TestGuard::new();
    test_input("{ echo a; echo b; echo c; }").unwrap();
    let out = g.read_output();
    assert_eq!(out, "a\nb\nc\n");
  }

  #[test]
  fn func_multi_stmt_body() {
    let g = TestGuard::new();
    test_input("f() { echo a; echo b; echo c; }; f").unwrap();
    let out = g.read_output();
    assert_eq!(out, "a\nb\nc\n");
  }

  #[test]
  fn for_multi_stmt_body() {
    let g = TestGuard::new();
    test_input("for x in 1 2; do echo $x; echo done-$x; done").unwrap();
    let out = g.read_output();
    assert_eq!(out, "1\ndone-1\n2\ndone-2\n");
  }

  #[test]
  fn case_arm_multi_stmt_body() {
    let g = TestGuard::new();
    test_input("case foo in foo) echo a; echo b; echo c;; esac").unwrap();
    let out = g.read_output();
    assert_eq!(out, "a\nb\nc\n");
  }

  #[test]
  fn backgrounded_brace_group_forks_no_state_leak() {
    // Regression (Bug A): a backgrounded builtin/brace-group must run in a
    // child, not inline. If it ran inline, the assignment would leak into the
    // parent and `$x` would be 5.
    let g = TestGuard::new();
    test_input("x=0; { x=5; } & wait; echo \"[$x]\"").unwrap();
    let out = g.read_output();
    assert!(
      out.contains("[0]"),
      "brace group leaked into parent: {out:?}"
    );
  }

  #[test]
  fn backgrounded_builtin_reports_real_status_via_wait() {
    // Regression (Bug A + wait ECHILD): `wait` on a finished backgrounded
    // builtin reports its real exit status, not 0.
    let g = TestGuard::new();
    test_input("false & wait %1; echo \"[$?]\"").unwrap();
    let out = g.read_output();
    assert!(out.contains("[1]"), "got: {out:?}");
  }

  #[test]
  fn mixed_and_or_with_sequence() {
    let g = TestGuard::new();
    test_input("true && echo a; false || echo b; echo c").unwrap();
    // && and || chains coexist with ; sequencing — all three echos run
    let out = g.read_output();
    assert_eq!(out, "a\nb\nc\n");
  }

  #[test]
  fn nested_compounds_with_lists() {
    let g = TestGuard::new();
    test_input("if true; true; then if true; then echo a; echo b; fi; echo c; fi").unwrap();
    let out = g.read_output();
    assert_eq!(out, "a\nb\nc\n");
  }

  #[test]
  fn top_level_sequence_runs_all() {
    let g = TestGuard::new();
    test_input("echo a; echo b; echo c").unwrap();
    let out = g.read_output();
    assert_eq!(out, "a\nb\nc\n");
  }

  #[test]
  fn top_level_sequence_status_is_last() {
    let _g = TestGuard::new();
    test_input("true; true; false").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  // ===================== function bodies as compound commands =====================
  // POSIX §2.9.5: function_body is compound_command, not just a brace group.
  // Every compound command type should be a valid function body.

  #[test]
  fn func_body_subshell() {
    let g = TestGuard::new();
    test_input("f() ( echo a; echo b ); f").unwrap();
    let out = g.read_output();
    assert_eq!(out, "a\nb\n");
  }

  #[test]
  fn func_body_subshell_isolates_state() {
    let g = TestGuard::new();
    // Subshell-bodied function shouldn't leak variable changes to caller.
    test_input("x=outer; f() ( x=inner; echo $x ); f; echo $x").unwrap();
    let out = g.read_output();
    assert_eq!(out, "inner\nouter\n");
  }

  #[test]
  fn func_body_brace_grp_leaks_state() {
    let g = TestGuard::new();
    // Counter-test: brace-bodied function DOES leak (no fork).
    test_input("x=outer; f() { x=inner; echo $x; }; f; echo $x").unwrap();
    let out = g.read_output();
    assert_eq!(out, "inner\ninner\n");
  }

  #[test]
  fn func_body_if() {
    let g = TestGuard::new();
    test_input("f() if true; then echo yes; else echo no; fi; f").unwrap();
    let out = g.read_output();
    assert_eq!(out, "yes\n");
  }

  #[test]
  fn func_body_if_takes_arg() {
    let g = TestGuard::new();
    test_input("f() if [ \"$1\" = ok ]; then echo good; else echo bad; fi; f ok; f nope").unwrap();
    let out = g.read_output();
    assert_eq!(out, "good\nbad\n");
  }

  #[test]
  fn func_body_while() {
    let g = TestGuard::new();
    test_input("f() while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done; i=0; f").unwrap();
    let out = g.read_output();
    assert_eq!(out, "0\n1\n2\n");
  }

  #[test]
  fn func_body_until() {
    let g = TestGuard::new();
    test_input("f() until [ $i -ge 2 ]; do echo $i; i=$((i+1)); done; i=0; f").unwrap();
    let out = g.read_output();
    assert_eq!(out, "0\n1\n");
  }

  #[test]
  fn func_body_for() {
    let g = TestGuard::new();
    test_input("f() for x in a b c; do echo $x; done; f").unwrap();
    let out = g.read_output();
    assert_eq!(out, "a\nb\nc\n");
  }

  #[test]
  fn func_body_case() {
    let g = TestGuard::new();
    test_input(
      "classify() case $1 in foo) echo F;; bar) echo B;; *) echo other;; esac; \
       classify foo; classify bar; classify quux",
    )
    .unwrap();
    let out = g.read_output();
    assert_eq!(out, "F\nB\nother\n");
  }

  #[test]
  fn func_body_status_propagates() {
    let _g = TestGuard::new();
    // Function exit status should be the last command's status, regardless
    // of which compound command shape the body uses.
    test_input("f() ( false ); f").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn func_body_recursive_with_if() {
    let g = TestGuard::new();
    // Recursive function whose body is an if-else (not a brace group).
    test_input(
      "countdown() if [ $1 -le 0 ]; then echo done; else echo $1; countdown $(($1 - 1)); fi; \
       countdown 3",
    )
    .unwrap();
    let out = g.read_output();
    assert_eq!(out, "3\n2\n1\ndone\n");
  }

  #[test]
  fn nested_cmd_sub_index() {
    let g = TestGuard::new();
    test_input("foo=(bar biz bam); echo \"$(echo ${foo[$(echo 1)+1]})\"").unwrap();
    let out = g.read_output();
    assert_eq!(out, "bam\n");
  }

  #[test]
  fn nested_cmd_sub_index_with_space() {
    let g = TestGuard::new();
    test_input("foo=(bar biz bam); echo \"$(echo ${foo[$(echo 1) + 1]})\"").unwrap();
    let out = g.read_output();
    assert_eq!(out, "bam\n");
  }

  // ===================== Assignment operators =====================

  use crate::var;

  // ─── Eq ─────────────────────────────────────────────────────────────

  #[test]
  fn assign_eq_basic() {
    let _g = TestGuard::new();
    test_input("x=hello").unwrap();
    assert_eq!(var!("x"), "hello");
  }

  #[test]
  fn assign_eq_overwrites() {
    let _g = TestGuard::new();
    test_input("x=hello").unwrap();
    test_input("x=world").unwrap();
    assert_eq!(var!("x"), "world");
  }

  #[test]
  fn assign_eq_cmd_sub_preserves_whitespace() {
    // Regression: assignment RHS was going through expand_to_words+join,
    // collapsing runs of whitespace into single spaces. POSIX says
    // assignment context does not apply word splitting.
    let _g = TestGuard::new();
    test_input(r#"ws=$(echo "FOO    BAR")"#).unwrap();
    assert_eq!(var!("ws"), "FOO    BAR");
  }

  #[test]
  fn assign_eq_cmd_sub_preserves_newlines() {
    let _g = TestGuard::new();
    test_input(r"ml=$(printf 'a\nb\nc')").unwrap();
    assert_eq!(var!("ml"), "a\nb\nc");
  }

  // ─── Assignment exit status (POSIX §2.9.1) ─────────────────────────
  // An assignment-only command's exit status is the status of the last
  // command substitution performed, or 0 if there were none. The prior
  // command's status must NOT leak through.

  #[test]
  fn assign_only_status_is_zero_with_no_cmdsub() {
    let _g = TestGuard::new();
    test_input("false; r=hi").unwrap();
    assert_status_eq!(0);
  }

  #[test]
  fn assign_only_status_zero_does_not_leak_after_success() {
    let _g = TestGuard::new();
    test_input("true; r=hi").unwrap();
    assert_status_eq!(0);
  }

  #[test]
  fn assign_only_status_reflects_failing_cmdsub() {
    let _g = TestGuard::new();
    test_input(r"r=$(false)").unwrap();
    assert_status_eq!(1);
  }

  #[test]
  fn assign_only_status_reflects_failing_cmdsub_after_failure() {
    // The prior `false` must not be what sets the status; the cmdsub does.
    let _g = TestGuard::new();
    test_input(r"false; r=$(false)").unwrap();
    assert_status_eq!(1);
  }

  #[test]
  fn assign_only_status_reflects_succeeding_cmdsub_after_failure() {
    // Key zoxide case: a failing condition before the assignment must not
    // leak through and make a succeeding cmdsub look like it failed.
    let _g = TestGuard::new();
    test_input(r"false; r=$(true)").unwrap();
    assert_status_eq!(0);
  }

  #[test]
  fn assign_only_status_is_last_cmdsub_across_multiple() {
    let _g = TestGuard::new();
    test_input(r"r=$(false) s=$(true)").unwrap();
    assert_status_eq!(0);
  }

  #[test]
  fn assign_only_status_is_last_cmdsub_failing() {
    let _g = TestGuard::new();
    test_input(r"r=$(true) s=$(false)").unwrap();
    assert_status_eq!(1);
  }

  #[test]
  fn assign_only_status_is_last_cmdsub_then_literal() {
    // A trailing literal assignment doesn't reset the status; the last
    // cmdsub's status wins.
    let _g = TestGuard::new();
    test_input(r"r=$(false) s=hello").unwrap();
    assert_status_eq!(1);
  }

  // ─── PlusEq on strings ──────────────────────────────────────────────

  #[test]
  fn assign_plus_eq_string_var_concatenates() {
    // Even when both sides parse as int, `+=` on a plain string var
    // concatenates (per POSIX/bash). Arithmetic only happens for
    // `declare -i` typed vars. Regression: was doing arithmetic when
    // both sides looked numeric, producing sum-of-digits bugs when
    // building strings character-by-character.
    let _g = TestGuard::new();
    test_input("x=5; x+=3").unwrap();
    assert_eq!(var!("x"), "53");
  }

  #[test]
  fn assign_plus_eq_int_var_does_arithmetic() {
    // declare -i opts into arithmetic +=.
    let _g = TestGuard::new();
    test_input("declare -i y=5; y+=3").unwrap();
    assert_eq!(var!("y"), "8");
  }

  #[test]
  fn assign_plus_eq_non_numeric_concatenates() {
    let _g = TestGuard::new();
    test_input("x=hello; x+=world").unwrap();
    assert_eq!(var!("x"), "helloworld");
  }

  #[test]
  fn assign_plus_eq_sum_of_digits_regression() {
    // Specific case from the CSV parser bug: building up a string
    // one numeric char at a time. With the broken arithmetic-sniff
    // path, this produced "9" (2+0+1+6) instead of "2016".
    let _g = TestGuard::new();
    test_input(r#"buf="""#).unwrap();
    test_input(r#"buf+="2""#).unwrap();
    test_input(r#"buf+="0""#).unwrap();
    test_input(r#"buf+="1""#).unwrap();
    test_input(r#"buf+="6""#).unwrap();
    assert_eq!(var!("buf"), "2016");
  }

  #[test]
  fn assign_plus_eq_mixed_falls_back_to_concat() {
    let _g = TestGuard::new();
    test_input("x=5; x+=hello").unwrap();
    // RHS not parseable as int → concatenation.
    assert_eq!(var!("x"), "5hello");
  }

  // ─── MinusEq / MultEq / DivEq ───────────────────────────────────────

  #[test]
  fn assign_minus_eq_int_subtracts() {
    let _g = TestGuard::new();
    test_input("x=10; x-=4").unwrap();
    assert_eq!(var!("x"), "6");
  }

  #[test]
  fn assign_mult_eq_multiplies() {
    let _g = TestGuard::new();
    test_input("x=6; x*=7").unwrap();
    assert_eq!(var!("x"), "42");
  }

  #[test]
  fn assign_div_eq_divides() {
    let _g = TestGuard::new();
    test_input("x=20; x/=4").unwrap();
    assert_eq!(var!("x"), "5");
  }

  // Failed standalone assignments set status=1 AND leave the var
  // unchanged. We check both, since either alone is weaker.

  #[test]
  fn assign_div_eq_by_zero_errors_and_leaves_var_unchanged() {
    let _g = TestGuard::new();
    test_input("x=5; x/=0").ok();
    assert_ne!(state::Shed::get_status(), 0);
    assert_eq!(var!("x"), "5");
  }

  #[test]
  fn assign_minus_eq_on_non_numeric_string_errors_and_leaves_var_unchanged() {
    let _g = TestGuard::new();
    test_input("x=hello; x-=3").ok();
    assert_ne!(state::Shed::get_status(), 0);
    assert_eq!(var!("x"), "hello");
  }

  #[test]
  fn assign_mult_eq_on_non_numeric_string_errors_and_leaves_var_unchanged() {
    let _g = TestGuard::new();
    test_input("x=hello; x*=3").ok();
    assert_ne!(state::Shed::get_status(), 0);
    assert_eq!(var!("x"), "hello");
  }

  // ─── Compound ops on undefined var (treated as empty) ───────────────

  #[test]
  fn assign_plus_eq_on_undefined_var_uses_empty_string() {
    let _g = TestGuard::new();
    // No prior `x=`. += starts from an empty Str default.
    test_input("x+=hello").unwrap();
    assert_eq!(var!("x"), "hello");
  }

  // ─── Compound ops on arrays ─────────────────────────────────────────

  #[test]
  fn assign_plus_eq_on_array_appends_scalar() {
    let g = TestGuard::new();
    test_input("arr=(a b c); arr+=d; echo ${arr[3]}").unwrap();
    let out = g.read_output();
    assert!(out.contains('d'), "got: {out:?}");
  }

  #[test]
  fn assign_plus_eq_on_array_extends_with_array() {
    let g = TestGuard::new();
    test_input("arr=(a b); arr+=(c d); echo \"${arr[@]}\"").unwrap();
    let out = g.read_output();
    assert_eq!(out.trim(), "a b c d");
  }

  #[test]
  fn assign_minus_eq_on_array_errors_and_leaves_array_unchanged() {
    let g = TestGuard::new();
    // Standalone `arr-=1` errors and sets status=1; subsequent
    // statements still run, so the echo proves the array wasn't
    // mutated.
    test_input("arr=(a b); arr-=1").ok();
    assert_ne!(state::Shed::get_status(), 0);
    test_input("echo \"${arr[@]}\"").unwrap();
    let out = g.read_output();
    assert!(
      out.ends_with("a b") || out.contains("\na b"),
      "got: {out:?}"
    );
  }

  #[test]
  fn assign_mult_eq_on_array_errors_and_leaves_array_unchanged() {
    let g = TestGuard::new();
    test_input("arr=(a b); arr*=2").ok();
    assert_ne!(state::Shed::get_status(), 0);
    test_input("echo \"${arr[@]}\"").unwrap();
    let out = g.read_output();
    assert!(
      out.ends_with("a b") || out.contains("\na b"),
      "got: {out:?}"
    );
  }

  // ─── Indexed-array assignment ───────────────────────────────────────

  #[test]
  fn assign_eq_with_index_sets_element() {
    let g = TestGuard::new();
    test_input("arr=(a b c); arr[1]=X; echo \"${arr[@]}\"").unwrap();
    let out = g.read_output();
    assert_eq!(out.trim(), "a X c");
  }

  #[test]
  fn assign_eq_with_index_extends_array() {
    let g = TestGuard::new();
    // Setting an index past the end should extend.
    test_input("arr=(a b); arr[3]=z; echo ${arr[3]}").unwrap();
    let out = g.read_output();
    assert!(out.contains('z'));
  }

  // ─── Export behavior ────────────────────────────────────────────────

  #[test]
  fn assign_with_export_sets_export_flag() {
    let g = TestGuard::new();
    // Inline assignment before a command (env var for that command).
    test_input("FOO=bar env | grep ^FOO=").unwrap();
    let out = g.read_output();
    assert!(out.contains("FOO=bar"), "got: {out:?}");
  }

  // ─── allexport shopt ────────────────────────────────────────────────

  #[test]
  fn assign_with_allexport_promotes_to_export() {
    let g = TestGuard::new();
    test_input("set -a; FOO=allexported; env | grep ^FOO=").unwrap();
    let out = g.read_output();
    assert!(out.contains("FOO=allexported"), "got: {out:?}");
  }

  // ===================== is_in_path =====================
  mod is_in_path_tests {
    use super::super::is_in_path;
    use super::super::{Span, Tk};
    use crate::eval::lex::TkRule;
    use crate::tests::testutil::{TestGuard, test_input};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::rc::Rc;
    use tempfile::TempDir;

    fn tk(s: &str) -> Tk {
      let src: Rc<str> = s.into();
      let span = Span::new(0..s.len(), src);
      Tk::new(TkRule::Str, span)
    }

    fn make_exec(dir: &Path, name: &str) -> std::path::PathBuf {
      let p = dir.join(name);
      std::fs::write(&p, "#!/bin/sh\n").unwrap();
      let mut perms = std::fs::metadata(&p).unwrap().permissions();
      perms.set_mode(0o755);
      std::fs::set_permissions(&p, perms).unwrap();
      p
    }

    fn make_non_exec(dir: &Path, name: &str) -> std::path::PathBuf {
      let p = dir.join(name);
      std::fs::write(&p, "data").unwrap();
      let mut perms = std::fs::metadata(&p).unwrap().permissions();
      perms.set_mode(0o644);
      std::fs::set_permissions(&p, perms).unwrap();
      p
    }

    // ─── absolute paths ──────────────────────────────────────────────

    #[test]
    fn abs_path_to_executable_returns_true() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      let exe = make_exec(dir.path(), "prog");
      assert!(is_in_path(&tk(&exe.to_string_lossy())));
    }

    #[test]
    fn abs_path_to_nonexistent_returns_false() {
      let _g = TestGuard::new();
      assert!(!is_in_path(&tk("/this/path/should/never/exist/xyz123")));
    }

    #[test]
    fn abs_path_to_directory_returns_false() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      assert!(!is_in_path(&tk(&dir.path().to_string_lossy())));
    }

    #[test]
    fn abs_path_to_non_executable_returns_false() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      let p = make_non_exec(dir.path(), "data.txt");
      assert!(!is_in_path(&tk(&p.to_string_lossy())));
    }

    #[test]
    fn abs_path_executable_only_group_bit_returns_true() {
      // 0o111 mask matches any of user/group/other exec bits.
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      let p = dir.path().join("only_group");
      std::fs::write(&p, "").unwrap();
      let mut perms = std::fs::metadata(&p).unwrap().permissions();
      perms.set_mode(0o010); // group-execute only
      std::fs::set_permissions(&p, perms).unwrap();
      assert!(is_in_path(&tk(&p.to_string_lossy())));
    }

    // ─── bare names searched in PATH ─────────────────────────────────

    #[test]
    fn bare_name_found_in_path() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      make_exec(dir.path(), "myprog");
      test_input(format!("PATH={}", dir.path().display())).unwrap();
      assert!(is_in_path(&tk("myprog")));
    }

    #[test]
    fn bare_name_not_in_path_returns_false() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      test_input(format!("PATH={}", dir.path().display())).unwrap();
      assert!(!is_in_path(&tk("definitely_not_a_program_xyz")));
    }

    #[test]
    fn bare_name_found_in_second_path_entry() {
      let _g = TestGuard::new();
      let d1 = TempDir::new().unwrap();
      let d2 = TempDir::new().unwrap();
      make_exec(d2.path(), "second");
      test_input(format!(
        "PATH={}:{}",
        d1.path().display(),
        d2.path().display()
      ))
      .unwrap();
      assert!(is_in_path(&tk("second")));
    }

    #[test]
    fn bare_name_first_match_wins_even_if_later_entries_have_it() {
      // First entry has it; we still return true. Sanity check that the
      // loop terminates on first hit (no panic, correct result).
      let _g = TestGuard::new();
      let d1 = TempDir::new().unwrap();
      let d2 = TempDir::new().unwrap();
      make_exec(d1.path(), "dup");
      make_exec(d2.path(), "dup");
      test_input(format!(
        "PATH={}:{}",
        d1.path().display(),
        d2.path().display()
      ))
      .unwrap();
      assert!(is_in_path(&tk("dup")));
    }

    #[test]
    fn bare_name_skips_directory_entry() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      // Create a *directory* with the program name — should not match.
      std::fs::create_dir(dir.path().join("subprog")).unwrap();
      test_input(format!("PATH={}", dir.path().display())).unwrap();
      assert!(!is_in_path(&tk("subprog")));
    }

    #[test]
    fn bare_name_skips_non_executable() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      make_non_exec(dir.path(), "noexec");
      test_input(format!("PATH={}", dir.path().display())).unwrap();
      assert!(!is_in_path(&tk("noexec")));
    }

    #[test]
    fn bare_name_falls_through_nonexistent_path_entry() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      make_exec(dir.path(), "real");
      test_input(format!("PATH=/nonexistent/xyz:{}", dir.path().display())).unwrap();
      assert!(is_in_path(&tk("real")));
    }

    #[test]
    fn bare_name_with_unset_path_returns_false() {
      let _g = TestGuard::new();
      // Use `unset` so try_var!("PATH") returns None.
      test_input("unset PATH").unwrap();
      assert!(!is_in_path(&tk("ls")));
    }

    // ─── relative paths ──────────────────────────────────────────────

    #[test]
    fn dot_slash_executable_in_cwd_returns_true() {
      let mut g = TestGuard::new();
      let dir = g.in_temp_dir();
      make_exec(&dir, "prog");
      assert!(is_in_path(&tk("./prog")));
    }

    #[test]
    fn dot_slash_nonexistent_returns_false() {
      let mut g = TestGuard::new();
      let _dir = g.in_temp_dir();
      assert!(!is_in_path(&tk("./nope_xyz")));
    }

    #[test]
    fn dot_slash_non_executable_returns_false() {
      let mut g = TestGuard::new();
      let dir = g.in_temp_dir();
      make_non_exec(&dir, "data.txt");
      assert!(!is_in_path(&tk("./data.txt")));
    }

    #[test]
    fn dot_slash_directory_returns_false() {
      let mut g = TestGuard::new();
      let dir = g.in_temp_dir();
      std::fs::create_dir(dir.join("subdir")).unwrap();
      assert!(!is_in_path(&tk("./subdir")));
    }

    #[test]
    fn dotdot_slash_executable_returns_true() {
      let mut g = TestGuard::new();
      let dir = g.in_temp_dir();
      make_exec(&dir, "outerprog");
      let inner = dir.join("inner");
      std::fs::create_dir(&inner).unwrap();
      std::env::set_current_dir(&inner).unwrap();
      assert!(is_in_path(&tk("../outerprog")));
    }

    // ─── absolute paths take precedence over PATH ────────────────────

    #[test]
    fn absolute_path_does_not_consult_path_var() {
      // Even with a bogus PATH, an absolute path is resolved directly.
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      let exe = make_exec(dir.path(), "prog");
      test_input("PATH=/nonexistent/xyz").unwrap();
      assert!(is_in_path(&tk(&exe.to_string_lossy())));
    }

    // ─── expansion behavior ──────────────────────────────────────────

    #[test]
    fn expansion_resolves_var_to_path() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      let exe = make_exec(dir.path(), "prog");
      test_input(format!("MYEXE={}", exe.display())).unwrap();
      assert!(is_in_path(&tk("$MYEXE")));
    }

    #[test]
    fn expansion_resolves_var_to_bare_name_in_path() {
      let _g = TestGuard::new();
      let dir = TempDir::new().unwrap();
      make_exec(dir.path(), "myprog");
      test_input(format!("PATH={}", dir.path().display())).unwrap();
      test_input("NAME=myprog").unwrap();
      assert!(is_in_path(&tk("$NAME")));
    }

    #[test]
    fn unset_var_expansion_yields_empty_returns_false() {
      let _g = TestGuard::new();
      // An unset, unquoted var expands to nothing; first word is None,
      // so the function bails out with false.
      assert!(!is_in_path(&tk("$UNSET_VAR_FOR_ISINPATH_TEST_xyz")));
    }
  }

  // ===================== $_ (last argument) =====================

  #[test]
  fn underscore_is_previous_commands_last_arg() {
    let guard = TestGuard::new();
    test_input("echo one two\necho \"[$_]\"").unwrap();
    assert!(guard.read_output().contains("[two]"));
  }

  #[test]
  fn underscore_mid_command_refers_to_previous_not_self() {
    // `$_` in a non-final position must resolve to the previous command's last
    // arg, not this command's (regression against set-before-exec/double-expand).
    let guard = TestGuard::new();
    test_input("echo one two\necho $_ END").unwrap();
    assert!(guard.read_output().contains("two END"));
  }

  #[test]
  fn pipeline_leaves_underscore_untouched() {
    // A pipeline's stages are subshells, so the shell's `$_` is whatever it was
    // before the pipeline, not a value leaked from a stage.
    let guard = TestGuard::new();
    test_input("echo keep\necho a | echo b\necho \"[$_]\"").unwrap();
    assert!(guard.read_output().contains("[keep]"));
  }
}
