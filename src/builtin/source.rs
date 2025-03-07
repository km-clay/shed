use crate::prelude::*;

pub fn source(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::Command { argv, redirs } = rule {
		shenv.collect_redirs(redirs);
		let mut argv_iter = argv.into_iter().skip(1);
		while let Some(arg) = argv_iter.next() {
			let arg_raw = arg.as_raw(shenv);
			let arg_path = PathBuf::from(arg_raw);
			shenv.source_file(arg_path)?;
		}
	} else { unreachable!() }
	Ok(())
}
