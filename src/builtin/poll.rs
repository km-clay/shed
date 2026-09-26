use crate::set_var;
use std::{
  fmt::Display,
  os::fd::{AsRawFd, RawFd},
  sync::Arc,
  time::Duration,
};

use bstr::ByteSlice;
use nix::libc;

use crate::{
  eval::lex::Span,
  opt,
  procio::Sink,
  sherr, signal,
  state::{
    Shed,
    vars::{VarKind, VarStr},
  },
  util::{
    self,
    error::{ShResult, ShResultExt},
    strops,
  },
  varstr,
};

use super::opt::OptSpec;

type PollEntries = (Vec<libc::pollfd>, Vec<(RawFd, Arc<dyn Sink>)>);
type PollEntry = (libc::pollfd, (RawFd, Arc<dyn Sink>));

pub(super) struct Poll;
impl super::Builtin for Poll {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("events" | b'i', 1),
      opt!("revents" | b'o', 1),
      opt!("timeout" | b't', 1),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let (mut pfds, meta) = if let Some(events) = args.opt_value("events") {
      Self::parse_events(events, &args).with_code(2)?
    } else {
      Self::parse_args(&args).with_code(2)?
    };

    let revents = args
      .opt_value("revents")
      .unwrap_or_else(|| VarStr::from("SHED_REVENTS"));
    let mut revents_arr: Vec<(VarStr, VarStr)> = vec![];

    let timeout: i32 = match args.opt_value("timeout") {
      None => -1,
      Some(t) => {
        if t.trim() == b"0" {
          0
        } else {
          let span = args.opt_span("timeout").unwrap();
          let micros = strops::TimeReader::parse_dur(&t.to_str_lossy())
            .promote_err(span)
            .with_code(2)?;
          let millis = Duration::from_micros(micros.cast_unsigned()).as_millis();
          // now we gotta do some weird casting stuff
          // to clamp overflows
          match millis {
            0 => 1,
            _ => millis.min(i32::MAX as u128) as i32,
          }
        }
      }
    };

    let n = loop {
      let r = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout) };
      if r >= 0 {
        // fds are ready, or we timed out
        break r;
      }
      // error
      let e = std::io::Error::last_os_error();
      if let Some(libc::EINTR) = e.raw_os_error() {
        signal::check_signals()?;
      } else {
        return Err(sherr!(ExecFail @ args.cmd_span(), "poll failed: {e}").with_code(2));
      }
    };

    for (pfd, (logical, _)) in pfds.iter().zip(&meta) {
      if let Some(re) = Self::fmt_revents(pfd.revents) {
        revents_arr.push((varstr!("{logical}"), re));
      }
    }

    set_var!(&revents.to_str_lossy(), VarKind::AssocArr(revents_arr)).with_code(2)?;

    util::with_status(i32::from(n == 0))
  }
}

impl Poll {
  fn resolve(logical: RawFd, events: libc::c_short, span: Span) -> ShResult<PollEntry> {
    let sink = Shed::sinks(|s| s.get(logical))
      .ok_or_else(|| sherr!(ExecFail @ span, "fd {logical} is not open"))?;
    let raw = sink
      .as_os_fd()
      .map_err(|e| sherr!(ExecFail @ span, "fd {logical} is not pollable: {e}"))?
      .as_raw_fd();
    let pfd = libc::pollfd {
      fd: raw,
      events,
      revents: 0,
    };

    Ok((pfd, (logical, sink)))
  }
  fn fmt_revents(re: libc::c_short) -> Option<VarStr> {
    let mut flags = vec![];

    if re & libc::POLLIN != 0 {
      flags.push("in");
    }
    if re & libc::POLLOUT != 0 {
      flags.push("out");
    }
    if re & libc::POLLPRI != 0 {
      flags.push("pri");
    }
    if re & libc::POLLERR != 0 {
      flags.push("err");
    }
    if re & libc::POLLHUP != 0 {
      flags.push("hup");
    }
    #[cfg(linux_like)]
    if re & libc::POLLRDHUP != 0 {
      flags.push("rdhup");
    }
    if re & libc::POLLNVAL != 0 {
      flags.push("nval");
    }

    if flags.is_empty() {
      None
    } else {
      Some(flags.join(" ").into())
    }
  }
  fn parse_flags(spec: &[u8], span: Span, ctx: &dyn Display) -> ShResult<libc::c_short> {
    let mut flags: libc::c_short = 0;
    for tok in spec
      .split(|&c| c == b',' || c.is_ascii_whitespace())
      .filter(|s| !s.is_empty())
    {
      flags |= match tok {
        b"in" => libc::POLLIN,
        b"out" => libc::POLLOUT,
        b"pri" => libc::POLLPRI,
        #[cfg(linux_like)]
        b"rdhup" => libc::POLLRDHUP,
        #[cfg(not(linux_like))]
        b"rdhup" => {
          return Err(
            sherr!(ExecFail @ span, "event flag `rdhup` is not supported on this platform"),
          );
        }
        _ => {
          return Err(sherr!(ExecFail @ span,
            "invalid event flag `{}` in {ctx}", tok.to_str_lossy()));
        }
      };
    }

    if flags == 0 {
      flags |= libc::POLLIN;
    }

    Ok(flags)
  }
  fn parse_args(args: &super::BuiltinArgs) -> ShResult<PollEntries> {
    let mut pfds = vec![];
    let mut meta = vec![];

    for (arg, span) in args.arguments() {
      let (fd, flags) = match arg.split_once_str(":") {
        Some((fd, flags)) => (fd, flags),
        None => (arg.as_bytes(), b"in".as_slice()),
      };

      let Ok(fd) = fd.to_str_lossy().parse::<RawFd>() else {
        let span = span.sub_span(|s, _| (s, s + fd.len()));
        let fd = fd.to_str_lossy();
        return Err(sherr!(ExecFail @ span, "invalid fd `{fd}` in argument `{arg}`"));
      };
      let flags = Self::parse_flags(flags, span, arg)?;

      let (pfd, m) = Self::resolve(fd, flags, span)?;
      pfds.push(pfd);
      meta.push(m);
    }

    Ok((pfds, meta))
  }
  fn parse_events(events: VarStr, args: &super::BuiltinArgs) -> ShResult<PollEntries> {
    let mut pfds = vec![];
    let mut meta = vec![];
    let span = args.opt_span("events").unwrap();

    let Some(arr) = Shed::vars(|m| m.try_get_var_meta(&events.to_str_lossy())) else {
      return Err(sherr!(ExecFail @ span, "events array `{events}` not found"));
    };
    let VarKind::AssocArr(arr) = arr.kind() else {
      return Err(
        sherr!(ExecFail @ span, "events variable `{events}` is not an associative array"),
      );
    };

    for (fd, flags) in arr {
      let Ok(fd) = fd.to_str_lossy().parse::<RawFd>() else {
        return Err(sherr!(ExecFail @ span, "invalid fd `{fd}` in events array `{events}`"));
      };

      let flags = Self::parse_flags(flags.as_bytes(), span, &events)?;
      let (pfd, m) = Self::resolve(fd, flags, span)?;
      pfds.push(pfd);
      meta.push(m);
    }

    Ok((pfds, meta))
  }
}

