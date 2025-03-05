use crate::prelude::*;

pub fn sh_flow(node: Node, shenv: &mut ShEnv, kind: ShErrKind) -> ShResult<()> {
	let rule = node.into_rule();
	let mut code: i32 = 0;
	if let NdRule::Command { argv, redirs } = rule {
		let mut argv_iter = argv.into_iter();
		while let Some(arg) = argv_iter.next() {
			if let Ok(code_arg) = shenv.input_slice(arg.span()).parse() {
				code = code_arg
			}
		}
	} else { unreachable!() }
	shenv.set_code(code);
	// Our control flow keywords are used as ShErrKinds
	// This design will halt the execution flow and start heading straight back upward
	// Function returns and loop breaks/continues will be caught in the proper context to allow
	// Execution to continue at the proper return point.
	Err(ShErr::simple(kind, ""))
}
