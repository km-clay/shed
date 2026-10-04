use std::{io::ErrorKind as EK, os::unix};

use crate::{
  sherr,
  util::{self, error::ShResult},
  varstr,
};

use super::super::{Builtin, BuiltinArgs};

pub(super) struct SymLink;
#[rustfmt::skip]
impl Builtin for SymLink {
  fn strict_opts(&self) -> bool { true }
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

      let msg = varstr!("cannot create symlink `{link}`");
      let err = match e.kind() {
        EK::AlreadyExists      => sherr!(ExecFail @ l_span, "{msg}: file exists"                      ),
        EK::ReadOnlyFilesystem => sherr!(ExecFail @ l_span, "{msg}: read-only filesystem"             ),
        EK::InvalidFilename    => sherr!(ExecFail @ l_span, "{msg}: invalid filename"                 ),
        EK::StorageFull        => sherr!(ExecFail @ l_span, "{msg}: storage full"                     ),
        EK::PermissionDenied   => sherr!(ExecFail @ l_span, "{msg}: permission denied"                ),
        EK::NotADirectory      => sherr!(ExecFail @ l_span, "{msg}: path component is not a directory"),
        _                      => sherr!(ExecFail @ l_span, "{msg}: {e}"                              ),
      };

      err.print_error();
      status = 1;
    }

    util::with_status(status)
  }
}
