use std::collections::{HashSet, VecDeque};


use crate::{builtin::{alias::alias, cd::cd, echo::echo, export::export, flowctl::flowctl, jobctl::{continue_job, jobs, JobBehavior}, pwd::pwd, shift::shift, shopt::shopt, source::source, zoltraak::zoltraak}, expand::expand_aliases, jobs::{dispatch_job, ChildProc, JobBldr, JobStack}, libsh::{error::{ShErr, ShErrKind, ShResult, ShResultExt}, utils::RedirVecUtils}, prelude::*, procio::{IoFrame, IoMode, IoStack}, state::{self, get_snapshots, read_logic, read_vars, restore_snapshot, write_logic, write_meta, write_vars, ShFunc, VarTab, LOGIC_TABLE}};

use super::{lex::{Span, Tk, TkFlags, KEYWORDS}, AssignKind, CaseNode, CondNode, ConjunctNode, ConjunctOp, LoopKind, NdFlags, NdRule, Node, ParsedSrc, Redir, RedirType};

pub enum AssignBehavior {
	Export,
	Set
}

/// Arguments to the execvpe function
pub struct ExecArgs {
	pub cmd: (CString,Span),
	pub argv: Vec<CString>,
	pub envp: Vec<CString>
}

impl ExecArgs {
	pub fn new(argv: Vec<Tk>) -> ShResult<Self> {
		assert!(!argv.is_empty());
		let argv = prepare_argv(argv)?;
		let cmd = Self::get_cmd(&argv);
		let argv = Self::get_argv(argv);
		let envp = Self::get_envp();
		Ok(Self { cmd, argv, envp })
	}
	pub fn get_cmd(argv: &[(String,Span)]) -> (CString,Span) {
		let cmd = argv[0].0.as_str();
		let span = argv[0].1.clone();
		(CString::new(cmd).unwrap(),span)
	}
	pub fn get_argv(argv: Vec<(String,Span)>) -> Vec<CString> {
		argv.into_iter().map(|s| CString::new(s.0).unwrap()).collect()
	}
	pub fn get_envp() -> Vec<CString> {
		std::env::vars().map(|v| CString::new(format!("{}={}",v.0,v.1)).unwrap()).collect()
	}
}

pub fn exec_input(input: String) -> ShResult<()> {
	write_meta(|m| m.start_timer());
	let log_tab = LOGIC_TABLE.read().unwrap();
	let input = expand_aliases(input, HashSet::new(), &log_tab);
	mem::drop(log_tab); // Release lock ASAP
	let mut parser = ParsedSrc::new(Arc::new(input));
	if let Err(errors) = parser.parse_src() {
		for error in errors {
			eprintln!("{error}");
		}
		return Ok(())
	}

	let mut dispatcher = Dispatcher::new(parser.extract_nodes());
	dispatcher.begin_dispatch()
}

pub struct Dispatcher {
	nodes: VecDeque<Node>,
	pub io_stack: IoStack,
	pub job_stack: JobStack
}

