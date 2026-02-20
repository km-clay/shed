use std::{env, path::PathBuf};

use nix::{libc::STDOUT_FILENO, unistd::write};

use crate::{builtin::setup_builtin, jobs::JobBldr, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{NdRule, Node, lex::Span}, procio::{IoStack, borrow_fd}, state::{self, read_meta, write_meta}};

enum StackIdx {
	FromTop(usize),
	FromBottom(usize),
}

fn print_dirs() -> ShResult<()> {
	let current_dir = env::current_dir()?;
	let dirs_iter = read_meta(|m| {
		m.dirs()
			.clone()
			.into_iter()
	});
	let all_dirs = [current_dir].into_iter().chain(dirs_iter)
			.map(|d| d.to_string_lossy().to_string())
			.map(|d| {
				let Ok(home) = env::var("HOME") else {
					return d;
				};

				if d.starts_with(&home) {
					let new = d.strip_prefix(&home).unwrap();
					format!("~{new}")
				} else {
					d
				}
			}).collect::<Vec<_>>()
			.join(" ");

	let stdout = borrow_fd(STDOUT_FILENO);
	write(stdout, all_dirs.as_bytes())?;
	write(stdout, b"\n")?;

	Ok(())
}

fn change_directory(target: &PathBuf, blame: Span) -> ShResult<()> {
	if !target.is_dir() {
		return Err(ShErr::full(
			ShErrKind::ExecFail,
			format!("not a directory: {}", target.display()),
			blame,
		));
	}

	if let Err(e) = env::set_current_dir(target) {
		return Err(ShErr::full(
			ShErrKind::ExecFail,
			format!("Failed to change directory: {}", e),
			blame,
		));
	}
	let new_dir = env::current_dir().map_err(|e| {
		ShErr::full(
			ShErrKind::ExecFail,
			format!("Failed to get current directory: {}", e),
			blame,
		)
	})?;
	unsafe { env::set_var("PWD", new_dir) };
	Ok(())
}

fn parse_stack_idx(arg: &str, blame: Span, cmd: &str) -> ShResult<StackIdx> {
	let (from_top, digits) = if let Some(rest) = arg.strip_prefix('+') {
		(true, rest)
	} else if let Some(rest) = arg.strip_prefix('-') {
		(false, rest)
	} else {
		unreachable!()
	};

	if digits.is_empty() {
		return Err(ShErr::full(
			ShErrKind::ExecFail,
			format!("{cmd}: missing index after '{}'", if from_top { "+" } else { "-" }),
			blame,
		));
	}

	for ch in digits.chars() {
		if !ch.is_ascii_digit() {
			return Err(ShErr::full(
				ShErrKind::ExecFail,
				format!("{cmd}: invalid argument: {arg}"),
				blame,
			));
		}
	}

	let n = digits.parse::<usize>().map_err(|e| {
		ShErr::full(
			ShErrKind::ExecFail,
			format!("{cmd}: invalid index: {e}"),
			blame,
		)
	})?;

	if from_top {
		Ok(StackIdx::FromTop(n))
	} else {
		Ok(StackIdx::FromBottom(n))
	}
}

pub fn pushd(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let blame = node.get_span().clone();
	let NdRule::Command {
		assignments: _,
		argv
	} = node.class else { unreachable!() };

	let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

	let mut dir = None;
	let mut rotate_idx = None;
	let mut no_cd = false;

	for (arg, _) in argv {
		if arg.starts_with('+') || (arg.starts_with('-') && arg.len() > 1 && arg.as_bytes()[1].is_ascii_digit()) {
			rotate_idx = Some(parse_stack_idx(&arg, blame.clone(), "pushd")?);
		} else if arg == "-n" {
			no_cd = true;
		} else if arg.starts_with('-') {
			return Err(ShErr::full(
				ShErrKind::ExecFail,
				format!("pushd: invalid option: {arg}"),
				blame.clone(),
			));
		} else {
			if dir.is_some() {
				return Err(ShErr::full(
					ShErrKind::ExecFail,
					"pushd: too many arguments".to_string(),
					blame.clone(),
				));
			}
			let target = PathBuf::from(&arg);
			if !target.is_dir() {
				return Err(ShErr::full(
					ShErrKind::ExecFail,
					format!("pushd: not a directory: {arg}"),
					blame.clone(),
				));
			}
			dir = Some(target);
		}
	}

	if let Some(idx) = rotate_idx {
		let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
		let new_cwd = write_meta(|m| {
			let dirs = m.dirs_mut();
			dirs.push_front(cwd);
			match idx {
				StackIdx::FromTop(n) => dirs.rotate_left(n),
				StackIdx::FromBottom(n) => dirs.rotate_right(n + 1),
			}
			dirs.pop_front()
		});

		if let Some(dir) = new_cwd
		&& !no_cd {
			change_directory(&dir, blame)?;
			print_dirs()?;
		}
	} else if let Some(dir) = dir {
		let old_dir = env::current_dir()?;
		if old_dir != dir {
			write_meta(|m| m.push_dir(old_dir));
		}

		if no_cd {
			state::set_status(0);
			return Ok(());
		}

		change_directory(&dir, blame)?;
		print_dirs()?;
	}

	state::set_status(0);
	Ok(())
}

