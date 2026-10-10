use std::{
  io::{self, ErrorKind as EK},
  os::unix::fs::PermissionsExt,
  path::Path,
};

use nix::libc;

use crate::{
  sherr,
  state::vars::VarStr,
  util::{
    self,
    error::{ShErr, ShResult, ShResultExt},
    strops,
  },
};

use super::super::{Builtin, BuiltinArgs};

pub(super) struct ChMod;
impl Builtin for ChMod {
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut status    = 0;
    let mut arguments = args.arguments().peekable();

    let Some((mode_s, m_span)) = arguments.next() else {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing mode operand for `fs chmod`").with_code(2),
      );
    };

    if arguments.peek().is_none() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing file operand for `fs chmod`").with_code(2),
      );
    }

    let spec   = mode_s.to_str_lossy();
    let digits = spec.strip_prefix("0o").unwrap_or(&spec);
    let octal  = u32::from_str_radix(digits, 8).ok().filter(|&n| n <= 0o7777);

    if octal.is_none() && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
      return Err(
        sherr!(ExecFail @ m_span, "invalid mode: `{spec}`")
          .with_note("octal mode must be in the range 0o0000 to 0o7777".into())
          .with_code(2),
      );
    }

    let clauses = octal
      .is_none()
      .then(|| strops::parse_mode_clauses(&spec))
      .transpose()
      .promote_err(m_span)
      .with_code(2)?;

    for (file, f_span) in arguments {
      let path: &Path = file.as_ref();

      let mode = match (octal, &clauses) {
        (Some(m), _) => m,
        (None, Some(clauses)) => {
          let meta = std::fs::metadata(path)
            .map_err(|e| handle_err(e, file))
            .promote_err(f_span);

          if let Err(e) = meta {
            e.print_error();
            status = 1;
            continue;
          }

          strops::apply_mode_clauses(meta?.permissions().mode() & 0o7777, clauses)
        }
        _ => unreachable!(),
      };

      let perms = std::fs::Permissions::from_mode(mode);
      let Err(e) = std::fs::set_permissions(path, perms) else {
        continue;
      };

      let err = handle_err(e, file).promote(f_span).with_code(1);

      err.print_error();
      status = 1;
    }

    util::with_status(status)
  }
}

pub(super) fn handle_err(err: io::Error, file: &VarStr) -> ShErr {
  match err.kind() {
    EK::NotFound => {
      sherr!(
        ExecFail,
        "cannot set mode of `{file}`: no such file or directory"
      )
    }
    EK::InvalidFilename => {
      sherr!(ExecFail, "cannot set mode of `{file}`: invalid filename")
    }
    EK::NotADirectory => {
      sherr!(
        ExecFail,
        "cannot set mode of `{file}`: path component is not a directory"
      )
    }
    EK::ReadOnlyFilesystem => {
      sherr!(
        ExecFail,
        "cannot set mode of `{file}`: read-only filesystem"
      )
    }
    EK::PermissionDenied => match err.raw_os_error() {
      Some(libc::EPERM) => sherr!(
        ExecFail,
        "cannot set mode of `{file}`: operation not permitted"
      )
      .with_note("only the file's owner or the superuser can change its mode".into()),
      _ => sherr!(ExecFail, "cannot set mode of `{file}`: permission denied"),
    },
    _ => {
      if err.raw_os_error() == Some(libc::ELOOP) {
        sherr!(
          ExecFail,
          "cannot set mode of `{file}`: too many levels of symbolic links"
        )
      } else {
        sherr!(ExecFail, "cannot set mode of `{file}`: {err}")
      }
    }
  }
}
