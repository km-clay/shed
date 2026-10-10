use std::{
  convert::Into,
  fs::OpenOptions,
  path::PathBuf,
  str::FromStr,
  sync::Arc,
  time::{Duration, UNIX_EPOCH},
};

use serde_json::{Map, Value};

use crate::{
  builtin::BuiltinArgs,
  errln,
  eval::lex::Span,
  procio::{self, OsSink, Sink},
  readline::{self, Branch, HistDump, HistEntry, MAIN_HIST_TABLE_NAME, ReflogEntry, Table},
  sherr,
  state::{paths, vars::VarStr},
  status_msg,
  util::{
    self,
    error::{ShResult, ShResultExt},
    random::Uuid,
  },
};

use super::super::{Builtin, opt::OptSpec};

use super::open_history;

pub(super) struct HistImport;
impl Builtin for HistImport {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::new_short("force", b'f')]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let source: Arc<dyn Sink> = if let Some((path, span)) = args.arguments().next() {
      let resolved = Self::resolve_source(&path.to_str_lossy(), span)?;
      let file = OpenOptions::new()
        .read(true)
        .open(&resolved)
        .map_err(|e| sherr!(ExecFail @ span, "failed to open {}: {e}", resolved.display()))?;

      Arc::new(OsSink::new(file.into()))
    } else {
      let stdin = procio::stdin_sink().promote_err(args.cmd_span())?;
      if stdin.isatty() {
        return Err(sherr!(ExecFail @ args.cmd_span(), "no input file given"));
      }
      stdin
    };

    let input: VarStr = procio::drain_sink(&*source)?.into();

    if Self::is_shed_dump(&input.to_str_lossy()) {
      Self::import_shed(&args, &input)
    } else {
      Self::import_other(&args, &input)
    }
  }
}

impl HistImport {
  fn resolve_source(arg: &str, span: Span) -> ShResult<PathBuf> {
    if !matches!(arg, "bash" | "zsh" | "fish") {
      return Ok(PathBuf::from(arg));
    }
    let Some(home) = paths::get_home() else {
      return Err(
        sherr!(ExecFail @ span, "cannot resolve '{arg}' history without a home directory"),
      );
    };
    Ok(match arg {
      "bash" => home.join(".bash_history"),
      "zsh"  => home.join(".zsh_history"),
      "fish" => paths::data_dir()
        .unwrap_or_else(|| PathBuf::from(format!("{}/.local/share", home.display())))
        .join("fish")
        .join("fish_history"),
      _ => unreachable!(),
    })
  }

  fn is_shed_dump(input: &str) -> bool {
    serde_json::from_str::<Value>(input)
      .ok()
      .and_then(|v| v.get("entries").map(Value::is_array))
      .unwrap_or(false)
  }

  fn import_shed(args: &BuiltinArgs, input: &VarStr) -> ShResult<()> {
    let span = args.cmd_span();
    let root: Value = serde_json::from_str(&input.to_str_lossy())
      .map_err(|e| sherr!(ExecFail @ span, "malformed backup: {e}"))?;

    let arr = |key: &str| -> ShResult<Vec<Value>> {
      match root.get(key) {
        Some(Value::Array(a)) => Ok(a.clone()),
        None    => Ok(vec![]),
        Some(_) => Err(sherr!(ExecFail @ span, "malformed backup: '{key}' is not an array")),
      }
    };
    let str_of =
      |o: &Map<String, Value>, k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
    let int_of = |o: &Map<String, Value>, k: &str| o.get(k).and_then(Value::as_i64).unwrap_or(0);

    let mut entries = Vec::new();
    for v in arr("entries")? {
      let Value::Object(o) = v else {
        return Err(sherr!(ExecFail @ span, "malformed backup: entry is not an object"));
      };
      let Some(token) = str_of(&o, "token") else {
        return Err(sherr!(ExecFail @ span, "malformed backup: entry is missing a token"));
      };
      let token = Uuid::from_str(&token)
        .map_err(|_| sherr!(ExecFail @ span, "malformed backup: bad token {token}"))?;

      let ent = HistEntry {
        command: str_of(&o, "command").unwrap_or_default(),
        cwd: str_of(&o, "cwd").unwrap_or_default(),
        status: int_of(&o, "status") as i32,
        timestamp: UNIX_EPOCH + Duration::from_secs(int_of(&o, "timestamp").max(0) as u64),
        runtime: Duration::from_micros(int_of(&o, "runtime").max(0) as u64),
        token,
      };

      entries.push((ent, str_of(&o, "parent"), str_of(&o, "joint")));
    }

    let mut branches = Vec::new();
    for v in arr("branches")? {
      let Value::Object(o) = v else { continue };

      let Some(name) = str_of(&o, "name") else {
        return Err(sherr!(ExecFail @ span, "malformed backup: branch is missing a name"));
      };

      branches.push((name, str_of(&o, "head")));
    }

    let mut reflog = Vec::new();
    for v in arr("reflog")? {
      let Value::Object(o) = v else { continue };

      let ent = ReflogEntry {
        old_head : str_of(&o, "old_head"),
        new_head : str_of(&o, "new_head"),
        op       : str_of(&o, "op").unwrap_or_default(),
        table    : Table::from(MAIN_HIST_TABLE_NAME),
        branch   : Branch::from(str_of(&o, "branch").unwrap_or_default().as_str()),
        timestamp: UNIX_EPOCH + Duration::from_secs(int_of(&o, "timestamp").max(0) as u64),
      };

      reflog.push(ent);
    }

    let count = entries.len();
    let hist  = open_history(span, false, true)?;
    let dump = HistDump {
      entries,
      branches,
      reflog,
    };

    hist
      .restore_dump(&dump, args.has_opt("force"))
      .promote_err(span)?;
    hist.refresh_hist_entries();

    status_msg!("hist: restored {count} entries");
    util::with_status(0)
  }
  fn import_other(args: &BuiltinArgs, input: &VarStr) -> ShResult<()> {
    let span = args.cmd_span();
    let hist = open_history(span, false, true)?;

    let entries: Vec<(i64, HistEntry)> = readline::deserialize_history(&input.to_str_lossy())
      .into_iter()
      .enumerate()
      .map(|(i, e)| ((i as u64).cast_signed(), e))
      .collect();

    let mut count = 0;
    hist.transaction(|conn| {
      for (_, entry) in entries {
        let pushed = hist.push_with(conn, entry).promote_err(span)?;
        count += i32::from(pushed.is_some());
      }
      Ok(())
    })?;

    errln!("hist: imported {count} entries.");

    hist.sort_by_timestamp()?;
    util::with_status(0)
  }
}
