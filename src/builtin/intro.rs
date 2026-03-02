use std::{env, os::unix::fs::PermissionsExt, path::Path};

use ariadne::{Fmt, Span};

use crate::{builtin::BUILTINS, libsh::error::{ShErr, ShErrKind, ShResult, next_color}, parse::{NdRule, Node, execute::prepare_argv, lex::KEYWORDS}, state::{self, ShAlias, ShFunc, read_logic}};

pub fn type_builtin(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }

	/*
	 * we have to check in the same order that the dispatcher checks this
	 * 1. function
	 * 2. builtin
	 * 3. command
	 */

	'outer: for (arg,span) in argv {
		if let Some(func) = read_logic(|v| v.get_func(&arg)) {
			let ShFunc { body: _, source } = func;
			let (line, col) = source.line_and_col();
			let name = source.source().name();
			println!("{arg} is a function defined at {name}:{}:{}", line + 1, col + 1);
		} else if let Some(alias) = read_logic(|v| v.get_alias(&arg)) {
			let ShAlias { body, source } = alias;
			let (line, col) = source.line_and_col();
			let name = source.source().name();
			println!("{arg} is an alias for '{body}' defined at {name}:{}:{}", line + 1, col + 1);
		} else if BUILTINS.contains(&arg.as_str()) {
			println!("{arg} is a shell builtin");
		} else if KEYWORDS.contains(&arg.as_str()) {
			println!("{arg} is a shell keyword");
		} else {
			let path = env::var("PATH").unwrap_or_default();
			let paths = path.split(':')
				.map(Path::new)
				.collect::<Vec<_>>();

			for path in paths {
				if let Ok(entries) = path.read_dir() {
					for entry in entries.flatten() {
						let Ok(meta) = std::fs::metadata(entry.path()) else {
							continue;
						};
						let is_exec = meta.permissions().mode() & 0o111 != 0;

						if meta.is_file()
						&& is_exec
						&& let Some(name) = entry.file_name().to_str()
						&& name == arg {
							println!("{arg} is {}", entry.path().display());
							continue 'outer;
						}
					}
				}
			}

			state::set_status(1);
			return Err(ShErr::at(ShErrKind::NotFound, span, format!("'{}' is not a command, function, or alias", arg.fg(next_color()))));
		}
	}

  state::set_status(0);
  Ok(())
}
