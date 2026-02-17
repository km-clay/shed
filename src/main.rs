#![allow(
  clippy::derivable_impls,
  clippy::tabs_in_doc_comments,
  clippy::while_let_on_iterator
)]
pub mod builtin;
pub mod expand;
pub mod getopt;
pub mod jobs;
pub mod libsh;
pub mod parse;
pub mod prelude;
pub mod procio;
pub mod prompt;
pub mod shopt;
pub mod signal;
pub mod state;
#[cfg(test)]
pub mod tests;

use std::os::fd::BorrowedFd;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use nix::libc::STDIN_FILENO;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::read;
use nix::errno::Errno;

use crate::libsh::error::{ShErr, ShErrKind, ShResult};
use crate::parse::execute::exec_input;
use crate::prelude::*;
use crate::prompt::get_prompt;
use crate::prompt::readline::term::raw_mode;
use crate::prompt::readline::{FernVi, ReadlineEvent};
use crate::signal::{QUIT_CODE, check_signals, sig_setup, signals_pending};
use crate::state::{source_rc, write_meta};
use clap::Parser;
use state::{read_vars, write_vars};

#[derive(Parser, Debug)]
struct FernArgs {
  script: Option<String>,

	#[arg(short)]
	command: Option<String>,

  #[arg(trailing_var_arg = true)]
  script_args: Vec<String>,

  #[arg(long)]
  version: bool,
}

/// Force evaluation of lazily-initialized values early in shell startup.
///
/// In particular, this ensures that the variable table is initialized, which
/// populates environment variables from the system. If this initialization is
/// deferred too long, features like prompt expansion may fail due to missing
/// environment variables.
///
/// This function triggers initialization by calling `read_vars` with a no-op
/// closure, which forces access to the variable table and causes its `LazyLock`
/// constructor to run.
fn kickstart_lazy_evals() {
  read_vars(|_| {});
}

fn main() -> ExitCode {
	env_logger::init();
  kickstart_lazy_evals();
  let args = FernArgs::parse();
  if args.version {
    println!("fern {}", env!("CARGO_PKG_VERSION"));
    return ExitCode::SUCCESS;
  }

  if let Err(e) = if let Some(path) = args.script {
    run_script(path, args.script_args)
	} else if let Some(cmd) = args.command {
		exec_input(cmd, None, false)
  } else {
    fern_interactive()
  } {
		eprintln!("fern: {e}");
	};

	ExitCode::from(QUIT_CODE.load(Ordering::SeqCst) as u8)
}

fn run_script<P: AsRef<Path>>(path: P, args: Vec<String>) -> ShResult<()> {
  let path = path.as_ref();
  if !path.is_file() {
    eprintln!("fern: Failed to open input file: {}", path.display());
		QUIT_CODE.store(1, Ordering::SeqCst);
		return Err(ShErr::simple(ShErrKind::CleanExit(1), "input file not found"));
  }
  let Ok(input) = fs::read_to_string(path) else {
    eprintln!("fern: Failed to read input file: {}", path.display());
		QUIT_CODE.store(1, Ordering::SeqCst);
		return Err(ShErr::simple(ShErrKind::CleanExit(1), "failed to read input file"));
  };

  write_vars(|v| v.cur_scope_mut().bpush_arg(path.to_string_lossy().to_string()));
  for arg in args {
    write_vars(|v| v.cur_scope_mut().bpush_arg(arg))
  }

	exec_input(input, None, false)
}

fn fern_interactive() -> ShResult<()> {
  let _raw_mode = raw_mode(); // sets raw mode, restores termios on drop
  sig_setup();

  if let Err(e) = source_rc() {
    eprintln!("{e}");
  }

  // Create readline instance with initial prompt
  let mut readline = match FernVi::new(get_prompt().ok()) {
    Ok(rl) => rl,
    Err(e) => {
      eprintln!("Failed to initialize readline: {e}");
      QUIT_CODE.store(1, Ordering::SeqCst);
			return Err(ShErr::simple(ShErrKind::CleanExit(1), "readline initialization failed"));
    }
  };

  // Main poll loop
  loop {
    // Handle any pending signals
    while signals_pending() {
      if let Err(e) = check_signals() {
        match e.kind() {
          ShErrKind::ClearReadline => {
            // Ctrl+C - clear current input and show new prompt
            readline.reset(get_prompt().ok());
          }
          ShErrKind::CleanExit(code) => {
            QUIT_CODE.store(*code, Ordering::SeqCst);
						return Ok(());
          }
          _ => eprintln!("{e}"),
        }
      }
    }

		readline.print_line()?;

    // Poll for stdin input
    let mut fds = [PollFd::new(
      unsafe { BorrowedFd::borrow_raw(STDIN_FILENO) },
      PollFlags::POLLIN,
    )];

    match poll(&mut fds, PollTimeout::MAX) {
      Ok(_) => {}
      Err(Errno::EINTR) => {
        // Interrupted by signal, loop back to handle it
        continue;
      }
      Err(e) => {
        eprintln!("poll error: {e}");
				break;
      }
    }

    // Check if stdin has data
    if fds[0].revents().is_some_and(|r| r.contains(PollFlags::POLLIN)) {
      let mut buffer = [0u8; 1024];
      match read(STDIN_FILENO, &mut buffer) {
        Ok(0) => {
          // EOF
          break;
        }
        Ok(n) => {
          readline.feed_bytes(&buffer[..n]);
        }
        Err(Errno::EINTR) => {
          // Interrupted, continue to handle signals
          continue;
        }
        Err(e) => {
          eprintln!("read error: {e}");
          break;
        }
      }
    }

    // Process any available input
    match readline.process_input() {
      Ok(ReadlineEvent::Line(input)) => {
        write_meta(|m| m.start_timer());
        if let Err(e) = exec_input(input, None, true) {
          match e.kind() {
            ShErrKind::CleanExit(code) => {
              QUIT_CODE.store(*code, Ordering::SeqCst);
              return Ok(());
            }
            _ => eprintln!("{e}"),
          }
        }
        write_meta(|m| m.stop_timer());

        // Reset for next command with fresh prompt
        readline.reset(get_prompt().ok());
      }
      Ok(ReadlineEvent::Eof) => {
        // Ctrl+D on empty line
        QUIT_CODE.store(0, Ordering::SeqCst);
        return Ok(());
      }
      Ok(ReadlineEvent::Pending) => {
        // No complete input yet, keep polling
      }
      Err(e) => {
        match e.kind() {
          ShErrKind::CleanExit(code) => {
            QUIT_CODE.store(*code, Ordering::SeqCst);
            return Ok(());
          }
          _ => eprintln!("{e}"),
        }
      }
    }
  }

	Ok(())
}
