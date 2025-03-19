use crate::{builtin::setup_builtin, jobs::JobBldr, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{execute::prepare_argv, NdRule, Node}, prelude::*, procio::IoStack, state};

pub fn flowctl(node: Node, kind: ShErrKind) -> ShResult<()> {
	use ShErrKind::*;
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};
	let mut code = 0;

	let mut argv = prepare_argv(argv)?;
	let cmd = argv.remove(0).0;

	if !argv.is_empty() {
		let (arg,span) = argv
			.into_iter()
			.next()
			.unwrap();

		let Ok(status) = arg.parse::<i32>() else {
			return Err(
				ShErr::full(ShErrKind::SyntaxErr, format!("{cmd}: Expected a number"), span)
			)
		};

		code = status;
	}

	flog!(DEBUG,code);

	let kind = match kind {
		LoopContinue(_) => LoopContinue(code),
		LoopBreak(_) => LoopBreak(code),
		FuncReturn(_) => FuncReturn(code),
		CleanExit(_) => CleanExit(code),
		_ => unreachable!()
	};

	Err(ShErr::simple(kind, ""))
}