impl Dispatcher {
	pub fn new(nodes: Vec<Node>) -> Self {
		let nodes = VecDeque::from(nodes);
		Self { nodes, io_stack: IoStack::new(), job_stack: JobStack::new() }
	}
	pub fn begin_dispatch(&mut self) -> ShResult<()> {
		flog!(TRACE, "beginning dispatch");
		while let Some(node) = self.nodes.pop_front() {
			let blame = node.get_span();
			self.dispatch_node(node).try_blame(blame)?;
		}
		Ok(())
	}
	pub fn dispatch_node(&mut self, node: Node) -> ShResult<()> {
		match node.class {
			NdRule::Conjunction {..} => self.exec_conjunction(node)?,
			NdRule::Pipeline {..} => self.exec_pipeline(node)?,
			NdRule::IfNode {..} => self.exec_if(node)?,
			NdRule::LoopNode {..} => self.exec_loop(node)?,
			NdRule::CaseNode {..} => self.exec_case(node)?,
			NdRule::BraceGrp {..} => self.exec_brc_grp(node)?,
			NdRule::FuncDef {..} => self.exec_func_def(node)?,
			NdRule::Command {..} => self.dispatch_cmd(node)?,
			_ => unreachable!()
		}
		Ok(())
	}
	pub fn dispatch_cmd(&mut self, node: Node) -> ShResult<()> {
		let Some(cmd) = node.get_command() else {
			return self.exec_cmd(node) // Argv is empty, probably an assignment
		};
		if cmd.flags.contains(TkFlags::BUILTIN) {
			self.exec_builtin(node)
		} else if is_func(node.get_command().cloned()) {
			self.exec_func(node)
		} else if is_subsh(node.get_command().cloned()) {
			self.exec_subsh(node)
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
				ConjunctOp::And => if status != 0 { break },
				ConjunctOp::Or => if status == 0 { break },
				ConjunctOp::Null => break
			}
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
			return Err(
				ShErr::full(
					ShErrKind::SyntaxErr,
					format!("function: Forbidden function name `{name}`"),
					blame
				)
			)
		}

		let mut func_parser = ParsedSrc::new(Arc::new(body));
		if let Err(errors) = func_parser.parse_src() {
			for error in errors {
				eprintln!("{error}");
			}
			return Ok(())
		}

