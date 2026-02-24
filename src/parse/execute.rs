use std::{collections::{HashSet, VecDeque}, os::unix::fs::PermissionsExt};

use crate::{
  builtin::{
    alias::{alias, unalias}, cd::cd, dirstack::{dirs, popd, pushd}, echo::echo, eval, exec, export::{export, local}, flowctl::flowctl, jobctl::{JobBehavior, continue_job, disown, jobs}, pwd::pwd, read::read_builtin, shift::shift, shopt::shopt, source::source, test::double_bracket_test, trap::{TrapTarget, trap}, zoltraak::zoltraak
  },
  expand::{expand_aliases, glob_to_regex},
  jobs::{ChildProc, JobStack, dispatch_job},
  libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt},
  prelude::*,
  procio::{IoMode, IoStack},
  state::{self, ShFunc, VarFlags, read_logic, read_shopts, write_jobs, write_logic, write_vars},
};

use super::{
  AssignKind, CaseNode, CondNode, ConjunctNode, ConjunctOp, LoopKind, NdFlags, NdRule, Node,
  ParsedSrc, Redir, RedirType,
  lex::{KEYWORDS, Span, Tk, TkFlags},
};

thread_local! {
  static RECURSE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
		let Ok(path) = env::var("PATH") else { return false };
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

pub struct ScopeGuard;

impl ScopeGuard {
  pub fn exclusive_scope(args: Option<Vec<(String, Span)>>) -> Self {
    let argv = args.map(|a| a.into_iter().map(|(s, _)| s).collect::<Vec<_>>());
    write_vars(|v| v.descend(argv));
    Self
  }
  pub fn shared_scope() -> Self {
    // used in environments that inherit from the parent, like subshells
    write_vars(|v| v.descend(None));
    Self
  }
}

impl Drop for ScopeGuard {
  fn drop(&mut self) {
    write_vars(|v| v.ascend());
  }
}

/// Used to throw away variables that exist in temporary contexts
/// such as 'VAR=value <command> <args>'
/// or for-loop variables
struct VarCtxGuard {
  vars: HashSet<String>,
}
impl VarCtxGuard {
  fn new(vars: HashSet<String>) -> Self {
    Self { vars }
  }
}
impl Drop for VarCtxGuard {
  fn drop(&mut self) {
    write_vars(|v| {
      for var in &self.vars {
        v.unset_var(var);
      }
    });
  }
}

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

pub fn exec_input(input: String, io_stack: Option<IoStack>, interactive: bool) -> ShResult<()> {
  let log_tab = read_logic(|l| l.clone());
  let input = expand_aliases(input, HashSet::new(), &log_tab);
  let lex_flags = if interactive {
    super::lex::LexFlags::INTERACTIVE
  } else {
    super::lex::LexFlags::empty()
  };
  let mut parser = ParsedSrc::new(Arc::new(input)).with_lex_flags(lex_flags);
  if let Err(errors) = parser.parse_src() {
    for error in errors {
      eprintln!("{error}");
    }
    return Ok(());
  }

	let nodes = parser.extract_nodes();

  let mut dispatcher = Dispatcher::new(nodes, interactive);
  if let Some(mut stack) = io_stack {
    dispatcher.io_stack.extend(stack.drain(..));
  }
  let result = dispatcher.begin_dispatch();

  if state::get_status() != 0
    && let Some(trap) = read_logic(|l| l.get_trap(TrapTarget::Error))
  {
    let saved_status = state::get_status();
    exec_input(trap, None, false)?;
    state::set_status(saved_status);
  }

  result
}

pub struct Dispatcher {
  nodes: VecDeque<Node>,
  interactive: bool,
  pub io_stack: IoStack,
  pub job_stack: JobStack,
}

impl Dispatcher {
  pub fn new(nodes: Vec<Node>, interactive: bool) -> Self {
    let nodes = VecDeque::from(nodes);
    Self {
      nodes,
      interactive,
      io_stack: IoStack::new(),
      job_stack: JobStack::new(),
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
    match node.class {
      NdRule::Conjunction { .. } => self.exec_conjunction(node)?,
      NdRule::Pipeline { .. } => self.exec_pipeline(node)?,
      NdRule::IfNode { .. } => self.exec_if(node)?,
      NdRule::LoopNode { .. } => self.exec_loop(node)?,
      NdRule::ForNode { .. } => self.exec_for(node)?,
      NdRule::CaseNode { .. } => self.exec_case(node)?,
      NdRule::BraceGrp { .. } => self.exec_brc_grp(node)?,
      NdRule::FuncDef { .. } => self.exec_func_def(node)?,
      NdRule::Command { .. } => self.dispatch_cmd(node)?,
      NdRule::Test { .. } => self.exec_test(node)?,
      _ => unreachable!(),
    }
    Ok(())
  }
  pub fn dispatch_cmd(&mut self, node: Node) -> ShResult<()> {
    let Some(cmd) = node.get_command() else {
      return self.exec_cmd(node); // Argv is empty, probably an assignment
    };
    if is_func(node.get_command().cloned()) {
      self.exec_func(node)
    } else if cmd.flags.contains(TkFlags::BUILTIN) {
      self.exec_builtin(node)
    } else if is_subsh(node.get_command().cloned()) {
      self.exec_subsh(node)
    } else if read_shopts(|s| s.core.autocd)
		&& Path::new(cmd.span.as_str()).is_dir()
		&& !is_in_path(cmd.span.as_str()) {
      let dir = cmd.span.as_str().to_string();
      let stack = IoStack {
        stack: self.io_stack.clone(),
      };
      exec_input(format!("cd {dir}"), Some(stack), self.interactive)
    } else {
      self.exec_cmd(node)
    }
  }
  pub fn exec_conjunction(&mut self, conjunction: Node) -> ShResult<()> {
    let NdRule::Conjunction { elements } = conjunction.class else {
      unreachable!()
    };

    let mut elem_iter = elements.into_iter();
    while let Some(element) = elem_iter.next() {
      let ConjunctNode { cmd, operator } = element;
      self.dispatch_node(*cmd)?;

      let status = state::get_status();
      match operator {
        ConjunctOp::And => {
          if status != 0 {
            break;
          }
        }
        ConjunctOp::Or => {
          if status == 0 {
            break;
          }
        }
        ConjunctOp::Null => break,
      }
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
    let NdRule::FuncDef { name, body } = func_def.class else {
      unreachable!()
    };
    let body_span = body.get_span();
    let body = body_span.as_str().to_string();
    let name = name.span.as_str().strip_suffix("()").unwrap();

    if KEYWORDS.contains(&name) {
      return Err(ShErr::full(
        ShErrKind::SyntaxErr,
        format!("function: Forbidden function name `{name}`"),
        blame,
      ));
    }

    let mut func_parser = ParsedSrc::new(Arc::new(body));
    if let Err(errors) = func_parser.parse_src() {
      for error in errors {
        eprintln!("{error}");
      }
      return Ok(());
    }

    let func = ShFunc::new(func_parser);
    write_logic(|l| l.insert_func(name, func)); // Store the AST
    Ok(())
  }
  fn exec_subsh(&mut self, subsh: Node) -> ShResult<()> {
    let NdRule::Command { assignments, argv } = subsh.class else {
      unreachable!()
    };

		self.run_fork("anonymous_subshell", |s| {
			if let Err(e) = s.set_assignments(assignments, AssignBehavior::Export) {
				eprintln!("{e}");
				return;
			};
			s.io_stack.append_to_frame(subsh.redirs);
			let mut argv = match prepare_argv(argv) {
				Ok(argv) => argv,
				Err(e) => {
					eprintln!("{e}");
					return;
				}
			};

			let subsh = argv.remove(0);
			let subsh_body = subsh.0.to_string();

			if let Err(e) = exec_input(subsh_body, None, s.interactive) {
				eprintln!("{e}");
			};
		})
  }
  fn exec_func(&mut self, func: Node) -> ShResult<()> {
    let blame = func.get_span().clone();
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
      return Err(ShErr::full(
        ShErrKind::InternalErr,
        format!("maximum recursion depth ({max_depth}) exceeded"),
        blame,
      ));
    }

    let env_vars = self.set_assignments(assignments, AssignBehavior::Export)?;
    let _var_guard = VarCtxGuard::new(env_vars.into_iter().collect());

    self.io_stack.append_to_frame(func.redirs);

    let func_name = argv.remove(0).span.as_str().to_string();
    let argv = prepare_argv(argv)?;
    let result = if let Some(ref mut func_body) = read_logic(|l| l.get_func(&func_name)) {
      let _guard = ScopeGuard::exclusive_scope(Some(argv));

			func_body.body_mut().flags = func.flags;

      if let Err(e) = self.exec_brc_grp(func_body.body().clone()) {
        match e.kind() {
          ShErrKind::FuncReturn(code) => {
            state::set_status(*code);
            Ok(())
          }
          _ => Err(e).blame(blame),
        }
      } else {
        Ok(())
      }
    } else {
      Err(ShErr::full(
        ShErrKind::InternalErr,
        format!("Failed to find function '{}'", func_name),
        blame,
      ))
    };

    RECURSE_DEPTH.with(|d| d.set(d.get() - 1));
    result
  }
  fn exec_brc_grp(&mut self, brc_grp: Node) -> ShResult<()> {
    let NdRule::BraceGrp { body } = brc_grp.class else {
      unreachable!()
    };
		let fork_builtins = brc_grp.flags.contains(NdFlags::FORK_BUILTINS);

    self.io_stack.append_to_frame(brc_grp.redirs);
		let _guard = self.io_stack.pop_frame().redirect()?;
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
					eprintln!("{e}");
				}
			})
		} else {
			brc_grp_logic(self)
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
		let _guard = self.io_stack.pop_frame().redirect()?;

		let case_logic = |s: &mut Self| -> ShResult<()> {
			let exp_pattern = pattern.clone().expand()?;
			let pattern_raw = exp_pattern
				.get_words()
				.first()
				.map(|s| s.to_string())
				.unwrap_or_default();

			'outer: for block in case_blocks {
				let CaseNode { pattern, body } = block;
				let block_pattern_raw = pattern.span.as_str().trim_end_matches(')').trim();
				// Split at '|' to allow for multiple patterns like `foo|bar)`
				let block_patterns = block_pattern_raw.split('|');

				for pattern in block_patterns {
					let pattern_regex = glob_to_regex(pattern, false);
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
					eprintln!("{e}");
				}
			})
		} else {
			case_logic(self).try_blame(blame)
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
		let _guard = self.io_stack.pop_frame().redirect()?;

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
					break;
				}
			}

			Ok(())
		};

		if fork_builtins {
			log::trace!("Forking builtin: loop");
			self.run_fork("loop", |s| {
				if let Err(e) = loop_logic(s) {
					eprintln!("{e}");
				}
			})
		} else {
			loop_logic(self).try_blame(blame)
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
		let _guard = self.io_stack.pop_frame().redirect()?;

		let for_logic = |s: &mut Self| -> ShResult<()> {
			// Expand all array variables
			let arr: Vec<String> = to_expanded_strings(arr)?;
			let vars: Vec<String> = to_expanded_strings(vars)?;

			let mut for_guard = VarCtxGuard::new(vars.iter().map(|v| v.to_string()).collect());

			'outer: for chunk in arr.chunks(vars.len()) {
				let empty = String::new();
				let chunk_iter = vars
					.iter()
					.zip(chunk.iter().chain(std::iter::repeat(&empty)));

				for (var, val) in chunk_iter {
					write_vars(|v| v.set_var(&var.to_string(), &val.to_string(), VarFlags::NONE));
					for_guard.vars.insert(var.to_string());
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
					eprintln!("{e}");
				}
			})
		} else {
			for_logic(self).try_blame(blame)
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
		let _guard = self.io_stack.pop_frame().redirect()?;

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

			if !matched && !else_block.is_empty() {
				for node in else_block {
					s.dispatch_node(node)?;
				}
			}

			Ok(())
		};

		if fork_builtins {
			log::trace!("Forking builtin: if");
			self.run_fork("if", |s| {
				if let Err(e) = if_logic(s) {
					eprintln!("{e}");
					state::set_status(1);
				}
			})
		} else {
			if_logic(self).try_blame(blame)
		}
  }
  fn exec_pipeline(&mut self, pipeline: Node) -> ShResult<()> {
    let NdRule::Pipeline { cmds, pipe_err: _ } = pipeline.class else {
      unreachable!()
    };
    self.job_stack.new_job();
		let fork_builtin = cmds.len() > 1; // If there's more than one command, we need to fork builtins

    // Zip the commands and their respective pipes into an iterator
    let pipes_and_cmds = get_pipe_stack(cmds.len()).into_iter().zip(cmds);

    for ((rpipe, wpipe), mut cmd) in pipes_and_cmds {
      if let Some(pipe) = rpipe {
        self.io_stack.push_to_frame(pipe);
      }
      if let Some(pipe) = wpipe {
        self.io_stack.push_to_frame(pipe);
      }

			if fork_builtin {
				cmd.flags |= NdFlags::FORK_BUILTINS;
			}
      self.dispatch_node(cmd)?;
    }
    let job = self.job_stack.finalize_job().unwrap();
    let is_bg = pipeline.flags.contains(NdFlags::BACKGROUND);
    dispatch_job(job, is_bg)?;
    Ok(())
  }
  fn exec_builtin(&mut self, cmd: Node) -> ShResult<()> {
		let fork_builtins = cmd.flags.contains(NdFlags::FORK_BUILTINS);
		let cmd_raw = cmd.get_command().unwrap_or_else(|| panic!("expected command NdRule, got {:?}", &cmd.class)).to_string();

		if fork_builtins {
			log::trace!("Forking builtin: {}", cmd_raw);
			let _guard = self.io_stack.pop_frame().redirect()?;
			self.run_fork(&cmd_raw, |s| {
				if let Err(e) = s.dispatch_builtin(cmd) {
					eprintln!("{e}");
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
    let NdRule::Command { assignments, argv } = &mut cmd.class else {
      unreachable!()
    };
    let env_vars = self.set_assignments(mem::take(assignments), AssignBehavior::Export)?;
    let _var_guard = VarCtxGuard::new(env_vars.into_iter().collect());

    let curr_job_mut = self.job_stack.curr_job_mut().unwrap();
    let io_stack_mut = &mut self.io_stack;
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
		match cmd_raw.as_str() {
			"echo" => echo(cmd, io_stack_mut, curr_job_mut),
			"cd" => cd(cmd, curr_job_mut),
			"export" => export(cmd, io_stack_mut, curr_job_mut),
			"local" => local(cmd, io_stack_mut, curr_job_mut),
			"pwd" => pwd(cmd, io_stack_mut, curr_job_mut),
			"source" => source(cmd, curr_job_mut),
			"shift" => shift(cmd, curr_job_mut),
			"fg" => continue_job(cmd, curr_job_mut, JobBehavior::Foregound),
			"bg" => continue_job(cmd, curr_job_mut, JobBehavior::Background),
			"disown" => disown(cmd, io_stack_mut, curr_job_mut),
			"jobs" => jobs(cmd, io_stack_mut, curr_job_mut),
			"alias" => alias(cmd, io_stack_mut, curr_job_mut),
			"unalias" => unalias(cmd, io_stack_mut, curr_job_mut),
			"return" => flowctl(cmd, ShErrKind::FuncReturn(0)),
			"break" => flowctl(cmd, ShErrKind::LoopBreak(0)),
			"continue" => flowctl(cmd, ShErrKind::LoopContinue(0)),
			"exit" => flowctl(cmd, ShErrKind::CleanExit(0)),
			"zoltraak" => zoltraak(cmd, io_stack_mut, curr_job_mut),
			"shopt" => shopt(cmd, io_stack_mut, curr_job_mut),
			"read" => read_builtin(cmd, io_stack_mut, curr_job_mut),
			"trap" => trap(cmd, io_stack_mut, curr_job_mut),
			"pushd" => pushd(cmd, io_stack_mut, curr_job_mut),
			"popd" => popd(cmd, io_stack_mut, curr_job_mut),
			"dirs" => dirs(cmd, io_stack_mut, curr_job_mut),
			"exec" => exec::exec_builtin(cmd, io_stack_mut, curr_job_mut),
			"eval" => eval::eval(cmd, io_stack_mut, curr_job_mut),
			_ => unimplemented!(
				"Have not yet added support for builtin '{}'",
				cmd_raw
			),
		}
	}
  fn exec_cmd(&mut self, cmd: Node) -> ShResult<()> {
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
    }

		let no_fork = cmd.flags.contains(NdFlags::NO_FORK);

    if argv.is_empty() {
      return Ok(());
    }

    self.io_stack.append_to_frame(cmd.redirs);

    let exec_args = ExecArgs::new(argv)?;
    let _guard = self.io_stack.pop_frame().redirect()?;
    let job = self.job_stack.curr_job_mut().unwrap();

		let child_logic = || -> ! {
			let cmd = &exec_args.cmd.0;
			let span = exec_args.cmd.1;

			let Err(e) = execvpe(cmd, &exec_args.argv, &exec_args.envp);

			// execvpe only returns on error
			let cmd_str = cmd.to_str().unwrap().to_string();
			match e {
				Errno::ENOENT => {
					let err = ShErr::full(ShErrKind::CmdNotFound(cmd_str), "", span);
					eprintln!("{err}");
				}
				_ => {
					let err = ShErr::full(ShErrKind::Errno(e), format!("{e}"), span);
					eprintln!("{err}");
				}
			}
			exit(e as i32)
		};

		if no_fork {
			child_logic();
		}

    match unsafe { fork()? } {
      ForkResult::Child => child_logic(),
      ForkResult::Parent { child } => {
        // Close proc sub pipe fds - the child has inherited them
        // and will access them via /proc/self/fd/N. Keeping them
        // open here would prevent EOF on the pipe.
        write_jobs(|j| j.drain_registered_fds());

        let cmd_name = exec_args.cmd.0.to_str().unwrap();

        let child_pgid = if let Some(pgid) = job.pgid() {
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
		match unsafe { fork()? } {
			ForkResult::Child => {
				f(self);
				exit(state::get_status())
			}
			ForkResult::Parent { child } => {
				write_jobs(|j| j.drain_registered_fds());
				let job = self.job_stack.curr_job_mut().unwrap();
				let child_pgid = if let Some(pgid) = job.pgid() {
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
    match behavior {
      AssignBehavior::Export => {
        for assign in assigns {
          let NdRule::Assignment { kind, var, val } = assign.class else {
            unreachable!()
          };
          let var = var.span.as_str();
          let val = val.expand()?.get_words().join(" ");
          match kind {
            AssignKind::Eq => write_vars(|v| v.set_var(var, &val, VarFlags::EXPORT)),
            AssignKind::PlusEq => todo!(),
            AssignKind::MinusEq => todo!(),
            AssignKind::MultEq => todo!(),
            AssignKind::DivEq => todo!(),
          }
          new_env_vars.push(var.to_string());
        }
      }
      AssignBehavior::Set => {
        for assign in assigns {
          let NdRule::Assignment { kind, var, val } = assign.class else {
            unreachable!()
          };
          let var = var.span.as_str();
          let val = val.expand()?.get_words().join(" ");
          match kind {
            AssignKind::Eq => write_vars(|v| v.set_var(var, &val, VarFlags::NONE)),
            AssignKind::PlusEq => todo!(),
            AssignKind::MinusEq => todo!(),
            AssignKind::MultEq => todo!(),
            AssignKind::DivEq => todo!(),
          }
        }
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

pub fn is_func(tk: Option<Tk>) -> bool {
  let Some(tk) = tk else { return false };
  read_logic(|l| l.get_func(&tk.to_string())).is_some()
}

pub fn is_subsh(tk: Option<Tk>) -> bool {
  tk.is_some_and(|tk| tk.flags.contains(TkFlags::IS_SUBSH))
}
