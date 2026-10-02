use crate::{
  builtin::BuiltinRouter,
  eval::lex::{Span, Tk},
  sherr,
  util::error::ShResult,
};

use super::opt::Parsed;

mod rename;

pub(super) struct Fs;
impl super::BuiltinRouter for Fs {
  fn default_sub(&self) -> &'static dyn super::Builtin {
    &FsErr
  }
  fn sub_for(&self, word: &[u8]) -> Option<&'static dyn super::Builtin> {
    match word {
      b"rename" => Some(&rename::Rename),
      _ => None,
    }
  }
}
impl super::Builtin for Fs {
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

struct FsErr;
impl super::Builtin for FsErr {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    for (arg, span) in args.arguments() {
      if !arg.starts_with(b"-") {
        return Err(sherr!(ExecFail @ span, "unknown subcommand `{arg}` for `fs`").with_code(2));
      }
    }
    Err(sherr!(ExecFail @ args.cmd_span(), "missing subcommand for `fs`").with_code(2))
  }
}
