use crate::{
  builtin::BuiltinRouter,
  errln,
  eval::lex::{Span, Tk},
  sherr,
  state::{cmd, vars::VarStr},
  util::{self, error::ShResult},
};

use super::opt::Parsed;

mod rename;
mod rmdir;

pub(super) struct Fs;
impl super::BuiltinRouter for Fs {
  fn default_sub(&self) -> &'static dyn super::Builtin {
    &FsErr
  }
  #[rustfmt::skip]
  fn sub_for(&self, word: &[u8]) -> Option<&'static dyn super::Builtin> {
    match word {
      b"rename" => Some(&rename::Rename),
      b"rmdir"  => Some(&rmdir::RmDir  ),
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

impl Fs {
  fn subcommands() -> Vec<VarStr> {
    vec![VarStr::from("rename"), VarStr::from("rmdir")]
  }
}

struct FsErr;
impl super::Builtin for FsErr {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    for (arg, span) in args.arguments() {
      if !arg.starts_with(b"-") {
        let suggestions = cmd::check_typo_against(arg.as_bytes(), Fs::subcommands());
        let err = sherr!(ExecFail @ span, "unknown subcommand `{arg}` for `fs`")
          .with_code(2)
          .with_suggestions(&suggestions);
        return Err(err);
      }
    }
    errln!(
      "fs: missing subcommand\nusage: fs <subcommand> ...\n\
       \n  rename <from> <to>   rename a file within one filesystem, atomically\
       \n  rmdir <dir> ...      remove empty directories\n\
       \nsee `help fs` for details"
    );
    util::with_status(2)
  }
}
