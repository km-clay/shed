use crate::{jobs::{ChildProc, JobBldr}, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{execute::prepare_argv, NdRule, Node}, prelude::*, state::{self, source_file}};

use super::setup_builtin;

pub fn source(node: Node, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let (argv,_) = setup_builtin(argv, job, None)?;

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

	state::set_status(0);
	Ok(())
}
