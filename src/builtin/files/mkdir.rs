use std::{
  fs::Permissions,
  io::{self, ErrorKind as EK},
  os::unix::fs::PermissionsExt,
  path::Path,
};

use nix::{
  libc,
  sys::stat::{self, Mode},
  unistd,
};

use crate::{
  opt, sherr,
  state::vars::VarStr,
  util::{self, error::ShResultExt, strops},
};

use super::{
  super::{Builtin, BuiltinArgs, ShResult, opt::OptSpec},
  chmod,
};

#[expect(clippy::needless_pass_by_value)]
fn get_mode(mode: VarStr) -> ShResult<stat::mode_t> {
  let spec = mode.to_str_lossy();
  let digits = spec.strip_prefix("0o").unwrap_or(&spec);
  let octal = u32::from_str_radix(digits, 8).ok().filter(|&n| n <= 0o7777);

  let mode = match octal {
    Some(octal) => octal,
    None if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
      return Err(
        sherr!(ExecFail, "invalid mode: `{spec}`")
          .with_note("octal mode must be in the range 0o0000 to 0o7777".into())
          .with_code(2),
      );
    }
    None => {
      let clauses = strops::parse_mode_clauses(&spec).with_code(2)?;
      strops::apply_mode_clauses(0o777, &clauses)
    }
  };

  Ok(mode as stat::mode_t)
}

pub(super) struct MkDir;
#[rustfmt::skip]
impl Builtin for MkDir {
  fn strict_opts(&self) -> bool { true }

  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("mode" | b'm', 1)
    ]
  }
  #[allow(clippy::useless_conversion)]
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    if args.no_arguments() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing argument for `fs mkdir`").with_code(2),
      );
    }

    let mut status = 0;
    let arguments = args.arguments();

    let mode_specified = args.has_opt("mode");
    let mode: stat::mode_t = args.opt_value("mode")
      .map(get_mode)
      .transpose()
      .option_promote(args.opt_span("mode"))?
      .unwrap_or(0o777);

    for (dir, d_span) in arguments {
      let path: &Path = dir.as_ref();

      if let Err(e) = unistd::mkdir(path, Mode::from_bits_truncate(mode)) {
        let io_err = io::Error::from_raw_os_error(e as i32);

        let err = match io_err.kind() {
          EK::AlreadyExists      => sherr!(ExecFail @ d_span, "cannot make directory `{dir}`: name already exists"),
          EK::PermissionDenied   => sherr!(ExecFail @ d_span, "cannot make directory `{dir}`: permission denied"),
          EK::InvalidFilename    => sherr!(ExecFail @ d_span, "cannot make directory `{dir}`: name too long"),
          EK::ReadOnlyFilesystem => sherr!(ExecFail @ d_span, "cannot make directory `{dir}`: read-only filesystem"),
          EK::StorageFull        => sherr!(ExecFail @ d_span, "cannot make directory `{dir}`: no space left on device"),
          EK::NotADirectory      => sherr!(ExecFail @ d_span, "cannot make directory `{dir}`: path component is not a directory"),
          EK::NotFound           => sherr!(ExecFail @ d_span, "cannot make directory `{dir}`: parent directory does not exist")
            .with_note("`fs mkdir` does not create parent directories; create the parents first".into()),

          _ => if io_err.raw_os_error() == Some(libc::ELOOP) {
            sherr!(ExecFail @ d_span, "cannot make directory `{dir}`: too many levels of symbolic links")
          } else {
            sherr!(ExecFail @ d_span, "cannot make directory `{dir}`: {io_err}")
          }
        };

        err.print_error();
        status = 1;
        continue
      }

      if mode_specified && let Err(e) = std::fs::set_permissions(path, Permissions::from_mode(mode.into())) {
        let err = chmod::handle_err(e, dir)
          .with_note("the directory was created; only its mode could not be set".into())
          .promote(d_span);

        err.print_error();
        status = 1;
      }
    }

    util::with_status(status)
  }
}
