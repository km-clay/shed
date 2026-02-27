use std::sync::LazyLock;

use termios::{LocalFlags, Termios};

use crate::prelude::*;

pub static TTY_FILENO: LazyLock<RawFd> = LazyLock::new(|| {
  open("/dev/tty", OFlag::O_RDWR, Mode::empty()).expect("Failed to open /dev/tty")
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
