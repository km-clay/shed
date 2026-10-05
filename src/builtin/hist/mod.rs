use std::{
  convert::Into,
  sync::{Arc, Mutex},
  time::UNIX_EPOCH,
};

use serde_json::{Map, Value};

use crate::{
  builtin::BuiltinRouter,
  eval::lex::Span,
  readline::{HistEntry, History, MAIN_HIST_TABLE_NAME},
  sherr,
  state::{Shed, db},
  sub_command,
  util::error::{ShResult, ShResultExt},
};

use super::{Builtin, SubCommand};

mod branch;
mod checkout;
mod export;
mod import;
mod list;
mod merge;
mod pull;
mod query;

#[cfg(test)]
mod tests;

use branch::HistBranch;
use checkout::HistCheckout;
use export::HistExport;
use import::HistImport;
use list::HistList;
use merge::HistMerge;
use pull::HistPull;

pub(super) fn open_history(span: Span, ex: bool, needs_mutable: bool) -> ShResult<History> {
  let (table, branch) = if ex {
    ("ex_history", "main".into())
  } else {
    (MAIN_HIST_TABLE_NAME, Shed::hist_branch())
  };
  match db::get_db_conn() {
    Some(conn) => History::new(conn, table, &branch).promote_err(span),
    None if needs_mutable => Err(
      sherr!(
        ExecFail,
        "hist: history can't be modified from a pipeline or subshell"
      )
      .promote(span),
    ),
    None => {
      let conn = db::open_db_conn_readonly().promote_err(span)?;
      Ok(History::attach(Arc::new(Mutex::new(conn)), table, &branch))
    }
  }
}

pub(super) fn entry_obj(e: &HistEntry) -> Value {
  let HistEntry {
    runtime,
    timestamp,
    command,
    cwd,
    status,
    token,
  } = e;
  let mut map = Map::new();
  map.insert(
    "runtime".into(),
    Value::Number((runtime.as_micros() as i64).into()),
  );
  map.insert(
    "timestamp".into(),
    Value::Number(
      timestamp
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .into(),
    ),
  );
  map.insert("command".into(), Value::String(command.clone()));
  map.insert("cwd".into(), Value::String(cwd.clone()));
  map.insert("status".into(), Value::Number(i64::from(*status).into()));
  map.insert("token".into(), Value::String(token.to_string()));
  Value::Object(map)
}

pub(super) struct Hist;
impl BuiltinRouter for Hist {
  fn name(&self) -> &'static str {
    "hist"
  }
  fn default_sub(&self) -> Option<&'static dyn Builtin> {
    Some(&HistList)
  }
  fn sub_commands(&self) -> &'static [SubCommand] {
    const SUB_COMMANDS: &[SubCommand] = &[
      sub_command!(&HistList, "list", "[<options>]", "list history entries"),
      sub_command!(
        &HistImport,
        "import",
        "[<file>|bash|zsh|fish]",
        "import history from a file or shell"
      ),
      sub_command!(
        &HistExport,
        "export",
        "[<file>]",
        "export history to a file"
      ),
      sub_command!(
        &HistCheckout,
        "checkout",
        "<branch>",
        "switch to a different history branch"
      ),
      sub_command!(
        &HistMerge,
        "merge",
        "<branch>",
        "merge a history branch into the current branch"
      ),
      sub_command!(
        &HistBranch,
        "branch",
        "[<subcommand>]",
        "manage history branches"
      ),
      sub_command!(
        &HistPull,
        "pull",
        "[<options>]",
        "pull new history entries from the shell"
      ),
    ];

    SUB_COMMANDS
  }
}
