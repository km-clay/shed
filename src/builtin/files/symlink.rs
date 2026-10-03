use std::{io, os::unix};

use crate::{
  sherr,
  util::{
    self,
    error::{ShErr, ShResult},
  },
};

use super::super::{Builtin, BuiltinArgs};

pub(super) struct SymLink;
impl Builtin for SymLink {
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut arguments = args.arguments().peekable();
    let Some((target, _)) = arguments.next() else {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing arguments for `fs symlink`").with_code(2),
      );
    };

    if arguments.peek().is_none() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing link name for `fs symlink`").with_code(2),
      );
    }

    let mut status = 0;

    for (link, l_span) in arguments {
      let Err(e) = unix::fs::symlink(target, link) else {
        continue;
      };
      let err = match e.kind() {
        io::ErrorKind::AlreadyExists => {
          sherr!(ExecFail @ l_span, "cannot create symlink `{link}`: file exists")
        }
        io::ErrorKind::NotADirectory => {
          sherr!(ExecFail @ l_span, "cannot create symlink `{link}`: path component is not a directory")
        }
        io::ErrorKind::ReadOnlyFilesystem => {
          sherr!(ExecFail @ l_span, "cannot create symlink `{link}`: read-only filesystem")
        }
        io::ErrorKind::InvalidFilename => {
          sherr!(ExecFail @ l_span, "cannot create symlink `{link}`: invalid filename")
        }
        io::ErrorKind::StorageFull => {
          sherr!(ExecFail @ l_span, "cannot create symlink `{link}`: storage full")
        }
        io::ErrorKind::PermissionDenied => {
          sherr!(ExecFail @ l_span, "cannot create symlink `{link}`: permission denied")
        }
        _ => ShErr::from(e).promote(l_span),
      };

      err.print_error();
      status = 1;
    }

    util::with_status(status)
  }
}
