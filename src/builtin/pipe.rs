use std::os::fd::AsRawFd;

use crate::{
  opt, procio, set_var, sherr,
  state::Shed,
  util::{
    self,
    error::{ShResult, ShResultExt},
    strops,
  },
  varstr,
};

use super::opt::OptSpec;

pub(super) struct Pipe;
impl super::Builtin for Pipe {
  #[rustfmt::skip]
  #[cfg(not(linux_like))]
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("array"      | b'a', 1),
      opt!("non-block"  | b'n'   ),
      opt!("no-cloexec" | b'C'   ),
    ]
  }
  #[rustfmt::skip]
  #[cfg(linux_like)]
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("array"      | b'a', 1),
      opt!("size"       | b's', 1),
      opt!("non-block"  | b'n'   ),
      opt!("no-cloexec" | b'C'   ),
      opt!("non-block"  | b'n'   ),
      opt!("no-cloexec" | b'C'   ),
      opt!("packet"     | b'p'   ),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let array_name = args.opt_value("array").unwrap_or("PIPE".into());
    let nonblock = args.has_opt("non-block");
    let cloexec = !args.has_opt("no-cloexec");
    let packet = args.has_opt("packet");
    let size = match args.opt_value("size") {
      Some(s) => {
        Some(strops::parse_size(&s.to_str_lossy()).promote_err(args.opt_span("size").unwrap())?)
      }
      None => None,
    };

    let (rd, wr) = match procio::OsPipe::pipes_with(cloexec, nonblock, size, packet) {
      Ok(p) => p,
      Err(e) => Err(sherr!(ExecFail @ args.cmd_span(), "failed to create pipe: {e}"))?,
    };

    let rfd = rd.as_os_fd()?.as_raw_fd();
    let wfd = wr.as_os_fd()?.as_raw_fd();
    Shed::sinks(|s| {
      s.clobber(rfd, rd);
      s.clobber(wfd, wr);
    });

    let array = vec![varstr!("{rfd}"), varstr!("{wfd}")];
    set_var!(&array_name.to_str_lossy(), arr(array))?;

    util::with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::Shed;
  use crate::tests::testutil::{TestGuard, test_input};

  fn failed(cmd: &str) -> bool {
    let _g = TestGuard::new();
    match test_input(cmd) {
      Err(_) => true,
      Ok(()) => Shed::get_status() != 0,
    }
  }

  #[test]
  fn creates_fd_pair() {
    let g = TestGuard::new();
    test_input(r#"pipe -a p; echo "${p[0]},${p[1]}""#).unwrap();
    let out = g.read_output();
    let (r, w) = out.trim().split_once(',').expect("two fds");
    assert!(r.parse::<i32>().unwrap() >= 10, "read fd not high: {out:?}");
    assert!(
      w.parse::<i32>().unwrap() >= 10,
      "write fd not high: {out:?}"
    );
  }

  #[test]
  fn defaults_to_pipe_array() {
    let g = TestGuard::new();
    test_input(r#"pipe; echo "${PIPE[0]}""#).unwrap();
    assert!(g.read_output().trim().parse::<i32>().unwrap() >= 10);
  }

  #[test]
  fn round_trip() {
    let g = TestGuard::new();
    test_input(r"pipe -a p; echo hello >&${p[1]}; exec {p[1]}>&-; thru <&${p[0]}").unwrap();
    assert_eq!(g.read_output(), "hello\n");
  }

  #[test]
  fn background_ipc() {
    let g = TestGuard::new();
    test_input(r"pipe -a p; seq 1 3 >&${p[1]} & exec {p[1]}>&-; thru <&${p[0]}; wait").unwrap();
    assert_eq!(g.read_output(), "1\n2\n3\n");
  }

  #[test]
  fn unset_size_no_panic() {
    let g = TestGuard::new();
    test_input(r#"pipe -a p; echo "${p[0]}""#).unwrap();
    assert!(g.read_output().trim().parse::<i32>().unwrap() >= 10);
  }

  #[cfg(linux_like)]
  #[test]
  fn packet_round_trip() {
    let g = TestGuard::new();
    test_input(r"pipe -p -a p; echo msg >&${p[1]}; exec {p[1]}>&-; thru <&${p[0]}").unwrap();
    assert_eq!(g.read_output(), "msg\n");
  }

  #[cfg(linux_like)]
  #[test]
  fn bad_size_is_error() {
    assert!(failed("pipe -a p -s bogus"));
  }

  #[cfg(linux_like)]
  #[test]
  fn size_sets_buffer() {
    use crate::procio::OsPipe;
    use nix::fcntl::{FcntlArg, fcntl};

    let (rd, _wr) = OsPipe::pipes_with(true, false, Some(256 * 1024), false).unwrap();
    let bfd = rd.as_os_fd().unwrap();
    let sz = fcntl(bfd, FcntlArg::F_GETPIPE_SZ).unwrap();
    assert!(
      sz as u64 >= 256 * 1024,
      "pipe buffer {sz} below requested 256K"
    );
  }
}
