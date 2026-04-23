use ariadne::Fmt;
use nix::sys::{
  resource::{Resource, getrlimit, setrlimit},
  stat::{Mode, umask},
};

use crate::{
  getopt::{Opt, OptSpec},
  parse::lex::Span,
  util::error::{ShResult, next_color},
};
use crate::{
  sherr,
  util::{with_status, write_ln_out},
};

pub(super) struct ULimit;
impl super::Builtin for ULimit {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::single_arg('n'), // file descriptors
      OptSpec::single_arg('u'), // max user processes
      OptSpec::single_arg('s'), // stack size
      OptSpec::single_arg('c'), // core dump file size
      OptSpec::single_arg('v'), // virtual memory
    ]
  }
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let mut fds = None;
    let mut procs = None;
    let mut stack = None;
    let mut core = None;
    let mut vmem = None;

    for o in args.opts {
      match o {
        Opt::ShortWithArg('n', arg) => {
          fds = Some(arg.parse::<u64>().map_err(|_| {
            sherr!(
              ParseErr,
              "invalid argument for -n: {}",
              arg.fg(next_color())
            )
          })?);
        }
        Opt::ShortWithArg('u', arg) => {
          procs = Some(arg.parse::<u64>().map_err(|_| {
            sherr!(
              ParseErr,
              "invalid argument for -u: {}",
              arg.fg(next_color())
            )
          })?);
        }
        Opt::ShortWithArg('s', arg) => {
          stack = Some(arg.parse::<u64>().map_err(|_| {
            sherr!(
              ParseErr,
              "invalid argument for -s: {}",
              arg.fg(next_color())
            )
          })?);
        }
        Opt::ShortWithArg('c', arg) => {
          core = Some(arg.parse::<u64>().map_err(|_| {
            sherr!(
              ParseErr,
              "invalid argument for -c: {}",
              arg.fg(next_color())
            )
          })?);
        }
        Opt::ShortWithArg('v', arg) => {
          vmem = Some(arg.parse::<u64>().map_err(|_| {
            sherr!(
              ParseErr,
              "invalid argument for -v: {}",
              arg.fg(next_color())
            )
          })?);
        }
        o => {
          return Err(sherr!(ParseErr, "invalid option: {}", o.fg(next_color()),));
        }
      }
    }

    if let Some(fds) = fds {
      let (_, hard) = getrlimit(Resource::RLIMIT_NOFILE).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to get file descriptor limit: {}", e,
        )
      })?;
      setrlimit(Resource::RLIMIT_NOFILE, fds, hard).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to set file descriptor limit: {}", e,
        )
      })?;
    }
    if let Some(procs) = procs {
      let (_, hard) = getrlimit(Resource::RLIMIT_NPROC).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to get process limit: {}", e,
        )
      })?;
      setrlimit(Resource::RLIMIT_NPROC, procs, hard).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to set process limit: {}", e,
        )
      })?;
    }
    if let Some(stack) = stack {
      let (_, hard) = getrlimit(Resource::RLIMIT_STACK).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to get stack size limit: {}", e,
        )
      })?;
      setrlimit(Resource::RLIMIT_STACK, stack, hard).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to set stack size limit: {}", e,
        )
      })?;
    }
    if let Some(core) = core {
      let (_, hard) = getrlimit(Resource::RLIMIT_CORE).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to get core dump size limit: {}", e,
        )
      })?;
      setrlimit(Resource::RLIMIT_CORE, core, hard).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to set core dump size limit: {}", e,
        )
      })?;
    }
    if let Some(vmem) = vmem {
      let (_, hard) = getrlimit(Resource::RLIMIT_AS).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to get virtual memory limit: {}", e,
        )
      })?;
      setrlimit(Resource::RLIMIT_AS, vmem, hard).map_err(|e| {
        sherr!(
          ExecFail @ span.clone(),
          "failed to set virtual memory limit: {}", e,
        )
      })?;
    }

    with_status(0)
  }
}

fn parse_rwx(bits: &str) -> u32 {
  let mut n = 0;
  if bits.contains('r') {
    n |= 4;
  }
  if bits.contains('w') {
    n |= 2;
  }
  if bits.contains('x') {
    n |= 1;
  }
  n
}

fn apply_op(old_bits: &mut u32, op: char, new_bits: u32, shift: u32, mask: u32) {
  match op {
    '=' => {
      *old_bits &= !mask;
      *old_bits |= (!new_bits & 0o7) << shift;
    }
    '+' => {
      *old_bits &= !((new_bits & 0o7) << shift);
    }
    '-' => {
      *old_bits |= (new_bits << shift) & mask;
    }
    _ => unreachable!(),
  }
}

