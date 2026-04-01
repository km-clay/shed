use std::{fmt::Display, str::FromStr};

use nix::{
  libc::{STDERR_FILENO, STDOUT_FILENO},
  sys::signal::Signal,
  unistd::write,
};

use crate::{
  libsh::error::{ShErr, ShResult},
  parse::{NdRule, Node, execute::prepare_argv},
  procio::borrow_fd,
  sherr,
  state::{self, read_logic, write_logic},
};

#[derive(Debug, Clone, Copy, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub enum TrapTarget {
  Exit,
  Error,
  Signal(Signal),
}

impl FromStr for TrapTarget {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "EXIT" => Ok(TrapTarget::Exit),
      "ERR" => Ok(TrapTarget::Error),

      "INT" => Ok(TrapTarget::Signal(Signal::SIGINT)),
      "QUIT" => Ok(TrapTarget::Signal(Signal::SIGQUIT)),
      "ILL" => Ok(TrapTarget::Signal(Signal::SIGILL)),
      "TRAP" => Ok(TrapTarget::Signal(Signal::SIGTRAP)),
      "ABRT" => Ok(TrapTarget::Signal(Signal::SIGABRT)),
      "BUS" => Ok(TrapTarget::Signal(Signal::SIGBUS)),
      "FPE" => Ok(TrapTarget::Signal(Signal::SIGFPE)),
      "KILL" => Ok(TrapTarget::Signal(Signal::SIGKILL)),
      "USR1" => Ok(TrapTarget::Signal(Signal::SIGUSR1)),
      "SEGV" => Ok(TrapTarget::Signal(Signal::SIGSEGV)),
      "USR2" => Ok(TrapTarget::Signal(Signal::SIGUSR2)),
      "PIPE" => Ok(TrapTarget::Signal(Signal::SIGPIPE)),
      "ALRM" => Ok(TrapTarget::Signal(Signal::SIGALRM)),
      "TERM" => Ok(TrapTarget::Signal(Signal::SIGTERM)),
      "STKFLT" => Ok(TrapTarget::Signal(Signal::SIGSTKFLT)),
      "CHLD" => Ok(TrapTarget::Signal(Signal::SIGCHLD)),
      "CONT" => Ok(TrapTarget::Signal(Signal::SIGCONT)),
      "STOP" => Ok(TrapTarget::Signal(Signal::SIGSTOP)),
      "TSTP" => Ok(TrapTarget::Signal(Signal::SIGTSTP)),
      "TTIN" => Ok(TrapTarget::Signal(Signal::SIGTTIN)),
      "TTOU" => Ok(TrapTarget::Signal(Signal::SIGTTOU)),
      "URG" => Ok(TrapTarget::Signal(Signal::SIGURG)),
      "XCPU" => Ok(TrapTarget::Signal(Signal::SIGXCPU)),
      "XFSZ" => Ok(TrapTarget::Signal(Signal::SIGXFSZ)),
      "VTALRM" => Ok(TrapTarget::Signal(Signal::SIGVTALRM)),
      "PROF" => Ok(TrapTarget::Signal(Signal::SIGPROF)),
      "WINCH" => Ok(TrapTarget::Signal(Signal::SIGWINCH)),
      "IO" => Ok(TrapTarget::Signal(Signal::SIGIO)),
      "PWR" => Ok(TrapTarget::Signal(Signal::SIGPWR)),
      "SYS" => Ok(TrapTarget::Signal(Signal::SIGSYS)),
      _ => Err(sherr!(ExecFail, "invalid trap target '{s}'")),
    }
  }
}

impl Display for TrapTarget {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      TrapTarget::Exit => write!(f, "EXIT"),
      TrapTarget::Error => write!(f, "ERR"),
      TrapTarget::Signal(s) => match s {
        Signal::SIGHUP => write!(f, "HUP"),
        Signal::SIGINT => write!(f, "INT"),
        Signal::SIGQUIT => write!(f, "QUIT"),
        Signal::SIGILL => write!(f, "ILL"),
        Signal::SIGTRAP => write!(f, "TRAP"),
        Signal::SIGABRT => write!(f, "ABRT"),
        Signal::SIGBUS => write!(f, "BUS"),
        Signal::SIGFPE => write!(f, "FPE"),
        Signal::SIGKILL => write!(f, "KILL"),
        Signal::SIGUSR1 => write!(f, "USR1"),
        Signal::SIGSEGV => write!(f, "SEGV"),
        Signal::SIGUSR2 => write!(f, "USR2"),
        Signal::SIGPIPE => write!(f, "PIPE"),
        Signal::SIGALRM => write!(f, "ALRM"),
        Signal::SIGTERM => write!(f, "TERM"),
        Signal::SIGSTKFLT => write!(f, "STKFLT"),
        Signal::SIGCHLD => write!(f, "CHLD"),
        Signal::SIGCONT => write!(f, "CONT"),
        Signal::SIGSTOP => write!(f, "STOP"),
        Signal::SIGTSTP => write!(f, "TSTP"),
        Signal::SIGTTIN => write!(f, "TTIN"),
        Signal::SIGTTOU => write!(f, "TTOU"),
        Signal::SIGURG => write!(f, "URG"),
        Signal::SIGXCPU => write!(f, "XCPU"),
        Signal::SIGXFSZ => write!(f, "XFSZ"),
        Signal::SIGVTALRM => write!(f, "VTALRM"),
        Signal::SIGPROF => write!(f, "PROF"),
        Signal::SIGWINCH => write!(f, "WINCH"),
        Signal::SIGIO => write!(f, "IO"),
        Signal::SIGPWR => write!(f, "PWR"),
        Signal::SIGSYS => write!(f, "SYS"),

        _ => {
          log::warn!("TrapTarget::fmt() : unrecognized signal {}", s);
          Err(std::fmt::Error)
        }
      },
    }
  }
}

pub fn trap(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }

  if argv.is_empty() {
    let stdout = borrow_fd(STDOUT_FILENO);

    return read_logic(|l| -> ShResult<()> {
      for l in l.traps() {
        let target = l.0;
        let command = l.1;
        write(stdout, format!("trap -- '{command}' {target}\n").as_bytes())?;
      }
      Ok(())
    });
  }

  if argv.len() == 1 {
    let stderr = borrow_fd(STDERR_FILENO);
    write(stderr, b"usage: trap <COMMAND> [SIGNAL...]\n")?;
    state::set_status(1);
    return Ok(());
  }

  let mut args = argv.into_iter();

  let command = args.next().unwrap().0;
  let mut targets = vec![];

  while let Some((arg, _)) = args.next() {
    let target = arg.parse::<TrapTarget>()?;
    targets.push(target);
  }

  for target in targets {
    if &command == "-" {
      write_logic(|l| l.remove_trap(target))
    } else {
      write_logic(|l| l.insert_trap(target, command.clone()))
    }
  }

  state::set_status(0);
  Ok(())
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
    let result = test_input("trap 'echo hi' BOGUS");
    assert!(result.is_err());
  }

  // ===================== Status =====================

  #[test]
  fn trap_status_zero() {
    let _g = TestGuard::new();
    test_input("trap 'echo bye' EXIT").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
