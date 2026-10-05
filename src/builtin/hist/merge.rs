use std::convert::Into;

use crate::{
  builtin::BuiltinArgs,
  readline::MergeResult,
  sherr, status_msg,
  util::{self, error::ShResult},
};

use super::super::Builtin;

use super::open_history;

pub(super) struct HistMerge;
impl Builtin for HistMerge {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let Some((name, span)) = args.arguments().next() else {
      return Err(sherr!(ParseErr @ args.cmd_span(), "missing branch name").with_code(2));
    };
    let hist = open_history(args.span(), false, true)?;
    let other = name.to_str_lossy();
    if !hist.branch_exists(&other)? {
      return Err(sherr!(ParseErr @ span, "branch does not exist: {name}"));
    }
    match hist.merge_branch(&other)? {
      MergeResult::Merged => {
        hist.refresh_hist_entries();
        status_msg!("hist: merged {name}");
      }
      MergeResult::FastForward => {
        hist.refresh_hist_entries();
        status_msg!("hist: fast-forwarded to {name}");
      }
      MergeResult::UpToDate => status_msg!("hist: already up to date"),
    }

    util::with_status(0)
  }
}
