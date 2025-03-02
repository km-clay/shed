use crate::prelude::*;

pub fn export(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::Command { argv, redirs: _ } = rule {
		let mut argv_iter = argv.into_iter();
		argv_iter.next(); // Ignore 'export'
		while let Some(arg) = argv_iter.next() {
			let arg_raw = arg.to_string();
			if let Some((var,val)) = arg_raw.split_once('=') {
				shenv.vars_mut().export(var, val);
			} else {
				eprintln!("Expected an assignment in export args, found this: {}", arg_raw)
			}
		}
	} else { unreachable!() }
	Ok(())
}
