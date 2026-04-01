use std::{
  cell::Cell,
  collections::{HashSet, VecDeque},
  os::unix::fs::PermissionsExt,
};

use ariadne::Fmt;

use crate::{
  builtin::{
    BUILTINS, alias::{alias, unalias}, arrops::{arr_fpop, arr_fpush, arr_pop, arr_push, arr_rotate}, autocmd::autocmd, cd::cd, complete::{compgen_builtin, complete_builtin}, dirstack::{dirs, popd, pushd}, echo::echo, eval, exec, fixcmd::fixcmd, flowctl::flowctl, getopts::getopts, help::help, hist::hist_builtin, intro, jobctl::{self, JobBehavior, continue_job, disown, jobs}, keymap, map, msg::msg, pwd::pwd, read::{self, read_builtin}, resource::{ulimit, umask_builtin}, seek::seek, set::set_builtin, shift::shift, shopt::shopt, source::source, test::double_bracket_test, trap::{TrapTarget, trap}, varcmds::{export, local, readonly, unset}
  },
  expand::{expand_aliases, expand_case_pattern, glob_to_regex},
  jobs::{ChildProc, JobStack, attach_tty, dispatch_job},
  libsh::{
    error::{ShErr, ShErrKind, ShResult, ShResultExt, next_color},
    guards::{scope_guard, var_ctx_guard},
    utils::RedirVecUtils,
  },
  prelude::*,
  procio::{IoMode, IoStack, PipeGenerator, borrow_fd},
  shopt::xtrace_print,
  signal::{check_signals, signals_pending},
  state::{
    self, ShFunc, VarFlags, VarKind, read_logic, read_shopts, read_vars, write_jobs, write_logic, write_meta, write_vars
  },
};

use super::{
  AssignKind, CaseNode, CondNode, ConjunctNode, ConjunctOp, LoopKind, NdFlags, NdRule, Node,
  ParsedSrc, Redir, RedirType,
  lex::{KEYWORDS, Span, Tk, TkFlags},
};