		let func = ShFunc::new(func_parser);
		write_logic(|l| l.insert_func(name, func)); // Store the AST
		Ok(())
	}
	fn exec_subsh(&mut self, subsh: Node) -> ShResult<()> {
		let NdRule::Command { assignments, argv } = subsh.class else {
			unreachable!()
		};

		self.set_assignments(assignments, AssignBehavior::Export);
		self.io_stack.append_to_frame(subsh.redirs);
		let mut argv = prepare_argv(argv)?;

		let subsh = argv.remove(0);
		let subsh_body = subsh.0.to_string();
		flog!(DEBUG, subsh_body);
		let snapshot = get_snapshots();

		if let Err(e) = exec_input(subsh_body) {
			restore_snapshot(snapshot);
			return Err(e.into())
		}

		restore_snapshot(snapshot);
		Ok(())
	}
	fn exec_func(&mut self, func: Node) -> ShResult<()> {
		let blame = func.get_span().clone();
		let NdRule::Command { assignments, mut argv } = func.class else {
			unreachable!()
		};

		self.set_assignments(assignments, AssignBehavior::Export);

		self.io_stack.append_to_frame(func.redirs);

		let func_name = argv.remove(0).span.as_str().to_string();
		if let Some(func) = read_logic(|l| l.get_func(&func_name)) {
			let snapshot = get_snapshots();
			// Set up the inner scope
			write_vars(|v| {
				**v = VarTab::new();
				v.clear_args();
				for arg in argv {
					v.bpush_arg(arg.to_string());
				}
			});

			if let Err(e) = self.exec_brc_grp((*func).clone()) {
				restore_snapshot(snapshot);
				match e.kind() {
					ShErrKind::FuncReturn(code) => {
						state::set_status(*code);
						return Ok(())
					}
					_ => return {
						Err(e.into())
					}
				}

			}

			// Return to the outer scope
			restore_snapshot(snapshot);
			Ok(())
		} else {
			Err(
				ShErr::full(
					ShErrKind::InternalErr,
					format!("Failed to find function '{}'",func_name),
					blame
				)
			)
		}
	}
	fn exec_brc_grp(&mut self, brc_grp: Node) -> ShResult<()> {
		let NdRule::BraceGrp { body } = brc_grp.class else {
			unreachable!()
		};
		let mut io_frame = self.io_stack.pop_frame();
		io_frame.extend(brc_grp.redirs);

		for node in body {
			let blame = node.get_span();
			self.io_stack.push_frame(io_frame.clone());
			self.dispatch_node(node).try_blame(blame)?;
		}

		Ok(())
	}
	fn exec_case(&mut self, case_stmt: Node) -> ShResult<()> {
		let NdRule::CaseNode { pattern, case_blocks } = case_stmt.class else {
			unreachable!()
		};

		self.io_stack.append_to_frame(case_stmt.redirs);

		flog!(DEBUG,pattern.span.as_str());
		let exp_pattern = pattern.clone().expand()?;
		let pattern_raw = exp_pattern
			.get_words()
			.first()
			.map(|s| s.to_string())
			.unwrap_or_default();
		flog!(DEBUG,exp_pattern);

		for block in case_blocks {
			let CaseNode { pattern, body } = block;
			let block_pattern_raw = pattern.span.as_str().trim_end_matches(')').trim();
			// Split at '|' to allow for multiple patterns like `foo|bar)`
			let block_patterns = block_pattern_raw.split('|');

			for pattern in block_patterns {
				if pattern_raw == pattern || pattern == "*" {
					for node in &body {
						self.dispatch_node(node.clone())?;
					}
				}
			}
		}

		Ok(())
	}
	fn exec_loop(&mut self, loop_stmt: Node) -> ShResult<()> {
		let NdRule::LoopNode { kind, cond_node } = loop_stmt.class else {
			unreachable!();
		};
		let keep_going = |kind: LoopKind, status: i32| -> bool {
			match kind {
				LoopKind::While => status == 0,
				LoopKind::Until => status != 0
			}
		};

		let io_frame = self.io_stack.pop_frame();
		let (mut cond_frame,mut body_frame) = io_frame.split_frame();
		let (in_redirs,out_redirs) = loop_stmt.redirs.split_by_channel();
		cond_frame.extend(in_redirs);
		body_frame.extend(out_redirs);

		let CondNode { cond, body } = cond_node;
		'outer: loop {
			self.io_stack.push(cond_frame.clone());

			if let Err(e) = self.dispatch_node(*cond.clone()) {
				state::set_status(1);
				return Err(e.into());
			}

			let status = state::get_status();
			if keep_going(kind,status) {
				self.io_stack.push(body_frame.clone());
				for node in &body {
					if let Err(e) = self.dispatch_node(node.clone()) {
						match e.kind() {
							ShErrKind::LoopBreak(code) => {
								state::set_status(*code);
								break 'outer
							}
							ShErrKind::LoopContinue(code) => {
								state::set_status(*code);
								continue 'outer
							}
							_ => return Err(e.into())
						}
					}
				}
			} else {
				break
			}
		}

		Ok(())
	}
	fn exec_if(&mut self, if_stmt: Node) -> ShResult<()> {
		let NdRule::IfNode { cond_nodes, else_block } = if_stmt.class else {
			unreachable!();
		};
		// Pop the current frame and split it
		let io_frame = self.io_stack.pop_frame();
		let (mut cond_frame,mut body_frame) = io_frame.split_frame();
		let (in_redirs,out_redirs) = if_stmt.redirs.split_by_channel();
		cond_frame.extend(in_redirs); // Condition gets input redirs
		body_frame.extend(out_redirs); // Body gets output redirs

		for node in cond_nodes {
			let CondNode { cond, body } = node;
			self.io_stack.push(cond_frame.clone());

			if let Err(e) = self.dispatch_node(*cond) {
				state::set_status(1);
				return Err(e.into());
			}

			match state::get_status() {
				0 => {
					for body_node in body {
						self.io_stack.push(body_frame.clone());
						self.dispatch_node(body_node)?;
					}
				}
				_ => continue
			}
		}

		if !else_block.is_empty() {
			for node in else_block {
				self.io_stack.push(body_frame.clone());
				self.dispatch_node(node)?;
			}
		}

		Ok(())
	}
	fn exec_pipeline(&mut self, pipeline: Node) -> ShResult<()> {
		let NdRule::Pipeline { cmds, pipe_err: _ } = pipeline.class else {
			unreachable!()
		};
		self.job_stack.new_job();
		// Zip the commands and their respective pipes into an iterator
		let pipes_and_cmds = get_pipe_stack(cmds.len())
			.into_iter()
			.zip(cmds);


		for ((rpipe,wpipe), cmd) in pipes_and_cmds {
			if let Some(pipe) = rpipe {
				self.io_stack.push_to_frame(pipe);
			}
			if let Some(pipe) = wpipe {
				self.io_stack.push_to_frame(pipe);
			}
			self.dispatch_node(cmd)?;
		}
		let job = self.job_stack.finalize_job().unwrap();
		let is_bg = pipeline.flags.contains(NdFlags::BACKGROUND);
		dispatch_job(job, is_bg)?;
		Ok(())
	}
	fn exec_builtin(&mut self, mut cmd: Node) -> ShResult<()> {
		let NdRule::Command { ref mut assignments, argv: _ } = &mut cmd.class else {
			unreachable!()
		};
		let env_vars_to_unset = self.set_assignments(mem::take(assignments), AssignBehavior::Export);
		let cmd_raw = cmd.get_command().unwrap();
		let curr_job_mut = self.job_stack.curr_job_mut().unwrap();
		let io_stack_mut = &mut self.io_stack;

		flog!(TRACE, "doing builtin");
		let result = match cmd_raw.span.as_str() {
			"echo" => echo(cmd, io_stack_mut, curr_job_mut),
			"cd" => cd(cmd, curr_job_mut),
			"export" => export(cmd, io_stack_mut, curr_job_mut),
			"pwd" => pwd(cmd, io_stack_mut, curr_job_mut),
			"source" => source(cmd, curr_job_mut),
			"shift" => shift(cmd, curr_job_mut),
			"fg" => continue_job(cmd, curr_job_mut, JobBehavior::Foregound),
			"bg" => continue_job(cmd, curr_job_mut, JobBehavior::Background),
			"jobs" => jobs(cmd, io_stack_mut, curr_job_mut),
			"alias" => alias(cmd, io_stack_mut, curr_job_mut),
			"return" => flowctl(cmd, ShErrKind::FuncReturn(0)),
			"break" => flowctl(cmd, ShErrKind::LoopBreak(0)),
			"continue" => flowctl(cmd, ShErrKind::LoopContinue(0)),
			"exit" => flowctl(cmd, ShErrKind::CleanExit(0)),
			"zoltraak" => zoltraak(cmd, io_stack_mut, curr_job_mut),
			"shopt" => shopt(cmd, io_stack_mut, curr_job_mut),
			_ => unimplemented!("Have not yet added support for builtin '{}'", cmd_raw.span.as_str())
		};

		for var in env_vars_to_unset {
			env::set_var(&var, "");
		}

		if let Err(e) = result {
			state::set_status(1);
			return Err(e.into())
		}
		Ok(())
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
			env_vars_to_unset = self.set_assignments(assignments, assign_behavior);
		}

		if argv.is_empty() {
			return Ok(())
		}

		self.io_stack.append_to_frame(cmd.redirs);

		let exec_args = ExecArgs::new(argv)?;
		let io_frame = self.io_stack.pop_frame();
		run_fork(
			io_frame,
			Some(exec_args),
			self.job_stack.curr_job_mut().unwrap(),
			def_child_action,
			def_parent_action
		)?;

		for var in env_vars_to_unset {
			std::env::set_var(&var, "");
		}

		Ok(())
	}
	fn set_assignments(&self, assigns: Vec<Node>, behavior: AssignBehavior) -> Vec<String> {
		let mut new_env_vars = vec![];
		match behavior {
			AssignBehavior::Export => {
				for assign in assigns {
					let NdRule::Assignment { kind, var, val } = assign.class else {
						unreachable!()
					};
					let var = var.span.as_str();
					let val = val.span.as_str();
					match kind {
						AssignKind::Eq => write_vars(|v| v.set_var(var, val, true)),
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
					let val = val.span.as_str();
					match kind {
						AssignKind::Eq => write_vars(|v| v.set_var(var, val, false)),
						AssignKind::PlusEq => todo!(),
						AssignKind::MinusEq => todo!(),
						AssignKind::MultEq => todo!(),
						AssignKind::DivEq => todo!(),
					}
				}
			}
		}
		new_env_vars
	}
}

