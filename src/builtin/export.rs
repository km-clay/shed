use crate::{jobs::{ChildProc, JobBldr}, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{execute::prepare_argv, NdRule, Node}, prelude::*, procio::{borrow_fd, IoStack}, state};

use super::setup_builtin;

pub fn export(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let (argv,io_frame) = setup_builtin(argv, job, Some((io_stack,node.redirs)))?;

	if argv.is_empty() {
		// Display the environment variables
		let mut env_output = env::vars()
			.map(|var| format!("{}={}",var.0,var.1)) // Get all of them, zip them into one string
			.collect::<Vec<_>>();
		env_output.sort(); // Sort them alphabetically
		let mut env_output = env_output.join("\n"); // Join them with newlines
		env_output.push('\n'); // Push a final newline

		let stdout = borrow_fd(STDOUT_FILENO);
		write(stdout, env_output.as_bytes())?; // Write it
	} else {
		for (arg,span) in argv {
			let Some((var,val)) = arg.split_once('=') else {
				return Err(
					ShErr::full(
						ShErrKind::SyntaxErr,
						"export: Expected an assignment in export args",
						span.into()
					)
				)
			};
			env::set_var(var, val);
		}
	}
	io_frame.unwrap().restore()?;
	state::set_status(0);
	Ok(())
}
