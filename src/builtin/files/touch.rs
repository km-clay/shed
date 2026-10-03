use std::{
  io::{self, ErrorKind as EKind},
  path::Path,
};

use chrono::{DateTime, Utc};
use nix::{
  fcntl, libc,
  sys::{
    stat::{self, UtimensatFlags},
    time::TimeSpec,
  },
};

use crate::{
  opt, sherr,
  util::{
    self,
    error::{ShErr, ShResultExt},
    strops,
  },
};

use super::super::{Builtin, BuiltinArgs, ShResult, opt::OptSpec};

pub(super) struct Touch;
#[rustfmt::skip]
impl Builtin for Touch {
  fn strict_opts(&self) -> bool { true }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("mtime"    | b'm'   ),
      opt!("atime"    | b'a'   ),
      opt!("no-deref" | b'h'   ),
      opt!("time"     | b't', 1)
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    if args.no_arguments() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing file operand for `fs touch`")
          .with_code(2)
      );
    }

    let mut status = 0;
    let arguments  = args.arguments();

    let mtime     = args.has_opt("mtime");
    let atime     = args.has_opt("atime");
    let no_deref  = args.has_opt("no-deref");
    let time      = args.opt_value("time").map(|t| {
      strops::TimeReader::interpret(&t.to_str_lossy())
        .option_promote(args.opt_span("time"))
        .promote_err(args.cmd_span())
        .with_code(2)
    }).transpose()?;

    let sys_timespec = |sys: Option<DateTime<Utc>>| {
      let Some(sys) = sys else {
        return TimeSpec::UTIME_NOW;
      };
      TimeSpec::new(sys.timestamp(), sys.timestamp_subsec_nanos().into())
    };

    let mut atime_spec = TimeSpec::UTIME_OMIT;
    let mut mtime_spec = TimeSpec::UTIME_OMIT;

    if atime || !mtime { atime_spec = sys_timespec(time); }
    if mtime || !atime { mtime_spec = sys_timespec(time); }

    let flags = if no_deref {
      UtimensatFlags::NoFollowSymlink
    } else {
      UtimensatFlags::FollowSymlink
    };

    for (file, f_span) in arguments {
      let path: &Path = file.as_ref();

      let res = stat::utimensat(fcntl::AT_FDCWD, path, &atime_spec, &mtime_spec, flags)
        .map_err(|e| io::Error::from_raw_os_error(e as i32));

      let Err(e) = res else { continue };

      let err = match e.kind() {
        EKind::NotFound => {
          sherr!(ExecFail @ f_span, "cannot touch `{file}`: no such file or directory")
        },
        EKind::NotADirectory => {
          sherr!(ExecFail @ f_span, "cannot touch `{file}`: a component of the path is not a directory")
        },
        EKind::ReadOnlyFilesystem => {
          sherr!(ExecFail @ f_span, "cannot touch `{file}`: read-only filesystem")
        },
        EKind::PermissionDenied => {
          let note = match (e.raw_os_error(), args.has_opt("time")) {
            (Some(libc::EPERM), _) => "setting a specific time requires owning the file",
            (_, false) => "setting times to now requires write permission on the file",
            (_, true) => "a component of the path could not be searched",
          };
          sherr!(ExecFail @ f_span, "cannot touch `{file}`: permission denied").with_note(note.into())
        }
        _ if e.raw_os_error() == Some(libc::ELOOP) => {
          sherr!(ExecFail @ f_span, "cannot touch `{file}`: too many symbolic links encountered")
        },
        _ => ShErr::from(e).promote(f_span),
      };

      err.print_error();
      status = 1;
    }

    util::with_status(status)
  }
}
