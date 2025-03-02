use shellenv::jobs::{ChildProc, JobBldr};

use crate::{libsh::utils::ArgVec, parse::parse::{Node, NdRule}, prelude::*};

pub fn echo(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();

	if let NdRule::Command { argv, redirs } = rule {
		let argv = argv.drop_first().as_strings(shenv);
		let mut formatted = argv.join(" ");
		formatted.push('\n');

		if shenv.ctx().flags().contains(ExecFlags::NO_FORK) {
			shenv.collect_redirs(redirs);
			if let Err(e) = shenv.ctx_mut().activate_rdrs() {
				eprintln!("{:?}",e);
				exit(1);
			}
			if let Err(e) = write_out(formatted) {
				eprintln!("{:?}",e);
				exit(1);
			}
			exit(0);
		} else {
			match unsafe { fork()? } {
				Child => {
					shenv.collect_redirs(redirs);
					if let Err(e) = shenv.ctx_mut().activate_rdrs() {
						eprintln!("{:?}",e);
						exit(1);
					}
					if let Err(e) = write_out(formatted) {
						eprintln!("{:?}",e);
						exit(1);
					}
					exit(0);
				}
				Parent { child } => {
					shenv.reset_io()?;
					let children = vec![
						ChildProc::new(child, Some("echo"), Some(child))?
					];
					let job = JobBldr::new()
						.with_children(children)
						.with_pgid(child)
						.build();
					wait_fg(job, shenv)?;
				}
			}
		}
	} else { unreachable!() }
	Ok(())
}
