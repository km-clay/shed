use crate::{builtin::setup_builtin, jobs::{ChildProc, JobBldr}, libsh::error::ShResult, parse::{execute::prepare_argv, NdRule, Node}, prelude::*, procio::{borrow_fd, IoStack}, state};

pub fn echo(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};
	assert!(!argv.is_empty());

	let (argv,io_frame) = setup_builtin(argv, job, Some((io_stack,node.redirs)))?;

	let stdout = borrow_fd(STDOUT_FILENO);

	let mut echo_output = argv.into_iter()
		.map(|a| a.0) // Extract the String from the tuple of (String,Span)
		.collect::<Vec<_>>()
		.join(" ");

	echo_output.push('\n');

	write(stdout, echo_output.as_bytes())?;

	io_frame.unwrap().restore()?;
	state::set_status(0);
	Ok(())
}
