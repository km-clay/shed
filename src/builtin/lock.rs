use std::os::fd::AsRawFd;

use nix::libc;

use crate::{
  opt, sherr, signal,
  state::Shed,
  util::{self, error::ShResult},
};

use super::opt::OptSpec;

pub(super) struct Lock;
impl super::Builtin for Lock {
  #[rustfmt::skip]
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("nonblock" | b'n'),
      opt!("shared"   | b's'),
      opt!("unlock"   | b'u'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut operands = args.arguments();

    let Some((arg, span)) = operands.next() else {
      return Err(sherr!(ExecFail @ args.cmd_span(), "missing file descriptor").with_code(2));
    };

    let (operands, rest) = super::argv::join_raw_arg_iter(operands);
    if !operands.is_empty() {
      return Err(sherr!(ExecFail @ rest, "expected exactly one file descriptor").with_code(2));
    }

    let Some(fd) = arg.parse::<i32>() else {
      let mut err = sherr!(ExecFail @ span, "invalid file descriptor `{arg}`").with_code(2);
      if arg.contains(&b'/') {
        err = err.with_note("lock expects a file descriptor, not a path".into());
      }
      return Err(err);
    };
    let Some(sink) = Shed::sinks(|s| s.get(fd)) else {
      return Err(sherr!(ExecFail @ span, "file descriptor `{fd}` not open").with_code(1));
    };
    let Ok(raw) = sink.as_os_fd().map(|f| f.as_raw_fd()) else {
      return Err(
        sherr!(ExecFail @ span, "file descriptor `{fd}` is not a valid OS file descriptor")
          .with_code(1),
      );
    };

    let nonblock = if args.has_opt("nonblock") {
      libc::LOCK_NB
    } else {
      0
    };
    let op = match (args.has_opt("unlock"), args.has_opt("shared")) {
      (true, _) => libc::LOCK_UN,
      (false, true) => libc::LOCK_SH | nonblock,
      (false, false) => libc::LOCK_EX | nonblock,
    };

    loop {
      if unsafe { libc::flock(raw, op) } == 0 {
        return util::with_status(0);
      }

      let err = std::io::Error::last_os_error();
      match err.raw_os_error() {
        Some(libc::EINTR) => signal::check_signals()?,
        // contention under `-n`, not a failure
        Some(libc::EWOULDBLOCK) => return util::with_status(1),
        _ => return Err(sherr!(ExecFail @ span, "fd {fd}: {err}").with_code(1)),
      }
    }
  }
}
