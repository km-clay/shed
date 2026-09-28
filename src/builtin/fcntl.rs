use std::os::fd::{AsRawFd, RawFd};

use bitflags::bitflags;
use nix::libc;

use crate::{
  builtin::opt::{self, Role},
  eval::{
    execute,
    lex::{Span, Tk},
  },
  out, outln, sherr,
  state::Shed,
  util::{
    self,
    error::{ShErr, ShResult, ShResultExt},
  },
};

use super::opt::Parsed;

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  struct FdFlags: u16 {
    const CLOEXEC  = 1 << 0;
    const NONBLOCK = 1 << 1;
    const APPEND   = 1 << 2;
  }
}

impl FdFlags {
  fn as_char(self) -> char {
    match self {
      _ if self == FdFlags::CLOEXEC => 'c',
      _ if self == FdFlags::NONBLOCK => 'n',
      _ if self == FdFlags::APPEND => 'a',
      _ => unreachable!(),
    }
  }
  fn role(ch: char) -> Role<Self> {
    match ch {
      'c' => Role::Set(FdFlags::CLOEXEC),
      'n' => Role::Set(FdFlags::NONBLOCK),
      'a' => Role::Set(FdFlags::APPEND),
      _ => Role::Unknown,
    }
  }
  fn get_set_bit(self) -> Option<(libc::c_int, libc::c_int, libc::c_int)> {
    match self {
      _ if self == FdFlags::CLOEXEC => Some((libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC)),
      _ if self == FdFlags::NONBLOCK => Some((libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK)),
      _ if self == FdFlags::APPEND => Some((libc::F_GETFL, libc::F_SETFL, libc::O_APPEND)),
      _ => None,
    }
  }
  fn parse_name(name: &[u8]) -> Option<Self> {
    match name {
      b"cloexec" => Some(FdFlags::CLOEXEC),
      b"nonblock" => Some(FdFlags::NONBLOCK),
      b"append" => Some(FdFlags::APPEND),
      _ => None,
    }
  }
}

pub(super) struct Fcntl;
impl super::Builtin for Fcntl {
  fn get_argv_and_opts(&self, cmd_span: Span, argv: &[Tk], no_split: bool) -> ShResult<Parsed> {
    // fcntl's flags work exactly the same way that `set`'s flags do
    // so we don't parse them here. pass through directly
    Ok(
      execute::prepare_argv_with(argv, no_split)
        .promote_err(cmd_span)?
        .into(),
    )
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let (arg_vec, _) = args.take_argv();
    let mut it = arg_vec.into_iter().peekable();

    let Some((fd_word, fd_span)) = it.next() else {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing file descriptor argument").with_code(2),
      );
    };

    let fd: RawFd = fd_word
      .to_str_lossy()
      .parse()
      .map_err(|_| sherr!(ExecFail @ fd_span, "invalid file descriptor: {fd_word}").with_code(2))?;

    if it.peek().is_none() {
      return Self::print_fd_flags(true, fd, fd_span);
    }

    opt::scan_options(
      &mut it,
      FdFlags::role,
      |on, flag, span| Self::set_fd_flag(fd, on, flag, span),
      |on, name, span| Self::set_long_flag(fd, on, name, span),
      |ch, _, _, span| Err(sherr!(ExecFail @ span, "unknown option: -{ch}").with_code(2)),
      true,
    )?;

    util::with_status(0)
  }
}

