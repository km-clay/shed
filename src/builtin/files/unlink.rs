use std::path::Path;

use nix::unistd;

use crate::{
  sherr,
  util::{
    self,
    error::{ShErr, ShResult},
  },
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

    for (link, span) in args.arguments() {
      let path: &Path = link.as_ref();
      if let Err(e) = unistd::unlink(path) {
        ShErr::from(e).promote(span).print_error();
        status = 1;
      }
    }

    util::with_status(status)
  }
}
