use crate::{jobs::{ChildProc, JobBldr}, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{execute::prepare_argv, NdRule, Node}, prelude::*, state::write_vars};

pub fn cd(node: Node, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let child_pgid = if let Some(pgid) = job.pgid() {
		pgid
	} else {
		job.set_pgid(Pid::this());
		Pid::this()
	};
	let child = ChildProc::new(Pid::this(), Some("cd"), Some(child_pgid))?;
	job.push_child(child);

	let argv = prepare_argv(argv);
	let new_dir = if let Some((arg,_)) = argv.into_iter().skip(1).next() {
		PathBuf::from(arg)
	} else {
		PathBuf::from(env::var("HOME").unwrap())
	};

	env::set_current_dir(new_dir).unwrap();
	let new_dir = env::current_dir().unwrap();
	env::set_var("PWD", new_dir);

	Ok(())
}