thread_local! {
  static RECURSE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub fn is_in_path(name: &str) -> bool {
  if name.starts_with("./") || name.starts_with("../") || name.starts_with('/') {
    let path = Path::new(name);
    if path.exists() && path.is_file() && !path.is_dir() {
      let meta = match path.metadata() {
        Ok(m) => m,
        Err(_) => return false,
      };
      if meta.permissions().mode() & 0o111 != 0 {
        return true;
      }
    }
    false
  } else {
    let Ok(path) = env::var("PATH") else {
      return false;
    };
    let paths = path.split(':');
    for path in paths {
      let full_path = Path::new(path).join(name);
      if full_path.exists() && full_path.is_file() && !full_path.is_dir() {
        let meta = match full_path.metadata() {
          Ok(m) => m,
          Err(_) => continue,
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

/// Arguments to the execvpe function
pub struct ExecArgs {
  pub cmd: (CString, Span),
  pub argv: Vec<CString>,
  pub envp: Vec<CString>,
}

impl ExecArgs {
  pub fn new(argv: Vec<Tk>) -> ShResult<Self> {
    assert!(!argv.is_empty());
    let argv = prepare_argv(argv)?;
    Self::from_expanded(argv)
  }
  pub fn from_expanded(argv: Vec<(String, Span)>) -> ShResult<Self> {
    assert!(!argv.is_empty());
    let cmd = Self::get_cmd(&argv);
    let argv = Self::get_argv(argv);
    let envp = Self::get_envp();
    Ok(Self { cmd, argv, envp })
  }
  pub fn get_cmd(argv: &[(String, Span)]) -> (CString, Span) {
    let cmd = argv[0].0.as_str();
    let span = argv[0].1.clone();
    (CString::new(cmd).unwrap(), span)
  }
  pub fn get_argv(argv: Vec<(String, Span)>) -> Vec<CString> {
    argv
      .into_iter()
      .map(|s| CString::new(s.0).unwrap())
      .collect()
  }
  pub fn get_envp() -> Vec<CString> {
    std::env::vars()
      .map(|v| CString::new(format!("{}={}", v.0, v.1)).unwrap())
      .collect()
  }
}

/// Execute a `-c` command string, optimizing single simple commands to exec
/// directly without forking. This avoids process group issues where grandchild
/// processes (e.g. nvim spawning opencode) lose their controlling terminal.
pub fn exec_dash_c(input: String) -> ShResult<()> {
  let log_tab = read_logic(|l| l.clone());
  let expanded = expand_aliases(input, HashSet::new(), &log_tab);
  let source_name = "<shed -c>".to_string();
  let mut parser = ParsedSrc::new(Arc::new(expanded))
    .with_lex_flags(super::lex::LexFlags::empty())
    .with_name(source_name.clone());
  if let Err(errors) = parser.parse_src() {
    for error in errors {
      error.print_error();
    }
    return Ok(());
  }

  let mut nodes = parser.extract_nodes();

  // Single simple command: exec directly without forking.
  // The parser wraps single commands as Conjunction → Pipeline → Command.
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
          NdRule::Command { .. } => break,
          _ => break,
        }
      }
      node.flags |= NdFlags::NO_FORK;
      nodes.push(node);
    }
  }

  let mut dispatcher = Dispatcher::new(nodes, false, source_name);
  // exec_cmd expects a job on the stack (normally set up by exec_pipeline).
  // For the NO_FORK exec-in-place path, create one so it doesn't panic.
  dispatcher.job_stack.new_job();
  dispatcher.begin_dispatch()
}

pub fn exec_input(
  input: String,
  io_stack: Option<IoStack>,
  interactive: bool,
  source_name: Option<String>,
) -> ShResult<()> {
  let log_tab = read_logic(|l| l.clone());
  let input = expand_aliases(input, HashSet::new(), &log_tab);
  let lex_flags = if interactive {
    super::lex::LexFlags::INTERACTIVE
  } else {
    super::lex::LexFlags::empty()
  };
  let source_name = source_name.unwrap_or("<stdin>".into());
  let mut parser = ParsedSrc::new(Arc::new(input))
    .with_lex_flags(lex_flags)
    .with_name(source_name.clone());
  if let Err(errors) = parser.parse_src() {
    for error in errors {
      error.print_error();
    }
    return Ok(());
  }

  let nodes = parser.extract_nodes();

  let mut dispatcher = Dispatcher::new(nodes, interactive, source_name.clone());
  if let Some(mut stack) = io_stack {
    dispatcher.io_stack.extend(stack.drain(..));
  }
  dispatcher.begin_dispatch()
}

pub struct Dispatcher {
  nodes: VecDeque<Node>,
  interactive: bool,
  source_name: String,
  pub io_stack: IoStack,
  pub job_stack: JobStack,
  fg_job: bool,
}

impl Dispatcher {
  pub fn new(nodes: Vec<Node>, interactive: bool, source_name: String) -> Self {
    let nodes = VecDeque::from(nodes);
    Self {
      nodes,
      interactive,
      source_name,
      io_stack: IoStack::new(),
      job_stack: JobStack::new(),
      fg_job: true,
    }
  }
  pub fn begin_dispatch(&mut self) -> ShResult<()> {
    while let Some(node) = self.nodes.pop_front() {
      let blame = node.get_span();
      self.dispatch_node(node).try_blame(blame)?;
    }
    Ok(())
  }
  pub fn dispatch_node(&mut self, node: Node) -> ShResult<()> {
    while signals_pending() {
      // If we have received SIGINT,
      // this will stop the execution here
      // and propagate back to the functions in main.rs
      check_signals()?;
    }
    let flags = node.flags;

    let result = match node.class {
      NdRule::Conjunction { .. } => self.exec_conjunction(node),
      NdRule::Pipeline { .. } => self.exec_pipeline(node),
      NdRule::IfNode { .. } => self.exec_if(node),
      NdRule::LoopNode { .. } => self.exec_loop(node),
      NdRule::ForNode { .. } => self.exec_for(node),
      NdRule::CaseNode { .. } => self.exec_case(node),
      NdRule::BraceGrp { .. } => self.exec_brc_grp(node),
      NdRule::FuncDef { .. } => self.exec_func_def(node),
      NdRule::Negate { .. } => self.exec_negated(node),
      NdRule::Command { .. } => self.dispatch_cmd(node),
      NdRule::Test { .. } => self.exec_test(node),
      _ => unreachable!(),
    };

    if let Err(mut e) = result {
      if e.is_flow_control() {
        return Err(e);
      }
      if state::get_status() != 0 && !flags.contains(NdFlags::NOT_ERR) {
        if let Some(trap) = read_logic(|l| l.get_trap(TrapTarget::Error)) {
          let saved_status = state::get_status();
          exec_input(trap, None, false, Some("trap ERR".to_string()))?;
          state::set_status(saved_status);
        }
        if read_shopts(|o| o.set.errexit) {
          e.set_kind(ShErrKind::ErrInterrupt);
          e.persist_redirs();
          return Err(e);
        }
      }
      return Err(e);
    }

    Ok(())
  }
  pub fn dispatch_cmd(&mut self, node: Node) -> ShResult<()> {
    let (line, _) = node.get_span().clone().line_and_col();
    write_vars(|v| {
      v.set_var(
        "LINENO",
        VarKind::Str((line + 1).to_string()),
        VarFlags::NONE,
      )
    })?;

    let Some(cmd) = node.get_command() else {
      return self.exec_cmd(node); // Argv is empty, probably an assignment
    };
		// We need to expand this token
		// so that a command smuggled inside of a variable is routed correctly,
		// instead of only hitting the exec_cmd path
		let cmd_word = cmd.clone()
			.expand()?
			.get_words()
			.into_iter()
			.next()
			.unwrap();

    if is_func(&cmd_word) {
      self.exec_func(node)
    } else if cmd.flags.contains(TkFlags::BUILTIN)
		|| BUILTINS.contains(&cmd_word.as_str()) {
      self.exec_builtin(node)
    } else if is_subsh(node.get_command().cloned()) {
      self.exec_subsh(node)
    } else if read_shopts(|s| s.core.autocd)
      && Path::new(cmd.span.as_str()).is_dir()
      && !is_in_path(cmd.span.as_str())
    {
      let dir = cmd.span.as_str().to_string();
      let stack = IoStack {
        stack: self.io_stack.clone(),
      };
      exec_input(
        format!("cd {dir}"),
        Some(stack),
        self.interactive,
        Some(self.source_name.clone()),
      )
    } else {
      self.exec_cmd(node)
    }
  }
  pub fn exec_negated(&mut self, node: Node) -> ShResult<()> {
    let NdRule::Negate { cmd } = node.class else {
      unreachable!()
    };
    self.dispatch_node(*cmd)?;
    let status = state::get_status();
    state::set_status(if status == 0 { 1 } else { 0 });

    Ok(())
  }
  pub fn exec_conjunction(&mut self, conjunction: Node) -> ShResult<()> {
    let span = conjunction.get_span().clone();
    let NdRule::Conjunction { elements } = conjunction.class else {
      unreachable!()
    };

    if read_shopts(|o| o.set.verbose) {
      let stderr = borrow_fd(STDERR_FILENO);
      let command = span.as_str().to_string();
      write(stderr, command.as_bytes()).ok();
      write(stderr, b"\n").ok();
    }

    let mut elem_iter = elements.into_iter();
    let mut skip = false;
    while let Some(element) = elem_iter.next() {
      let ConjunctNode { cmd, operator } = element;
      if !skip {
        self.dispatch_node(*cmd)?;
      }

      let status = state::get_status();
      skip = match operator {
        ConjunctOp::And => status != 0,
        ConjunctOp::Or => status == 0,
        ConjunctOp::Null => break,
      };
    }
    Ok(())
  }
  pub fn exec_test(&mut self, node: Node) -> ShResult<()> {
    let test_result = double_bracket_test(node)?;
    match test_result {
      true => state::set_status(0),
      false => state::set_status(1),
    }
    Ok(())
  }
  pub fn exec_func_def(&mut self, func_def: Node) -> ShResult<()> {
    let blame = func_def.get_span();
    let ctx = func_def.context.clone();
    let NdRule::FuncDef { name, body } = func_def.class else {
      unreachable!()
    };
    let body_span = body.get_span();
    let body = body_span.as_str().to_string();
    let name = name
      .span
      .as_str()
      .strip_suffix("()")
      .unwrap_or(name.span.as_str());

    if KEYWORDS.contains(&name) {
      return Err(ShErr::at(
        ShErrKind::SyntaxErr,
        blame,
        format!("function: Forbidden function name `{name}`"),
      ));
    }

    let mut func_parser = ParsedSrc::new(Arc::new(body)).with_context(ctx);
    if let Err(errors) = func_parser.parse_src() {
      for error in errors {
        error.print_error();
      }
      return Ok(());
    }

    let func = ShFunc::new(func_parser, blame);
    write_logic(|l| l.insert_func(name, func)); // Store the AST
    write_meta(|m| m.rehash_commands());
    Ok(())
  }
  fn exec_subsh(&mut self, subsh: Node) -> ShResult<()> {
    let _blame = subsh.get_span().clone();
    let NdRule::Command { assignments, argv } = subsh.class else {
      unreachable!()
    };
    let name = self.source_name.clone();

    self.io_stack.append_to_frame(subsh.redirs);
    let _guard = self.io_stack.pop_frame().redirect()?;

    self.run_fork("anonymous_subshell", |s| {
      if let Err(e) = s.set_assignments(assignments, AssignBehavior::Export) {
        e.print_error();
        return;
      };

      let subsh_raw = argv[0].span.as_str();
      let subsh_body = subsh_raw[1..subsh_raw.len() - 1].to_string(); // Remove surrounding parentheses

      if let Err(e) = exec_input(subsh_body, None, s.interactive, Some(name)) {
        e.print_error();
      };
    })
  }
  fn exec_func(&mut self, func: Node) -> ShResult<()> {
    let mut blame = func.get_span().clone();
    let func_name = func.get_command()
			.unwrap()
			.clone()
			.expand()?
			.get_first_word()
			.unwrap_or_default();

    let func_ctx = func.get_context(format!(
      "in call to function '{}'",
      func_name.fg(next_color())
    ));
    let NdRule::Command {
      assignments,
      mut argv,
    } = func.class
    else {
      unreachable!()
    };

    let max_depth = read_shopts(|s| s.core.max_recurse_depth);
    let depth = RECURSE_DEPTH.with(|d| {
      let cur = d.get();
      d.set(cur + 1);
      cur + 1
    });
    if depth > max_depth {
      RECURSE_DEPTH.with(|d| d.set(d.get() - 1));
      return Err(ShErr::at(
        ShErrKind::InternalErr,
        blame,
        format!("maximum recursion depth ({max_depth}) exceeded"),
      ));
    }

    let env_vars = self.set_assignments(assignments, AssignBehavior::Export)?;
    let func_name = argv.remove(0);
    let _var_guard = var_ctx_guard(env_vars.into_iter().collect());

    self.io_stack.append_to_frame(func.redirs);

    let name = func_name.clone()
			.expand()?
			.get_first_word()
			.unwrap_or_default();
    blame.rename(name.clone());

    argv.insert(0, func_name.clone());
    let argv = prepare_argv(argv).try_blame(blame.clone())?;
    let result = if let Some(ref mut func_body) = read_logic(|l| l.get_func(&name)) {
      let _guard = scope_guard(Some(argv));
      func_body.body_mut().propagate_context(func_ctx);
      func_body.body_mut().flags = func.flags;

      if let Err(e) = self.exec_pipeline(func_body.body().clone()) {
        match e.kind() {
          ShErrKind::FuncReturn(code) => {
            state::set_status(*code);
            Ok(())
          }
          ShErrKind::ErrInterrupt => {
            // set -e caught an error
            Err(e.with_context(func_body.body().context.clone()))
          }
          _ => Err(e),
        }
      } else {
        Ok(())
      }
    } else {
      Err(ShErr::at(
        ShErrKind::InternalErr,
        blame,
        format!("Failed to find function '{}'", func_name),
      ))
    };

    RECURSE_DEPTH.with(|d| d.set(d.get() - 1));
    result
  }
  fn exec_brc_grp(&mut self, brc_grp: Node) -> ShResult<()> {
    let NdRule::BraceGrp { body } = brc_grp.class else {
      unreachable!("expected BraceGrp node, got {:?}", brc_grp.class)
    };
    let fork_builtins = brc_grp.flags.contains(NdFlags::FORK_BUILTINS);

    self.io_stack.append_to_frame(brc_grp.redirs);
    let guard = self.io_stack.pop_frame().redirect()?;
    let brc_grp_logic = |s: &mut Self| -> ShResult<()> {
      for node in body {
        let blame = node.get_span();
        s.dispatch_node(node).try_blame(blame)?;
      }

      Ok(())
    };

    if fork_builtins {
      log::trace!("Forking brace group");
      self.run_fork("brace group", |s| {
        if let Err(e) = brc_grp_logic(s) {
          e.print_error();
        }
      })
    } else {
      brc_grp_logic(self).map_err(|e| e.with_redirs(guard))
    }
  }
  fn exec_case(&mut self, case_stmt: Node) -> ShResult<()> {
    let blame = case_stmt.get_span().clone();
    let NdRule::CaseNode {
      pattern,
      case_blocks,
    } = case_stmt.class
    else {
      unreachable!()
    };

    let fork_builtins = case_stmt.flags.contains(NdFlags::FORK_BUILTINS);

    self.io_stack.append_to_frame(case_stmt.redirs);
    let guard = self.io_stack.pop_frame().redirect()?;

    let case_logic = |s: &mut Self| -> ShResult<()> {
      let exp_pattern = pattern.clone().expand()?;
      let pattern_raw = exp_pattern
        .get_words()
        .first()
        .map(|s| s.to_string())
        .unwrap_or_default();

      'outer: for block in case_blocks {
        let CaseNode { pattern, body } = block;
        let block_pattern_raw = pattern
          .span
          .as_str()
          .strip_suffix(')')
          .unwrap_or(pattern.span.as_str())
          .trim();
        // Split at '|' to allow for multiple patterns like `foo|bar)`
        let block_patterns = block_pattern_raw.split('|');

        for pattern in block_patterns {
          let pattern_exp = expand_case_pattern(pattern)?;
          let pattern_regex = glob_to_regex(&pattern_exp, false);
          if pattern_regex.is_match(&pattern_raw) {
            for node in &body {
              s.dispatch_node(node.clone())?;
            }
            break 'outer;
          }
        }
      }

      Ok(())
    };

    if fork_builtins {
      log::trace!("Forking builtin: case");
      self.run_fork("case", |s| {
        if let Err(e) = case_logic(s) {
          e.print_error();
        }
      })
    } else {
      case_logic(self)
        .try_blame(blame)
        .map_err(|e| e.with_redirs(guard))
    }
  }
  fn exec_loop(&mut self, loop_stmt: Node) -> ShResult<()> {
    let blame = loop_stmt.get_span().clone();
    let NdRule::LoopNode { kind, cond_node } = loop_stmt.class else {
      unreachable!();
    };
    let keep_going = |kind: LoopKind, status: i32| -> bool {
      match kind {
        LoopKind::While => status == 0,
        LoopKind::Until => status != 0,
      }
    };

    let fork_builtins = loop_stmt.flags.contains(NdFlags::FORK_BUILTINS);

    self.io_stack.append_to_frame(loop_stmt.redirs);
    let guard = self.io_stack.pop_frame().redirect()?;

    let loop_logic = |s: &mut Self| -> ShResult<()> {
      let CondNode { cond, body } = cond_node;
      'outer: loop {
        if let Err(e) = s.dispatch_node(*cond.clone()) {
          state::set_status(1);
          return Err(e);
        }

        let status = state::get_status();
        if keep_going(kind, status) {
          for node in &body {
            if let Err(e) = s.dispatch_node(node.clone()) {
              match e.kind() {
                ShErrKind::LoopBreak(code) => {
                  state::set_status(*code);
                  break 'outer;
                }
                ShErrKind::LoopContinue(code) => {
                  state::set_status(*code);
                  continue 'outer;
                }
                _ => {
                  return Err(e);
                }
              }
            }
          }
        } else {
          state::set_status(0);
          break;
        }
      }

      Ok(())
    };

    if fork_builtins {
      log::trace!("Forking builtin: loop");
      self.run_fork("loop", |s| {
        if let Err(e) = loop_logic(s) {
          e.print_error();
        }
      })
    } else {
      loop_logic(self)
        .try_blame(blame)
        .map_err(|e| e.with_redirs(guard))
    }
  }
  fn exec_for(&mut self, for_stmt: Node) -> ShResult<()> {
    let blame = for_stmt.get_span().clone();
    let NdRule::ForNode { vars, arr, body } = for_stmt.class else {
      unreachable!();
    };

    let fork_builtins = for_stmt.flags.contains(NdFlags::FORK_BUILTINS);

    let to_expanded_strings = |tks: Vec<Tk>| -> ShResult<Vec<String>> {
      Ok(
        tks
          .into_iter()
          .map(|tk| tk.expand().map(|tk| tk.get_words()))
          .collect::<ShResult<Vec<Vec<String>>>>()?
          .into_iter()
          .flatten()
          .collect::<Vec<_>>(),
      )
    };

    self.io_stack.append_to_frame(for_stmt.redirs);
    let guard = self.io_stack.pop_frame().redirect()?;

    let for_logic = |s: &mut Self| -> ShResult<()> {
      // Expand all array variables
      let arr: Vec<String> = to_expanded_strings(arr)?;
      let vars: Vec<String> = to_expanded_strings(vars)?;

      let mut for_guard = var_ctx_guard(vars.iter().map(|v| v.to_string()).collect());

      'outer: for chunk in arr.chunks(vars.len()) {
        let empty = String::new();
        let chunk_iter = vars
          .iter()
          .zip(chunk.iter().chain(std::iter::repeat(&empty)));

        for (var, val) in chunk_iter {
          write_vars(|v| {
            v.set_var(
              &var.to_string(),
              VarKind::Str(val.to_string()),
              VarFlags::NONE,
            )
          })?;
          for_guard.insert(var.to_string());
        }

        for node in body.clone() {
          if let Err(e) = s.dispatch_node(node) {
            match e.kind() {
              ShErrKind::LoopBreak(code) => {
                state::set_status(*code);
                break 'outer;
              }
              ShErrKind::LoopContinue(code) => {
                state::set_status(*code);
                continue 'outer;
              }
              _ => return Err(e),
            }
          }
        }
      }

      Ok(())
    };

    if fork_builtins {
      log::trace!("Forking builtin: for");
      self.run_fork("for", |s| {
        if let Err(e) = for_logic(s) {
          e.print_error();
        }
      })
    } else {
      for_logic(self)
        .try_blame(blame)
        .map_err(|e| e.with_redirs(guard))
    }
  }
  fn exec_if(&mut self, if_stmt: Node) -> ShResult<()> {
    let blame = if_stmt.get_span().clone();
    let NdRule::IfNode {
      cond_nodes,
      else_block,
    } = if_stmt.class
    else {
      unreachable!();
    };
    let fork_builtins = if_stmt.flags.contains(NdFlags::FORK_BUILTINS);

    self.io_stack.append_to_frame(if_stmt.redirs);
    let guard = self.io_stack.pop_frame().redirect()?;

    let if_logic = |s: &mut Self| -> ShResult<()> {
      let mut matched = false;
      for node in cond_nodes {
        let CondNode { cond, body } = node;

        if let Err(e) = s.dispatch_node(*cond) {
          state::set_status(1);
          return Err(e);
        }

        match state::get_status() {
          0 => {
            matched = true;
            for body_node in body {
              s.dispatch_node(body_node)?;
            }
            break; // Don't check remaining elif conditions
          }
          _ => continue,
        }
      }

      if !matched {
        if !else_block.is_empty() {
          for node in else_block {
            s.dispatch_node(node)?;
          }
        } else {
          state::set_status(0);
        }
      }

      Ok(())
    };

    if fork_builtins {
      log::trace!("Forking builtin: if");
      self.run_fork("if", |s| {
        if let Err(e) = if_logic(s) {
          e.print_error();
          state::set_status(1);
        }
      })
    } else {
      if_logic(self)
        .try_blame(blame)
        .map_err(|e| e.with_redirs(guard))
    }
  }
  fn exec_pipeline(&mut self, pipeline: Node) -> ShResult<()> {
    let pipeline_span = pipeline.get_span().clone();
    let pipeline_flags = pipeline.flags;
    let NdRule::Pipeline { cmds } = pipeline.class else {
      unreachable!()
    };
    let is_bg = pipeline.flags.contains(NdFlags::BACKGROUND);
    self.job_stack.new_job();
    let pipeline_result = if cmds.len() == 1 {
      self.fg_job = !is_bg && self.interactive;
      let cmd = cmds.into_iter().next().unwrap();
      let result = if is_bg && !matches!(cmd.class, NdRule::Command { .. }) {
        self.run_fork(
          &cmd.get_command().map(|t| t.to_string()).unwrap_or_default(),
          |s| {
            if let Err(e) = s.dispatch_node(cmd) {
              e.print_error();
            }
          },
        )
      } else {
        self.dispatch_node(cmd)
      };

      // Give the pipeline terminal control as soon as the first child
      // establishes the PGID, so later children (e.g. nvim) don't get
      // SIGTTOU when they try to modify terminal attributes.
      // Only for interactive (top-level) pipelines — command substitution
      // and other non-interactive contexts must not steal the terminal.
      if !is_bg
        && self.interactive
        && let Some(pgid) = self.job_stack.curr_job_mut().unwrap().pgid()
      {
        attach_tty(pgid).ok();
      }
      result
    } else {
      let (mut in_redirs, mut out_redirs) = self.io_stack.pop_frame().redirs.split_by_channel();

      let mut pipes = PipeGenerator::new(cmds.len()).as_io_frames();

      self.fg_job = !is_bg && self.interactive;
      let mut tty_attached = false;

      let last_cmd = cmds.len() - 1;
      let mut result = Ok(());
      for (i, mut cmd) in cmds.into_iter().enumerate() {
        let mut frame = pipes.next().ok_or_else(|| {
          ShErr::at(
            ShErrKind::InternalErr,
            cmd.get_span(),
            "failed to set up pipeline redirections".to_string(),
          )
        })?;
        if i == 0 {
          for redir in std::mem::take(&mut in_redirs) {
            frame.push(redir);
          }
        } else if i == last_cmd {
          for redir in std::mem::take(&mut out_redirs) {
            frame.push(redir);
          }
        }

        let _guard = frame.redirect()?;

        cmd.flags |= NdFlags::FORK_BUILTINS; // multiple cmds means builtins must fork
        result = self.dispatch_node(cmd);

        // Give the pipeline terminal control as soon as the first child
        // establishes the PGID, so later children (e.g. nvim) don't get
        // SIGTTOU when they try to modify terminal attributes.
        // Only for interactive (top-level) pipelines — command substitution
        // and other non-interactive contexts must not steal the terminal.
        if !tty_attached
          && !is_bg
          && self.interactive
          && let Some(pgid) = self.job_stack.curr_job_mut().unwrap().pgid()
        {
          attach_tty(pgid).ok();
          tty_attached = true;
        }

        if result.is_err() {
          break;
        }
      }
      result
    };
    let job = self.job_stack.finalize_job().unwrap();
    // Always dispatch the job (which reclaims the terminal via take_term)
    // even if an error occurred, to prevent terminal ownership from being
    // left with a dead process group.
    let dispatch_result = dispatch_job(job, is_bg, self.interactive);
    pipeline_result?;
    dispatch_result?;

    // Errexit check after the job has been waited on, so the status
    // reflects the actual exit code of the (possibly forked) command.
    if state::get_status() != 0
      && !pipeline_flags.contains(NdFlags::NOT_ERR)
      && read_shopts(|o| o.set.errexit)
    {
      if let Some(trap) = read_logic(|l| l.get_trap(TrapTarget::Error)) {
        let saved_status = state::get_status();
        exec_input(trap, None, false, Some("trap ERR".to_string()))?;
        state::set_status(saved_status);
      }
      return Err(ShErr::at(
        ShErrKind::ErrInterrupt,
        pipeline_span,
        "Command returned non-zero exit status",
      ));
    }
    Ok(())
  }

  fn exec_builtin(&mut self, cmd: Node) -> ShResult<()> {
    let fork_builtins = cmd.flags.contains(NdFlags::FORK_BUILTINS);
    let cmd_raw = cmd
      .get_command()
      .unwrap_or_else(|| panic!("expected command NdRule, got {:?}", &cmd.class))
      .to_string();

    if fork_builtins {
      log::trace!("Forking builtin: {}", cmd_raw);
      let guard = self.io_stack.pop_frame().redirect()?;
      if cmd_raw.as_str() == "exec" {
        guard.persist();
      }
      self.run_fork(&cmd_raw, |s| {
        if let Err(e) = s.dispatch_builtin(cmd) {
          e.print_error();
        }
      })
    } else {
      let result = self.dispatch_builtin(cmd);

      if let Err(e) = result {
        let code = state::get_status();
        if code == 0 {
          state::set_status(1);
        }
        return Err(e);
      }
      Ok(())
    }
  }
  fn dispatch_builtin(&mut self, mut cmd: Node) -> ShResult<()> {
    let cmd_raw = cmd.get_command().unwrap().to_string();
    let context = cmd.context.clone();
    let NdRule::Command { assignments, argv } = &mut cmd.class else {
      unreachable!()
    };
    let env_vars = self.set_assignments(mem::take(assignments), AssignBehavior::Export)?;
    let _var_guard = var_ctx_guard(env_vars.into_iter().collect());

    // Handle builtin/command recursion before redirect/job setup
    if cmd_raw.as_str() == "builtin" {
      *argv = argv
        .iter_mut()
        .skip(1)
        .map(|tk| tk.clone())
        .collect::<Vec<Tk>>();
      return self.exec_builtin(cmd);
    } else if cmd_raw.as_str() == "command" {
      *argv = argv
        .iter_mut()
        .skip(1)
        .map(|tk| tk.clone())
        .collect::<Vec<Tk>>();
      if cmd.flags.contains(NdFlags::FORK_BUILTINS) {
        cmd.flags |= NdFlags::NO_FORK;
      }
      return self.exec_cmd(cmd);
    }

    // Set up redirections here so we can attach the guard to propagated errors.
    self.io_stack.append_to_frame(mem::take(&mut cmd.redirs));
    let frame = self.io_stack.pop_frame();
    let redir_guard = frame.redirect()?;

    // Register ChildProc in current job
    let job = self.job_stack.curr_job_mut().unwrap();
    let child_pgid = if let Some(pgid) = job.pgid() {
      pgid
    } else {
      job.set_pgid(Pid::this());
      Pid::this()
    };
    let child = ChildProc::new(Pid::this(), Some(&cmd_raw), Some(child_pgid))?;
    job.push_child(child);

    // Handle exec specially — persist redirections before dispatch
    if cmd_raw.as_str() == "exec" {
      redir_guard.persist();
      let result = exec::exec_builtin(cmd);
      return if let Err(e) = result {
        Err(e.with_context(context))
      } else {
        Ok(())
      };
    }

    let result = match cmd_raw.as_str() {
      "echo" => echo(cmd),
      "cd" => cd(cmd),
      "export" => export(cmd),
      "local" => local(cmd),
      "pwd" => pwd(cmd),
      "source" | "." => source(cmd),
      "shift" => shift(cmd),
      "fg" => continue_job(cmd, JobBehavior::Foregound),
      "bg" => continue_job(cmd, JobBehavior::Background),
      "disown" => disown(cmd),
      "jobs" => jobs(cmd),
      "alias" => alias(cmd),
      "unalias" => unalias(cmd),
      "return" => flowctl(cmd, ShErrKind::FuncReturn(0)),
      "break" => flowctl(cmd, ShErrKind::LoopBreak(0)),
      "continue" => flowctl(cmd, ShErrKind::LoopContinue(0)),
      "exit" => flowctl(cmd, ShErrKind::CleanExit(0)),
      "shopt" => shopt(cmd),
      "read" => read_builtin(cmd),
      "trap" => trap(cmd),
      "pushd" => pushd(cmd),
      "popd" => popd(cmd),
      "dirs" => dirs(cmd),
      "eval" => eval::eval(cmd),
      "readonly" => readonly(cmd),
      "unset" => unset(cmd),
      "complete" => complete_builtin(cmd),
      "compgen" => compgen_builtin(cmd),
      "map" => map::map(cmd),
      "pop" => arr_pop(cmd),
      "fpop" => arr_fpop(cmd),
      "push" => arr_push(cmd),
      "fpush" => arr_fpush(cmd),
      "rotate" => arr_rotate(cmd),
      "wait" => jobctl::wait(cmd),
      "type" => intro::type_builtin(cmd),
      "getopts" => getopts(cmd),
      "keymap" => keymap::keymap(cmd),
      "read_key" => read::read_key(cmd),
      "autocmd" => autocmd(cmd),
      "ulimit" => ulimit(cmd),
      "umask" => umask_builtin(cmd),
      "seek" => seek(cmd),
      "help" => help(cmd),
      "set" => set_builtin(cmd),
      "msg" => msg(cmd),
			"fc" => fixcmd(cmd, self.interactive),
			"hist" => hist_builtin(cmd),
      "true" | ":" => {
        state::set_status(0);
        Ok(())
      }
      "false" => {
        state::set_status(1);
        Ok(())
      }
      _ => unimplemented!("Have not yet added support for builtin '{}'", cmd_raw),
    };

    if let Err(e) = result {
      if !e.is_flow_control() {
        state::set_status(1);
      }
      Err(e.with_context(context).with_redirs(redir_guard))
    } else {
      Ok(())
    }
  }
  fn exec_cmd(&mut self, cmd: Node) -> ShResult<()> {
    let blame = cmd.get_span().clone();
    let context = cmd.context.clone();
    let NdRule::Command { assignments, argv } = cmd.class else {
      unreachable!()
    };
    let mut env_vars_to_unset = vec![];
    if !assignments.is_empty() {
      let assign_behavior = if argv.is_empty() {
        AssignBehavior::Set
      } else {
        AssignBehavior::Export
      };
      env_vars_to_unset = self.set_assignments(assignments, assign_behavior)?;

      if let AssignBehavior::Set = assign_behavior {
        state::set_status(0);
      }
    }

    let no_fork = cmd.flags.contains(NdFlags::NO_FORK);
    if argv.is_empty() {
      return Ok(());
    }

    self.io_stack.append_to_frame(cmd.redirs);

    let exec_args = ExecArgs::new(argv).blame(blame)?;
    let _guard = self.io_stack.pop_frame().redirect()?;
    let job = self.job_stack.curr_job_mut().unwrap();
    let existing_pgid = job.pgid();

    let fg_job = self.fg_job;
    let interactive = self.interactive;
    let child_logic = |pgid: Option<Pid>| -> ! {
      // For non-interactive exec-in-place (e.g. shed -c), skip process group
      // and terminal setup — just transparently replace the current process.
      if interactive || !no_fork {
        // Put ourselves in the correct process group before exec.
        // For the first child in a pipeline pgid is None, so we
        // become our own group leader (setpgid(0,0)).  For later
        // children we join the leader's group.
        let our_pgid = pgid.unwrap_or(Pid::from_raw(0));
        let _ = setpgid(Pid::from_raw(0), our_pgid);

        if fg_job {
          let tty_pgid = if our_pgid == Pid::from_raw(0) {
            nix::unistd::getpid()
          } else {
            our_pgid
          };
          let _ = tcsetpgrp(
            unsafe { BorrowedFd::borrow_raw(*crate::libsh::sys::TTY_FILENO) },
            tty_pgid,
          );
        }
      }

      if interactive || !no_fork {
        crate::signal::reset_signals(fg_job);
      }

      let cmd = &exec_args.cmd.0;
      let span = exec_args.cmd.1;

      let Err(e) = execvpe(cmd, &exec_args.argv, &exec_args.envp);

      // execvpe only returns on error
      let cmd_str = cmd.to_str().unwrap().to_string();
      match e {
        Errno::ENOENT => {
          ShErr::new(ShErrKind::NotFound, span.clone())
            .labeled(span, format!("{cmd_str}: command not found"))
            .with_context(context)
            .print_error();
        }
        _ => {
          ShErr::at(ShErrKind::Errno(e), span, format!("{e}"))
            .with_context(context)
            .print_error();
        }
      }
      exit(e as i32)
    };

    if no_fork {
      child_logic(existing_pgid);
    }

    match unsafe { fork()? } {
      ForkResult::Child => child_logic(existing_pgid),
      ForkResult::Parent { child } => {
        // Close proc sub pipe fds - the child has inherited them
        // and will access them via /proc/self/fd/N. Keeping them
        // open here would prevent EOF on the pipe.
        write_jobs(|j| j.drain_registered_fds());

        let cmd_name = exec_args.cmd.0.to_str().unwrap();

        let child_pgid = if let Some(pgid) = existing_pgid {
          pgid
        } else {
          job.set_pgid(child);
          child
        };
        let child_proc = ChildProc::new(child, Some(cmd_name), Some(child_pgid))?;
        job.push_child(child_proc);
      }
    }

    for var in env_vars_to_unset {
      unsafe { std::env::set_var(&var, "") };
    }

    Ok(())
  }
  fn run_fork(&mut self, name: &str, f: impl FnOnce(&mut Self)) -> ShResult<()> {
    let existing_pgid = self.job_stack.curr_job_mut().unwrap().pgid();
    match unsafe { fork()? } {
      ForkResult::Child => {
        let _ = setpgid(Pid::from_raw(0), existing_pgid.unwrap_or(Pid::from_raw(0)));
        self.interactive = false;
        f(self);
        exit(state::get_status())
      }
      ForkResult::Parent { child } => {
        write_jobs(|j| j.drain_registered_fds());
        let job = self.job_stack.curr_job_mut().unwrap();
        let child_pgid = if let Some(pgid) = existing_pgid {
          pgid
        } else {
          job.set_pgid(child);
          child
        };
        let child_proc = ChildProc::new(child, Some(name), Some(child_pgid))?;
        job.push_child(child_proc);
        Ok(())
      }
    }
  }
  fn set_assignments(&self, assigns: Vec<Node>, behavior: AssignBehavior) -> ShResult<Vec<String>> {
    let mut new_env_vars = vec![];
    let flags = match behavior {
      AssignBehavior::Export => VarFlags::EXPORT,
      AssignBehavior::Set => VarFlags::NONE,
    };

    for assign in assigns {
      let is_arr = assign.flags.contains(NdFlags::ARR_ASSIGN);
      let NdRule::Assignment { kind, var, val } = assign.class else {
        unreachable!()
      };
      let var = var.span.as_str();
      let val = if is_arr {
        VarKind::arr_from_tk(val)?
      } else {
        VarKind::Str(val.expand()?.get_words().join(" "))
      };

      // Parse and expand array index BEFORE entering write_vars borrow
      let indexed = state::parse_arr_bracket(var)
        .map(|(name, idx_raw)| state::expand_arr_index(&idx_raw).map(|idx| (name, idx)))
        .transpose()?;

      match kind {
        AssignKind::Eq => {
          if let Some((name, idx)) = indexed
            && let Err(e) = write_vars(|v| v.set_var_indexed(&name, idx, val.to_string(), flags))
          {
            state::set_status(1);
            return Err(e);
          } else if let Err(e) = write_vars(|v| v.set_var(var, val, flags)) {
            state::set_status(1);
            return Err(e);
          }
        }
        AssignKind::PlusEq => {
					let _var = read_vars(|v| v.get_var(var));
				}
        AssignKind::MinusEq => todo!(),
        AssignKind::MultEq => todo!(),
        AssignKind::DivEq => todo!(),
      }

      if matches!(behavior, AssignBehavior::Export) {
        new_env_vars.push(var.to_string());
      }
    }

    Ok(new_env_vars)
  }
}

