use ariadne::Fmt;
use nix::{
  libc::STDOUT_FILENO,
  sys::{
    resource::{Resource, getrlimit, setrlimit},
    stat::{Mode, umask},
  },
  unistd::write,
};

use crate::{
  getopt::{Opt, OptArg, OptSpec, get_opts_from_tokens_strict},
  libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt, next_color},
  parse::{NdRule, Node},
  procio::borrow_fd,
  state::{self},
};

fn ulimit_opt_spec() -> [OptSpec; 5] {
  [
    OptSpec {
      opt: Opt::Short('n'), // file descriptors
      takes_arg: OptArg::Single,
    },
    OptSpec {
      opt: Opt::Short('u'), // max user processes
      takes_arg: OptArg::Single,
    },
    OptSpec {
      opt: Opt::Short('s'), // stack size
      takes_arg: OptArg::Single,
    },
    OptSpec {
      opt: Opt::Short('c'), // core dump file size
      takes_arg: OptArg::Single,
    },
    OptSpec {
      opt: Opt::Short('v'), // virtual memory
      takes_arg: OptArg::Single,
    },
  ]
}

struct UlimitOpts {
  fds: Option<u64>,
  procs: Option<u64>,
  stack: Option<u64>,
  core: Option<u64>,
  vmem: Option<u64>,
}

fn get_ulimit_opts(opt: &[Opt]) -> ShResult<UlimitOpts> {
  let mut opts = UlimitOpts {
    fds: None,
    procs: None,
    stack: None,
    core: None,
    vmem: None,
  };

  for o in opt {
    match o {
      Opt::ShortWithArg('n', arg) => {
        opts.fds = Some(arg.parse().map_err(|_| {
          ShErr::simple(
            ShErrKind::ParseErr,
            format!("invalid argument for -n: {}", arg.fg(next_color())),
          )
        })?);
      }
      Opt::ShortWithArg('u', arg) => {
        opts.procs = Some(arg.parse().map_err(|_| {
          ShErr::simple(
            ShErrKind::ParseErr,
            format!("invalid argument for -u: {}", arg.fg(next_color())),
          )
        })?);
      }
      Opt::ShortWithArg('s', arg) => {
        opts.stack = Some(arg.parse().map_err(|_| {
          ShErr::simple(
            ShErrKind::ParseErr,
            format!("invalid argument for -s: {}", arg.fg(next_color())),
          )
        })?);
      }
      Opt::ShortWithArg('c', arg) => {
        opts.core = Some(arg.parse().map_err(|_| {
          ShErr::simple(
            ShErrKind::ParseErr,
            format!("invalid argument for -c: {}", arg.fg(next_color())),
          )
        })?);
      }
      Opt::ShortWithArg('v', arg) => {
        opts.vmem = Some(arg.parse().map_err(|_| {
          ShErr::simple(
            ShErrKind::ParseErr,
            format!("invalid argument for -v: {}", arg.fg(next_color())),
          )
        })?);
      }
      o => {
        return Err(ShErr::simple(
          ShErrKind::ParseErr,
          format!("invalid option: {}", o.fg(next_color())),
        ));
      }
    }
  }

  Ok(opts)
}

pub fn ulimit(node: Node) -> ShResult<()> {
  let span = node.get_span();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (_, opts) =
    get_opts_from_tokens_strict(argv, &ulimit_opt_spec()).promote_err(span.clone())?;
  let ulimit_opts = get_ulimit_opts(&opts).promote_err(span.clone())?;

  if let Some(fds) = ulimit_opts.fds {
    let (_, hard) = getrlimit(Resource::RLIMIT_NOFILE).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to get file descriptor limit: {}", e),
      )
    })?;
    setrlimit(Resource::RLIMIT_NOFILE, fds, hard).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to set file descriptor limit: {}", e),
      )
    })?;
  }
  if let Some(procs) = ulimit_opts.procs {
    let (_, hard) = getrlimit(Resource::RLIMIT_NPROC).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to get process limit: {}", e),
      )
    })?;
    setrlimit(Resource::RLIMIT_NPROC, procs, hard).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to set process limit: {}", e),
      )
    })?;
  }
  if let Some(stack) = ulimit_opts.stack {
    let (_, hard) = getrlimit(Resource::RLIMIT_STACK).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to get stack size limit: {}", e),
      )
    })?;
    setrlimit(Resource::RLIMIT_STACK, stack, hard).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to set stack size limit: {}", e),
      )
    })?;
  }
  if let Some(core) = ulimit_opts.core {
    let (_, hard) = getrlimit(Resource::RLIMIT_CORE).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to get core dump size limit: {}", e),
      )
    })?;
    setrlimit(Resource::RLIMIT_CORE, core, hard).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to set core dump size limit: {}", e),
      )
    })?;
  }
  if let Some(vmem) = ulimit_opts.vmem {
    let (_, hard) = getrlimit(Resource::RLIMIT_AS).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to get virtual memory limit: {}", e),
      )
    })?;
    setrlimit(Resource::RLIMIT_AS, vmem, hard).map_err(|e| {
      ShErr::at(
        ShErrKind::ExecFail,
        span.clone(),
        format!("failed to set virtual memory limit: {}", e),
      )
    })?;
  }

  state::set_status(0);
  Ok(())
}

