use std::collections::VecDeque;


use crate::{builtin::{cd::cd, echo::echo, export::export, jobctl::{continue_job, jobs, JobBehavior}, pwd::pwd, shift::shift, source::source}, exec_input, jobs::{dispatch_job, ChildProc, Job, JobBldr, JobStack}, libsh::{error::{ErrSpan, ShErr, ShErrKind, ShResult}, utils::RedirVecUtils}, prelude::*, procio::{IoFrame, IoMode, IoStack}, state::{self, read_logic, read_vars, write_logic, write_vars}};

use super::{lex::{LexFlags, LexStream, Span, Tk, TkFlags}, AssignKind, CondNode, ConjunctNode, ConjunctOp, LoopKind, NdFlags, NdRule, Node, ParseStream, Redir, RedirType};

pub enum AssignBehavior {
	Export,
	Set
}

/// Arguments to the execvpe function
pub struct ExecArgs {
	pub cmd: CString,
	pub argv: Vec<CString>,
	pub envp: Vec<CString>
}

impl ExecArgs {
	pub fn new(argv: Vec<Tk>) -> Self {
		assert!(!argv.is_empty());
		let argv = prepare_argv(argv);
		let cmd = Self::get_cmd(&argv);
		let argv = Self::get_argv(argv);
		let envp = Self::get_envp();
		Self { cmd, argv, envp }
	}
	pub fn get_cmd(argv: &[(String,Span)]) -> CString {
		CString::new(argv[0].0.as_str()).unwrap()
	}
	pub fn get_argv(argv: Vec<(String,Span)>) -> Vec<CString> {
		argv.into_iter().map(|s| CString::new(s.0).unwrap()).collect()
	}
	pub fn get_envp() -> Vec<CString> {
		std::env::vars().map(|v| CString::new(format!("{}={}",v.0,v.1)).unwrap()).collect()
	}
}

pub struct Dispatcher<'t> {
	nodes: VecDeque<Node<'t>>,
	pub io_stack: IoStack,
	pub job_stack: JobStack
}

