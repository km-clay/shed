use std::collections::VecDeque;


use crate::{builtin::{cd::cd, echo::echo, export::export, pwd::pwd, shift::shift, source::source}, jobs::{dispatch_job, ChildProc, Job, JobBldr}, libsh::error::ShResult, prelude::*, procio::{IoFrame, IoPipe, IoStack}, state::{self, write_vars}};

use super::{lex::{Span, Tk, TkFlags}, AssignKind, ConjunctNode, ConjunctOp, NdFlags, NdRule, Node, Redir, RedirType};

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
	pub curr_job: Option<JobBldr>
}

impl<'t> Dispatcher<'t> {
	pub fn new(nodes: Vec<Node<'t>>) -> Self {
		let nodes = VecDeque::from(nodes);
		Self { nodes, io_stack: IoStack::new(), curr_job: None }
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
			NdRule::CmdList {..} => self.exec_conjunction(node)?,
			NdRule::Pipeline {..} => self.exec_pipeline(node)?,
			NdRule::Command {..} => self.dispatch_cmd(node)?,
			_ => unreachable!()
		}
		Ok(())
	}
	pub fn dispatch_cmd(&mut self, node: Node<'t>) -> ShResult<()> {
		let Some(cmd) = node.get_command() else {
			return self.exec_cmd(node)
		};
		if cmd.flags.contains(TkFlags::BUILTIN) {
			self.exec_builtin(node)
		} else {
			self.exec_cmd(node)
		}
	}
	pub fn exec_conjunction(&mut self, conjunction: Node<'t>) -> ShResult<()> {
		let NdRule::CmdList { elements } = conjunction.class else {
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
	pub fn exec_pipeline(&mut self, pipeline: Node<'t>) -> ShResult<()> {
		let NdRule::Pipeline { cmds, pipe_err } = pipeline.class else {
			unreachable!()
		};
		self.curr_job = Some(JobBldr::new());
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
		let job = self.finalize_job();
		let is_bg = pipeline.flags.contains(NdFlags::BACKGROUND);
		dispatch_job(job, is_bg)?;
		Ok(())
	}
	pub fn finalize_job(&mut self) -> Job {
		self.curr_job.take().unwrap().build()
	}
	pub fn exec_builtin(&mut self, mut cmd: Node<'t>) -> ShResult<()> {
		let NdRule::Command { ref mut assignments, argv } = &mut cmd.class else {
			unreachable!()
		};
		let env_vars_to_unset = self.set_assignments(mem::take(assignments), AssignBehavior::Export);
		let cmd_raw = cmd.get_command().unwrap();
		flog!(TRACE, "doing builtin");
		let curr_job_mut = self.curr_job.as_mut().unwrap();
		let io_stack_mut = &mut self.io_stack;
		let result = match cmd_raw.span.as_str() {
			"echo" => echo(cmd, io_stack_mut, curr_job_mut),
			"cd" => cd(cmd, curr_job_mut),
			"export" => export(cmd, curr_job_mut),
			"pwd" => pwd(cmd, io_stack_mut, curr_job_mut),
			"source" => source(cmd, curr_job_mut),
			"shift" => shift(cmd, curr_job_mut),
			_ => unimplemented!("Have not yet added support for builtin '{}'", cmd_raw.span.as_str())
		};

		for var in env_vars_to_unset {
			env::set_var(&var, "");
		}

		Ok(result?)
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
			self.curr_job.as_mut().unwrap(),
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
			let (rpipe,wpipe) = IoPipe::get_pipes();
			let r_redir = Redir::new(Box::new(rpipe), RedirType::Input);
			let w_redir = Redir::new(Box::new(wpipe), RedirType::Output);

			// Push (prev_read, Some(w_redir)) and set prev_read to r_redir
			stack.push((prev_read.take(), Some(w_redir)));
			prev_read = Some(r_redir);
		}
	}
	stack
}
