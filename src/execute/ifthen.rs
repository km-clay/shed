use crate::{parse::parse::SynTree, prelude::*};

pub fn exec_if(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::IfThen { cond_blocks, else_block } = rule {
		if shenv.ctx().flags().contains(ExecFlags::NO_FORK) {
			shenv.ctx_mut().unset_flag(ExecFlags::NO_FORK);
		}
		let mut cond_blocks = cond_blocks.into_iter();

		while let Some(block) = cond_blocks.next() {
			let cond = block.0;
			let body = block.1;
			let ast = SynTree::from_vec(cond);
			Executor::new(ast,shenv).walk()?;
			if shenv.get_code() == 0 {
				let ast = SynTree::from_vec(body);
				return Executor::new(ast,shenv).walk()
			}
		}

		if let Some(block) = else_block {
			let ast = SynTree::from_vec(block);
			Executor::new(ast,shenv).walk()?;
		}
	} else { unreachable!() }
	Ok(())
}
