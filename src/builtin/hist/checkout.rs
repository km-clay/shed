use std::convert::Into;

use crate::{
  builtin::BuiltinArgs,
  opt, sherr,
  state::Shed,
  status_msg,
  util::{self, error::ShResult},
};

use super::super::{Builtin, opt::OptSpec};

use super::open_history;

pub(super) struct HistCheckout;
impl Builtin for HistCheckout {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::new_short("branch", b'b'), opt!("orphan")]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let Some((name, span)) = args.arguments().next() else {
      return Err(sherr!(ParseErr @ args.cmd_span(), "missing branch name").with_code(2));
    };
    let name_s        = name.to_string();
    let create_branch = args.has_opt("branch");
    let orphan        = args.has_opt("orphan");

    let hist          = open_history(args.span(), false, true)?;

    if orphan {
      if hist.branch_exists(&name_s)? {
        return Err(sherr!(ParseErr @ span, "branch already exists: {name}"));
      }
      // don't create a branch, that connects it to HEAD
      // just switching the branch name creates an orphaned branch
      Shed::set_hist_branch(name_s);
      return util::with_status(0);
    }

    if !hist.branch_exists(&name_s)? {
      if create_branch {
        hist.create_branch(&name_s)?;
      } else {
        return Err(sherr!(ParseErr @ span, "branch does not exist: {name}"));
      }
    }
    // readline picks up this change in interactive.rs
    Shed::set_hist_branch(name_s);
    status_msg!("hist: switched to branch {name}");

    util::with_status(0)
  }
}
