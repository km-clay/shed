#![warn(clippy::pedantic)]
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

//! The main entry point for the shell.
//! Handles setup and teardown of the shell's environment, and dispatches the execution logic
/*
MIT License

Copyright (c) 2026 Kyler Clay

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

use state::Shed;

use std::{process::ExitCode, sync::atomic::Ordering};

use expand::expand_keymap;
use keys::KeyEvent;
use keys::KeyMapMatch;
use nix::sys::wait::WaitStatus as WtStat;
use readline::{Hint, LineData, Lines, Prompt, ReadlineEvent, ShedLine};
use rustc_hash::FxHashMap as HashMap;
use rustc_hash::FxHashSet as HashSet;
use signal::QUIT_CODE;
use util::{ShErrKind, ShResult};

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
    Ok(()) => QUIT_CODE.store(Shed::get_status(), Ordering::SeqCst),

    Err(e) => {
      if let ShErrKind::CleanExit(code) = e.kind() {
        // manual `exit` call or something similar
        QUIT_CODE.store(*code, Ordering::SeqCst);
      } else {
        // actual error
        e.print_error();
        if QUIT_CODE.load(Ordering::SeqCst) == 0 {
          QUIT_CODE.store(1, Ordering::SeqCst);
        }
      }
    }
  }

  lifecycle::tear_down()
}
