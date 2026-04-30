use std::{fmt::Display, str::FromStr};

use nix::sys::signal::Signal;

use crate::{
  errln, expand::as_var_val_display, outln, signal::parse_signal, state::{read_logic, write_logic}, util::{
    error::{ShErr, ShResult, ShResultExt},
    with_status,
  }
};

#[derive(Debug, Clone, Copy, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub enum TrapTarget {
  Exit,
  Error,
  Return,
  Signal(Signal),
}

impl FromStr for TrapTarget {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "EXIT" => Ok(TrapTarget::Exit),
      "RETURN" => Ok(TrapTarget::Return),
      "ERR" => Ok(TrapTarget::Error),
      _ => Ok(TrapTarget::Signal(parse_signal(s)?)),
    }
  }
}

impl Display for TrapTarget {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      TrapTarget::Exit => write!(f, "EXIT"),
      TrapTarget::Return => write!(f, "RETURN"),
      TrapTarget::Error => write!(f, "ERR"),
      TrapTarget::Signal(s) => {
        let name = s.to_string();
        write!(f, "{}", name.strip_prefix("SIG").unwrap_or(&name))
      }
    }
  }
}

pub(super) struct Trap;
impl super::Builtin for Trap {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      read_logic(|l| -> ShResult<()> {
        for l in l.traps() {
          let target = l.0;
          let command = as_var_val_display(l.1);
          outln!("trap -- {command} {target}")?;
        }
        Ok(())
      })?;
      return with_status(0);
    } else if args.argv.len() == 1 {
      errln!("usage: trap <COMMAND> [SIGNAL...]")?;
      return with_status(1);
    }

    let mut argv = args.argv.into_iter();
    let command = argv.next().unwrap().0;
    let mut targets = vec![];

    for (arg, span) in argv {
      let target = arg.parse::<TrapTarget>().promote_err(span)?;
      targets.push(target);
    }

    for target in targets {
      if &command == "-" {
        write_logic(|l| l.remove_trap(target))
      } else {
        write_logic(|l| l.insert_trap(target, command.clone()))
      }
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use super::TrapTarget;
  use crate::state::{self, read_logic};
  use crate::testutil::{TestGuard, test_input};
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
    let cmd = read_logic(|l| l.get_trap(TrapTarget::Exit));
    assert_eq!(cmd.unwrap(), "echo bye");
  }

  #[test]
  fn trap_registers_signal() {
    let _g = TestGuard::new();
    test_input("trap 'echo caught' INT").unwrap();
    let cmd = read_logic(|l| l.get_trap(TrapTarget::Signal(Signal::SIGINT)));
    assert_eq!(cmd.unwrap(), "echo caught");
  }

  #[test]
  fn trap_multiple_signals() {
    let _g = TestGuard::new();
    test_input("trap 'handle' INT TERM").unwrap();
    let int = read_logic(|l| l.get_trap(TrapTarget::Signal(Signal::SIGINT)));
    let term = read_logic(|l| l.get_trap(TrapTarget::Signal(Signal::SIGTERM)));
    assert_eq!(int.unwrap(), "handle");
    assert_eq!(term.unwrap(), "handle");
  }

  #[test]
  fn trap_remove() {
    let _g = TestGuard::new();
    test_input("trap 'echo hi' EXIT").unwrap();
    assert!(read_logic(|l| l.get_trap(TrapTarget::Exit)).is_some());
    test_input("trap - EXIT").unwrap();
    assert!(read_logic(|l| l.get_trap(TrapTarget::Exit)).is_none());
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
    assert_eq!(state::get_status(), 1);
  }

  #[test]
  fn trap_invalid_signal() {
    let _g = TestGuard::new();
    test_input("trap 'echo hi' BOGUS").ok();
    assert_ne!(state::get_status(), 0);
  }

  // ===================== Status =====================

  #[test]
  fn trap_status_zero() {
    let _g = TestGuard::new();
    test_input("trap 'echo bye' EXIT").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
