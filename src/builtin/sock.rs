use std::{
  io,
  net::{TcpListener, TcpStream},
  os::{
    fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
    unix::{
      fs::FileTypeExt,
      net::{UnixListener, UnixStream},
    },
  },
  path::PathBuf,
};

// The abstract namespace only exists on Linux-like platforms, so the address
// types used to resolve it live behind the same gate as `connect_abstract`.
#[cfg(linux_like)]
use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};

use nix::{
  errno::Errno,
  libc,
  sys::socket::{self, AddressFamily, SockFlag, SockType, accept, connect, socket},
  unistd::{ForkResult, Pid},
};

use crate::{
  ShErrKind, ShResult, Shed,
  builtin::getopt::{Opt, OptSpec},
  eval, lifecycle, procio, sherr, shopt, signal,
  state::vars::{VarFlags, VarKind, VarStr},
  util::{ShErr, ShResultExt, with_status},
  varstr,
};

fn set_cloexec(fd: RawFd) {
  unsafe {
    let flags = libc::fcntl(fd, libc::F_GETFD);
    if flags >= 0 {
      libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
    }
  }
}

fn set_nonblocking(fd: RawFd) {
  unsafe {
    let flags = libc::fcntl(fd, libc::F_GETFL);
    if flags >= 0 {
      libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
  }
}

/// Convert an `io::Error` into a `ShErr`
///
/// The returned `ShErr` contains a status code related to the reason for the connection failing.
fn socket_error(op: &str, err: &io::Error) -> ShErr {
  let coded = |title: &str, reason: &str, code: i32| -> ShErr {
    ShErr::simple(
      ShErrKind::Custom(title.into(), code),
      varstr!("{op}: {reason}"),
    )
  };

  match err.kind() {
    io::ErrorKind::NotFound => coded("Address not found", "address not found", 3),
    io::ErrorKind::ConnectionRefused => coded("Connection refused", "connection refused", 4),
    io::ErrorKind::AddrInUse => coded("Address in use", "address in use", 5),
    io::ErrorKind::TimedOut => coded("Connection timed out", "connection timed out", 6),
    io::ErrorKind::PermissionDenied => coded("Permission denied", "permission denied", 7),
    io::ErrorKind::ConnectionReset => coded("Connection reset", "connection reset", 8),
    io::ErrorKind::ConnectionAborted => coded("Connection aborted", "connection aborted", 9),

    _ => match err.raw_os_error() {
      Some(c) if c == libc::EBADF || c == libc::ENOTSOCK => {
        coded("Bad file descriptor", "not a valid socket descriptor", 2)
      }
      _ => ShErr::simple(ShErrKind::ExecFail, varstr!("{op}: {err}")),
    },
  }
}

#[cfg(linux_like)]
fn connect_abstract(name: &str) -> io::Result<OwnedFd> {
  // connect to abstract socket
  let addr = SocketAddr::from_abstract_name(name.as_bytes())?;
  let stream = UnixStream::connect_addr(&addr)?;

  Ok(stream.into())
}

#[cfg(linux_like)]
fn bind_abstract(name: &str) -> io::Result<OwnedFd> {
  let addr = SocketAddr::from_abstract_name(name.as_bytes())?;
  let listener = UnixListener::bind_addr(&addr)?;

  Ok(listener.into())
}

#[cfg(not(linux_like))]
fn bind_abstract(_name: &str) -> io::Result<OwnedFd> {
  Err(sherr!(
    ExecFail,
    "Abstract sockets are not supported on this platform"
  ))
}

#[cfg(not(linux_like))]
fn connect_abstract(_name: &str) -> io::Result<OwnedFd> {
  Err(sherr!(
    ExecFail,
    "Abstract sockets are not supported on this platform"
  ))
}

enum TcpHost {
  Hostname(VarStr),
  IpAddr(std::net::IpAddr),
}

struct TcpSocket {
  host: TcpHost,
  port: u16,
}

impl TcpSocket {
  fn connect(self) -> io::Result<OwnedFd> {
    let stream = match self.host {
      TcpHost::Hostname(hostname) => TcpStream::connect((hostname.as_str(), self.port))?,
      TcpHost::IpAddr(ip)/*-----*/=> TcpStream::connect((ip, self.port))?
    };

    Ok(stream.into())
  }
  fn bind(self) -> io::Result<OwnedFd> {
    let listener = match self.host {
      TcpHost::Hostname(hostname) => TcpListener::bind((hostname.as_str(), self.port))?,
      TcpHost::IpAddr(ip)/*-----*/=> TcpListener::bind((ip, self.port))?
    };

    Ok(OwnedFd::from(listener))
  }
}

enum UnixAddr {
  Path(PathBuf),
  Abstract(VarStr),
}

impl UnixAddr {
  pub fn connect(self) -> io::Result<OwnedFd> {
    match self {
      UnixAddr::Abstract(var_str) => connect_abstract(&var_str),
      UnixAddr::Path(path_buf) => UnixStream::connect(&path_buf).map(OwnedFd::from),
    }
  }
  fn poke(&self) -> io::Result<bool> {
    // let's check if this path is a live socket or not
    // true = "yes this socket exists and is being used"
    let Self::Path(p) = self else {
      return Ok(false);
    };

    let probe = socket(
      AddressFamily::Unix,
      SockType::Stream,
      SockFlag::empty(),
      None,
    )?;
    set_nonblocking(probe.as_raw_fd());
    set_cloexec(probe.as_raw_fd());

    let addr = socket::UnixAddr::new(p)?;

    let poke_result = match connect(probe.as_raw_fd(), &addr) {
      Ok(()) | Err(Errno::EAGAIN | Errno::EINPROGRESS) => true,
      Err(Errno::ECONNREFUSED | Errno::ENOENT) => false,
      Err(e) => return Err(io::Error::from_raw_os_error(e as i32)),
    };

    Ok(poke_result)
  }
  pub fn bind(self) -> io::Result<OwnedFd> {
    match self {
      UnixAddr::Abstract(var_str) => bind_abstract(&var_str),
      UnixAddr::Path(ref path_buf) => {
        // a bunch of checks now to make sure we can bind to this address
        // the checks go like this:
        // 1. if it exists and is a socket, remove the socket if it is not in use. If noclobber is set, this is an error.
        // 2. if it exists and is not a socket, this is an error.
        // 3. if it does not exist, we are free to bind to it.
        match std::fs::symlink_metadata(path_buf) {
          Err(e) if e.kind() == std::io::ErrorKind::NotFound => { /* free to bind */ }
          Err(e) => {
            return Err(io::Error::other(
              varstr!("Failed to stat Unix socket '{}': {e}", path_buf.display()).as_str(),
            ));
          }
          Ok(_) if shopt!(set.noclobber) => {
            return Err(io::Error::other(
              varstr!(
                "Cannot bind to Unix socket '{}': already exists and noclobber is set",
                path_buf.display()
              )
              .as_str(),
            ));
          }
          Ok(m) if !m.file_type().is_socket() => {
            return Err(io::Error::other(
              varstr!(
                "Cannot bind to Unix socket '{}': path exists and is not a socket",
                path_buf.display()
              )
              .as_str(),
            ));
          }
          Ok(_) if self.poke()? => {
            return Err(io::Error::other(
              varstr!(
                "Cannot bind to Unix socket '{}': already in use",
                path_buf.display()
              )
              .as_str(),
            ));
          }
          Ok(_) => {
            // if we are here, we can remove it
            std::fs::remove_file(path_buf)?;
          }
        }

        // now bind
        UnixListener::bind(path_buf).map(OwnedFd::from)
      }
    }
  }
}

enum SockTarget {
  Unix(UnixAddr),
  Tcp(TcpSocket),
}

struct SockOpts {
  target: SockTarget,
  fd_var: Option<VarStr>,
}

impl SockOpts {
  fn from_opts(opts: &[Opt]) -> ShResult<Self> {
    let mut unix_addr = None;
    let mut tcp_addr = None;
    let mut tcp_port = None;
    let mut fd_var = None;

    for opt in opts {
      match opt {
        Opt::ShortWithArg('U', arg) => {
          let addr = match arg.strip_prefix('@') {
            Some(name) => UnixAddr::Abstract(name.into()),
            None => UnixAddr::Path(PathBuf::from(arg.as_str())),
          };
          unix_addr = Some(addr);
        }
        Opt::ShortWithArg('t', arg) => {
          let host = if let Ok(ip) = arg.parse::<std::net::IpAddr>() {
            TcpHost::IpAddr(ip)
          } else {
            TcpHost::Hostname(arg.clone())
          };
          tcp_addr = Some(host);
        }
        Opt::ShortWithArg('p', arg) => {
          let Ok(port) = arg.parse::<u16>() else {
            return Err(sherr!(ExecFail, "Invalid port number '{arg}'"));
          };

          tcp_port = Some(port);
        }
        Opt::ShortWithArg('v', arg) => fd_var = Some(arg.clone()),

        Opt::LongWithArg(name, arg) => match name.as_str() {
          "tcp" => {
            let host = if let Ok(ip) = arg.parse::<std::net::IpAddr>() {
              TcpHost::IpAddr(ip)
            } else {
              TcpHost::Hostname(arg.clone())
            };
            tcp_addr = Some(host);
          }
          "port" => {
            let Ok(port) = arg.parse::<u16>() else {
              return Err(sherr!(ExecFail, "Invalid port number '{arg}'"));
            };

            tcp_port = Some(port);
          }
          _ => return Err(sherr!(ExecFail, "Unknown option '--{name}'")),
        },
        _ => return Err(sherr!(ExecFail, "Unexpected option '{opt}'")),
      }
    }

    if unix_addr.is_some() && (tcp_addr.is_some() || tcp_port.is_some()) {
      return Err(sherr!(
        ExecFail,
        "Cannot specify both a Unix socket path and a TCP host/port",
      ));
    } else if unix_addr.is_none() && (tcp_addr.is_none() || tcp_port.is_none()) {
      return Err(sherr!(
        ExecFail,
        "Must specify either a Unix socket path, or a TCP host and port",
      ));
    }

    let target = unix_addr.map_or_else(
      || {
        SockTarget::Tcp(TcpSocket {
          host: tcp_addr.unwrap(),
          port: tcp_port.unwrap(),
        })
      },
      SockTarget::Unix,
    );

    Ok(Self { target, fd_var })
  }
}

/// Install an owned socket fd into the shell and record its number.
fn install_socket_fd(
  owned: OwnedFd,
  target_fd: Option<RawFd>,
  var_name: Option<VarStr>,
  default_var: &str,
) -> ShResult<()> {
  let fd = if let Some(fd) = target_fd {
    // Stage high first so the user asking for e.g. `3` can't collide with the
    // fd the socket happened to be allocated on.
    let staged = procio::move_high_no_cloexec(owned)?;
    procio::Redir::new(fd, staged).apply()?;
    fd
  } else {
    procio::move_high_no_cloexec(owned)?.into_raw_fd()
  };

  match (target_fd, var_name) {
    (None, None) => {
      Shed::vars_mut(|v| v.set_var(default_var, VarKind::Int(fd), VarFlags::empty()))?;
    }
    (_, Some(var)) => {
      Shed::vars_mut(|v| v.set_var(&var, VarKind::Int(fd), VarFlags::empty()))?;
    }
    _ => {}
  }

  with_status(0)
}

pub(super) struct Accept;
impl super::Builtin for Accept {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::single_arg('v'), // variable to store auto-allocated FD in (`$SHED_ACCEPT` by default)
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut var = None;
    let cmd_span = args.cmd_span();
    for opt in &args.opts {
      match opt {
        Opt::ShortWithArg('v', arg) => var = Some(arg.clone()),
        _ => return Err(sherr!(ExecFail @ args.cmd_span(), "Unexpected option '{opt}'")),
      }
    }

    let mut argv_iter = args.argv.into_iter();
    let Some((fd, span)) = argv_iter.next() else {
      return Err(sherr!(
        ExecFail @ cmd_span,
        "Missing file descriptor argument for accept",
      ));
    };

    let Ok(listen) = fd.parse::<u32>() else {
      return Err(sherr!(ExecFail @ span.clone(), "Invalid file descriptor '{fd}'"));
    };

    match argv_iter.next() {
      None => Self::bind_mode(listen as i32, None, var),
      Some((arg, _)) => {
        if let Ok(fd) = arg.parse::<u32>()
          && fd < 10
        {
          Self::bind_mode(listen as i32, Some(fd as RawFd), var)
        } else {
          Self::serve_mode(listen as i32, arg)
        }
      }
    }
  }
}

