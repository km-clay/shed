use termios::{LocalFlags, Termios};

use crate::{prelude::*, state::write_jobs};
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

pub fn save_termios() {
  unsafe {
    SAVED_TERMIOS = Some(if isatty(std::io::stdin().as_raw_fd()).unwrap() {
      let mut termios = termios::tcgetattr(std::io::stdin()).unwrap();
      termios.local_flags &= !LocalFlags::ECHOCTL;
      termios::tcsetattr(
        std::io::stdin(),
        nix::sys::termios::SetArg::TCSANOW,
        &termios,
      )
      .unwrap();
      Some(termios)
    } else {
      None
    });
  }
}
#[allow(static_mut_refs)]
///Access the saved termios
///
///# Safety
///This function is unsafe because it accesses a public mutable static value.
/// This function should only ever be called after save_termios() has already
/// been called.
pub unsafe fn get_saved_termios() -> Option<Termios> {
  // SAVED_TERMIOS should *only ever* be set once and accessed once
  // Set at the start of the program, and accessed during the exit of the program
  // to reset the termios. Do not use this variable anywhere else
  SAVED_TERMIOS.clone().flatten()
}

/// Set termios to not echo control characters, like ^Z for instance
pub fn set_termios() {
  if isatty(std::io::stdin().as_raw_fd()).unwrap() {
    let mut termios = termios::tcgetattr(std::io::stdin()).unwrap();
    termios.local_flags &= !LocalFlags::ECHOCTL;
    termios::tcsetattr(
      std::io::stdin(),
      nix::sys::termios::SetArg::TCSANOW,
      &termios,
    )
    .unwrap();
  }
}

pub fn sh_quit(code: i32) -> ! {
  write_jobs(|j| {
    for job in j.jobs_mut().iter_mut().flatten() {
      job.killpg(Signal::SIGTERM).ok();
    }
  });
  if let Some(termios) = unsafe { get_saved_termios() } {
    termios::tcsetattr(std::io::stdin(), termios::SetArg::TCSANOW, &termios).unwrap();
  }
  if code == 0 {
    eprintln!("exit");
  } else {
    eprintln!("exit {code}");
  }
  exit(code);
}
