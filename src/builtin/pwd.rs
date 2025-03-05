use shellenv::jobs::{ChildProc, JobBldr};

use crate::{parse::parse::{Node, NdRule}, prelude::*};

pub fn pwd(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::Command { argv: _, redirs } = rule {
		let mut pwd = shenv.vars().get_var("PWD").to_string();
		pwd.push('\n');

		if shenv.ctx().flags().contains(ExecFlags::NO_FORK) {
			shenv.collect_redirs(redirs);
			if let Err(e) = shenv.ctx_mut().activate_rdrs() {
				eprintln!("{:?}",e);
				exit(1);
			}
			if let Err(e) = write_out(pwd) {
				eprintln!("{:?}",e);
				exit(1);
			}
			exit(0);
		} else {
			match unsafe { fork()? } {
				Child => {
					if let Err(e) = shenv.ctx_mut().activate_rdrs() {
						eprintln!("{:?}",e);
						exit(1);
					}
					if let Err(e) = write_out(pwd) {
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
