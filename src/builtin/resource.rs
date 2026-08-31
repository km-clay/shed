use nix::{
  libc::rlim_t,
  sys::{
    resource::{Resource, getrlimit, setrlimit},
    stat::{self, Mode, umask},
  },
};

use crate::{
  eval::lex::Span,
  outln, sherr,
  state::{
    Shed,
    vars::{VarFlags, VarKind},
  },
  util::{
    self,
    error::{ShResult, ShResultExt},
  },
};

use super::opt::{Opt, OptSpec};

struct RLimits {
  fds: Option<u64>,
  procs: Option<u64>,
  stack: Option<u64>,
  core: Option<u64>,
  vmem: Option<u64>,
}

impl RLimits {
  pub(crate) fn from_opts(opts: &[Opt]) -> ShResult<Self> {
    let mut fds = None;
    let mut procs = None;
    let mut stack = None;
    let mut core = None;
    let mut vmem = None;

    for o in opts {
      match o.key() {
        "fds" => {
          let arg = o.value()?;
          fds = Some(
            arg
              .parse::<u64>()
              .map_err(|_| sherr!(ParseErr @ o.span(), "invalid argument for -n: {arg}",))?,
          );
        }
        "procs" => {
          let arg = o.value()?;
          procs = Some(
            arg
              .parse::<u64>()
              .map_err(|_| sherr!(ParseErr @ o.span(), "invalid argument for -u: {arg}",))?,
          );
        }
        "stack" => {
          let arg = o.value()?;
          stack = Some(
            arg
              .parse::<u64>()
              .map_err(|_| sherr!(ParseErr @ o.span(), "invalid argument for -s: {arg}",))?,
          );
        }
        "core" => {
          let arg = o.value()?;
          core = Some(
            arg
              .parse::<u64>()
              .map_err(|_| sherr!(ParseErr @ o.span(), "invalid argument for -c: {arg}",))?,
          );
        }
        "vmem" => {
          let arg = o.value()?;
          vmem = Some(
            arg
              .parse::<u64>()
              .map_err(|_| sherr!(ParseErr @ o.span(), "invalid argument for -v: {arg}",))?,
          );
        }
        _ => {
          return Err(sherr!(ParseErr @ o.span(), "invalid option: {o}"));
        }
      }
    }

    Ok(Self {
      fds,
      procs,
      stack,
      core,
      vmem,
    })
  }
}

pub(super) struct ULimit;
impl super::Builtin for ULimit {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("fds", b'n').argc(1),
      OptSpec::new_short("procs", b'u').argc(1),
      OptSpec::new_short("stack", b's').argc(1),
      OptSpec::new_short("core", b'c').argc(1),
      OptSpec::new_short("vmem", b'v').argc(1),
    ]
  }
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let (_, opts) = args.take_argv();
    let RLimits {
      fds,
      procs,
      stack,
      core,
      vmem,
    } = RLimits::from_opts(&opts).promote_err(span.clone())?;

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
      ulimit_nproc(&span, procs)?;
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

    util::with_status(0)
  }
}

fn parse_rwx(bits: &str) -> stat::mode_t {
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

#[cfg(linux_like)]
fn ulimit_nproc(span: &Span, procs: rlim_t) -> ShResult<()> {
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
  Ok(())
}

#[cfg(not(linux_like))]
fn ulimit_nproc(span: &Span, _procs: rlim_t) -> ShResult<()> {
  Err(sherr!(
    ExecFail @ span.clone(),
    "ulimit -u (max user processes) is not supported on this platform",
  ))
}

fn apply_op(
  old_bits: &mut stat::mode_t,
  op: char,
  new_bits: stat::mode_t,
  shift: stat::mode_t,
  mask: stat::mode_t,
) {
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
  old_bits: &mut stat::mode_t,
  who: &str,
  op: char,
  new_bits: stat::mode_t,
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
          "invalid umask 'who' character: {ch}",
        ));
      }
    }
  }
  Ok(())
}

fn format_symbolic(bits: stat::mode_t) -> String {
  let format_triple = |shift: stat::mode_t, prefix: &str| -> String {
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
    vec![OptSpec::new_short("symbolic", b'S')]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let (arg_vec, opts) = args.take_argv();
    let symbolic = opts.iter().any(|o| o.key() == "symbolic");

    let old = umask(Mode::empty());
    umask(old);
    let mut old_bits = old.bits();

    if let Some((raw, span)) = arg_vec.first() {
      if arg_vec.len() > 1 {
        return Err(sherr!(
          ParseErr @ args.span(),
          "umask takes at most one argument, got {}", arg_vec.len(),
        ));
      }

      if raw.to_str_lossy().chars().any(|c| c.is_ascii_digit()) {
        // Numeric mode: umask 022
        let mode_raw = stat::mode_t::from_str_radix(&raw.to_str_lossy(), 8)
          .map_err(|_| sherr!(ParseErr @ span.clone(), "invalid numeric umask: {raw}"))?;
        // need to use the mode_t type alias
        // since the int size varies between Linux/MacOS
        let mode = Mode::from_bits(mode_raw as stat::mode_t)
          .ok_or_else(|| sherr!(ParseErr @ span.clone(), "invalid umask value: {raw}"))?;
        change_umask(mode.bits());
      } else {
        // Symbolic mode: umask u=rwx,g=rx,o=
        for part in raw.to_str_lossy().split(',') {
          let (who, op, bits) = if let Some((w, b)) = part.split_once('=') {
            (w, '=', b)
          } else if let Some((w, b)) = part.split_once('+') {
            (w, '+', b)
          } else if let Some((w, b)) = part.split_once('-') {
            (w, '-', b)
          } else {
            return Err(sherr!(
              ParseErr @ span.clone(),
              "invalid symbolic umask: {part}",
            ));
          };
          apply_symbolic(&mut old_bits, who, op, parse_rwx(bits), span)?;
        }
        change_umask(old_bits);
      }
    } else if symbolic {
      let symbolic = format_symbolic(old_bits);
      outln!("{symbolic}");
    } else {
      outln!("{old_bits:04o}");
    }

    util::with_status(0)
  }
}

