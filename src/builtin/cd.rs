use crate::prelude::*;

pub fn cd(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::Command { argv, redirs: _ } = rule {
		let mut argv_iter = argv.into_iter();
		argv_iter.next(); // Ignore 'cd'
		let dir_raw = argv_iter.next().map(|arg| shenv.input_slice(arg.span()).into()).unwrap_or(std::env::var("HOME")?);
		let dir = PathBuf::from(&dir_raw);
		std::env::set_current_dir(dir)?;
		let new_dir = std::env::current_dir()?;
		shenv.vars_mut().export("PWD",new_dir.to_str().unwrap());
		shenv.set_code(0);
	}
	Ok(())
}
