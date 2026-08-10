use std::{
  net::TcpStream,
  os::{
    fd::{IntoRawFd, OwnedFd},
    unix::net::UnixStream,
  },
  path::PathBuf,
};

// The abstract namespace only exists on Linux-like platforms, so the address
// types used to resolve it live behind the same gate as `connect_abstract`.
#[cfg(linux_like)]
use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};

use crate::{
  ShResult, Shed,
  builtin::getopt::{Opt, OptSpec},
  procio, sherr,
  state::vars::{VarFlags, VarKind, VarStr},
  util::{ShResultExt, with_status},
};

#[cfg(linux_like)]
fn connect_abstract(name: &str) -> ShResult<OwnedFd> {
  // connect to abstract socket
  let addr = SocketAddr::from_abstract_name(name.as_bytes())
    .map_err(|e| sherr!(ExecFail, "Invalid abstract socket '@{name}': {e}"))?;
  let stream = UnixStream::connect_addr(&addr).map_err(|e| {
    sherr!(
      ExecFail,
      "Failed to connect to abstract socket '@{name}': {e}"
    )
  })?;

  Ok(stream.into())
}

#[cfg(not(linux_like))]
fn connect_abstract(name: &str) -> ShResult<OwnedFd> {
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
  fn connect(self) -> ShResult<OwnedFd> {
    let stream = match self.host {
      TcpHost::Hostname(hostname) => TcpStream::connect((hostname.as_str(), self.port)),
      TcpHost::IpAddr(ip)/*-----*/=> TcpStream::connect((ip, self.port))
    }
    .map_err(|e| sherr!(ExecFail, "Failed to connect to TCP socket: {e}"))?;

    Ok(stream.into())
  }
}

enum UnixAddr {
  Path(PathBuf),
  Abstract(VarStr),
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
      SockTarget::Tcp(tcp_socket) => tcp_socket.connect().promote_err(args.cmd_span())?,
      SockTarget::Unix(unix_addr) => match unix_addr {
        UnixAddr::Abstract(var_str) => connect_abstract(&var_str).promote_err(args.cmd_span())?,
        UnixAddr::Path(path_buf) => UnixStream::connect(&path_buf)
          .map_err(|e| sherr!(ExecFail @ args.cmd_span(), "Failed to connect to Unix socket '{}': {e}", path_buf.display()))?
          .into(),
      }
    };

    let fd = if let Some(fd) = target_fd {
      // move the fd to a definitely-open one
      let staged = procio::move_high_no_cloexec(stream)?;
      // now redirect it to the specified one
      // this protects from the case of the user specifying "3"
      // and the stream allocating to fd 3
      procio::Redir::new(fd as i32, staged).apply()?;
      fd as i32
    } else {
      procio::move_high_no_cloexec(stream)?.into_raw_fd()
    };

    match (target_fd, fd_var) {
      (None, None) => {
        // set $SHED_CONN to the auto-allocated fd
        Shed::vars_mut(|v| v.set_var("SHED_CONN", VarKind::Int(fd), VarFlags::empty()))?;
      }

      (_, Some(var)) => {
        // fd was specified, and variable was specified
        // set the given variable
        Shed::vars_mut(|v| v.set_var(&var, VarKind::Int(fd), VarFlags::empty()))?;
      }

      _ => {
        // fd was specified but no variable specified
        // so we don't do anything
      }
    }

    with_status(0)
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

  // ─── data flow ────────────────────────────────────────────────────

  #[test]
  fn sock_explicit_fd_write_reaches_peer() {
    // Targets the lowest free fd, which is also the fd a fresh socket will
    // grab — so this exercises the source==target install collision (the
    // staging-through-a-high-fd fix). Without it, the socket would be closed
    // on connect and this write would fail.
    let mut g = TestGuard::new();
    let (listener, path) = bind_listener(&mut g, "write");
    let fd = lowest_free_fd();
    assert!(
      fd < 10,
      "test env has too many open fds (lowest free = {fd})"
    );

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

  #[test]
  fn sock_reads_data_from_peer() {
    let mut g = TestGuard::new();
    let (listener, path) = bind_listener(&mut g, "read");
    let fd = lowest_free_fd();
    assert!(
      fd < 10,
      "test env has too many open fds (lowest free = {fd})"
    );

    test_input(format!("sock -U {} {fd}", path.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);

    let (mut conn, _) = listener.accept().unwrap();
    conn.write_all(b"fromserver\n").unwrap();

    test_input(format!("read line <&{fd}\necho \"got=$line\"")).unwrap();
    assert_eq!(g.read_output().trim(), "got=fromserver");

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
    assert!(
      fd < 10,
      "test env has too many open fds (lowest free = {fd})"
    );

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
    assert!(
      fd < 10,
      "test env has too many open fds (lowest free = {fd})"
    );

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
