//! Posix extensions

use nix::errno::Errno;
use nix::unistd::execve;
use std::convert::Infallible;
use std::ffi::{CStr, CString};

use crate::eval::execute::reexec_as_script;
use crate::state::Shed;

pub(crate) fn execvpe(
  filename: &CStr,
  args: &[CString],
  env: &[CString],
) -> nix::Result<Infallible> {
  // for nix::unistd::execve
  let mut envp = env.to_vec();

  let mut is_denied = false;

  if filename.to_bytes().contains(&b'/') {
    let path_str = filename.to_string_lossy();
    let path_bytes = path_str.as_bytes();
    envp.retain(|e| !e.as_bytes().starts_with(b"_="));
    envp.push(unsafe { CString::from_vec_unchecked([b"_=", path_bytes].concat()) });

    let Err(e) = execve(filename, args, &envp);
    let Errno::ENOEXEC = e else {
      return Err(e);
    };
    return reexec_as_script(filename, args, &envp);
  }

  let path = Shed::vars(|v| v.get_var("PATH"));
  for dir in std::env::split_paths(&path) {
    let full_path_str = dir.join(filename.to_str().unwrap());

    let path_bytes = full_path_str.to_str().unwrap_or_default().as_bytes();
    envp.retain(|e| !e.as_bytes().starts_with(b"_="));
    envp.push(unsafe { CString::from_vec_unchecked([b"_=", path_bytes].concat()) });

    let c_path = std::ffi::CString::new(full_path_str.to_str().unwrap()).unwrap();
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
