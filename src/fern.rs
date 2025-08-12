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

use crate::libsh::sys::{save_termios, set_termios};
use crate::parse::execute::exec_input;
use crate::prelude::*;
use crate::signal::sig_setup;
use crate::state::source_rc;
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

fn main() {
  kickstart_lazy_evals();
  let args = FernArgs::parse();
  if args.version {
    println!("fern {}", env!("CARGO_PKG_VERSION"));
    return;
  }

  if let Some(path) = args.script {
    run_script(path, args.script_args);
  } else {
    fern_interactive();
  }
}

fn run_script<P: AsRef<Path>>(path: P, args: Vec<String>) {
  let path = path.as_ref();
  if !path.is_file() {
    eprintln!("fern: Failed to open input file: {}", path.display());
    exit(1);
  }
  let Ok(input) = fs::read_to_string(path) else {
    eprintln!("fern: Failed to read input file: {}", path.display());
    exit(1);
  };

  write_vars(|v| v.bpush_arg(path.to_string_lossy().to_string()));
  for arg in args {
    write_vars(|v| v.bpush_arg(arg))
  }

  if let Err(e) = exec_input(input, None) {
    eprintln!("{e}");
    exit(1);
  }
}

fn fern_interactive() {
  save_termios();
  set_termios();
  sig_setup();

  if let Err(e) = source_rc() {
    eprintln!("{e}");
  }

  let mut readline_err_count: u32 = 0;

  loop {
    // Main loop
    let edit_mode = write_shopts(|opt| opt.query("prompt.edit_mode"))
      .unwrap()
      .map(|mode| mode.parse::<FernEditMode>().unwrap_or_default())
      .unwrap();
    let input = match prompt::readline(edit_mode) {
      Ok(line) => {
        readline_err_count = 0;
        line
      }
      Err(e) => {
        eprintln!("{e}");
        readline_err_count += 1;
        if readline_err_count == 20 {
          eprintln!("reached maximum readline error count, exiting");
          break;
        } else {
          continue;
        }
      }
    };

    if let Err(e) = exec_input(input, None) {
      eprintln!("{e}");
    }
  }
}
