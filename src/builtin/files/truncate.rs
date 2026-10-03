use std::{
  io::{self, ErrorKind as EK},
  path::Path,
};

use nix::{libc, unistd};

use crate::{
  builtin::BuiltinArgs,
  sherr,
  util::{
    self,
    error::{ShResult, ShResultExt},
    strops,
  },
};

pub(super) struct Truncate;
impl super::super::Builtin for Truncate {
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut arguments = args.arguments().peekable();
    let mut status = 0;

    let Some((size, s_span)) = arguments.next() else {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing size argument for `fs truncate`").with_code(2),
      );
    };

    let bytes = strops::parse_size(&size.to_str_lossy())
      .promote_err(s_span)
      .with_code(2)?;
    let Ok(size) = libc::off_t::try_from(bytes) else {
      return Err(sherr!(ExecFail @ s_span, "size `{size}` is too large to truncate").with_code(2));
    };

    if arguments.peek().is_none() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing file argument for `fs truncate`").with_code(2),
      );
    }

    for (file, f_span) in arguments {
      let path: &Path = file.as_ref();
      if let Err(e) = unistd::truncate(path, size) {
        let io_err = io::Error::from_raw_os_error(e as i32);
        let err = match io_err.kind() {
          EK::NotFound => {
            sherr!(ExecFail @ f_span, "cannot truncate `{file}`: file does not exist")
          }
          EK::IsADirectory => sherr!(ExecFail @ f_span, "cannot truncate `{file}`: is a directory"),
          EK::PermissionDenied => {
            sherr!(ExecFail @ f_span, "cannot truncate `{file}`: permission denied")
          }
          EK::ReadOnlyFilesystem => {
            sherr!(ExecFail @ f_span, "cannot truncate `{file}`: read-only filesystem")
          }
          EK::FileTooLarge => sherr!(ExecFail @ f_span, "cannot truncate `{file}`: file too large"),
          EK::InvalidInput => {
            sherr!(ExecFail @ f_span, "cannot truncate `{file}`: not a regular file")
              .with_note("FIFOs, sockets, char devices, etc. cannot be truncated".into())
          }

          _ => sherr!(ExecFail @ f_span, "cannot truncate `{file}`: {io_err}"),
        };

        err.print_error();
        status = 1;
      }
    }

    util::with_status(status)
  }
}
