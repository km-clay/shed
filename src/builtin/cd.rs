use crate::{jobs::JobBldr, libsh::error::ShResult, parse::{NdRule, Node}, prelude::*, state::{self}};

use super::setup_builtin;

pub fn cd(node: Node, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let (argv,_) = setup_builtin(argv,job,None)?;

	let new_dir = if let Some((arg,_)) = argv.into_iter().skip(1).next() {
		PathBuf::from(arg)
	} else {
		PathBuf::from(env::var("HOME").unwrap())
	};

	env::set_current_dir(new_dir).unwrap();
	let new_dir = env::current_dir().unwrap();
	env::set_var("PWD", new_dir);

	state::set_status(0);
	Ok(())
}
