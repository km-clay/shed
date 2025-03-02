use std::fmt::Display;

use crate::prelude::*;

pub const SIG_EXIT_OFFSET: i32 = 128;

pub fn get_bin_path(command: &str, shenv: &ShEnv) -> Option<PathBuf> {
	let env = shenv.vars().env();
	let path_var = env.get("PATH")?;
	let mut paths = path_var.split(':');
	while let Some(raw_path) = paths.next() {
		let mut path = PathBuf::from(raw_path);
		path.push(command);
		//TODO: handle this unwrap
		if path.exists() {
			return Some(path)
		}
	}
	None
}

pub fn write_out(text: impl Display) -> ShResult<()> {
	write(borrow_fd(1), text.to_string().as_bytes())?;
	Ok(())
}

pub fn write_err(text: impl Display) -> ShResult<()> {
	write(borrow_fd(2), text.to_string().as_bytes())?;
	Ok(())
}

/// Return is `readpipe`, `writepipe`
/// Contains all of the necessary boilerplate for grabbing two pipe fds using libc::pipe()
pub fn c_pipe() -> Result<(RawFd,RawFd),Errno> {
	let mut pipes: [i32;2] = [0;2];
	let ret = unsafe { libc::pipe(pipes.as_mut_ptr()) };
	if ret < 0 {
		return Err(Errno::from_raw(ret))
	}
	Ok((pipes[0],pipes[1]))
}

pub fn execvpe(cmd: String, argv: Vec<String>, envp: Vec<String>) -> Result<(),Errno> {
	let cmd_raw = CString::new(cmd).unwrap();

	let argv = argv.into_iter().map(|arg| CString::new(arg).unwrap()).collect::<Vec<CString>>();
	let envp = envp.into_iter().map(|var| CString::new(var).unwrap()).collect::<Vec<CString>>();

	nix::unistd::execvpe(&cmd_raw, &argv, &envp).unwrap();
	Ok(())
}
