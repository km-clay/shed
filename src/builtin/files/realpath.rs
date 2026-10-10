use std::{io::ErrorKind as EK, os::unix::ffi::OsStrExt, path::PathBuf};

use nix::libc;

use crate::{
  builtin::opt::OptSpec,
  opt, procio, sherr,
  state::paths,
  util::{
    self,
    error::{ShErr, ShResult},
  },
};

use super::super::{Builtin, BuiltinArgs};

pub(super) struct RealPath;
impl Builtin for RealPath {
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("lenient" | b'm')]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let     lenient = args.has_opt("lenient");
    let mut status  = 0;

    if args.no_arguments() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing argument for `fs realpath`").with_code(2),
      );
    }

    let cwd = lenient
      .then(|| {
        std::env::current_dir().map_err(|e| ShErr::from(e).promote(args.cmd_span()).with_code(1))
      })
      .transpose()?;

    for (arg, span) in args.arguments() {
      let target = PathBuf::from(arg);

      let resolved = match std::fs::canonicalize(&target) {
        Ok(p) => p,
        Err(e)
          if let Some(cwd) = &cwd
            && e.kind() == EK::NotFound =>
        {
          paths::lex_normalize_path(&cwd.join(&target))
        }
        Err(e) => {
          let err = match e.kind() {
            EK::NotFound => {
              sherr!(ExecFail @ span, "cannot resolve `{arg}`: no such file or directory"        )
            }
            EK::PermissionDenied => {
              sherr!(ExecFail @ span, "cannot resolve `{arg}`: permission denied"                )
            }
            EK::NotADirectory => {
              sherr!(ExecFail @ span, "cannot resolve `{arg}`: path component is not a directory")
            }

            _ => {
              if let Some(libc::ELOOP) = e.raw_os_error() {
                sherr!(ExecFail @ span, "cannot resolve `{arg}`: too many levels of symbolic links")
              } else {
                sherr!(ExecFail @ span, "cannot resolve `{arg}`: {e}")
              }
            }
          };

          err.print_error();
          status = 1;
          continue;
        }
      };

      let resolved_bytes = resolved.as_os_str().as_bytes();
      procio::outln_bytes(resolved_bytes);
    }

    util::with_status(status)
  }
}