pub fn popd(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let blame = node.get_span().clone();
	let NdRule::Command {
		assignments: _,
		argv
	} = node.class else { unreachable!() };

	let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

	let mut remove_idx = None;
	let mut no_cd = false;

	for (arg, _) in argv {
		if arg.starts_with('+') || (arg.starts_with('-') && arg.len() > 1 && arg.as_bytes()[1].is_ascii_digit()) {
			remove_idx = Some(parse_stack_idx(&arg, blame.clone(), "popd")?);
		} else if arg == "-n" {
			no_cd = true;
		} else if arg.starts_with('-') {
			return Err(ShErr::full(
				ShErrKind::ExecFail,
				format!("popd: invalid option: {arg}"),
				blame.clone(),
			));
		}
	}

	if let Some(idx) = remove_idx {
		match idx {
			StackIdx::FromTop(0) => {
				// +0 is same as plain popd: pop top, cd to it
				let dir = write_meta(|m| m.pop_dir());
				if !no_cd {
					if let Some(dir) = dir {
						change_directory(&dir, blame.clone())?;
					} else {
						return Err(ShErr::full(
							ShErrKind::ExecFail,
							"popd: directory stack empty".to_string(),
							blame.clone(),
						));
					}
				}
			}
			StackIdx::FromTop(n) => {
				// +N (N>0): remove (N-1)th stored entry, no cd
				write_meta(|m| {
					let dirs = m.dirs_mut();
					let idx = n - 1;
					if idx >= dirs.len() {
						return Err(ShErr::full(
							ShErrKind::ExecFail,
							format!("popd: directory index out of range: +{n}"),
							blame.clone(),
						));
					}
					dirs.remove(idx);
					Ok(())
				})?;
			}
			StackIdx::FromBottom(n) => {
				write_meta(|m| -> ShResult<()> {
					let dirs = m.dirs_mut();
					let actual = dirs.len().checked_sub(n + 1).ok_or_else(|| {
						ShErr::full(
							ShErrKind::ExecFail,
							format!("popd: directory index out of range: -{n}"),
							blame.clone(),
						)
					})?;
					dirs.remove(actual);
					Ok(())
				})?;
			}
		}
		print_dirs()?;
	} else {
		let dir = write_meta(|m| m.pop_dir());

		if no_cd {
			state::set_status(0);
			return Ok(());
		}

		if let Some(dir) = dir {
			change_directory(&dir, blame.clone())?;
			print_dirs()?;
		} else {
			return Err(ShErr::full(
				ShErrKind::ExecFail,
				"popd: directory stack empty".to_string(),
				blame.clone(),
			));
		}
	}

	Ok(())
}

pub fn dirs(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let blame = node.get_span().clone();
	let NdRule::Command {
		assignments: _,
		argv
	} = node.class else { unreachable!() };

	let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

	let mut abbreviate_home = true;
	let mut one_per_line = false;
	let mut one_per_line_indexed = false;
	let mut clear_stack = false;
	let mut target_idx: Option<StackIdx> = None;

	for (arg,_) in argv {
		match arg.as_str() {
			"-p" => one_per_line = true,
			"-v" => one_per_line_indexed = true,
			"-c" => clear_stack = true,
			"-l" => abbreviate_home = false,
		 _ if (arg.starts_with('+') || arg.starts_with('-')) && arg.len() > 1 && arg.as_bytes()[1].is_ascii_digit() => {
				target_idx = Some(parse_stack_idx(&arg, blame.clone(), "dirs")?);
			}
			_ if arg.starts_with('-') => {
				return Err(ShErr::full(
					ShErrKind::ExecFail,
					format!("dirs: invalid option: {arg}"),
					blame.clone(),
				));
			}
			_ => {
				return Err(ShErr::full(
					ShErrKind::ExecFail,
					format!("dirs: unexpected argument: {arg}"),
					blame.clone(),
				));
			}
		}
	}

	if clear_stack {
		write_meta(|m| m.dirs_mut().clear());
		return Ok(())
	}


	let mut dirs: Vec<String> = read_meta(|m| {
		let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
		let stack = [current_dir].into_iter()
			.chain(m.dirs().clone())
			.map(|d| d.to_string_lossy().to_string());

		if abbreviate_home {
			let Ok(home) = env::var("HOME") else {
				return stack.collect();
			};
			stack.map(|d| {
				if d.starts_with(&home) {
					let new = d.strip_prefix(&home).unwrap();
					format!("~{new}")
				} else {
					d
				}
			}).collect()
		} else {
			stack.collect()
		}
	});

	if let Some(idx) = target_idx {
		let target = match idx {
			StackIdx::FromTop(n) => dirs.get(n),
			StackIdx::FromBottom(n) => dirs.get(dirs.len().saturating_sub(n + 1)),
		};

		if let Some(dir) = target {
			dirs = vec![dir.clone()];
		} else {
			return Err(ShErr::full(
				ShErrKind::ExecFail,
				format!("dirs: directory index out of range: {}", match idx {
					StackIdx::FromTop(n) => format!("+{n}"),
					StackIdx::FromBottom(n) => format!("-{n}"),
				}),
				blame.clone(),
			));
		}
	}

	let mut output = String::new();

	if one_per_line {
		output = dirs.join("\n");
	} else if one_per_line_indexed {
		for (i, dir) in dirs.iter_mut().enumerate() {
			*dir = format!("{i}\t{dir}");
		}
		output = dirs.join("\n");
		output.push('\n');
	} else {
		print_dirs()?;
	}

	let stdout = borrow_fd(STDOUT_FILENO);
	write(stdout, output.as_bytes())?;

	Ok(())
}