pub fn prepare_argv(argv: Vec<Tk>) -> ShResult<Vec<(String,Span)>> {
	let mut args = vec![];

	for arg in argv {
		let span = arg.span.clone();
		let expanded = arg.expand()?;
		for exp in expanded.get_words() {
			args.push((exp,span.clone()))
		}
	}
	Ok(args)
}

pub fn run_fork<'t,C,P>(
	io_frame: IoFrame,
	exec_args: Option<ExecArgs>,
	job: &mut JobBldr,
	child_action: C,
	parent_action: P,
) -> ShResult<()>
where
		C: Fn(IoFrame,Option<ExecArgs>),
		P: Fn(&mut JobBldr,Option<&str>,Pid) -> ShResult<()>
{
	match unsafe { fork()? } {
		ForkResult::Child => {
			child_action(io_frame,exec_args);
			exit(0); // Just in case
		}
		ForkResult::Parent { child } => {
			let cmd = if let Some(args) = exec_args {
				Some(args.cmd.0.to_str().unwrap().to_string())
			} else {
				None
			};
			parent_action(job,cmd.as_deref(),child)
		}
	}
}

/// The default behavior for the child process after forking
pub fn def_child_action(mut io_frame: IoFrame, exec_args: Option<ExecArgs>) {
	if let Err(e) = io_frame.redirect() {
		eprintln!("{e}");
	}
	let exec_args = exec_args.unwrap();
	let cmd = &exec_args.cmd.0;
	let span = exec_args.cmd.1;

	let Err(e) = execvpe(&cmd, &exec_args.argv, &exec_args.envp);

	let cmd = cmd.to_str().unwrap().to_string();
	match e {
		Errno::ENOENT => {
			let err = ShErr::full(
				ShErrKind::CmdNotFound(cmd),
				"",
				span
			);
			eprintln!("{err}");
		}
		_ => {
			let err = ShErr::full(
				ShErrKind::Errno,
				format!("{e}"),
				span
			);
			eprintln!("{err}");
		}
	}
	exit(e as i32)
}

