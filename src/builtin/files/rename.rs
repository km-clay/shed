use std::io;

use crate::{
  sherr,
  util::{
    self,
    error::{ShResult, ShResultExt},
  },
};

use super::super::{Builtin, BuiltinArgs, argv};

pub(super) struct Rename;
impl Builtin for Rename {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut paths = args.arguments();

    let Some((from, f_span)) = paths.next() else {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing source path for `fs rename`").with_code(2),
      );
    };

    let Some((to, t_span)) = paths.next() else {
      return Err(sherr!(ExecFail @ f_span, "missing destination path for `{from}`").with_code(2));
    };

    let (surplus, rest) = argv::join_raw_arg_iter(paths);
    if !surplus.is_empty() {
      return Err(sherr!(ExecFail @ rest, "expected exactly two paths").with_code(2));
    }

    match std::fs::rename(from, to) {
      Ok(()) => util::with_status(0),
      Err(e) => match e.kind() {
        io::ErrorKind::NotFound => {
          Err(sherr!(ExecFail @ f_span, "source path `{from}` does not exist"))
        }
        io::ErrorKind::IsADirectory => {
          Err(sherr!(ExecFail @ t_span, "cannot rename file `{from}` to directory `{to}`"))
        }
        io::ErrorKind::DirectoryNotEmpty => Err(
          sherr!(ExecFail @ t_span, "cannot rename directory `{from}` to non-empty directory `{to}`"),
        ),
        io::ErrorKind::NotADirectory => {
          Err(sherr!(ExecFail @ t_span, "cannot rename `{from}` to `{to}`: not a directory"))
        }
        io::ErrorKind::CrossesDevices => Err(
          sherr!(ExecFail @ t_span, "cannot rename `{from}` to `{to}`: different filesystems")
            .with_note("rename cannot cross filesystems; use `mv` to copy instead".into()),
        ),
        _ => Err(e.into()).promote_err(f_span),
      },
    }
  }
}
