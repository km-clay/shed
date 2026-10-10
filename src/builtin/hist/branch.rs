use std::convert::Into;

use crate::{
  builtin::BuiltinArgs,
  outln, sherr,
  state::Shed,
  status_msg,
  util::{
    self,
    error::{ShResult, ShResultExt},
  },
};

use super::super::{Builtin, opt::OptSpec};

use super::open_history;

pub(super) struct HistBranch;
impl Builtin for HistBranch {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("delete", b'd'),
      OptSpec::new_short("force-delete", b'D'),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let force_delete = args.has_opt("force-delete");
    let delete       = force_delete || args.has_opt("delete");
    let name         = args.arguments().next();

    if delete {
      let Some((name, span)) = name else {
        return Err(sherr!(ParseErr @ args.cmd_span(), "missing branch name").with_code(2));
      };
      let hist = open_history(args.span(), false, true)?;
      hist
        .delete_branch(&name.to_str_lossy(), force_delete)
        .promote_err(span)?;
      status_msg!("hist: deleted branch {name}");
    } else if let Some((name, _)) = name {
      // create new branch
      let hist = open_history(args.span(), false, true)?;
      hist.create_branch(&name.to_str_lossy())?;
      status_msg!("hist: created branch {name}");
    } else {
      // no argument, list branches instead
      let hist    = open_history(args.cmd_span(), false, false)?;
      let current = Shed::hist_branch();
      for b in hist.list_branches()? {
        let marker = if b == current { "* " } else { "  " };
        outln!("{marker}{b}");
      }
    }

    util::with_status(0)
  }
}
