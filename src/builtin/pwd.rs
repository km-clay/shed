use crate::{jobs::{ChildProc, JobBldr}, libsh::error::ShResult, parse::{NdRule, Node}, prelude::*, procio::{borrow_fd, IoStack}};

pub fn pwd(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments, argv } = node.class else {
		unreachable!()
	};

	let child_pgid = if let Some(pgid) = job.pgid() {
		pgid
	} else {
		job.set_pgid(Pid::this());
		Pid::this()
	};
	let child = ChildProc::new(Pid::this(), Some("pwd"), Some(child_pgid))?;
	job.push_child(child);

	io_stack.append_to_frame(node.redirs);
	let mut io_frame = io_stack.pop_frame();

	io_frame.redirect()?;

	let stdout = borrow_fd(STDOUT_FILENO);

	let mut curr_dir = env::current_dir().unwrap().to_str().unwrap().to_string();
	curr_dir.push('\n');
	write(stdout, curr_dir.as_bytes())?;
	io_frame.restore().unwrap();

	Ok(())
}
