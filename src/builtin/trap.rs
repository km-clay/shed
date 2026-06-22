use super::{
  Shed, errln,
  expand::shell_quote,
  getopt::{Opt, OptSpec},
  outln, sherr,
  state::logic::TrapTarget,
  util::{ShResult, ShResultExt, with_status},
};

pub(super) struct Trap;
impl super::Builtin for Trap {
  fn is_special(&self) -> bool {
    true
  }

  fn strict_opts(&self) -> bool {
    true
  }

  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::flag('l'), OptSpec::flag('p')]
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut list_signals = false;
    let mut print = false;
    for opt in &args.opts {
      match opt {
        Opt::Short('l') => list_signals = true,
        Opt::Short('p') => print = true,
        _ => return Err(sherr!(ExecFail, "trap: Unsupported option '{opt}'")),
      }
    }

    if list_signals {
      super::jobctl::list_all_signals();
      return with_status(0);
    }

    // Print mode: explicit `-p`, or a bare `trap` with no operands. Any
    // operands are parsed as targets and used to filter the output.
    if print || args.argv.is_empty() {
      let filter = args
        .argv
        .iter()
        .map(|(arg, span)| arg.parse::<TrapTarget>().promote_err(span.clone()))
        .collect::<ShResult<Vec<_>>>()?;

      Shed::logic(|l| -> ShResult<()> {
        for entry in l.traps() {
          let target = entry.0;
          if filter.is_empty() || filter.contains(&target) {
            let command = shell_quote(entry.1);
            outln!("trap -- {command} {target}");
          }
        }
        Ok(())
      })?;
      return with_status(0);
    }

    if args.argv.len() == 1 {
      errln!("usage: trap <COMMAND> [SIGNAL...]");
      return with_status(1);
    }

    let mut arg_vec = args.argv.into_iter();
    let command = arg_vec.next().unwrap().0;
    let mut targets = vec![];

    for (arg, span) in arg_vec {
      let target = arg.parse::<TrapTarget>().promote_err(span)?;
      targets.push(target);
    }

    for target in targets {
      if &command == "-" {
        Shed::logic_mut(|l| l.remove_trap(target));
      } else {
        Shed::logic_mut(|l| l.insert_trap(target, command.clone()));
      }
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::logic::TrapTarget;
  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, test_input};
  use nix::sys::signal::Signal;
  use std::str::FromStr;

  // ===================== Pure: TrapTarget parsing =====================

  #[test]
  fn parse_exit() {
    assert_eq!(TrapTarget::from_str("EXIT").unwrap(), TrapTarget::Exit);
  }

  #[test]
  fn parse_err() {
    assert_eq!(TrapTarget::from_str("ERR").unwrap(), TrapTarget::Error);
  }

  #[test]
  fn parse_signal_int() {
    assert_eq!(
      TrapTarget::from_str("INT").unwrap(),
      TrapTarget::Signal(Signal::SIGINT)
    );
  }

  #[test]
  fn parse_signal_term() {
    assert_eq!(
      TrapTarget::from_str("TERM").unwrap(),
      TrapTarget::Signal(Signal::SIGTERM)
    );
  }

  #[test]
  fn parse_signal_usr1() {
    assert_eq!(
      TrapTarget::from_str("USR1").unwrap(),
      TrapTarget::Signal(Signal::SIGUSR1)
    );
  }

  #[test]
  fn parse_invalid() {
    assert!(TrapTarget::from_str("BOGUS").is_err());
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
      let target = TrapTarget::from_str(name).unwrap();
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
