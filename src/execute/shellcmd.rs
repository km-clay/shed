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

pub fn exec_for(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();

	if let NdRule::ForLoop { vars, arr, body, redirs } = rule {
		shenv.collect_redirs(redirs);
		let saved_vars = shenv.vars().clone();

		if shenv.ctx().flags().contains(ExecFlags::NO_FORK) {
			shenv.ctx_mut().unset_flag(ExecFlags::NO_FORK);
		}
		log!(DEBUG, vars);
		log!(DEBUG, arr);

		for chunk in arr.chunks(vars.len()) {
			log!(DEBUG, "input: {}", shenv.get_input());
			for (var,value) in vars.iter().zip(chunk.iter()) {
				let var = var.as_raw(shenv);
				let val = value.as_raw(shenv);
				log!(DEBUG,var);
				log!(DEBUG,val);
				shenv.vars_mut().set_var(&var, &val);
			}

			if chunk.len() < vars.len() {
				for var in &vars[chunk.len()..] { // If 'vars' is longer than the chunk, then unset the orphaned vars
					let var = var.as_raw(shenv);
					log!(DEBUG, "unsetting");
					log!(DEBUG, var);
					shenv.vars_mut().unset_var(&var);
				}
			}

			shenv.exec_as_body(body.clone())?;
		}
		*shenv.vars_mut() = saved_vars;

	} else { unreachable!() }
	Ok(())
}

pub fn exec_case(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();

	if let NdRule::Case { pat, blocks, redirs } = rule {
		shenv.collect_redirs(redirs);
		let mut blocks_iter = blocks.into_iter();
		let pat_raw = expand_token(pat, shenv)
			.iter()
			.map(|tk| tk.as_raw(shenv))
			.collect::<Vec<_>>()
			.join(" ");

		while let Some((block_pat, block)) = blocks_iter.next() {
			let block_pat_raw = block_pat.as_raw(shenv);
			let block_pat_raw = block_pat_raw.trim_end_matches(')');
			if block_pat_raw == "*" {
				let _ret = shenv.exec_as_body(block)?;
				return Ok(())
			} else if block_pat_raw.contains('|') {
				let pats = block_pat_raw.split('|');
				for pat in pats {
					if pat_raw.trim() == pat.trim() {
						let _ret = shenv.exec_as_body(block)?;
						return Ok(())
					}
				}
			} else if pat_raw.trim() == block_pat_raw.trim() {
				let _ret = shenv.exec_as_body(block)?;
				return Ok(())
			}
		}
	} else { unreachable!() }
	Ok(())
}