pub fn prepare_argv(argv: Vec<Tk>) -> ShResult<Vec<(String, Span)>> {
  let mut args = vec![];

  for arg in argv {
    let span = arg.span.clone();
    let expanded = arg.expand()?;
    for exp in expanded.get_words() {
      args.push((exp, span.clone()))
    }
  }

  xtrace_print(&args);
  Ok(args)
}

/// Initialize the pipes for a pipeline
/// The first command gets `(None, WPipe)`
/// The last command gets `(RPipe, None)`
/// Commands inbetween get `(RPipe, WPipe)`
/// If there is only one command, it gets `(None, None)`
pub fn get_pipe_stack(num_cmds: usize) -> Vec<(Option<Redir>, Option<Redir>)> {
  let mut stack = Vec::with_capacity(num_cmds);
  let mut prev_read: Option<Redir> = None;

  for i in 0..num_cmds {
    if i == num_cmds - 1 {
      stack.push((prev_read.take(), None));
    } else {
      let (rpipe, wpipe) = IoMode::get_pipes();
      let r_redir = Redir::new(rpipe, RedirType::Input);
      let w_redir = Redir::new(wpipe, RedirType::Output);

      // Push (prev_read, Some(w_redir)) and set prev_read to r_redir
      stack.push((prev_read.take(), Some(w_redir)));
      prev_read = Some(r_redir);
    }
  }
  stack
}

