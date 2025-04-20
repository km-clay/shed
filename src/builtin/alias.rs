use crate::{jobs::JobBldr, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{NdRule, Node}, prelude::*, procio::{borrow_fd, IoStack}, state::{self, read_logic, write_logic}};

use super::setup_builtin;

pub fn alias(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let (argv,io_frame) = setup_builtin(argv, job, Some((io_stack,node.redirs)))?;

	if argv.is_empty() {
		// Display the environment variables
		let mut alias_output = read_logic(|l| {
			l.aliases()
				.iter()
				.map(|ent| format!("{} = \"{}\"", ent.0, ent.1))
				.collect::<Vec<_>>()
		});
		alias_output.sort(); // Sort them alphabetically
		let mut alias_output = alias_output.join("\n"); // Join them with newlines
		alias_output.push('\n'); // Push a final newline

		let stdout = borrow_fd(STDOUT_FILENO);
		write(stdout, alias_output.as_bytes())?; // Write it
	} else {
		for (arg,span) in argv {
			if arg == "command" || arg == "builtin" {
				return Err(
					ShErr::full(
						ShErrKind::ExecFail,
						format!("alias: Cannot assign alias to reserved name '{arg}'"),
						span
					)
				)
			}

			let Some((name,body)) = arg.split_once('=') else {
				return Err(
					ShErr::full(
						ShErrKind::SyntaxErr,
						"alias: Expected an assignment in alias args",
						span
					)
				)
			};
			write_logic(|l| l.insert_alias(name, body));
		}
	}
	io_frame.unwrap().restore()?;
	state::set_status(0);
	Ok(())
}

pub fn unalias(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let (argv, io_frame) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;


	if argv.is_empty() {
		// Display the environment variables
		let mut alias_output = read_logic(|l| {
			l.aliases()
				.iter()
				.map(|ent| format!("{} = \"{}\"", ent.0, ent.1))
				.collect::<Vec<_>>()
		});
		alias_output.sort(); // Sort them alphabetically
		let mut alias_output = alias_output.join("\n"); // Join them with newlines
		alias_output.push('\n'); // Push a final newline

		let stdout = borrow_fd(STDOUT_FILENO);
		write(stdout, alias_output.as_bytes())?; // Write it
	} else {
		for (arg,span) in argv {
			flog!(DEBUG, arg);
			if read_logic(|l| l.get_alias(&arg)).is_none() {
				return Err(
					ShErr::full(
						ShErrKind::SyntaxErr,
						format!("unalias: alias '{arg}' not found"),
						span
					)
				)
			};
			write_logic(|l| l.remove_alias(&arg))
		}
	}
	io_frame.unwrap().restore()?;
	state::set_status(0);
	Ok(())
}
