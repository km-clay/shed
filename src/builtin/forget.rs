//! Forget specific aspects of shell state.
//!
//! Allows the user to selectively forget any of the following:
//! * `Variables`
//! * `Traps`
//! * `Aliases`
//! * `Functions`
//! * `Shopts`
//! * `Keymaps`
//! * `Completions`
//! * `Autocmds`
//! * `Deferred` commands
//!
//! The [`genrc`](super::genrc::Genrc) builtin can be used to dump shell state for round trip sourcing.

use crate::{
  opt, sherr,
  state::{ForgetFlags, Shed},
  util::{self, error::ShResult},
};

use super::{ForkBehavior, opt::OptSpec};

pub(super) struct Forget;
impl super::Builtin for Forget {
  fn fork_behavior(&self) -> ForkBehavior {
    ForkBehavior::Subshell
  }
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("vars" | b'v'),
      opt!("traps" | b't'),
      opt!("aliases" | b'a'),
      opt!("functions" | b'f'),
      opt!("shopts" | b's'),
      opt!("keymaps" | b'k'),
      opt!("comps" | b'c'),
      opt!("autocmds" | b'A'),
      opt!("deferred" | b'd'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if !args.no_arguments() {
      let (_, span) = super::argv::join_raw_arg_iter(args.arguments());
      return Err(
        sherr!(
          ParseErr @ span,
          "unexpected argument(s)"
        )
        .with_note("'forget' does not take positional arguments. see 'help builtin-forget'".into()),
      );
    }
    if args.no_options() {
      // user gave no flags, forget everything
      Shed::forget(ForgetFlags::all());
      return util::with_status(0);
    }

    let mut flags = ForgetFlags::empty();

    for opt in args.options() {
      match opt.key() {
        "vars"/*------*/=> flags |= ForgetFlags::VARS,
        "traps"/*-----*/=> flags |= ForgetFlags::TRAPS,
        "aliases"/*---*/=> flags |= ForgetFlags::ALIASES,
        "functions"/*-*/=> flags |= ForgetFlags::FUNCS,
        "shopts"/*----*/=> flags |= ForgetFlags::SHOPTS,
        "keymaps"/*---*/=> flags |= ForgetFlags::KEYMAPS,
        "comps"/*-----*/=> flags |= ForgetFlags::COMPS,
        "autocmds"/*--*/=> flags |= ForgetFlags::AUTOCMDS,
        "deferred"/*--*/=> flags |= ForgetFlags::DEFERRED,
        _ => return Err(sherr!(ParseErr @ opt.span(), "unrecognized option '{}'", opt)),
      }
    }

    Shed::forget(flags);

    util::with_status(0)
  }
}
