use crate::prelude::*;

pub fn exec_if(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::IfThen { cond_blocks, else_block, redirs } = rule {
		shenv.collect_redirs(redirs);
		if shenv.ctx().flags().contains(ExecFlags::NO_FORK) {
			shenv.ctx_mut().unset_flag(ExecFlags::NO_FORK);
		}
		let mut cond_blocks = cond_blocks.into_iter();

		while let Some(block) = cond_blocks.next() {
			let cond = block.0;
			let body = block.1;
			let ret = shenv.exec_as_cond(cond)?;
			if ret == 0 {
				shenv.exec_as_body(body)?;
				return Ok(())
			}
		}

		if let Some(block) = else_block {
			shenv.exec_as_body(block)?;
		}
	} else { unreachable!() }
	Ok(())
}
