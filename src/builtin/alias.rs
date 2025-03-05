use crate::prelude::*;

pub fn alias(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::Command { argv, redirs: _ } = rule {
		let argv = argv.drop_first();
		let mut argv_iter = argv.into_iter();
		while let Some(arg) = argv_iter.next() {
			let arg_raw = shenv.input_slice(arg.span()).to_string();
			if let Some((alias,body)) = arg_raw.split_once('=') {
				log!(DEBUG, "{:?}",arg.span());
				log!(DEBUG, arg_raw);
				log!(DEBUG, body);
				let clean_body = trim_quotes(&body);
				log!(DEBUG, clean_body);
				shenv.logic_mut().set_alias(alias, &clean_body);
			} else {
				return Err(ShErr::full(ShErrKind::SyntaxErr, "Expected an assignment in alias args", shenv.get_input(), arg.span().clone()))
			}
		}
	} else { unreachable!() }
	Ok(())
}