#[cfg(test)]
mod tests {
  use std::os::fd::{AsRawFd, OwnedFd};
  use std::sync::Arc;

  use crate::procio::{self, OsSink, Sink};
  use crate::state::{Shed, vars::VarKind};
  use crate::tests::testutil::{TestGuard, test_input};

  fn install(logical: i32, fd: OwnedFd) {
    let sink: Arc<dyn Sink> = Arc::new(OsSink::new(fd));
    Shed::sinks(|s| s.clobber(logical, sink));
  }

  fn revents(var: &str, key: &str) -> Option<String> {
    Shed::vars(|v| v.try_get_var_meta(var)).and_then(|m| match m.kind() {
      VarKind::AssocArr(arr) => arr
        .iter()
        .find(|(k, _)| k.to_str_lossy() == key)
        .map(|(_, v)| v.to_string()),
      _ => None,
    })
  }

  fn write_byte(fd: &OwnedFd) {
    unsafe { nix::libc::write(fd.as_raw_fd(), b"x".as_ptr().cast(), 1) };
  }

  #[test]
  fn poll_readable_and_timeout() {
    let _g = TestGuard::new();
    let (r, w) = procio::pipes_high().unwrap();
    install(7, r);

    // empty pipe, writer still open -> times out
    test_input("poll 7:in -t 100ms").unwrap();
    assert_eq!(Shed::get_status(), 1, "empty pipe should time out");

    // data pending -> readable
    write_byte(&w);
    test_input("poll 7:in -t 500ms").unwrap();
    assert_eq!(Shed::get_status(), 0, "pipe with data should be ready");
    assert_eq!(revents("SHED_REVENTS", "7").as_deref(), Some("in"));

    drop(w);
  }

  #[test]
  fn poll_reports_hup_when_writer_closes() {
    let _g = TestGuard::new();
    let (r, w) = procio::pipes_high().unwrap();
    install(8, r);
    drop(w); // close the write end -> read end hangs up

    test_input("poll 8:in -t 500ms").unwrap();
    assert_eq!(Shed::get_status(), 0);
    let re = revents("SHED_REVENTS", "8").unwrap_or_default();
    assert!(re.contains("hup"), "expected hup, got: {re:?}");
  }

  #[test]
  fn poll_writable_nonblocking() {
    let _g = TestGuard::new();
    let (r, w) = procio::pipes_high().unwrap();
    install(9, w); // the write end of an empty pipe is writable

    test_input("poll 9:out -t 0").unwrap();
    assert_eq!(Shed::get_status(), 0);
    assert_eq!(revents("SHED_REVENTS", "9").as_deref(), Some("out"));

    drop(r);
  }

  #[test]
  fn poll_array_form_with_custom_output() {
    let _g = TestGuard::new();
    let (r, w) = procio::pipes_high().unwrap();
    install(5, r);
    write_byte(&w);

    test_input("declare -A watch\nwatch[5]=in\npoll -i watch -o ready -t 200ms").unwrap();
    assert_eq!(Shed::get_status(), 0);
    assert_eq!(revents("ready", "5").as_deref(), Some("in"));

    drop(w);
  }

  #[test]
  fn poll_nonopen_fd_errors() {
    let _g = TestGuard::new();
    test_input("poll 91:in -t 0").ok();
    assert_eq!(
      Shed::get_status(),
      2,
      "polling an unopened fd should be a usage error"
    );
  }

  #[test]
  fn poll_invalid_flag_errors() {
    let _g = TestGuard::new();
    let (r, w) = procio::pipes_high().unwrap();
    install(6, r);

    test_input("poll 6:bogus -t 0").ok();
    assert_eq!(
      Shed::get_status(),
      2,
      "invalid event flag should be a usage error"
    );

    drop(w);
  }
}