impl Accept {
  fn accept_conn(listen: RawFd) -> ShResult<OwnedFd> {
    let conn = loop {
      match accept(listen) {
        Ok(fd) => {
          set_cloexec(fd);
          break fd;
        }
        Err(Errno::EINTR) => signal::check_signals()?,
        Err(e) => {
          return Err(socket_error(
            "accept failed",
            &io::Error::from_raw_os_error(e as i32),
          ));
        }
      }
    };
    Ok(unsafe { OwnedFd::from_raw_fd(conn) })
  }
  fn bind_mode(listen: RawFd, target_fd: Option<RawFd>, var_name: Option<VarStr>) -> ShResult<()> {
    let conn = Self::accept_conn(listen)?;
    install_socket_fd(conn, target_fd, var_name, "SHED_ACCEPT")
  }
  fn serve_mode(listen: RawFd, handler: VarStr) -> ShResult<()> {
    let conn = Self::accept_conn(listen)?;

    match unsafe { nix::unistd::fork()? } {
      ForkResult::Parent { child: _ } => {
        nix::unistd::close(conn).ok();
        with_status(0)
      }
      ForkResult::Child => {
        lifecycle::setup_child();

        nix::unistd::dup2_stdin(&conn).ok();
        nix::unistd::dup2_stdout(&conn).ok();
        std::mem::drop(conn);
        nix::unistd::close(listen).ok();

        nix::unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0)).ok();

        signal::reset_signals(false);
        let _guard = Shed::term_mut(|t| t.interactive_guard(false));

        if let Err(e) = eval::execute::exec_nonint(handler, Some("accept handler".into())) {
          e.print_error();
        }

        unsafe { libc::_exit(Shed::get_status()) }
      }
    }
  }
}

