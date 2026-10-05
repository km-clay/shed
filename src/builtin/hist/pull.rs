use crate::{
  builtin::BuiltinArgs,
  opt,
  readline::{self},
  status_msg,
  util::{self, error::ShResult},
};

use super::super::{Builtin, opt::OptSpec};

use super::open_history;

pub(super) struct HistPull;
impl Builtin for HistPull {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("ex")]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let has_ex = args.has_opt("ex");

    let before = readline::cached_command_count(has_ex);

    let hist = open_history(args.span(), has_ex, true)?;
    hist.refresh_hist_entries();

    let after = readline::cached_command_count(has_ex);

    let pulled = after.saturating_sub(before);
    status_msg!("hist: pulled {pulled} commands");

    util::with_status(0)
  }
}
