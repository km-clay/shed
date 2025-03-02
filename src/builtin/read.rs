use crate::prelude::*;

pub fn read_builtin(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::Command { argv, redirs: _ } = rule {
		let argv = argv.drop_first();
		let mut argv_iter = argv.iter();
		// TODO: properly implement redirections
		// using activate_redirs() was causing issues, may require manual handling

		let mut buf = vec![0u8; 1024];
		let bytes_read = read(0, &mut buf)?;
		buf.truncate(bytes_read);

		let read_input = String::from_utf8_lossy(&buf).trim_end().to_string();

		if let Some(var) = argv_iter.next() {
			/*
			let words: Vec<&str> = read_input.split_whitespace().collect();

			for (var, value) in argv_iter.zip(words.iter().chain(std::iter::repeat(&""))) {
				shenv.vars_mut().set_var(&var.to_string(), value);
			}

			// Assign the rest of the string to the first variable if there's only one
			if argv.len() == 1 {
				shenv.vars_mut().set_var(&first_var.to_string(), &read_input);
			}
			*/
			shenv.vars_mut().set_var(&var.to_string(), &read_input);
		}
	} else {
		unreachable!()
	}

	log!(TRACE, "leaving read");
	shenv.set_code(0);
	Ok(())
}
