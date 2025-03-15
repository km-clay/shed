use crate::{jobs::{ChildProc, JobBldr}, libsh::error::ShResult, parse::{execute::prepare_argv, NdRule, Node}, prelude::*, procio::{borrow_fd, IoStack}};

pub fn echo(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};
	assert!(!argv.is_empty());

	let child_pgid = if let Some(pgid) = job.pgid() {
		pgid
	} else {
		job.set_pgid(Pid::this());
		Pid::this()
	};
	let child = ChildProc::new(Pid::this(), Some("echo"), Some(child_pgid))?;
	job.push_child(child);

	for redir in node.redirs {
		io_stack.push_to_frame(redir);
	}
	let mut io_frame = io_stack.pop_frame();

	io_frame.redirect()?;

	let stdout = borrow_fd(STDOUT_FILENO);

	let mut echo_output = prepare_argv(argv)
		.into_iter()
		.skip(1) // Skip 'echo'
		.collect::<Vec<_>>()
		.join(" ");

	echo_output.push('\n');

	write(stdout, echo_output.as_bytes())?;

	io_frame.restore()?;
	Ok(())
}
