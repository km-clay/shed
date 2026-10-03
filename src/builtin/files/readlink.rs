use std::{os::unix::ffi::OsStrExt, path::PathBuf};

use crate::{
  builtin::BuiltinArgs,
  procio, sherr,
  util::{self, error::ShResult},
};

use super::super::Builtin;

pub(super) struct ReadLink;
impl Builtin for ReadLink {
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut arguments = args.arguments().peekable();

    let Some((target, t_span)) = arguments.next() else {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing arguments for `fs readlink`").with_code(2),
      );
    };
    if arguments.peek().is_some() {
      let (_, rest) = super::super::argv::join_raw_arg_iter(arguments);
      return Err(sherr!(ExecFail @ rest, "too many arguments for `fs readlink`").with_code(2));
    }

    let link = PathBuf::from(target);

    match std::fs::read_link(link) {
      Ok(path) => {
        let path_bytes = path.as_os_str().as_bytes();
        procio::outln_bytes(path_bytes);

        util::with_status(0)
      }
      Err(e) => {
        let err = match e.kind() {
          std::io::ErrorKind::InvalidInput => {
            sherr!(ExecFail @ t_span, "cannot readlink `{target}`: not a symbolic link")
          }
          std::io::ErrorKind::NotFound => {
            sherr!(ExecFail @ t_span, "cannot readlink `{target}`: file does not exist")
          }
          std::io::ErrorKind::PermissionDenied => {
            sherr!(ExecFail @ t_span, "cannot readlink `{target}`: permission denied")
          }
          _ => sherr!(ExecFail @ t_span, "cannot readlink `{target}`: {e}"),
        };
        Err(err.with_code(1))
      }
    }
  }
}
