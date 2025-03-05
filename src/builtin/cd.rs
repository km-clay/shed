use crate::{parse::parse::{Node, NdRule}, prelude::*};

pub fn cd(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::Command { argv, redirs: _ } = rule {
		let mut argv_iter = argv.into_iter();
		argv_iter.next(); // Ignore 'cd'
		let dir_raw = argv_iter.next().map(|arg| shenv.input_slice(arg.span()).into()).unwrap_or(std::env::var("HOME")?);
		let dir = PathBuf::from(&dir_raw);
		std::env::set_current_dir(dir)?;
		shenv.vars_mut().export("PWD",&dir_raw);
		shenv.set_code(0);
	}
	Ok(())
}
