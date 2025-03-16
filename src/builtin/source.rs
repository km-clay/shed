use crate::{jobs::{ChildProc, JobBldr}, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{execute::prepare_argv, NdRule, Node}, prelude::*, state::source_file};

pub fn source(node: Node, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let child_pgid = if let Some(pgid) = job.pgid() {
		pgid
	} else {
		job.set_pgid(Pid::this());
		Pid::this()
	};
	let child = ChildProc::new(Pid::this(), Some("source"), Some(child_pgid))?;
	job.push_child(child);

	let argv = prepare_argv(argv).into_iter().skip(1);

	for (arg,span) in argv {
		let path = PathBuf::from(arg);
		if !path.exists() {
			return Err(
				ShErr::full(
					ShErrKind::ExecFail,
					"source: File not found",
					span.into()
				)
			);
		}
		if !path.is_file() {
			return Err(
				ShErr::full(
					ShErrKind::ExecFail,
					"source: Given path is not a file",
					span.into()
				)
			);
		}
		source_file(path)?;
	}

	Ok(())
}