pub(super) struct Listen;
impl super::Builtin for Listen {
  fn is_special(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::single_arg('U'), // filesystem Unix socket
      OptSpec::single_arg('t'), // TCP host
      OptSpec::single_arg("tcp"),
      OptSpec::single_arg('p'), // port number
      OptSpec::single_arg("port"),
      OptSpec::single_arg('v'), // variable to store auto-allocated FD in (`$SHED_LISTEN` by default)
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let SockOpts { target, fd_var } =
      SockOpts::from_opts(&args.opts).promote_err(args.cmd_span())?;

    if args.argv.len() > 1 {
      return Err(sherr!(
        ExecFail @ args.cmd_span(),
        "Too many arguments, expected at most 1 file descriptor argument",
      ));
    }

    let target_fd = if let Some((arg, span)) = args.argv.first() {
      let Ok(arg) = arg.parse::<u32>() else {
        return Err(sherr!(ExecFail @ span.clone(), "Invalid file descriptor '{arg}'"));
      };

      if arg >= 10 {
        return Err(sherr!(
          ExecFail @ span.clone(),
          "File descriptor '{arg}' is too high, must be less than 10",
        ));
      }

      Some(arg)
    } else {
      None
    };

    let listen = match target {
      SockTarget::Tcp(tcp_socket) => tcp_socket
        .bind()
        .map_err(|e| socket_error("failed to bind TCP socket", &e))
        .promote_err(args.cmd_span())?,
      SockTarget::Unix(unix_addr) => unix_addr
        .bind()
        .map_err(|e| socket_error("failed to bind Unix socket", &e))
        .promote_err(args.cmd_span())?,
    };

    install_socket_fd(
      listen,
      target_fd.map(|fd| fd as RawFd),
      fd_var,
      "SHED_LISTEN",
    )
  }
}