/// The default behavior for the parent process after forking
pub fn def_parent_action(
	job: &mut JobBldr,
	cmd: Option<&str>,
	child_pid: Pid
) -> ShResult<()> {
	let child_pgid = if let Some(pgid) = job.pgid() {
		pgid
	} else {
		job.set_pgid(child_pid);
		child_pid
	};
	let child = ChildProc::new(child_pid, cmd, Some(child_pgid))?;
	job.push_child(child);
	Ok(())
}


/// Initialize the pipes for a pipeline
/// The first command gets `(None, WPipe)`
/// The last command gets `(RPipe, None)`
/// Commands inbetween get `(RPipe, WPipe)`
/// If there is only one command, it gets `(None, None)`
pub fn get_pipe_stack(num_cmds: usize) -> Vec<(Option<Redir>,Option<Redir>)> {
	let mut stack = Vec::with_capacity(num_cmds);
	let mut prev_read: Option<Redir> = None;

	for i in 0..num_cmds {
		if i == num_cmds - 1 {
			stack.push((prev_read.take(), None));
		} else {
			let (rpipe,wpipe) = IoMode::get_pipes();
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
	let Some(tk) = tk else {
		return false
	};
	read_logic(|l| l.get_func(&tk.to_string())).is_some()
}

pub fn is_subsh(tk: Option<Tk>) -> bool {
	tk.is_some_and(|tk| tk.flags.contains(TkFlags::IS_SUBSH))
}
