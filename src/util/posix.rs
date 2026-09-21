//! Posix extensions

use nix::errno::Errno;
use nix::unistd::execve;
use std::convert::Infallible;
use std::ffi::{CStr, CString, OsStr};
use std::os::unix::ffi::OsStrExt;

use crate::eval::execute::reexec_as_script;
use crate::state::Shed;
use crate::state::vars::VarStr;

pub(crate) fn execvpe(
  filename: &CStr,
  args: &[CString],
  env: &[CString],
) -> nix::Result<Infallible> {
  // for nix::unistd::execve
  let mut envp = env.to_vec();

  let mut is_denied = false;

  if filename.to_bytes().contains(&b'/') {
    let path_bytes = filename.to_bytes();

    let path = VarStr::from([b"_=", path_bytes].concat());
    envp.push(path.to_cstring_lossy());

    let Err(e) = execve(filename, args, &envp);
    let Errno::ENOEXEC = e else {
      return Err(e);
    };
    return reexec_as_script(filename, args, &envp);
  }

  let path = Shed::vars(|v| v.get_var("PATH"));
  for dir in std::env::split_paths(&path) {
    let full_path = dir.join(OsStr::from_bytes(filename.to_bytes()));
    let full_path_str = VarStr::from(full_path);

    envp.retain(|e| !e.as_bytes().starts_with(b"_="));
    let path = VarStr::from([b"_=", full_path_str.as_bytes()].concat());
    envp.push(path.to_cstring_lossy());

    let c_path = full_path_str.to_cstring_lossy();
    let Err(e) = execve(c_path.as_c_str(), args, &envp);
    match e {
      Errno::ENOEXEC => {
        reexec_as_script(c_path.as_c_str(), args, &envp)?;
      }
      Errno::ENOENT | Errno::ENOTDIR => (), // Try next path
      Errno::EACCES => is_denied = true,    // Permission denied
      _ => return Err(e),                   // Other error
    }
  }

  // Not found
  if is_denied {
    Err(Errno::EACCES)
  } else {
    Err(Errno::ENOENT)
  }
}
