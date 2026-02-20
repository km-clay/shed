use std::sync::LazyLock;

use termios::{LocalFlags, Termios};

use crate::prelude::*;
///
/// The previous state of the terminal options.
///
/// This variable stores the terminal settings at the start of the program and
/// restores them when the program exits. It is initialized exactly once at the
/// start of the program and accessed exactly once at the end of the program. It
/// will not be mutated or accessed under any other circumstances.
///
/// This ended up being necessary because wrapping Termios in a thread-safe way
/// was unreasonably tricky.
///
/// The possible states of this variable are:
/// - `None`: The terminal options have not been set yet (before
///   initialization).
/// - `Some(None)`: There were no terminal options to save (i.e., no terminal
///   input detected).
/// - `Some(Some(Termios))`: The terminal options (as `Termios`) have been
///   saved.
///
/// **Important:** This static variable is mutable and accessed via unsafe code.
/// It is only safe to use because:
/// - It is set once during program startup and accessed once during program
///   exit.
/// - It is not mutated or accessed after the initial setup and final read.
///
/// **Caution:** Future changes to this code should respect these constraints to
/// ensure safety. Modifying or accessing this variable outside the defined
/// lifecycle could lead to undefined behavior.
pub(crate) static mut SAVED_TERMIOS: Option<Option<Termios>> = None;

pub static TTY_FILENO: LazyLock<RawFd> = LazyLock::new(|| {
	open("/dev/tty", OFlag::O_RDWR, Mode::empty())
		.expect("Failed to open /dev/tty")
});

#[derive(Debug)]
pub struct TermiosGuard {
  saved_termios: Option<Termios>,
}

impl TermiosGuard {
  pub fn new(new_termios: Termios) -> Self {
    let mut new = Self {
      saved_termios: None,
    };

    if isatty(*TTY_FILENO).unwrap() {
      let current_termios = termios::tcgetattr(std::io::stdin()).unwrap();
      new.saved_termios = Some(current_termios);

      termios::tcsetattr(
        std::io::stdin(),
        nix::sys::termios::SetArg::TCSANOW,
        &new_termios,
      )
      .unwrap();
    }

    new
  }
}

impl Default for TermiosGuard {
  fn default() -> Self {
    let mut termios = termios::tcgetattr(std::io::stdin()).unwrap();
    termios.local_flags &= !LocalFlags::ECHOCTL;
    Self::new(termios)
  }
}

impl Drop for TermiosGuard {
  fn drop(&mut self) {
    if let Some(saved) = &self.saved_termios {
      termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, saved).unwrap();
    }
  }
}
