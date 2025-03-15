use crate::{libsh::error::ShResult, parse::{execute::prepare_argv, NdRule, Node}, prelude::*, procio::{borrow_fd, IoStack}};

pub fn echo(node: Node, io_stack: &mut IoStack) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};
	assert!(!argv.is_empty());

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
