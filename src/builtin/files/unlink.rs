use std::{
  io::{self, ErrorKind as EK},
  path::Path,
};

use nix::{libc, unistd};

use crate::{
  sherr,
  util::{self, error::ShResult},
};

use super::super::BuiltinArgs;

pub(super) struct Unlink;
impl super::super::Builtin for Unlink {
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    if args.no_arguments() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing argument for `fs unlink`").with_code(2),
      );
    }

    let mut status = 0;

    for link in args.arguments() {
      let path: &Path = link.as_ref();
      if let Err(e) = unistd::unlink(path) {
        let io_err = io::Error::from_raw_os_error(e as i32);

        let err = match io_err.kind() {
          EK::IsADirectory => sherr!(ExecFail, "cannot unlink `{link}`: is a directory"),
          EK::NotFound     => sherr!(ExecFail, "cannot unlink `{link}`: no such file or directory"),
          EK::NotADirectory => {
            sherr!(ExecFail, "cannot unlink `{link}`: path component is not a directory")
          }
          EK::ReadOnlyFilesystem => {
            sherr!(ExecFail, "cannot unlink `{link}`: read-only file system")
          }

          EK::PermissionDenied => match io_err.raw_os_error() {
            Some(libc::EACCES) => {
              sherr!(ExecFail, "cannot unlink `{link}`: unsearchable directory in path")
            }
            Some(libc::EPERM) => {
              sherr!(ExecFail, "cannot unlink `{link}`: operation not permitted")
            }
            _ => sherr!(ExecFail, "cannot unlink `{link}`: {io_err}"),
          },

          _ => {
            if io_err.raw_os_error() == Some(libc::ELOOP) {
              sherr!(ExecFail, "failed to unlink `{link}`: too many levels of symbolic links")
            } else {
              sherr!(ExecFail, "failed to unlink `{link}`: {io_err}")
            }
          }
        };

        err.option_promote(link.span()).print_error();
        status = 1;
      }
    }

    util::with_status(status)
  }
}
