use std::{fmt::Display, str::FromStr};

use nix::{
  libc::{STDERR_FILENO, STDOUT_FILENO},
  sys::signal::Signal,
  unistd::write,
};

use crate::{
  builtin::setup_builtin,
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node},
  procio::{IoStack, borrow_fd},
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
      _ => Err(ShErr::simple(
        ShErrKind::ExecFail,
        format!("invalid trap target '{}'", s),
      )),
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

pub fn trap(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(Some(argv), job, Some((io_stack, node.redirs)))?;
  let argv = argv.unwrap();

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
