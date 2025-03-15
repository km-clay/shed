use std::collections::VecDeque;


use crate::{builtin::echo::echo, libsh::error::ShResult, prelude::*, procio::{IoFrame, IoPipe, IoStack}, state::{self, write_vars}};

use super::{lex::{Tk, TkFlags}, AssignKind, ConjunctNode, ConjunctOp, NdRule, Node, Redir, RedirType};

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
	pub fn get_cmd(argv: &[String]) -> CString {
		CString::new(argv[0].as_str()).unwrap()
	}
	pub fn get_argv(argv: Vec<String>) -> Vec<CString> {
		argv.into_iter().map(|s| CString::new(s).unwrap()).collect()
	}
	pub fn get_envp() -> Vec<CString> {
		std::env::vars().map(|v| CString::new(format!("{}={}",v.0,v.1)).unwrap()).collect()
	}
}

pub struct Dispatcher<'t> {
	nodes: VecDeque<Node<'t>>,
	pub io_stack: IoStack
}

impl<'t> Dispatcher<'t> {
	pub fn new(nodes: Vec<Node<'t>>) -> Self {
		let nodes = VecDeque::from(nodes);
		Self { nodes, io_stack: IoStack::new() }
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
		Ok(())
	}
	pub fn exec_builtin(&mut self, mut cmd: Node<'t>) -> ShResult<()> {
		let NdRule::Command { ref mut assignments, argv } = &mut cmd.class else {
			unreachable!()
		};
		let env_vars_to_unset = self.set_assignments(mem::take(assignments), AssignBehavior::Export);
		let cmd_raw = cmd.get_command().unwrap();
		flog!(TRACE, "doing builtin");
		let result = match cmd_raw.span.as_str() {
			"echo" => echo(cmd, &mut self.io_stack),
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
		for redir in cmd.redirs {
			self.io_stack.push_to_frame(redir);
		}
		if argv.is_empty() {
			return Ok(())
		}

		let exec_args = ExecArgs::new(argv);
		let io_frame = self.io_stack.pop_frame();
		run_fork(
			io_frame,
			exec_args,
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

pub fn prepare_argv(argv: Vec<Tk>) -> Vec<String> {
	let mut args = vec![];

	for arg in argv {
		let flags = arg.flags;
		let span = arg.span.clone();
		let expanded = arg.expand(span, flags);
		args.extend(expanded.get_words());
	}
	args
}

pub fn run_fork<'t,C,P>(
	io_frame: IoFrame,
	exec_args: ExecArgs,
	child_action: C,
	parent_action: P,
) -> ShResult<()>
where
		C: Fn(IoFrame,ExecArgs) -> Errno,
		P: Fn(IoFrame,Pid) -> ShResult<()>
{
	match unsafe { fork()? } {
		ForkResult::Child => {
			let cmd = &exec_args.cmd.to_str().unwrap().to_string();
			let errno = child_action(io_frame,exec_args);
			match errno {
				Errno::ENOENT => eprintln!("Command not found: {}", cmd),
				_ => eprintln!("{errno}")
			}
			exit(errno as i32);
		}
		ForkResult::Parent { child } => {
			parent_action(io_frame,child)
		}
	}
}

/// The default behavior for the child process after forking
pub fn def_child_action<'t>(mut io_frame: IoFrame, exec_args: ExecArgs) -> Errno {
	if let Err(e) = io_frame.redirect() {
		eprintln!("{e}");
	}
	let Err(e) = execvpe(&exec_args.cmd, &exec_args.argv, &exec_args.envp);
	e
}

/// The default behavior for the parent process after forking
pub fn def_parent_action<'t>(io_frame: IoFrame, child: Pid) -> ShResult<()> {
	let status = waitpid(child, Some(WtFlag::WSTOPPED))?;
	match status {
		WtStat::Exited(_, status) => state::set_status(status),
		_ => unimplemented!()
	}
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
