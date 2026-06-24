#![expect(
  clippy::unnecessary_wraps,
  clippy::too_many_lines,
  clippy::cast_sign_loss,
  clippy::cast_possible_wrap,
  clippy::cast_possible_truncation,
  clippy::cast_precision_loss,
  clippy::derivable_impls,
  clippy::tabs_in_doc_comments,
  clippy::while_let_on_iterator,
  clippy::result_large_err
)]

use state::Shed;

use std::{process::ExitCode, sync::atomic::Ordering};

use expand::expand_keymap;
use keys::KeyEvent;
use nix::sys::wait::WaitStatus as WtStat;
use readline::{Hint, LineData, Lines, Prompt, ReadlineEvent, ShedLine};
use rustc_hash::FxHashMap as HashMap;
use rustc_hash::FxHashSet as HashSet;
use signal::QUIT_CODE;
use util::{ShErrKind, ShResult};

pub(crate) mod builtin;
pub(crate) mod eval;
pub(crate) mod expand;
pub(crate) mod input;
pub(crate) mod interactive;
pub(crate) mod lifecycle;
pub(crate) mod procio;
pub(crate) mod readline;
pub(crate) mod signal;
pub(crate) mod socket;
pub(crate) mod state;

pub(crate) mod keys;
pub(crate) mod util;
use keys::KeyMapMatch;

#[cfg(test)]
pub mod tests;

fn main() -> ExitCode {
  let Some(args) = lifecycle::setup() else {
    return ExitCode::SUCCESS;
  };

  match input::dispatch_input(args) {
    Ok(()) => QUIT_CODE.store(Shed::get_status(), Ordering::SeqCst),

    Err(e) => {
      if let ShErrKind::CleanExit(code) = e.kind() {
        QUIT_CODE.store(*code, Ordering::SeqCst);
      } else {
        e.print_error();
        if QUIT_CODE.load(Ordering::SeqCst) == 0 {
          QUIT_CODE.store(1, Ordering::SeqCst);
        }
      }
    }
  }

  lifecycle::tear_down()
}