pub(super) struct Sock;
impl super::Builtin for Sock {
  fn is_special(&self) -> bool {
    true
  }

  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::single_arg('U'), // filesystem Unix socket
      OptSpec::single_arg('t'), // TCP host
      OptSpec::single_arg("tcp"),
      OptSpec::single_arg('p'), // port number
      OptSpec::single_arg("port"),
      OptSpec::single_arg('v'), // variable to store auto-allocated FD in (`$SHED_CONN` by default)
    ]
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let SockOpts { target, fd_var } =
      SockOpts::from_opts(&args.opts).promote_err(args.cmd_span())?;

    if args.argv.len() > 1 {
      return Err(sherr!(
        ExecFail @ args.cmd_span(),
        "Too many arguments, expected at most 1 file descriptor argument",
      ));
    }

    let target_fd = if let Some((arg, span)) = args.argv.first() {
      let Ok(arg) = arg.parse::<u32>() else {
        return Err(sherr!(ExecFail @ span.clone(), "Invalid file descriptor '{arg}'"));
      };

      if arg >= 10 {
        return Err(sherr!(
          ExecFail @ span.clone(),
          "File descriptor '{arg}' is too high, must be less than 10",
        ));
      }

      Some(arg)
    } else {
      None
    };

    let stream = match target {
      SockTarget::Tcp(tcp_socket) => tcp_socket
        .connect()
        .map_err(|e| socket_error("failed to connect to TCP socket", &e))
        .promote_err(args.cmd_span())?,
      SockTarget::Unix(unix_addr) => unix_addr
        .connect()
        .map_err(|e| socket_error("failed to connect to Unix socket", &e))
        .promote_err(args.cmd_span())?,
    };

    install_socket_fd(stream, target_fd.map(|fd| fd as RawFd), fd_var, "SHED_CONN")
  }
}

