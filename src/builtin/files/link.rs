use std::io::ErrorKind as EK;

use nix::libc;

use crate::{
  sherr,
  util::{self, error::ShResult},
};

use super::super::{Builtin, BuiltinArgs};

pub(super) struct Link;
impl Builtin for Link {
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut arguments = args.arguments().peekable();
    let Some((target, t_span)) = arguments.next() else {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing arguments for `fs link`").with_code(2),
      );
    };

    if arguments.peek().is_none() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing link name for `fs link`").with_code(2),
      );
    }

    let mut status = 0;

    for (link, l_span) in arguments {
      let Err(e) = std::fs::hard_link(target, link) else {
        continue;
      };
      #[rustfmt::skip]
      let err = match e.kind() {
        EK::AlreadyExists      => sherr!(ExecFail @ l_span, "cannot create link `{link}`: file exists"                      ),
        EK::CrossesDevices     => sherr!(ExecFail @ l_span, "cannot create link `{link}`: cross-device link"                ),
        EK::NotFound           => sherr!(ExecFail @ t_span, "cannot create link `{link}`: target `{target}` does not exist" ),
        EK::TooManyLinks       => sherr!(ExecFail @ l_span, "cannot create link `{link}`: too many links"                   ),
        EK::StorageFull        => sherr!(ExecFail @ l_span, "cannot create link `{link}`: storage full"                     ),
        EK::ReadOnlyFilesystem => sherr!(ExecFail @ l_span, "cannot create link `{link}`: read-only filesystem"             ),
        EK::InvalidFilename    => sherr!(ExecFail @ l_span, "cannot create link `{link}`: invalid filename"                 ),
        EK::NotADirectory      => sherr!(ExecFail @ l_span, "cannot create link `{link}`: path component is not a directory"),

        EK::PermissionDenied | EK::IsADirectory => match e.raw_os_error() {
          Some(libc::EPERM)    => sherr!(ExecFail @ l_span, "cannot create link `{link}`: is a directory"                   ),
          Some(libc::EACCES)   => sherr!(ExecFail @ l_span, "cannot create link `{link}`: permission denied"                ),
          _                    => sherr!(ExecFail @ l_span, "cannot create link `{link}`: {e}"                              ),
        },
        _                      => sherr!(ExecFail @ l_span, "cannot create link `{link}`: {e}"                              ),
      };

      err.print_error();
      status = 1;
    }

    util::with_status(status)
  }
}