pub fn umask_builtin(node: Node) -> ShResult<()> {
  let span = node.get_span();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, opts) = get_opts_from_tokens_strict(
    argv,
    &[OptSpec {
      opt: Opt::Short('S'),
      takes_arg: OptArg::None,
    }],
  )?;
  let argv = &argv[1..]; // skip command name

  let old = umask(Mode::empty());
  umask(old);
  let mut old_bits = old.bits();

  if !argv.is_empty() {
    if argv.len() > 1 {
      return Err(ShErr::at(
        ShErrKind::ParseErr,
        span.clone(),
        format!("umask takes at most one argument, got {}", argv.len()),
      ));
    }
    let (ref raw, _) = argv[0];
    if raw.chars().any(|c| c.is_ascii_digit()) {
      let mode_raw = u32::from_str_radix(raw, 8).map_err(|_| {
        ShErr::at(
          ShErrKind::ParseErr,
          span.clone(),
          format!("invalid numeric umask: {}", raw.fg(next_color())),
        )
      })?;

      let mode = Mode::from_bits(mode_raw).ok_or_else(|| {
        ShErr::at(
          ShErrKind::ParseErr,
          span.clone(),
          format!("invalid umask value: {}", raw.fg(next_color())),
        )
      })?;

      umask(mode);
    } else {
      let parts = raw.split(',');

      for part in parts {
        if let Some((who, bits)) = part.split_once('=') {
          let mut new_bits = 0;
          if bits.contains('r') {
            new_bits |= 4;
          }
          if bits.contains('w') {
            new_bits |= 2;
          }
          if bits.contains('x') {
            new_bits |= 1;
          }

          for ch in who.chars() {
            match ch {
              'o' => {
                old_bits &= !0o7;
                old_bits |= !new_bits & 0o7;
              }
              'g' => {
                old_bits &= !(0o7 << 3);
                old_bits |= (!new_bits & 0o7) << 3;
              }
              'u' => {
                old_bits &= !(0o7 << 6);
                old_bits |= (!new_bits & 0o7) << 6;
              }
              'a' => {
                let denied = !new_bits & 0o7;
                old_bits = denied | (denied << 3) | (denied << 6);
              }
              _ => {
                return Err(ShErr::at(
                  ShErrKind::ParseErr,
                  span.clone(),
                  format!("invalid umask 'who' character: {}", ch.fg(next_color())),
                ));
              }
            }
          }

          umask(Mode::from_bits_truncate(old_bits));
        } else if let Some((who, bits)) = part.split_once('+') {
          let mut new_bits = 0;
          if bits.contains('r') {
            new_bits |= 4;
          }
          if bits.contains('w') {
            new_bits |= 2;
          }
          if bits.contains('x') {
            new_bits |= 1;
          }

          for ch in who.chars() {
            match ch {
              'o' => {
                old_bits &= !(new_bits & 0o7);
              }
              'g' => {
                old_bits &= !((new_bits & 0o7) << 3);
              }
              'u' => {
                old_bits &= !((new_bits & 0o7) << 6);
              }
              'a' => {
                let mask = new_bits & 0o7;
                old_bits &= !(mask | (mask << 3) | (mask << 6));
              }
              _ => {
                return Err(ShErr::at(
                  ShErrKind::ParseErr,
                  span.clone(),
                  format!("invalid umask 'who' character: {}", ch.fg(next_color())),
                ));
              }
            }
          }

          umask(Mode::from_bits_truncate(old_bits));
        } else if let Some((who, bits)) = part.split_once('-') {
          let mut new_bits = 0;
          if bits.contains('r') {
            new_bits |= 4;
          }
          if bits.contains('w') {
            new_bits |= 2;
          }
          if bits.contains('x') {
            new_bits |= 1;
          }

          for ch in who.chars() {
            match ch {
              'o' => {
                old_bits |= new_bits & 0o7;
              }
              'g' => {
                old_bits |= (new_bits << 3) & (0o7 << 3);
              }
              'u' => {
                old_bits |= (new_bits << 6) & (0o7 << 6);
              }
              'a' => {
                old_bits |= (new_bits | (new_bits << 3) | (new_bits << 6)) & 0o777;
              }
              _ => {
                return Err(ShErr::at(
                  ShErrKind::ParseErr,
                  span.clone(),
                  format!("invalid umask 'who' character: {}", ch.fg(next_color())),
                ));
              }
            }
          }

          umask(Mode::from_bits_truncate(old_bits));
        } else {
          return Err(ShErr::at(
            ShErrKind::ParseErr,
            span.clone(),
            format!("invalid symbolic umask part: {}", part.fg(next_color())),
          ));
        }
      }
    }
  } else if !opts.is_empty() {
    let u = (old_bits >> 6) & 0o7;
    let g = (old_bits >> 3) & 0o7;
    let o = old_bits & 0o7;
    let mut u_str = String::from("u=");
    let mut g_str = String::from("g=");
    let mut o_str = String::from("o=");
    let stuff = [(u, &mut u_str), (g, &mut g_str), (o, &mut o_str)];
    for (bits, out) in stuff.into_iter() {
      if bits & 4 == 0 {
        out.push('r');
      }
      if bits & 2 == 0 {
        out.push('w');
      }
      if bits & 1 == 0 {
        out.push('x');
      }
    }

    let msg = [u_str, g_str, o_str].join(",");
    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, msg.as_bytes())?;
    write(stdout, b"\n")?;
  } else {
    let raw = format!("{:04o}\n", old_bits);

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, raw.as_bytes())?;
  }

  state::set_status(0);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::get_ulimit_opts;
  use crate::getopt::Opt;
  use crate::state;
  use crate::testutil::{TestGuard, test_input};
  use nix::sys::resource::{Resource, getrlimit};
  use nix::sys::stat::{Mode, umask};

  // ===================== Pure: option parsing =====================

  #[test]
  fn parse_fds() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('n', "1024".into())]).unwrap();
    assert_eq!(opts.fds, Some(1024));
  }

  #[test]
  fn parse_procs() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('u', "512".into())]).unwrap();
    assert_eq!(opts.procs, Some(512));
  }

  #[test]
  fn parse_stack() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('s', "8192".into())]).unwrap();
    assert_eq!(opts.stack, Some(8192));
  }

  #[test]
  fn parse_core() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('c', "0".into())]).unwrap();
    assert_eq!(opts.core, Some(0));
  }

  #[test]
  fn parse_vmem() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('v', "100000".into())]).unwrap();
    assert_eq!(opts.vmem, Some(100000));
  }

  #[test]
  fn parse_multiple() {
    let opts = get_ulimit_opts(&[
      Opt::ShortWithArg('n', "256".into()),
      Opt::ShortWithArg('c', "0".into()),
    ])
    .unwrap();
    assert_eq!(opts.fds, Some(256));
    assert_eq!(opts.core, Some(0));
    assert!(opts.procs.is_none());
  }

  #[test]
  fn parse_non_numeric_fails() {
    let result = get_ulimit_opts(&[Opt::ShortWithArg('n', "abc".into())]);
    assert!(result.is_err());
  }

  #[test]
  fn parse_invalid_option() {
    let result = get_ulimit_opts(&[Opt::Short('z')]);
    assert!(result.is_err());
  }

  // ===================== Integration =====================

  #[test]
  fn ulimit_set_core_zero() {
    let _g = TestGuard::new();
    // Setting core dump size to 0 is always safe
    test_input("ulimit -c 0").unwrap();
    let (soft, _) = getrlimit(Resource::RLIMIT_CORE).unwrap();
    assert_eq!(soft, 0);
  }

  #[test]
  fn ulimit_invalid_flag() {
    let _g = TestGuard::new();
    let result = test_input("ulimit -z 100");
    assert!(result.is_err());
  }

  #[test]
  fn ulimit_non_numeric_value() {
    let _g = TestGuard::new();
    let result = test_input("ulimit -n abc");
    assert!(result.is_err());
  }

  #[test]
  fn ulimit_status_zero() {
    let _g = TestGuard::new();
    test_input("ulimit -c 0").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  // ===================== umask =====================

  fn with_umask(mask: u32, f: impl FnOnce()) {
    let saved = umask(Mode::from_bits_truncate(mask));
    f();
    umask(saved);
  }

  #[test]
  fn umask_display_octal() {
    let g = TestGuard::new();
    with_umask(0o022, || {
      test_input("umask").unwrap();
    });
    assert_eq!(g.read_output(), "0022\n");
  }

  #[test]
  fn umask_display_symbolic() {
    let g = TestGuard::new();
    with_umask(0o022, || {
      test_input("umask -S").unwrap();
    });
    assert_eq!(g.read_output(), "u=rwx,g=rx,o=rx\n");
  }

  #[test]
  fn umask_display_symbolic_all_denied() {
    let g = TestGuard::new();
    with_umask(0o777, || {
      test_input("umask -S").unwrap();
    });
    assert_eq!(g.read_output(), "u=,g=,o=\n");
  }

  #[test]
  fn umask_display_symbolic_none_denied() {
    let g = TestGuard::new();
    with_umask(0o000, || {
      test_input("umask -S").unwrap();
    });
    assert_eq!(g.read_output(), "u=rwx,g=rwx,o=rwx\n");
  }

  #[test]
  fn umask_set_octal() {
    let _g = TestGuard::new();
    let saved = umask(Mode::from_bits_truncate(0o022));
    test_input("umask 077").unwrap();
    let cur = umask(saved);
    assert_eq!(cur.bits(), 0o077);
  }

  #[test]
  fn umask_set_symbolic_equals() {
    let _g = TestGuard::new();
    let saved = umask(Mode::from_bits_truncate(0o000));
    test_input("umask u=rwx,g=rx,o=rx").unwrap();
    let cur = umask(saved);
    assert_eq!(cur.bits(), 0o022);
  }

  #[test]
  fn umask_set_symbolic_plus() {
    let _g = TestGuard::new();
    let saved = umask(Mode::from_bits_truncate(0o077));
    test_input("umask g+r").unwrap();
    let cur = umask(saved);
    // 0o077 with g+r (clear read bit in group) → 0o037
    assert_eq!(cur.bits(), 0o037);
  }

  #[test]
  fn umask_set_symbolic_minus() {
    let _g = TestGuard::new();
    let saved = umask(Mode::from_bits_truncate(0o022));
    test_input("umask o-r").unwrap();
    let cur = umask(saved);
    // 0o022 with o-r (set read bit in other) → 0o026
    assert_eq!(cur.bits(), 0o026);
  }

  #[test]
  fn umask_set_symbolic_all() {
    let _g = TestGuard::new();
    let saved = umask(Mode::from_bits_truncate(0o000));
    test_input("umask a=rx").unwrap();
    let cur = umask(saved);
    // a=rx → deny w for all → 0o222
    assert_eq!(cur.bits(), 0o222);
  }

  #[test]
  fn umask_set_symbolic_plus_all() {
    let _g = TestGuard::new();
    let saved = umask(Mode::from_bits_truncate(0o777));
    test_input("umask a+rwx").unwrap();
    let cur = umask(saved);
    assert_eq!(cur.bits(), 0o000);
  }

  #[test]
  fn umask_set_symbolic_minus_all() {
    let _g = TestGuard::new();
    let saved = umask(Mode::from_bits_truncate(0o000));
    test_input("umask a-rwx").unwrap();
    let cur = umask(saved);
    assert_eq!(cur.bits(), 0o777);
  }

  #[test]
  fn umask_invalid_octal() {
    let _g = TestGuard::new();
    let result = test_input("umask 999");
    assert!(result.is_err());
  }

  #[test]
  fn umask_too_many_args() {
    let _g = TestGuard::new();
    let result = test_input("umask 022 077");
    assert!(result.is_err());
  }

  #[test]
  fn umask_invalid_who() {
    let _g = TestGuard::new();
    let result = test_input("umask z=rwx");
    assert!(result.is_err());
  }

  #[test]
  fn umask_status_zero() {
    let _g = TestGuard::new();
    with_umask(0o022, || {
      test_input("umask").unwrap();
    });
    assert_eq!(state::get_status(), 0);
  }
}
