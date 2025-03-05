use crate::{parse::parse::LoopKind, prelude::*};

pub fn exec_loop(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();

	if let NdRule::Loop { kind, cond, body, redirs } = rule {
		shenv.collect_redirs(redirs);

		if shenv.ctx().flags().contains(ExecFlags::NO_FORK) {
			shenv.ctx_mut().unset_flag(ExecFlags::NO_FORK);
		}

		loop {
			let ret = shenv.exec_as_cond(cond.clone())?;
			match kind {
				LoopKind::While => {
					if ret == 0 {
						match shenv.exec_as_body(body.clone()) {
							Ok(_) => continue,
							Err(e) => {
								match e.kind() {
									ShErrKind::LoopContinue => continue,
									ShErrKind::LoopBreak => break,
									_ => return Err(e.into())
								}
							}
						}
					} else { break }
				}
				LoopKind::Until => {
					if ret != 0 {
						match shenv.exec_as_body(body.clone()) {
							Ok(_) => continue,
							Err(e) => {
								match e.kind() {
									ShErrKind::LoopContinue => continue,
									ShErrKind::LoopBreak => break,
									_ => return Err(e.into())
								}
							}
						}
					} else { break }
				}
			}
		}
	} else { unreachable!() }
	Ok(())
}
