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

use std::process::ExitCode;
use std::sync::atomic::Ordering;

use crate::libsh::error::ShErrKind;
use crate::libsh::sys::TermiosGuard;
use crate::parse::execute::exec_input;
use crate::prelude::*;
use crate::signal::{QUIT_CODE, check_signals, sig_setup, signals_pending};
use crate::state::{source_rc, write_meta};
use clap::Parser;
use shopt::FernEditMode;
use state::{read_vars, write_shopts, write_vars};

#[derive(Parser, Debug)]
struct FernArgs {
  script: Option<String>,

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
  kickstart_lazy_evals();
  let args = FernArgs::parse();
  if args.version {
    println!("fern {}", env!("CARGO_PKG_VERSION"));
    return ExitCode::SUCCESS;
  }

  if let Some(path) = args.script {
    run_script(path, args.script_args);
  } else {
    fern_interactive();
  }

	ExitCode::from(QUIT_CODE.load(Ordering::SeqCst) as u8)
}

fn run_script<P: AsRef<Path>>(path: P, args: Vec<String>) {
  let path = path.as_ref();
  if !path.is_file() {
    eprintln!("fern: Failed to open input file: {}", path.display());
		QUIT_CODE.store(1, Ordering::SeqCst);
		return;
  }
  let Ok(input) = fs::read_to_string(path) else {
    eprintln!("fern: Failed to read input file: {}", path.display());
		QUIT_CODE.store(1, Ordering::SeqCst);
		return;
  };

  write_vars(|v| v.cur_scope_mut().bpush_arg(path.to_string_lossy().to_string()));
  for arg in args {
    write_vars(|v| v.cur_scope_mut().bpush_arg(arg))
  }

  if let Err(e) = exec_input(input, None) {
    eprintln!("{e}");
		match e.kind() {
			ShErrKind::CleanExit(code) => {
				QUIT_CODE.store(*code, Ordering::SeqCst);
			}
			_ => {
				QUIT_CODE.store(1, Ordering::SeqCst);
			}
		}
  }
}

fn fern_interactive() {
	let _termios_guard = TermiosGuard::default(); // sets raw mode, restores termios on drop
  sig_setup();

  if let Err(e) = source_rc() {
    eprintln!("{e}");
  }

  let mut readline_err_count: u32 = 0;

	// Initialize a new string, we will use this to store
	// partial line inputs when read() calls are interrupted by EINTR
	let mut partial_input = String::new();

  'outer: loop {
		while signals_pending() {
			if let Err(e) = check_signals() {
				if let ShErrKind::ClearReadline = e.kind() {
					partial_input.clear();
					if !signals_pending() {
						continue 'outer;
					}
				};
				eprintln!("{e}");
			}
		}
    // Main loop
    let edit_mode = write_shopts(|opt| opt.query("prompt.edit_mode"))
      .unwrap()
      .map(|mode| mode.parse::<FernEditMode>().unwrap_or_default())
      .unwrap();
    let input = match prompt::readline(edit_mode, Some(&partial_input)) {
      Ok(line) => {
        readline_err_count = 0;
				partial_input.clear();
        line
      }
      Err(e) => {
				match e.kind() {
					ShErrKind::ReadlineIntr(partial) => {
						// Did we get signaled? Check signal flags
						// If nothing to worry about, retry the readline with the unfinished input
						while signals_pending() {
							if let Err(e) = check_signals() {
								if let ShErrKind::ClearReadline = e.kind() {
									partial_input.clear();
									if !signals_pending() {
										continue 'outer;
									}
								};
								eprintln!("{e}");
							}
						}
						partial_input = partial.to_string();
						continue;
					}
					ShErrKind::CleanExit(code) => {
						QUIT_CODE.store(*code, Ordering::SeqCst);
						return;
					}
					_ => {
						eprintln!("{e}");
						readline_err_count += 1;
						if readline_err_count == 20 {
							eprintln!("reached maximum readline error count, exiting");
							break;
						} else {
							continue;
						}
					}
				}
      }
    };

		write_meta(|m| m.start_timer());
    if let Err(e) = exec_input(input, None) {
			match e.kind() {
				ShErrKind::CleanExit(code) => {
					QUIT_CODE.store(*code, Ordering::SeqCst);
					return;
				}
				_ => {
					eprintln!("{e}");
				}
			}
    }
		write_meta(|m| m.stop_timer());
  }
}