impl Fcntl {
  fn set_fd_flag(fd: RawFd, on: bool, flag: FdFlags, span: Span) -> ShResult<()> {
    let Some((get, set, bit)) = flag.get_set_bit() else {
      unreachable!()
    };

    let Some(sink) = Shed::sinks(|s| s.get(fd)) else {
      return Err(sherr!(ExecFail @ span, "fd {fd} is not open").with_code(1));
    };
    let Ok(sink_fd) = sink.as_os_fd() else {
      return Err(sherr!(ExecFail @ span, "fd {fd} is not a valid file descriptor").with_code(1));
    };
    let raw = sink_fd.as_raw_fd();

    let cur = unsafe { libc::fcntl(raw, get) };
    if cur < 0 {
      return Err(Self::fd_err(raw, span));
    }

    let new = if on { cur | bit } else { cur & !bit };

    if unsafe { libc::fcntl(raw, set, new) } < 0 {
      return Err(Self::fd_err(fd, span));
    }

    Ok(())
  }
  fn set_long_flag(fd: RawFd, on: bool, name: Option<&str>, span: Span) -> ShResult<()> {
    match name {
      Some(name) => {
        let flag = FdFlags::parse_name(name.as_bytes())
          .ok_or_else(|| sherr!(ExecFail @ span, "unknown flag: '{name}'").with_code(2))?;
        Self::set_fd_flag(fd, on, flag, span)
      }
      None => Self::print_fd_flags(on, fd, span),
    }
  }
  fn print_fd_flags(verbose: bool, fd: RawFd, fd_span: Span) -> ShResult<()> {
    if verbose {
      Self::print_flags_verbose(fd, fd_span)
    } else {
      Self::print_flags_terse(fd, fd_span)
    }
  }
  fn read_flags(fd: RawFd, span: Span) -> ShResult<(FdFlags, libc::c_int)> {
    let Some(sink) = Shed::sinks(|s| s.get(fd)) else {
      return Err(sherr!(ExecFail @ span, "fd {fd} is not open").with_code(1));
    };
    let Ok(sink_fd) = sink.as_os_fd() else {
      return Err(sherr!(ExecFail @ span, "fd {fd} is not a valid file descriptor").with_code(1));
    };
    let raw = sink_fd.as_raw_fd();

    let fd_flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    let status = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if fd_flags < 0 || status < 0 {
      return Err(Self::fd_err(fd, span));
    }

    let mut flags = FdFlags::empty();
    flags.set(FdFlags::CLOEXEC, fd_flags & libc::FD_CLOEXEC != 0);
    flags.set(FdFlags::NONBLOCK, status & libc::O_NONBLOCK != 0);
    flags.set(FdFlags::APPEND, status & libc::O_APPEND != 0);
    Ok((flags, status))
  }
  fn print_flags_verbose(fd: RawFd, fd_span: Span) -> ShResult<()> {
    let (flags, status) = Self::read_flags(fd, fd_span)?;
    let mode = match status & libc::O_ACCMODE {
      libc::O_WRONLY => "write-only",
      libc::O_RDWR => "read-write",
      _ => "read-only",
    };
    outln!("mode\t\t{mode}");
    outln!(
      "cloexec\t\t{}",
      Self::on_off(flags.contains(FdFlags::CLOEXEC))
    );
    outln!(
      "nonblock\t\t{}",
      Self::on_off(flags.contains(FdFlags::NONBLOCK))
    );
    outln!(
      "append\t\t{}",
      Self::on_off(flags.contains(FdFlags::APPEND))
    );
    util::with_status(0)
  }
  fn print_flags_terse(fd: RawFd, fd_span: Span) -> ShResult<()> {
    let (flags, _) = Self::read_flags(fd, fd_span)?;
    let on: String = flags.iter().map(FdFlags::as_char).collect();
    let off: String = (!flags).iter().map(FdFlags::as_char).collect();

    out!("fcntl {fd}");
    if !on.is_empty() {
      out!(" -{on}");
    }
    if !off.is_empty() {
      out!(" +{off}");
    }
    outln!();

    util::with_status(0)
  }

  fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
  }

  fn fd_err(fd: RawFd, span: Span) -> ShErr {
    sherr!(ExecFail @ span, "fd {fd}: {}", std::io::Error::last_os_error()).with_code(1)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::Shed;
  use crate::tests::testutil::{TestGuard, test_input};

  /// Run `cmd` and return the exit status.
  fn status_of(cmd: &str) -> i32 {
    let _g = TestGuard::new();
    assert!(test_input(cmd).is_ok(), "test_input errored: {cmd}");
    Shed::get_status()
  }

  #[test]
  fn set_nonblock() {
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 -n; fcntl 3").unwrap();
    let out = g.read_output();
    assert!(out.contains("nonblock\t\ton"), "{out:?}");
  }

  #[test]
  fn clear_nonblock() {
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 -n; fcntl 3 +n; fcntl 3").unwrap();
    let out = g.read_output();
    assert!(out.contains("nonblock\t\toff"), "{out:?}");
  }

  #[test]
  fn toggle_cloexec() {
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 +c; fcntl 3").unwrap();
    let out = g.read_output();
    assert!(out.contains("cloexec\t\toff"), "{out:?}");
  }

  #[test]
  fn bundled_shorts_set_both() {
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 +nc; fcntl 3 -nc; fcntl 3").unwrap();
    let out = g.read_output();
    assert!(
      out.contains("cloexec\t\ton") && out.contains("nonblock\t\ton"),
      "{out:?}"
    );
  }

  #[test]
  fn mixed_polarity_in_one_word() {
    // `-n +c`: set nonblock, clear cloexec, in a single invocation.
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 -n +c; fcntl 3").unwrap();
    let out = g.read_output();
    assert!(
      out.contains("nonblock\t\ton") && out.contains("cloexec\t\toff"),
      "{out:?}"
    );
  }

  #[test]
  fn long_form_set() {
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 -o nonblock; fcntl 3").unwrap();
    let out = g.read_output();
    assert!(out.contains("nonblock\t\ton"), "{out:?}");
  }

  #[test]
  fn long_form_clear() {
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 -n; fcntl 3 +o nonblock; fcntl 3").unwrap();
    let out = g.read_output();
    assert!(out.contains("nonblock\t\toff"), "{out:?}");
  }

  #[test]
  fn unknown_short_flag_is_usage_error() {
    assert_eq!(status_of("fcntl 3 -z"), 2);
  }

  #[test]
  fn unknown_long_flag_is_usage_error() {
    assert_eq!(status_of("fcntl 3 -o bogus"), 2);
  }

  #[test]
  fn missing_fd_is_usage_error() {
    assert_eq!(status_of("fcntl"), 2);
  }

  #[test]
  fn invalid_fd_is_usage_error() {
    assert_eq!(status_of("fcntl abc"), 2);
  }

  #[test]
  fn unopened_fd_errors() {
    assert_eq!(status_of("fcntl 99"), 1);
  }

  #[test]
  fn terse_form_both_on() {
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 -nc; fcntl 3 +o").unwrap();
    assert!(
      g.read_output().contains("fcntl 3 -cn"),
      "{:?}",
      g.read_output()
    );
  }

  #[test]
  fn terse_form_mixed() {
    // nonblock on, cloexec off → `-n` cluster and `+c` cluster.
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 -n +c; fcntl 3 +o").unwrap();
    assert!(
      g.read_output().contains("fcntl 3 -n +c"),
      "{:?}",
      g.read_output()
    );
  }

  #[test]
  fn dash_o_is_verbose() {
    let g = TestGuard::new();
    test_input("exec 3</dev/null; fcntl 3 -o").unwrap();
    let out = g.read_output();
    assert!(
      out.contains("cloexec") && out.contains("nonblock"),
      "{out:?}"
    );
  }

  #[test]
  fn set_append() {
    let g = TestGuard::new();
    test_input("exec 3>/dev/null; fcntl 3 -a; fcntl 3").unwrap();
    assert!(
      g.read_output().contains("append\t\ton"),
      "{:?}",
      g.read_output()
    );
  }

  #[test]
  fn append_long_form() {
    let g = TestGuard::new();
    test_input("exec 3>/dev/null; fcntl 3 -o append; fcntl 3").unwrap();
    assert!(
      g.read_output().contains("append\t\ton"),
      "{:?}",
      g.read_output()
    );
  }

  #[test]
  fn verbose_reports_access_mode() {
    // A write redirect to a real file opens write-only.
    let g = TestGuard::new();
    test_input("exec 3>/tmp/shed_fcntl_mode_test; fcntl 3").unwrap();
    assert!(
      g.read_output().contains("mode\t\twrite-only"),
      "{:?}",
      g.read_output()
    );
  }

  #[test]
  fn read_redirect_is_read_only() {
    let g = TestGuard::new();
    test_input("printf '' > /tmp/shed_fcntl_ro_test; exec 4</tmp/shed_fcntl_ro_test; fcntl 4")
      .unwrap();
    assert!(
      g.read_output().contains("mode\t\tread-only"),
      "{:?}",
      g.read_output()
    );
  }
}
