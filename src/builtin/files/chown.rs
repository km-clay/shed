use std::{io, os::unix::fs, path::Path};

use bstr::ByteSlice;
use nix::{
  libc,
  unistd::{Gid, Group, Uid, User},
};

use crate::{
  builtin::opt::OptSpec,
  opt, sherr,
  util::{
    self,
    error::{ShErr, ShResult, ShResultExt},
  },
};

use super::super::{Builtin, BuiltinArgs};

fn split_owner(spec: &[u8]) -> (Option<&[u8]>, Option<&[u8]>) {
  let Some(i) = spec.iter().position(|&b| b == b':') else {
    return (Some(spec), None);
  };
  let (user, group) = (&spec[..i], &spec[i + 1..]);
  (
    (!user.is_empty()).then_some(user),
    (!group.is_empty()).then_some(group),
  )
}

fn resolve_uid(name: &str) -> ShResult<Uid> {
  match User::from_name(name) {
    Ok(Some(user)) => Ok(user.uid),
    Ok(None) => name
      .parse::<libc::uid_t>()
      .map(Uid::from_raw)
      .map_err(|_| sherr!(ExecFail, "no such user `{name}`").with_code(2)),
    Err(e) => Err(sherr!(ExecFail, "failed to resolve user `{name}`: {e}").with_code(2)),
  }
}

fn resolve_gid(name: &str) -> ShResult<Gid> {
  match Group::from_name(name) {
    Ok(Some(group)) => Ok(group.gid),
    Ok(None) => name
      .parse::<libc::gid_t>()
      .map(Gid::from_raw)
      .map_err(|_| sherr!(ExecFail, "no such group `{name}`").with_code(2)),
    Err(e) => Err(sherr!(ExecFail, "failed to resolve group `{name}`: {e}").with_code(2)),
  }
}

pub(super) struct ChOwn;
impl Builtin for ChOwn {
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("no-deref" | b'h')]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut status = 0;
    let mut arguments = args.arguments().peekable();
    let no_deref = args.has_opt("no-deref");

    let Some((owner, o_span)) = arguments.next() else {
      return Err(sherr!(ExecFail @ args.cmd_span(), "missing owner for `fs chown`").with_code(2));
    };

    if arguments.peek().is_none() {
      return Err(sherr!(ExecFail @ args.cmd_span(), "missing file for `fs chown`").with_code(2));
    }

    let (user, group) = split_owner(owner);

    if let (None, None) = (user, group) {
      return Err(sherr!(ExecFail @ o_span, "no user or group given for `fs chown`").with_code(2));
    }

    let u_span = user.map(|u| o_span.sub_span(|s, _| (s, s + u.len())));
    let g_span = group.map(|g| o_span.sub_span(|s, e| (s + owner.len() - g.len(), e)));

    let uid = user
      .map(|u| resolve_uid(&u.to_str_lossy()))
      .transpose()
      .option_promote(u_span)?
      .map(Uid::as_raw);

    let gid = group
      .map(|g| resolve_gid(&g.to_str_lossy()))
      .transpose()
      .option_promote(g_span)?
      .map(Gid::as_raw);

    for (file, f_span) in arguments {
      let path: &Path = file.as_ref();
      let res = if no_deref {
        fs::lchown(path, uid, gid)
      } else {
        fs::chown(path, uid, gid)
      };

      if let Err(e) = res {
        let err = match e.kind() {
          io::ErrorKind::NotFound => {
            sherr!(ExecFail @ f_span, "cannot change owner of `{file}`: no such file or directory")
          }
          io::ErrorKind::InvalidFilename => {
            sherr!(ExecFail @ f_span, "cannot change owner of `{file}`: invalid filename")
          }
          io::ErrorKind::NotADirectory => {
            sherr!(ExecFail @ f_span, "cannot change owner of `{file}`: path component is not a directory")
          }
          io::ErrorKind::ReadOnlyFilesystem => {
            sherr!(ExecFail @ f_span, "cannot change owner of `{file}`: read-only filesystem")
          }
          io::ErrorKind::PermissionDenied => match e.raw_os_error() {
            Some(libc::EPERM) => {
              sherr!(ExecFail @ f_span, "cannot change owner of `{file}`: operation not permitted")
                .with_note(
                  "only the superuser can change a file's user; a group must be one you belong to"
                    .into(),
                )
            }
            _ => sherr!(ExecFail @ f_span, "cannot change owner of `{file}`: permission denied"),
          },
          _ if e.raw_os_error() == Some(libc::ELOOP) => {
            sherr!(ExecFail @ f_span, "cannot change owner of `{file}`: too many levels of symbolic links")
          }
          _ => ShErr::from(e).promote(f_span),
        };

        err.print_error();
        status = 1;
      }
    }

    util::with_status(status)
  }
}