#[cfg(test)]
mod tests {
  use std::io::{Read, Write};
  use std::os::fd::AsRawFd;
  use std::os::unix::net::UnixListener;
  use std::path::PathBuf;
  use std::time::Duration;

  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  fn uniq() -> u128 {
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_nanos()
  }

  fn temp_sock_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("shed_sock_{tag}_{}.sock", uniq()))
  }

  /// The lowest-numbered free fd right now. Opening `/dev/null` grabs the
  /// lowest free fd; dropping it frees that number again. In a single-threaded
  /// test nothing claims it before the next `connect()`, so a socket opened
  /// immediately after lands on exactly this fd — which is what makes the
  /// source==target collision case deterministic.
  fn lowest_free_fd() -> i32 {
    let f = std::fs::File::open("/dev/null").unwrap();
    f.as_raw_fd()
    // `f` drops here, freeing the number we just returned
  }

  /// Bind a filesystem unix listener at a unique temp path and register its
  /// removal on the guard.
  fn bind_listener(g: &mut TestGuard, tag: &str) -> (UnixListener, PathBuf) {
    let path = temp_sock_path(tag);
    std::fs::remove_file(&path).ok();
    let listener = UnixListener::bind(&path).unwrap();
    let cleanup = path.clone();
    g.add_cleanup(move || {
      std::fs::remove_file(&cleanup).ok();
    });
    (listener, path)
  }

  // ─── data flow (auto-allocated fd + variable redirection) ─────────
  //
  // These auto-allocate a high fd (kept out of the user range) and reference it
  // through variable redirection, so they need no free low fd and run on every
  // platform. The test harness can hold every fd below 10 (observed on macOS),
  // so relying on a free low fd here is not portable. `eval` closes the fd,
  // since a redirection's fd operand must be a literal after expansion.

  #[test]
  fn sock_write_reaches_peer() {
    let mut g = TestGuard::new();
    let (listener, path) = bind_listener(&mut g, "write");

    test_input(format!(
      "sock -U {} -v conn\nprintf 'ping' >&$conn",
      path.display()
    ))
    .unwrap();
    assert_eq!(state::Shed::get_status(), 0, "sock should connect");

    let (mut conn, _) = listener.accept().unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut buf = [0u8; 16];
    let n = conn.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"ping");

    test_input("eval \"exec $conn>&-\"").ok();
  }

  #[test]
  fn sock_reads_data_from_peer() {
    let mut g = TestGuard::new();
    let (listener, path) = bind_listener(&mut g, "read");

    test_input(format!("sock -U {} -v conn", path.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);

    let (mut conn, _) = listener.accept().unwrap();
    conn.write_all(b"fromserver\n").unwrap();

    test_input("read line <&$conn\necho \"got=$line\"").unwrap();
    assert_eq!(g.read_output().trim(), "got=fromserver");

    test_input("eval \"exec $conn>&-\"").ok();
  }

  // ─── explicit fd (needs a free fd in the user range 0-9) ──────────
  //
  // A free low fd isn't guaranteed: the harness can hold every fd below 10, and
  // clobbering a harness fd is unsafe (it can wedge the pty reader). So these
  // skip when none is free — the behavior they cover is platform-independent
  // and exercised wherever a low fd is available.

  #[test]
  fn sock_explicit_fd_collision() {
    let mut g = TestGuard::new();
    let (listener, path) = bind_listener(&mut g, "collision");
    let fd = lowest_free_fd();
    if fd >= 10 {
      return; // no free user-range fd in this env
    }

    // Targeting the lowest free fd — which is also the fd a fresh socket grabs —
    // exercises the source==target install collision (the staging-through-a-
    // high-fd fix). Without it the socket would be closed on connect.
    test_input(format!("sock -U {} {fd}", path.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0, "sock should connect");
    test_input(format!("printf 'ping' >&{fd}")).unwrap();

    let (mut conn, _) = listener.accept().unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut buf = [0u8; 16];
    let n = conn.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"ping");

    test_input(format!("exec {fd}>&-")).ok();
  }

  // ─── fd delivery policy ───────────────────────────────────────────

  #[test]
  fn sock_auto_alloc_sets_shed_conn() {
    let mut g = TestGuard::new();
    let (_listener, path) = bind_listener(&mut g, "auto");

    test_input(format!("sock -U {}\necho \"c=$SHED_CONN\"", path.display())).unwrap();
    let out = g.read_output();
    let num: i32 = out
      .trim()
      .strip_prefix("c=")
      .and_then(|s| s.parse().ok())
      .unwrap_or_else(|| panic!("SHED_CONN not a number: {out:?}"));
    assert!(num >= 10, "auto-allocated fd should be >= 10, got {num}");

    test_input(format!("exec {num}>&-")).ok();
  }

  #[test]
  fn sock_v_flag_sets_named_var() {
    let mut g = TestGuard::new();
    let (_listener, path) = bind_listener(&mut g, "vflag");

    test_input(format!(
      "sock -U {} -v myconn\necho \"c=$myconn\"",
      path.display()
    ))
    .unwrap();
    let out = g.read_output();
    let num: i32 = out
      .trim()
      .strip_prefix("c=")
      .and_then(|s| s.parse().ok())
      .unwrap_or_else(|| panic!("myconn not a number: {out:?}"));
    assert!(num >= 10, "auto-allocated fd should be >= 10, got {num}");

    test_input(format!("exec {num}>&-")).ok();
  }

  #[test]
  fn sock_explicit_fd_does_not_set_shed_conn() {
    let mut g = TestGuard::new();
    let (_listener, path) = bind_listener(&mut g, "noconn");
    let fd = lowest_free_fd();
    if fd >= 10 {
      return; // no free user-range fd in this env
    }

    test_input(format!(
      "unset SHED_CONN\nsock -U {} {fd}\necho \"c=[$SHED_CONN]\"",
      path.display()
    ))
    .unwrap();
    assert_eq!(g.read_output().trim(), "c=[]");

    test_input(format!("exec {fd}>&-")).ok();
  }

  // ─── usage / connect errors (no listener needed) ──────────────────

  #[test]
  fn sock_no_target_is_error() {
    let _g = TestGuard::new();
    test_input("sock").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn sock_conflicting_unix_and_tcp_is_error() {
    let _g = TestGuard::new();
    test_input("sock -U /tmp/x.sock -t localhost -p 80").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn sock_too_many_args_is_error() {
    let _g = TestGuard::new();
    test_input("sock -U /tmp/x.sock 3 4").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn sock_explicit_fd_too_high_is_error() {
    // The fd bound is checked before any connect attempt.
    let _g = TestGuard::new();
    test_input("sock -U /tmp/x.sock 42").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn sock_invalid_fd_is_error() {
    let _g = TestGuard::new();
    test_input("sock -U /tmp/x.sock notanumber").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn sock_nonexistent_unix_path_is_error() {
    let _g = TestGuard::new();
    test_input("sock -U /no/such/shed/socket/here.sock -v c").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── abstract namespace (Linux/Android only) ──────────────────────

  #[cfg(linux_like)]
  #[test]
  fn sock_connects_to_abstract_socket() {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::SocketAddr;

    let _g = TestGuard::new();
    let name = format!("shed_abs_{}", uniq());
    let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    let listener = UnixListener::bind_addr(&addr).unwrap();

    let fd = lowest_free_fd();
    if fd >= 10 {
      return; // no free user-range fd in this env
    }

    test_input(format!("sock -U @{name} {fd}")).unwrap();
    assert_eq!(state::Shed::get_status(), 0);

    test_input(format!("printf 'abs' >&{fd}")).unwrap();

    let (mut conn, _) = listener.accept().unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut buf = [0u8; 16];
    let n = conn.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"abs");

    test_input(format!("exec {fd}>&-")).ok();
  }
}
