#![warn(clippy::pedantic)]
#![warn(unreachable_pub)]
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

//! `shed`, a POSIX shell that focuses on rich interactive features, customizability, and powerful scripting.
//!
//! Copyright © Kyler Clay. Licensed under GPLv3 or later.

use std::process::ExitCode;
use std::sync::atomic::Ordering;

use rustc_hash::FxHashMap as HashMap;
use rustc_hash::FxHashSet as HashSet;

pub(crate) mod autoload;
pub(crate) mod builtin;
pub(crate) mod eval;
pub(crate) mod expand;
pub(crate) mod input;
pub(crate) mod interactive;
pub(crate) mod keys;
pub(crate) mod lifecycle;
pub(crate) mod procio;
pub(crate) mod readline;
pub(crate) mod signal;
pub(crate) mod socket;
pub(crate) mod state;
pub(crate) mod util;

// include embedded functions/completions/help pages
include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

#[cfg(test)]
pub mod tests;

/// The entry point for `shed`.
///
/// Dispatches [`lifecycle::setup()`], [`input::dispatch_input()`], and [`lifecycle::tear_down()`].
fn main() -> ExitCode {
  let Some(args) = lifecycle::setup() else {
    return ExitCode::SUCCESS;
  };

  // each type of input (`-c`, stdin, script path, etc) is handled in `input::dispatch_input()`
  match input::dispatch_input(args) {
    Ok(()) => {
      // if SHOULD_QUIT is already set, the QUIT_CODE has already been handled
      if !signal::SHOULD_QUIT.load(Ordering::SeqCst) {
        signal::QUIT_CODE.store(state::Shed::get_status(), Ordering::SeqCst);
      }
    }

    Err(e) => {
      if let util::error::ShErrKind::CleanExit(code) = e.kind() {
        // manual `exit` call or something similar
        signal::QUIT_CODE.store(*code, Ordering::SeqCst);
      } else {
        // actual error
        e.print_error();
        if signal::QUIT_CODE.load(Ordering::SeqCst) == 0 {
          signal::QUIT_CODE.store(1, Ordering::SeqCst);
        }
      }
    }
  }

  lifecycle::tear_down()
}