pub fn is_func(name: &str) -> bool {
  read_logic(|l| l.get_func(name)).is_some()
}

pub fn is_subsh(tk: Option<Tk>) -> bool {
  tk.is_some_and(|tk| tk.flags.contains(TkFlags::IS_SUBSH))
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::testutil::{TestGuard, test_input};

  // ===================== while/until status =====================

  #[test]
  fn while_loop_status_zero_after_completion() {
    let _g = TestGuard::new();
    test_input("while false; do :; done").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn while_loop_status_zero_after_iterations() {
    let _g = TestGuard::new();
    test_input("X=0; while [[ $X -lt 3 ]]; do X=$((X+1)); done").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn until_loop_status_zero_after_completion() {
    let _g = TestGuard::new();
    test_input("until true; do :; done").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn until_loop_status_zero_after_iterations() {
    let _g = TestGuard::new();
    test_input("X=3; until [[ $X -le 0 ]]; do X=$((X-1)); done").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn while_break_preserves_status() {
    let _g = TestGuard::new();
    test_input("while true; do break; done").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn while_body_status_propagates() {
    let _g = TestGuard::new();
    test_input("X=0; while [[ $X -lt 1 ]]; do X=$((X+1)); false; done").unwrap();
    // Loop body ended with `false` (status 1), but the loop itself
    // completed normally when the condition failed, so status should be 0
    assert_eq!(state::get_status(), 0);
  }

  // ===================== if/elif/else status =====================

  #[test]
  fn if_true_body_status() {
    let _g = TestGuard::new();
    test_input("if true; then echo ok; fi").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn if_false_no_else_status() {
    let _g = TestGuard::new();
    test_input("if false; then echo ok; fi").unwrap();
    // No branch taken, POSIX says status is 0
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn if_else_branch_status() {
    let _g = TestGuard::new();
    test_input("if false; then true; else false; fi").unwrap();
    assert_eq!(state::get_status(), 1);
  }

  // ===================== for loop status =====================

  #[test]
  fn for_loop_empty_list_status() {
    let _g = TestGuard::new();
    test_input("for x in; do echo $x; done").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn for_loop_body_status() {
    let _g = TestGuard::new();
    test_input("for x in a b c; do true; done").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  // ===================== case status =====================

  #[test]
  fn case_match_status() {
    let _g = TestGuard::new();
    test_input("case foo in foo) true;; esac").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn case_no_match_status() {
    let _g = TestGuard::new();
    test_input("case foo in bar) true;; esac").unwrap();
    assert_eq!(state::get_status(), 0);
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

  // ===================== negation (!) status =====================

  #[test]
  fn negate_true() {
    let _g = TestGuard::new();
    test_input("! true").unwrap();
    assert_eq!(state::get_status(), 1);
  }

  #[test]
  fn negate_false() {
    let _g = TestGuard::new();
    test_input("! false").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn double_negate_true() {
    let _g = TestGuard::new();
    test_input("! ! true").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn double_negate_false() {
    let _g = TestGuard::new();
    test_input("! ! false").unwrap();
    assert_eq!(state::get_status(), 1);
  }

  #[test]
  fn negate_pipeline_last_cmd() {
    let _g = TestGuard::new();
    // pipeline status = last cmd (false) = 1, negated → 0
    test_input("! true | false").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn negate_pipeline_last_cmd_true() {
    let _g = TestGuard::new();
    // pipeline status = last cmd (true) = 0, negated → 1
    test_input("! false | true").unwrap();
    assert_eq!(state::get_status(), 1);
  }

  #[test]
  fn negate_in_conjunction() {
    let _g = TestGuard::new();
    // ! binds to pipeline, not conjunction: (! (true && false)) && true
    test_input("! (true && false) && true").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn negate_in_if_condition() {
    let g = TestGuard::new();
    test_input("if ! false; then echo yes; fi").unwrap();
    assert_eq!(state::get_status(), 0);
    assert_eq!(g.read_output(), "yes\n");
  }

  #[test]
  fn empty_var_in_test() {
    let _g = TestGuard::new();
    // POSIX specifies that a quoted unset variable expands to an empty string, so the shell actually sees `[ -n "" ]`, which returns false
    test_input("[ -n \"$EMPTYVAR_PROBABLY_NOT_SET_TO_ANYTHING\" ]").unwrap();
    assert_eq!(state::get_status(), 1);
    // Without quotes, word splitting causes an empty var to be removed entirely, so the shell actually sees `[ -n ]`, testing the value of ']', which returns true
    test_input("[ -n $EMPTYVAR_PROBABLY_NOT_SET_TO_ANYTHING ]").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
