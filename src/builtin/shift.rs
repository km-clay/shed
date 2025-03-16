use crate::{jobs::{ChildProc, JobBldr}, libsh::error::{ErrSpan, ShErr, ShErrKind, ShResult}, parse::{execute::prepare_argv, NdRule, Node}, prelude::*, state::write_vars};

pub fn shift(node: Node, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let child_pgid = if let Some(pgid) = job.pgid() {
		pgid
	} else {
		job.set_pgid(Pid::this());
		Pid::this()
	};
	let child = ChildProc::new(Pid::this(), Some("shift"), Some(child_pgid))?;
	job.push_child(child);

	let mut argv = prepare_argv(argv).into_iter().skip(1);

	if let Some((arg,span)) = argv.next() {
		let Ok(count) = arg.parse::<usize>() else {
			return Err(
				ShErr::full(
					ShErrKind::ExecFail,
					"Expected a number in shift args",
					span.into()
				)
			)
		};
		for _ in 0..count {
			write_vars(|v| v.fpop_arg());
		}
	}

	Ok(())
}
