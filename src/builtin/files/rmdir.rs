use std::io;

use crate::{
  sherr,
  util::{
    self,
    error::{ShErr, ShResult},
  },
};

use super::super::{Builtin, BuiltinArgs};

pub(super) struct RmDir;
impl Builtin for RmDir {
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut status = 0;

    if args.no_arguments() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing argument for `fs rmdir`").with_code(2),
      );
    }

    for (path, span) in args.arguments() {
      let Err(e) = std::fs::remove_dir(path) else {
        continue;
      };
      let err = match e.kind() {
        io::ErrorKind::NotFound => {
          sherr!(ExecFail @ span, "cannot remove directory `{path}`: no such file or directory")
        }
        io::ErrorKind::PermissionDenied => {
          sherr!(ExecFail @ span, "cannot remove directory `{path}`: permission denied")
        }
        io::ErrorKind::DirectoryNotEmpty => {
          sherr!(ExecFail @ span, "cannot remove directory `{path}`: directory not empty")
        }
        io::ErrorKind::NotADirectory => {
          sherr!(ExecFail @ span, "cannot remove directory `{path}`: not a directory")
        }
        io::ErrorKind::ResourceBusy => {
          sherr!(ExecFail @ span, "cannot remove directory `{path}`: directory is busy")
        }
        io::ErrorKind::InvalidInput => {
          sherr!(ExecFail @ span, "cannot remove directory `{path}`: invalid path")
        }
        io::ErrorKind::ReadOnlyFilesystem => {
          sherr!(ExecFail @ span, "cannot remove directory `{path}`: read-only filesystem")
        }
        io::ErrorKind::InvalidFilename => {
          sherr!(ExecFail @ span, "cannot remove directory `{path}`: invalid filename")
        }
        _ => ShErr::from(e).promote(span),
      };

      err.print_error();
      status = 1;
    }

    util::with_status(status)
  }
}