fn change_umask(mask: stat::mode_t) -> Mode {
  Shed::vars_mut(|v| {
    v.set_var(
      "UMASK",
      VarKind::string(format!("{mask:04o}").into()),
      VarFlags::EXPORT | VarFlags::READONLY,
    )
  })
  .ok();
  umask(Mode::from_bits_truncate(mask))
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};
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
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn ulimit_non_numeric_value() {
    let _g = TestGuard::new();
    test_input("ulimit -n abc").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn ulimit_status_zero() {
    let _g = TestGuard::new();
    test_input("ulimit -c 0").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── all branches via "set to current" no-ops ────────────────────
  // setrlimit accepts soft <= hard, so setting to the current soft is
  // always safe and exercises the per-flag setrlimit path without
  // breaking the test process.

  #[test]
  fn ulimit_no_opts_succeeds() {
    let _g = TestGuard::new();
    test_input("ulimit").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn ulimit_set_fds_to_current() {
    let _g = TestGuard::new();
    let (current, _) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
    test_input(format!("ulimit -n {current}")).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(getrlimit(Resource::RLIMIT_NOFILE).unwrap().0, current);
  }

  #[test]
  #[cfg(linux_like)]
  fn ulimit_set_procs_to_current() {
    let _g = TestGuard::new();
    let (current, _) = getrlimit(Resource::RLIMIT_NPROC).unwrap();
    test_input(format!("ulimit -u {current}")).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(getrlimit(Resource::RLIMIT_NPROC).unwrap().0, current);
  }

  #[test]
  fn ulimit_set_stack_to_current() {
    let _g = TestGuard::new();
    let (current, _) = getrlimit(Resource::RLIMIT_STACK).unwrap();
    test_input(format!("ulimit -s {current}")).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(getrlimit(Resource::RLIMIT_STACK).unwrap().0, current);
  }

  #[test]
  fn ulimit_set_vmem_to_current() {
    let _g = TestGuard::new();
    let (current, _) = getrlimit(Resource::RLIMIT_AS).unwrap();
    test_input(format!("ulimit -v {current}")).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(getrlimit(Resource::RLIMIT_AS).unwrap().0, current);
  }

  // ─── observable state change for core (the safe-to-vary one) ────

  #[test]
  fn ulimit_set_core_to_nonzero_value() {
    let _g = TestGuard::new();
    let (_, hard) = getrlimit(Resource::RLIMIT_CORE).unwrap();
    // Pick a value well within the hard cap; 4096 bytes is plenty
    // small that any reasonable system allows it.
    let target = if hard >= 4096 { 4096 } else { hard };
    test_input(format!("ulimit -c {target}")).unwrap();
    assert_eq!(getrlimit(Resource::RLIMIT_CORE).unwrap().0, target);
    // Restore to 0 to leave the test process where it was.
    let _ = nix::sys::resource::setrlimit(Resource::RLIMIT_CORE, 0, hard);
  }

  // ─── multi-flag invocation ───────────────────────────────────────

  #[test]
  fn ulimit_applies_multiple_options_in_one_call() {
    let _g = TestGuard::new();
    let (fds_current, _) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
    test_input(format!("ulimit -c 0 -n {fds_current}")).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(getrlimit(Resource::RLIMIT_CORE).unwrap().0, 0);
    assert_eq!(getrlimit(Resource::RLIMIT_NOFILE).unwrap().0, fds_current);
  }

  // ─── non-numeric value errors for each flag ──────────────────────

  #[test]
  fn ulimit_non_numeric_u() {
    let _g = TestGuard::new();
    test_input("ulimit -u abc").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn ulimit_non_numeric_s() {
    let _g = TestGuard::new();
    test_input("ulimit -s abc").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn ulimit_non_numeric_c() {
    let _g = TestGuard::new();
    test_input("ulimit -c abc").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn ulimit_non_numeric_v() {
    let _g = TestGuard::new();
    test_input("ulimit -v abc").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== umask =====================

  fn with_umask(mask: nix::sys::stat::mode_t, f: impl FnOnce()) {
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
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn umask_too_many_args() {
    let _g = TestGuard::new();
    test_input("umask 022 077").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn umask_invalid_who() {
    let _g = TestGuard::new();
    test_input("umask z=rwx").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn umask_status_zero() {
    let _g = TestGuard::new();
    with_umask(0o022, || {
      test_input("umask").unwrap();
    });
    assert_eq!(state::Shed::get_status(), 0);
  }
}
