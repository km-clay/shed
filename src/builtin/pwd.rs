use crate::{jobs::{ChildProc, JobBldr}, libsh::error::ShResult, parse::{NdRule, Node}, prelude::*, procio::{borrow_fd, IoStack}, state};

use super::setup_builtin;

pub fn pwd(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments, argv } = node.class else {
		unreachable!()
	};

	let (_,io_frame) = setup_builtin(argv, job, Some((io_stack,node.redirs)))?;

	let stdout = borrow_fd(STDOUT_FILENO);

	let mut curr_dir = env::current_dir().unwrap().to_str().unwrap().to_string();
	curr_dir.push('\n');
	write(stdout, curr_dir.as_bytes())?;

	io_frame.unwrap().restore().unwrap();
	state::set_status(0);
	Ok(())
}
