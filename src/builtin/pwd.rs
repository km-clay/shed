use shellenv::jobs::{ChildProc, JobBldr};

use crate::prelude::*;

pub fn pwd(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::Command { argv: _, redirs } = rule {
		let mut pwd = shenv.vars().get_var("PWD").to_string();
		pwd.push('\n');

		shenv.collect_redirs(redirs);
		shenv.ctx_mut().activate_rdrs()?;
		write_out(pwd)?;

	} else { unreachable!() }
	Ok(())
}
