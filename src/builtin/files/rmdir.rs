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
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut status = 0;

    if args.no_arguments() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing argument for `fs rmdir`").with_code(2),
      );
    }

    for (path, span) in args.arguments() {
      if let Err(e) = std::fs::remove_dir(path) {
        status = 1;
        ShErr::from(e).promote(span).print_error();
      }
    }

    util::with_status(status)
  }
}
