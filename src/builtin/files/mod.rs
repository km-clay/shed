use crate::{
  builtin::BuiltinRouter,
  errln,
  eval::lex::{Span, Tk},
  sherr,
  state::{cmd, vars::VarStr},
  util::{self, error::ShResult},
};

use super::opt::Parsed;

mod chmod;
mod chown;
mod link;
mod readlink;
mod realpath;
mod rename;
mod rmdir;
mod symlink;
mod truncate;
mod unlink;

pub(super) struct Fs;
impl super::BuiltinRouter for Fs {
  fn default_sub(&self) -> &'static dyn super::Builtin {
    &FsErr
  }
  #[rustfmt::skip]
  fn sub_for(&self, word: &[u8]) -> Option<&'static dyn super::Builtin> {
    match word {
      b"rename"   => Some(&rename::Rename    ),
      b"rmdir"    => Some(&rmdir::RmDir      ),
      b"unlink"   => Some(&unlink::Unlink    ),
      b"link"     => Some(&link::Link        ),
      b"symlink"  => Some(&symlink::SymLink  ),
      b"readlink" => Some(&readlink::ReadLink),
      b"realpath" => Some(&realpath::RealPath),
      b"truncate" => Some(&truncate::Truncate),
      b"chown"    => Some(&chown::ChOwn      ),
      b"chmod"    => Some(&chmod::ChMod      ),
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
  #[rustfmt::skip]
  const SUBCOMMANDS: &[super::SubInfo] = &[
    ("rename",   "<from> <to>",                  "rename a file within one filesystem, atomically"),
    ("rmdir",    "<dir> ...",                    "remove empty directories"),
    ("unlink",   "<file> ...",                   "remove files"),
    ("link",     "<target> <link> ...",          "create hard links to files"),
    ("symlink",  "<target> <link> ...",          "create symbolic links to files"),
    ("readlink", "<link>",                       "print the value of a symbolic link"),
    ("realpath", "<file> ...",                   "print the canonicalized absolute pathname"),
    ("truncate", "<size> <file> ...",            "truncate files to a specified size"),
    ("chown",    "<owner>[:<group>] <file> ...", "change the owner and/or group of files"),
    ("chmod",    "<mode> <file> ...",            "change the permissions of files"),
  ];

  fn subcommands() -> impl Iterator<Item = VarStr> {
    Self::SUBCOMMANDS
      .iter()
      .map(|(name, ..)| VarStr::from(*name))
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
      "fs: missing subcommand\n{}",
      super::sub_usage("fs", Fs::SUBCOMMANDS)
    );
    util::with_status(2)
  }
}
