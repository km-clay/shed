use crate::{jobs::{ChildProc, JobBldr}, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{execute::prepare_argv, NdRule, Node}, prelude::*};

pub fn export(node: Node, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let child_pgid = if let Some(pgid) = job.pgid() {
		pgid
	} else {
		job.set_pgid(Pid::this());
		Pid::this()
	};
	let child = ChildProc::new(Pid::this(), Some("export"), Some(child_pgid))?;
	job.push_child(child);

	let argv = prepare_argv(argv);

	for (arg,span) in argv {
		let Some((var,val)) = arg.split_once('=') else {
			return Err(
				ShErr::full(
					ShErrKind::ExecFail,
					"Expected an assignment in export args",
					span.into()
				)
			)
		};
		env::set_var(var, val);
	}
	Ok(())
}
