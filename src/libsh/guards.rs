use std::cell::RefCell;
use std::collections::HashSet;
use std::os::fd::{BorrowedFd, RawFd};

use nix::sys::termios::{self, LocalFlags, Termios, tcgetattr, tcsetattr};
use nix::unistd::isatty;
use scopeguard::guard;

thread_local! {
  static ORIG_TERMIOS: RefCell<Option<Termios>> = const { RefCell::new(None) };
}

use crate::parse::lex::Span;
use crate::procio::{IoFrame, borrow_fd};
use crate::readline::term::get_win_size;
use crate::state::write_vars;

use super::sys::TTY_FILENO;

// ============================================================================
// ScopeGuard — RAII variable scope management
// ============================================================================

pub fn scope_guard(args: Option<Vec<(String, Span)>>) -> impl Drop {
  let argv = args.map(|a| a.into_iter().map(|(s, _)| s).collect::<Vec<_>>());
  write_vars(|v| v.descend(argv));
  guard((), |_| {
    write_vars(|v| v.ascend());
  })
}

pub fn shared_scope_guard() -> impl Drop {
  write_vars(|v| v.descend(None));
  guard((), |_| {
    write_vars(|v| v.ascend());
  })
}

// ============================================================================
// VarCtxGuard — RAII variable context cleanup
// ============================================================================

pub fn var_ctx_guard(
  vars: HashSet<String>,
) -> scopeguard::ScopeGuard<HashSet<String>, impl FnOnce(HashSet<String>)> {
  guard(vars, |vars| {
    write_vars(|v| {
      for var in &vars {
        v.unset_var(var).ok();
      }
    });
  })
}

// ============================================================================
// RedirGuard — RAII I/O redirection restoration
// ============================================================================

#[derive(Debug)]
pub struct RedirGuard(pub(crate) IoFrame);

impl RedirGuard {
  pub(crate) fn new(frame: IoFrame) -> Self {
    Self(frame)
  }
  pub fn persist(mut self) {
    use nix::unistd::close;
    if let Some(saved) = self.0.saved_io.take() {
      close(saved.0).ok();
      close(saved.1).ok();
      close(saved.2).ok();
    }
  }
}

impl Drop for RedirGuard {
  fn drop(&mut self) {
    self.0.restore().ok();
  }
}

// ============================================================================
// RawModeGuard — RAII terminal raw mode management
// ============================================================================

pub fn raw_mode() -> RawModeGuard {
  let orig = termios::tcgetattr(unsafe { BorrowedFd::borrow_raw(*TTY_FILENO) })
    .expect("Failed to get terminal attributes");
  let mut raw = orig.clone();
  termios::cfmakeraw(&mut raw);
  // Keep ISIG enabled so Ctrl+C/Ctrl+Z still generate signals
  raw.local_flags |= termios::LocalFlags::ISIG;
  // Keep OPOST enabled so \n is translated to \r\n on output
  raw.output_flags |= termios::OutputFlags::OPOST;
  termios::tcsetattr(
    unsafe { BorrowedFd::borrow_raw(*TTY_FILENO) },
    termios::SetArg::TCSANOW,
    &raw,
  )
  .expect("Failed to set terminal to raw mode");

  let (_cols, _rows) = get_win_size(*TTY_FILENO);

  ORIG_TERMIOS.with(|cell| *cell.borrow_mut() = Some(orig.clone()));

  RawModeGuard {
    orig,
    fd: *TTY_FILENO,
  }
}

pub struct RawModeGuard {
  orig: termios::Termios,
  fd: RawFd,
}

impl RawModeGuard {
  /// Disable raw mode temporarily for a specific operation
  pub fn disable_for<F: FnOnce() -> R, R>(&self, func: F) -> R {
    unsafe {
      let fd = BorrowedFd::borrow_raw(self.fd);
      // Temporarily restore the original termios
      termios::tcsetattr(fd, termios::SetArg::TCSANOW, &self.orig)
        .expect("Failed to temporarily disable raw mode");

      // Run the function
      let result = func();

      // Re-enable raw mode
      let mut raw = self.orig.clone();
      termios::cfmakeraw(&mut raw);
      // Keep ISIG enabled so Ctrl+C/Ctrl+Z still generate signals
      raw.local_flags |= termios::LocalFlags::ISIG;
      // Keep OPOST enabled so \n is translated to \r\n on output
      raw.output_flags |= termios::OutputFlags::OPOST;
      termios::tcsetattr(fd, termios::SetArg::TCSANOW, &raw).expect("Failed to re-enable raw mode");

      result
    }
  }

  pub fn with_cooked_mode<F, R>(f: F) -> R
  where
    F: FnOnce() -> R,
  {
    let current = tcgetattr(borrow_fd(*TTY_FILENO)).expect("Failed to get terminal attributes");
    let orig = ORIG_TERMIOS.with(|cell| cell.borrow().clone())
      .expect("with_cooked_mode called before raw_mode()");
    tcsetattr(borrow_fd(*TTY_FILENO), termios::SetArg::TCSANOW, &orig)
      .expect("Failed to restore cooked mode");
    let res = f();
    tcsetattr(borrow_fd(*TTY_FILENO), termios::SetArg::TCSANOW, &current)
      .expect("Failed to restore raw mode");
    res
  }
}

impl Drop for RawModeGuard {
  fn drop(&mut self) {
    unsafe {
      let _ = termios::tcsetattr(
        BorrowedFd::borrow_raw(self.fd),
        termios::SetArg::TCSANOW,
        &self.orig,
      );
    }
  }
}

// ============================================================================
// TermiosGuard — RAII termios state management
// ============================================================================

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
    let mut termios_val = termios::tcgetattr(std::io::stdin()).unwrap();
    termios_val.local_flags &= !LocalFlags::ECHOCTL;
    Self::new(termios_val)
  }
}

impl Drop for TermiosGuard {
  fn drop(&mut self) {
    if let Some(saved) = &self.saved_termios {
      termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, saved).unwrap();
    }
  }
}
