use std::convert::Into;

use crate::{
  builtin::BuiltinArgs,
  errln, opt,
  procio::{self, SinkIo},
  sherr,
  util::{
    self,
    error::{ShResult, ShResultExt},
  },
};

use super::super::{Builtin, opt::OptSpec};

use super::{open_history, query::HistQuery};

pub(super) struct HistList;
impl Builtin for HistList {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("no-numbers", b'n'),
      OptSpec::new_short("reverse", b'r'),
      opt!("delete"),
      opt!("ex"),
      opt!("restore"),
      opt!("count"),
      opt!("not"),
      opt!("json"),
      opt!("quoted"),
      opt!("no-dupes"),
      opt!("after", 1),
      opt!("lines-gt", 1),
      opt!("lines-lt", 1),
      opt!("before", 1),
      opt!("ends-with", 1),
      opt!("contains", 1),
      opt!("starts-with", 1),
      opt!("matches", 1),
      opt!("duration-gt", 1),
      opt!("duration-lt", 1),
      opt!("with-status", 1),
      opt!("with-token", 1),
      opt!("in-dir", 1),
      opt!("limit", 1),
    ]
  }
  fn execute(&self, mut args: BuiltinArgs) -> ShResult<()> {
    let     span            = args.span();
    let     (arg_vec, opts) = args.take_argv();
    let mut query           = HistQuery::from_opts(&opts).promote_err(span)?;

    let     needs_write     = query.delete || query.restore;
    let     hist            = open_history(span, query.ex_hist, needs_write)?;

    for (arg, span) in arg_vec {
      let Ok(id) = arg.to_str_lossy().parse::<i64>() else {
        return Err(sherr!(ParseErr @ span, "Invalid command ID: {arg}").with_code(2));
      };
      query.specific_ids.push(id);
    }

    if query.restore {
      let num_restored = hist.restore_backup()?;
      errln!("hist: restored {num_restored} entries from backup.");

      return util::with_status(0);
    }

    let     entries = query.execute(&hist).promote_err(span)?;
    let mut out     = SinkIo(procio::stdout_sink()?);
    query.format_entries(&entries, &mut out).ok();

    if query.delete {
      let num_deleted = entries.len();
      errln!("hist: deleted {num_deleted} entries.");
    }

    util::with_status(0)
  }
}
