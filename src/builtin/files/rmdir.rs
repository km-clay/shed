use std::io::ErrorKind as EK;

use crate::{
  sherr,
  util::{self, error::ShResult},
};

use super::super::{Builtin, BuiltinArgs};

pub(super) struct RmDir;
impl Builtin for RmDir {
  fn strict_opts(&self) -> bool {
    true
  }
  #[rustfmt::skip]
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
        EK::NotFound           => sherr!(ExecFail @ span, "cannot remove directory `{path}`: no such file or directory"),
        EK::PermissionDenied   => sherr!(ExecFail @ span, "cannot remove directory `{path}`: permission denied"),
        EK::DirectoryNotEmpty  => sherr!(ExecFail @ span, "cannot remove directory `{path}`: directory not empty"),
        EK::NotADirectory      => sherr!(ExecFail @ span, "cannot remove directory `{path}`: not a directory"),
        EK::ResourceBusy       => sherr!(ExecFail @ span, "cannot remove directory `{path}`: directory is busy"),
        EK::InvalidInput       => sherr!(ExecFail @ span, "cannot remove directory `{path}`: invalid path"),
        EK::ReadOnlyFilesystem => sherr!(ExecFail @ span, "cannot remove directory `{path}`: read-only filesystem"),
        EK::InvalidFilename    => sherr!(ExecFail @ span, "cannot remove directory `{path}`: invalid filename"),
        _                      => sherr!(ExecFail @ span, "cannot remove directory `{path}`: {e}"),
      };

      err.print_error();
      status = 1;
    }

    util::with_status(status)
  }
}