fn apply_symbolic(
  old_bits: &mut u32,
  who: &str,
  op: char,
  new_bits: u32,
  span: &Span,
) -> ShResult<()> {
  for ch in who.chars() {
    match ch {
      'u' => apply_op(old_bits, op, new_bits, 6, 0o7 << 6),
      'g' => apply_op(old_bits, op, new_bits, 3, 0o7 << 3),
      'o' => apply_op(old_bits, op, new_bits, 0, 0o7),
      'a' => {
        for s in [0, 3, 6] {
          apply_op(old_bits, op, new_bits, s, 0o7 << s);
        }
      }
      _ => {
        return Err(sherr!(
          ParseErr @ span.clone(),
          "invalid umask 'who' character: {}", ch.fg(next_color()),
        ));
      }
    }
  }
  Ok(())
}

fn format_symbolic(bits: u32) -> String {
  let format_triple = |shift: u32, prefix: &str| -> String {
    let b = (bits >> shift) & 0o7;
    let mut s = String::from(prefix);
    if b & 4 == 0 {
      s.push('r');
    }
    if b & 2 == 0 {
      s.push('w');
    }
    if b & 1 == 0 {
      s.push('x');
    }
    s
  };
  [
    format_triple(6, "u="),
    format_triple(3, "g="),
    format_triple(0, "o="),
  ]
  .join(",")
}

pub(super) struct UMask;
impl super::Builtin for UMask {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::flag('S')]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let symbolic = args.opts.iter().any(|o| matches!(o, Opt::Short('S')));

    let old = umask(Mode::empty());
    umask(old);
    let mut old_bits = old.bits();

    if let Some((raw, span)) = args.argv.first() {
      if args.argv.len() > 1 {
        return Err(sherr!(
          ParseErr @ args.span(),
          "umask takes at most one argument, got {}", args.argv.len(),
        ));
      }

      if raw.chars().any(|c| c.is_ascii_digit()) {
        // Numeric mode: umask 022
        let mode_raw = u32::from_str_radix(raw, 8).map_err(
          |_| sherr!(ParseErr @ span.clone(), "invalid numeric umask: {}", raw.fg(next_color())),
        )?;
        let mode = Mode::from_bits(mode_raw).ok_or_else(
          || sherr!(ParseErr @ span.clone(), "invalid umask value: {}", raw.fg(next_color())),
        )?;
        umask(mode);
      } else {
        // Symbolic mode: umask u=rwx,g=rx,o=
        for part in raw.split(',') {
          let (who, op, bits) = if let Some((w, b)) = part.split_once('=') {
            (w, '=', b)
          } else if let Some((w, b)) = part.split_once('+') {
            (w, '+', b)
          } else if let Some((w, b)) = part.split_once('-') {
            (w, '-', b)
          } else {
            return Err(sherr!(
              ParseErr @ span.clone(),
              "invalid symbolic umask: {}", part.fg(next_color()),
            ));
          };
          apply_symbolic(&mut old_bits, who, op, parse_rwx(bits), span)?;
        }
        umask(Mode::from_bits_truncate(old_bits));
      }
    } else if symbolic {
      write_ln_out(format_symbolic(old_bits))?;
    } else {
      write_ln_out(format!("{:04o}", old_bits))?;
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::testutil::{TestGuard, test_input};
  use nix::sys::resource::{Resource, getrlimit};
  use nix::sys::stat::{Mode, umask};

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
    test_input("ulimit -z 100").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn ulimit_non_numeric_value() {
    let _g = TestGuard::new();
    test_input("ulimit -n abc").ok();
    assert_ne!(state::get_status(), 0);
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
    // 0o077 with g+r (clear read bit in group) -> 0o037
    assert_eq!(cur.bits(), 0o037);
  }

  #[test]
  fn umask_set_symbolic_minus() {
    let _g = TestGuard::new();
    let saved = umask(Mode::from_bits_truncate(0o022));
    test_input("umask o-r").unwrap();
    let cur = umask(saved);
    // 0o022 with o-r (set read bit in other) -> 0o026
    assert_eq!(cur.bits(), 0o026);
  }

  #[test]
  fn umask_set_symbolic_all() {
    let _g = TestGuard::new();
    let saved = umask(Mode::from_bits_truncate(0o000));
    test_input("umask a=rx").unwrap();
    let cur = umask(saved);
    // a=rx -> deny w for all -> 0o222
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
    test_input("umask 999").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn umask_too_many_args() {
    let _g = TestGuard::new();
    test_input("umask 022 077").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn umask_invalid_who() {
    let _g = TestGuard::new();
    test_input("umask z=rwx").ok();
    assert_ne!(state::get_status(), 0);
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
