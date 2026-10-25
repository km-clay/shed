use std::io::ErrorKind as EK;

use crate::{
  sherr,
  util::{self, error::ShResult},
};

use super::super::{Builtin, BuiltinArgs, argv};

pub(super) struct Rename;
impl Builtin for Rename {
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut paths = args.arguments();

    let Some(from) = paths.next() else {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing source path for `fs rename`").with_code(2),
      );
    };

    let Some(to) = paths.next() else {
      return Err(
        sherr!(ExecFail ? from.span(), "missing destination path for `{from}`").with_code(2),
      );
    };

    let surplus = argv::join_raw_arg_iter(paths);
    if !surplus.is_empty() {
      return Err(sherr!(ExecFail ? surplus.span(), "expected exactly two paths").with_code(2));
    }

    match std::fs::rename(from, to) {
      Ok(()) => util::with_status(0),
      Err(e) => match e.kind() {
        EK::NotFound      => Err(sherr!(ExecFail ? from.span(), "source path `{from}` does not exist")),
        EK::IsADirectory  => Err(sherr!(ExecFail ? to.span(), "cannot rename file `{from}` to directory `{to}`")),
        EK::NotADirectory => Err(sherr!(ExecFail ? to.span(), "cannot rename `{from}` to `{to}`: not a directory")),
        EK::DirectoryNotEmpty => {
          Err(sherr!(ExecFail ? to.span(), "cannot rename directory `{from}` to non-empty directory `{to}`"))
        }

        EK::CrossesDevices => Err(
          sherr!(ExecFail ? to.span(), "cannot rename `{from}` to `{to}`: different filesystems")
            .with_note("rename cannot cross filesystems; use `mv` to copy instead".into()),
        ),
        _ => Err(sherr!(ExecFail ? to.span(), "cannot rename `{from}` to `{to}`: {e}")),
      },
    }
  }
}