impl<'t> Dispatcher<'t> {
	pub fn new(nodes: Vec<Node<'t>>) -> Self {
		let nodes = VecDeque::from(nodes);
		Self { nodes, io_stack: IoStack::new(), job_stack: JobStack::new() }
	}
	pub fn begin_dispatch(&mut self) -> ShResult<()> {
		flog!(TRACE, "beginning dispatch");
		while let Some(list) = self.nodes.pop_front() {
			self.dispatch_node(list)?;
		}
		Ok(())
	}
	pub fn dispatch_node(&mut self, node: Node<'t>) -> ShResult<()> {
		match node.class {
			NdRule::Conjunction {..} => self.exec_conjunction(node)?,
			NdRule::Pipeline {..} => self.exec_pipeline(node)?,
			NdRule::IfNode {..} => self.exec_if(node)?,
			NdRule::LoopNode {..} => self.exec_loop(node)?,
			NdRule::BraceGrp {..} => self.exec_brc_grp(node)?,
			NdRule::FuncDef {..} => self.exec_func_def(node)?,
			NdRule::Command {..} => self.dispatch_cmd(node)?,
			_ => unreachable!()
		}
		Ok(())
	}
	pub fn dispatch_cmd(&mut self, node: Node<'t>) -> ShResult<()> {
		let Some(cmd) = node.get_command() else {
			return self.exec_cmd(node) // Argv is empty, probably an assignment
		};
		if cmd.flags.contains(TkFlags::BUILTIN) {
			self.exec_builtin(node)
		} else if is_func(node.get_command().cloned()) {
			self.exec_func(node)
		} else {
			self.exec_cmd(node)
		}
	}
	pub fn exec_conjunction(&mut self, conjunction: Node<'t>) -> ShResult<()> {
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
	pub fn exec_func_def(&mut self, func_def: Node<'t>) -> ShResult<()> {
		let NdRule::FuncDef { name, body } = func_def.class else {
			unreachable!()
		};
		let body_span = body.get_span();
		let body = body_span.as_str();
		let name = name.span.as_str().strip_suffix("()").unwrap();
		write_logic(|l| l.insert_func(name, body));
		Ok(())
	}
	pub fn exec_func(&mut self, func: Node<'t>) -> ShResult<()> {
		let blame: ErrSpan = func.get_span().into();
		// TODO: Find a way to store functions as pre-parsed nodes so we don't have to re-parse them
		let NdRule::Command { assignments, mut argv } = func.class else {
			unreachable!()
		};

		self.set_assignments(assignments, AssignBehavior::Export);

		let mut io_frame = self.io_stack.pop_frame();
		io_frame.extend(func.redirs);

		let func_name = argv.remove(0).span.as_str().to_string();
		if let Some(func_body) = read_logic(|l| l.get_func(&func_name)) {
			let saved_sh_args = read_vars(|v| v.sh_argv().clone());
			write_vars(|v| {
				v.clear_args();
				for arg in argv {
					v.bpush_arg(arg.to_string());
				}
			});

			let result = exec_input(&func_body, Some(io_frame));

			write_vars(|v| *v.sh_argv_mut() = saved_sh_args);
			Ok(result?)
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
	pub fn exec_brc_grp(&mut self, brc_grp: Node<'t>) -> ShResult<()> {
		let NdRule::BraceGrp { body } = brc_grp.class else {
			unreachable!()
		};
		let mut io_frame = self.io_stack.pop_frame();
		io_frame.extend(brc_grp.redirs);

		for node in body {
			self.io_stack.push_frame(io_frame.clone());
			self.dispatch_node(node)?;
		}

		Ok(())
	}
	pub fn exec_loop(&mut self, loop_stmt: Node<'t>) -> ShResult<()> {
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
		loop {
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
							ShErrKind::LoopBreak => break,
							ShErrKind::LoopContinue => continue,
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
	pub fn exec_if(&mut self, if_stmt: Node<'t>) -> ShResult<()> {
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
	pub fn exec_pipeline(&mut self, pipeline: Node<'t>) -> ShResult<()> {
		let NdRule::Pipeline { cmds, pipe_err } = pipeline.class else {
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
	pub fn exec_builtin(&mut self, mut cmd: Node<'t>) -> ShResult<()> {
		let NdRule::Command { ref mut assignments, argv } = &mut cmd.class else {
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
	pub fn exec_cmd(&mut self, cmd: Node<'t>) -> ShResult<()> {
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

		let exec_args = ExecArgs::new(argv);
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
	pub fn set_assignments(&self, assigns: Vec<Node<'t>>, behavior: AssignBehavior) -> Vec<String> {
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
						AssignKind::Eq => std::env::set_var(var, val),
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
						AssignKind::Eq => write_vars(|v| v.new_var(var, val)),
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

pub fn prepare_argv(argv: Vec<Tk>) -> Vec<(String,Span)> {
	let mut args = vec![];

	for arg in argv {
		let flags = arg.flags;
		let span = arg.span.clone();
		let expanded = arg.expand(span.clone(), flags);
		for exp in expanded.get_words() {
			args.push((exp,span.clone()))
		}
	}
	args
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
		P: Fn(IoFrame,&mut JobBldr,Option<&str>,Pid) -> ShResult<()>
{
	match unsafe { fork()? } {
		ForkResult::Child => {
			child_action(io_frame,exec_args);
			exit(0); // Just in case
		}
		ForkResult::Parent { child } => {
			let cmd = if let Some(args) = exec_args {
				Some(args.cmd.to_str().unwrap().to_string())
			} else {
				None
			};
			parent_action(io_frame,job,cmd.as_deref(),child)
		}
	}
}

/// The default behavior for the child process after forking
pub fn def_child_action<'t>(mut io_frame: IoFrame, exec_args: Option<ExecArgs>) {
	if let Err(e) = io_frame.redirect() {
		eprintln!("{e}");
	}
	let exec_args = exec_args.unwrap();
	let cmd = &exec_args.cmd.to_str().unwrap().to_string();
	let Err(e) = execvpe(&exec_args.cmd, &exec_args.argv, &exec_args.envp);
	match e {
		Errno::ENOENT => eprintln!("Command not found: {}", cmd),
		_ => eprintln!("{e}")
	}
	exit(e as i32)
}

/// The default behavior for the parent process after forking
pub fn def_parent_action<'t>(
	io_frame: IoFrame,
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

pub fn is_func<'t>(tk: Option<Tk<'t>>) -> bool {
	let Some(tk) = tk else {
		return false
	};
	read_logic(|l| l.get_func(&tk.to_string())).is_some()
}
