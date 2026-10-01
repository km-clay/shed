use crate::{
  eval::lex::{Span, Tk},
  sherr,
  util::error::ShResult,
};

use super::{Builtin, BuiltinArgs, BuiltinRouter, opt::Parsed};

mod sleep;
mod timer;

pub(super) struct Chrono;
impl BuiltinRouter for Chrono {
  fn default_sub(&self) -> &'static dyn Builtin {
    &ChronoError
  }

  fn sub_for(&self, word: &[u8]) -> Option<&'static dyn Builtin> {
    match word {
      b"timer" => Some(&timer::Timer),
      b"sleep" => Some(&sleep::Sleep),
      _ => None,
    }
  }
}

impl Builtin for Chrono {
  fn as_router(&self) -> Option<&dyn BuiltinRouter> {
    Some(self)
  }
  fn get_argv_and_opts(&self, cmd_span: Span, argv: &[Tk], no_split: bool) -> ShResult<Parsed> {
    self.route_parse(cmd_span, argv, no_split)
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.dispatch_sub(args)
  }
}

struct ChronoError;
impl Builtin for ChronoError {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    Err(sherr!(ExecFail @ args.cmd_span(), "no subcommand specified for `chrono`"))
  }
}
