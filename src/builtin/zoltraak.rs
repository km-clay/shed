use std::{os::unix::fs::OpenOptionsExt, sync::LazyLock};

use crate::{getopt::{get_opts_from_tokens, Opt, OptSet}, jobs::JobBldr, libsh::error::{Note, ShErr, ShErrKind, ShResult, ShResultExt}, parse::{NdRule, Node}, prelude::*, procio::IoStack};

use super::setup_builtin;

pub const ZOLTRAAK_OPTS: LazyLock<OptSet> = LazyLock::new(|| {
	[
		Opt::Long("dry-run".into()),
		Opt::Long("confirm".into()),
		Opt::Long("no-preserve-root".into()),
		Opt::Short('r'),
		Opt::Short('f'),
		Opt::Short('v')
	].into()
});

/// Annihilate a file
///
/// This command works similarly to 'rm', but behaves more destructively.
/// The file given as an argument is completely destroyed. The command works by shredding all of the data contained in the file, before truncating the length of the file to 0 to ensure that not even any metadata remains.
pub fn zoltraak(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments, argv } = node.class else {
		unreachable!()
	};

	let (argv,opts) = get_opts_from_tokens(argv);

	let (argv, io_frame) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

	for (arg,span) in argv {
		annihilate(&arg, false).blame(span)?;
	}

	Ok(())
}

fn annihilate(path: &str, allow_dirs: bool) -> ShResult<()> {
	let path_buf = PathBuf::from(path);

	const BLOCK_SIZE: u64 = 4096;

	if !path_buf.exists() {
		return Err(
			ShErr::simple(
				ShErrKind::ExecFail,
				format!("zoltraak: File '{path}' not found")
			)
		)
	}

	if path_buf.is_file() {
		let mut file = OpenOptions::new()
			.write(true)
			.custom_flags(libc::O_DIRECT)
			.open(path_buf)?;

		let meta = file.metadata()?;
		let file_size = meta.len();
		let full_blocks = file_size / BLOCK_SIZE;
		let byte_remainder = file_size % BLOCK_SIZE;

		let full_buf = vec![0; BLOCK_SIZE as usize];
		let remainder_buf = vec![0; byte_remainder as usize];

		for _ in 0..full_blocks {
			file.write_all(&full_buf)?;
		}

		if byte_remainder > 0 {
			file.write_all(&remainder_buf)?;
		}

		file.set_len(0)?;
		mem::drop(file);
		fs::remove_file(path)?;

	} else if path_buf.is_dir() {
		if allow_dirs {
			annihilate_recursive(path)?; // scary
		} else {
			return Err(
				ShErr::simple(
					ShErrKind::ExecFail,
					format!("zoltraak: '{path}' is a directory")
				)
				.with_note(
					Note::new("Use the '-r' flag to recursively shred directories")
					.with_sub_notes(vec![
						"Example: 'zoltraak -r directory'"
					])
				)
			)
		}
	}

	Ok(())
}

fn annihilate_recursive(dir: &str) -> ShResult<()> {
	let dir_path = PathBuf::from(dir);

	for dir_entry in fs::read_dir(&dir_path)? {
		let entry = dir_entry?.path();
		let file = entry.to_str().unwrap();

		if entry.is_file() {
			annihilate(file, true)?;
		} else if entry.is_dir() {
			annihilate_recursive(file)?;
		}
	}
	fs::remove_dir(dir)?;
	Ok(())
}
