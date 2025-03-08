use shellenv::jobs::{ChildProc, JobBldr};

use crate::prelude::*;

bitflags! {
	#[derive(Debug,Clone,Copy)]
	pub struct EchoFlags: u32 {
		const USE_ESCAPE = 0b0001;
		const NO_ESCAPE  = 0b0010;
		const STDERR     = 0b0100;
		const NO_NEWLINE = 0b1000;
	}
}

pub fn echo(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();

	if let NdRule::Command { argv, redirs } = rule {
		let mut argv_iter = argv.into_iter().skip(1).peekable();
		let mut echo_flags = EchoFlags::empty();
		while let Some(arg) = argv_iter.peek() {
			let blame = arg.span();
			let raw = arg.as_raw(shenv);
			if raw.starts_with('-') {
				let _ = argv_iter.next();
				let mut options = raw.strip_prefix('-').unwrap().chars();
				while let Some(opt) = options.next() {
					match opt {
						'r' => echo_flags |= EchoFlags::STDERR,
						'n' => echo_flags |= EchoFlags::NO_NEWLINE,
						'e' => {
							if echo_flags.contains(EchoFlags::NO_ESCAPE) {
								return Err(
									ShErr::full(
										ShErrKind::ExecFail,
										"the 'e' and 'E' flags are mutually exclusive",
										shenv.get_input(),
										blame
									)
								)
							}
							echo_flags |= EchoFlags::USE_ESCAPE;
						}
						'E' => {
							if echo_flags.contains(EchoFlags::USE_ESCAPE) {
								return Err(
									ShErr::full(
										ShErrKind::ExecFail,
										"the 'e' and 'E' flags are mutually exclusive",
										shenv.get_input(),
										blame
									)
								)
							}
							echo_flags |= EchoFlags::NO_ESCAPE;
						}
						_ => return Err(
							ShErr::full(
								ShErrKind::ExecFail,
								format!("Unrecognized echo option"),
								shenv.get_input(),
								blame
							)
						)
					}
				}
			} else {
				break
			}
		}
		let argv = argv_iter.collect::<Vec<_>>().as_strings(shenv);
		let mut formatted = argv.join(" ");
		if !echo_flags.contains(EchoFlags::NO_NEWLINE) {
			formatted.push('\n');
		}

		shenv.collect_redirs(redirs);
		shenv.ctx_mut().activate_rdrs()?;
		write_out(formatted)?;

	} else { unreachable!() }
	Ok(())
}
