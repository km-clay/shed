use crate::{
  errln,
  expand::escape,
  procio, sherr,
  state::{Shed, logic::TrapTarget},
  util::{
    self,
    error::{ShResult, ShResultExt},
  },
};

use super::opt::OptSpec;

pub(super) struct Trap;
impl super::Builtin for Trap {
  fn is_special(&self) -> bool {
    true
  }

  fn always_forks(&self) -> bool {
    true
  }

  fn strict_opts(&self) -> bool {
    true
  }

  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("list", b'l'),
      OptSpec::new_short("print", b'p'),
    ]
  }

  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let mut list_signals = false;
    let mut print = false;
    let (arg_vec, opts) = args.take_argv();

    for opt in opts {
      match opt.key() {
        "list" => list_signals = true,
        "print" => print = true,
        _ => return Err(sherr!(ExecFail @ opt.span(), "trap: Unsupported option '{opt}'")),
      }
    }

    if list_signals {
      super::jobctl::list_all_signals();
      return util::with_status(0);
    }

    // Print mode: explicit `-p`, or a bare `trap` with no operands. Any
    // operands are parsed as targets and used to filter the output.
    if print || arg_vec.is_empty() {
      let filter = arg_vec
        .iter()
        .map(|(arg, span)| TrapTarget::parse(arg).promote_err(span.clone()))
        .collect::<ShResult<Vec<_>>>()?;

      Shed::logic(|l| -> ShResult<()> {
        for entry in l.traps() {
          let target = entry.0;
          if filter.is_empty() || filter.contains(target) {
            let mut line = b"trap -- ".to_vec();
            line.extend_from_slice(&escape::shell_quote_bytes(entry.1.as_bytes()));
            line.push(b' ');
            line.extend_from_slice(target.to_string().as_bytes());
            procio::outln_bytes(&line);
          }
        }
        Ok(())
      })?;
      return util::with_status(0);
    }

    // if the first operand is an unsigned integer, every operand is a trap target
    // to reset to its default action. e.g. `trap 0 2`, `trap 2 EXIT`, etc
    if arg_vec
      .first()
      .is_some_and(|(a, _)| a.to_str_lossy().parse::<usize>().is_ok())
    {
      for (arg, span) in arg_vec {
        let target = TrapTarget::parse(&arg).promote_err(span)?;
        Shed::logic_mut(|l| l.remove_trap(target));
      }
      return util::with_status(0);
    }

    if arg_vec.len() == 1 {
      errln!("usage: trap <COMMAND> [SIGNAL...]");
      return util::with_status(1);
    }

    let mut arg_iter = arg_vec.into_iter();

    let command = arg_iter.next().unwrap().0;
    let mut targets = vec![];

    for (arg, span) in arg_iter {
      let target = TrapTarget::parse(&arg).promote_err(span)?;
      targets.push(target);
    }

    for target in targets {
      if &command == "-" {
        Shed::logic_mut(|l| l.remove_trap(target));
      } else {
        Shed::logic_mut(|l| l.insert_trap(target, command.clone()));
      }
    }

    util::with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::logic::TrapTarget;
  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, test_input};
  use nix::sys::signal::Signal;

  // ===================== Pure: TrapTarget parsing =====================

  #[test]
  fn parse_exit() {
    assert_eq!(TrapTarget::parse(&"EXIT".into()).unwrap(), TrapTarget::Exit);
  }

  #[test]
  fn parse_err() {
    assert_eq!(TrapTarget::parse(&"ERR".into()).unwrap(), TrapTarget::Error);
  }

  #[test]
  fn parse_signal_int() {
    assert_eq!(
      TrapTarget::parse(&"INT".into()).unwrap(),
      TrapTarget::Signal(Signal::SIGINT)
    );
  }

  #[test]
  fn parse_signal_term() {
    assert_eq!(
      TrapTarget::parse(&"TERM".into()).unwrap(),
      TrapTarget::Signal(Signal::SIGTERM)
    );
  }

  #[test]
  fn parse_signal_usr1() {
    assert_eq!(
      TrapTarget::parse(&"USR1".into()).unwrap(),
      TrapTarget::Signal(Signal::SIGUSR1)
    );
  }

  #[test]
  fn parse_invalid() {
    assert!(TrapTarget::parse(&"BOGUS".into()).is_err());
  }

  // ===================== Pure: Display round-trip =====================

  #[test]
  fn display_exit() {
    assert_eq!(TrapTarget::Exit.to_string(), "EXIT");
  }

  #[test]
  fn display_err() {
    assert_eq!(TrapTarget::Error.to_string(), "ERR");
  }

  #[test]
  fn display_signal_roundtrip() {
    for name in &[
      "INT", "QUIT", "TERM", "USR1", "USR2", "ALRM", "CHLD", "WINCH",
    ] {
      let target = TrapTarget::parse(&crate::state::vars::VarStr::from(*name)).unwrap();
      assert_eq!(target.to_string(), *name);
    }
  }

  // ===================== Integration: registration =====================

  #[test]
  fn trap_registers_exit() {
    let _g = TestGuard::new();
    test_input("trap 'echo bye' EXIT").unwrap();
    let cmd = Shed::logic(|l| l.get_trap(TrapTarget::Exit));
    assert_eq!(cmd.unwrap(), "echo bye");
  }

  #[test]
  fn trap_registers_signal() {
    let _g = TestGuard::new();
    test_input("trap 'echo caught' INT").unwrap();
    let cmd = Shed::logic(|l| l.get_trap(TrapTarget::Signal(Signal::SIGINT)));
    assert_eq!(cmd.unwrap(), "echo caught");
  }

  #[test]
  fn trap_multiple_signals() {
    let _g = TestGuard::new();
    test_input("trap 'handle' INT TERM").unwrap();
    let int = Shed::logic(|l| l.get_trap(TrapTarget::Signal(Signal::SIGINT)));
    let term = Shed::logic(|l| l.get_trap(TrapTarget::Signal(Signal::SIGTERM)));
    assert_eq!(int.unwrap(), "handle");
    assert_eq!(term.unwrap(), "handle");
  }

  #[test]
  fn trap_remove() {
    let _g = TestGuard::new();
    test_input("trap 'echo hi' EXIT").unwrap();
    assert!(Shed::logic(|l| l.get_trap(TrapTarget::Exit)).is_some());
    test_input("trap - EXIT").unwrap();
    assert!(Shed::logic(|l| l.get_trap(TrapTarget::Exit)).is_none());
  }

  #[test]
  fn trap_display() {
    let guard = TestGuard::new();
    test_input("trap 'echo bye' EXIT").unwrap();
    test_input("trap").unwrap();
    let out = guard.read_output();
    assert!(out.contains("echo bye"));
    assert!(out.contains("EXIT"));
  }

  // ===================== Error cases =====================

  #[test]
  fn trap_single_arg_usage() {
    let _g = TestGuard::new();
    // Single arg prints usage and sets status 1
    test_input("trap 'echo hi'").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn trap_invalid_signal() {
    let _g = TestGuard::new();
    test_input("trap 'echo hi' BOGUS").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Status =====================

  #[test]
  fn trap_status_zero() {
    let _g = TestGuard::new();
    test_input("trap 'echo bye' EXIT").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Flags: -l / -p / -- =====================

  #[test]
  fn trap_list_signals() {
    let guard = TestGuard::new();
    test_input("trap -l").unwrap();
    let out = guard.read_output();
    assert!(out.contains("INT"), "got: {out:?}");
    assert!(out.contains("TERM"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn trap_print_filters_by_target() {
    let guard = TestGuard::new();
    test_input("trap 'echo hi' INT").unwrap();
    test_input("trap 'cleanup' EXIT").unwrap();
    let _ = guard.read_output();
    test_input("trap -p INT").unwrap();
    let out = guard.read_output();
    assert!(out.contains("INT"), "got: {out:?}");
    assert!(
      !out.contains("EXIT"),
      "EXIT should be filtered out: {out:?}"
    );
  }

  #[test]
  fn trap_print_does_not_register() {
    // Regression: `trap -p INT` used to install "-p" as the handler.
    let _g = TestGuard::new();
    test_input("trap -p INT").unwrap();
    assert!(Shed::logic(|l| l.get_trap(TrapTarget::Signal(Signal::SIGINT))).is_none());
  }

  #[test]
  fn trap_double_dash_round_trips() {
    // The listing emits `trap -- <cmd> <target>`; feeding it back must set
    // the trap rather than choke on `--`.
    let _g = TestGuard::new();
    test_input("trap -- 'echo bye' EXIT").unwrap();
    assert_eq!(
      Shed::logic(|l| l.get_trap(TrapTarget::Exit)).unwrap(),
      "echo bye"
    );
  }

  #[test]
  fn trap_unknown_flag_errors() {
    let _g = TestGuard::new();
    test_input("trap -z 'echo hi' INT").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
