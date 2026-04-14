use std::{
  cmp::Ordering, path::PathBuf, time::{Duration, UNIX_EPOCH}
};

use chrono::Utc;
use chrono_english::{Dialect, Interval, parse_date_string};
use nix::{
  libc::{STDERR_FILENO, STDOUT_FILENO},
  unistd::write,
};
use regex::Regex;

use crate::{
  getopt::{Opt, OptArg, OptSpec, get_opts_from_tokens},
  libsh::{
    self,
    error::{ShResult, ShResultExt},
  },
  parse::{NdRule, Node},
  procio::borrow_fd,
  readline::history::{HistEntry, History},
  sherr,
  state::{self},
};

/// Helper macro to reduce repetition when adding conditions to the query. It handles the '--not' logic and parameter binding.
macro_rules! cond {
  ($not:expr, $conditions:expr, $params:expr, $idx:expr, $query:expr, $param:expr) => {
    let mut query = $query;
    if *$not {
      query = format!("NOT ({query})");
    }
    $conditions.push(query);
    $params.push(Box::new($param));
    $idx += 1;
  };
}

fn interval_to_micros(interval: Interval) -> i64 {
  let secs = match interval {
    Interval::Seconds(n) => n as u64,
    Interval::Days(n) => (n * 24 * 3600) as u64,
    Interval::Months(n) => (n * 30 * 24 * 3600) as u64,
  };

  Duration::from_secs(secs).as_micros() as i64
}

#[derive(Debug, Default, Clone)]
pub struct HistQuery {
  after: (Option<String>, bool),
  before: (Option<String>, bool),
  contains: (Option<String>, bool),
  lines_gt: (Option<usize>, bool),
  lines_lt: (Option<usize>, bool),
  starts_with: (Option<String>, bool),
  matches: (Option<String>, bool),
  duration_gt: (Option<String>, bool),
  duration_lt: (Option<String>, bool),
  with_status: (Option<i32>, bool),
  with_token: (Option<String>, bool),
  in_dir: (Option<String>, bool),
  limit: Option<usize>,
  specific_ids: Vec<i64>,
  no_numbers: bool,
  reverse: bool,
  json: bool,
  count: bool,
  delete: bool,
  restore: bool,
  import: Option<String>,
  ex_hist: bool,
}

impl HistQuery {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn execute(&self, hist: &History) -> ShResult<Vec<(i64, HistEntry)>> {
    let mut conditions: Vec<String> = vec![];
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![];
    let mut idx = 1;

    if let (Some(after), not) = &self.after {
      let ts = parse_date_string(after, Utc::now(), Dialect::Us)
        .map_err(|e| sherr!(ParseErr, "Failed to parse date for --after: {e}"))?;
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("timestamp >= ?{idx}"),
        ts.timestamp()
      );
    }
    if let (Some(before), not) = &self.before {
      let ts = parse_date_string(before, Utc::now(), Dialect::Us)
        .map_err(|e| sherr!(ParseErr, "Failed to parse date for --before: {e}"))?;
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("timestamp <= ?{idx}"),
        ts.timestamp()
      );
    }
    if let (Some(contains), not) = &self.contains {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("command LIKE ?{idx}"),
        format!("%{contains}%")
      );
    }
    if let (Some(prefix), not) = &self.starts_with {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("command LIKE ?{idx}"),
        format!("{prefix}%")
      );
    }
    if let (Some(status), not) = &self.with_status {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("status = ?{idx}"),
        *status
      );
    }
    if let (Some(token), not) = &self.with_token {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("token = ?{idx}"),
        token.to_string()
      );
    }
    if let (Some(dir), not) = &self.in_dir {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("cwd LIKE ?{idx}"),
        dir.to_string()
      );
    }
    if let (Some(ceiling), not) = &self.lines_lt {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("(LENGTH(command) - LENGTH(REPLACE(command, char(10), ''))) + 1 < ?{idx}"),
        *ceiling as i64
      );
    }
    if let (Some(floor), not) = &self.lines_gt {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("(LENGTH(command) - LENGTH(REPLACE(command, char(10), ''))) + 1 > ?{idx}"),
        *floor as i64
      );
    }
    if let (Some(duration), not) = &self.duration_gt {
      let secs = chrono_english::parse_duration(duration)
        .map_err(|e| sherr!(ParseErr, "Failed to parse duration for --longer-than: {e}"))?;
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("runtime >= ?{idx}"),
        interval_to_micros(secs)
      );
    }
    if let (Some(duration), not) = &self.duration_lt {
      let secs = chrono_english::parse_duration(duration)
        .map_err(|e| sherr!(ParseErr, "Failed to parse duration for --shorter-than: {e}"))?;
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("runtime <= ?{idx}"),
        interval_to_micros(secs)
      );
    }
    if !self.specific_ids.is_empty() {
      let mut id_strings = vec![];
			let last_id = hist.last_id();

      for id in &self.specific_ids {
				let id = match id.cmp(&0) {
					Ordering::Greater => *id, // positive number, literal ID

					// user gave a negative number or 0
					// negative -> go backwards from end
					// zero -> lands on current command
					_ => last_id + 1 + (*id - 1),
				};

        id_strings.push(format!("id = ?{idx}"));
        params.push(Box::new(id));
        idx += 1;
      }
      conditions.push(format!("({})", id_strings.join(" OR ")))
    }

    let where_clause = if conditions.is_empty() {
      String::new()
    } else {
      format!("WHERE {}", conditions.join(" AND "))
    };

    let limit = self.limit.map(|n| format!("LIMIT {n}")).unwrap_or_default();

    // hardcoding DESC ordering so that limit always starts from the most recent entry
    let query = format!("{where_clause} ORDER BY id DESC {limit}");

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let mut entries = if self.delete {
      hist.delete(&query, &param_refs)?
    } else {
      hist.query(&query, &param_refs)?
    };

    // 'self.reverse' actually means "dont reverse the list" internally
    if !self.reverse {
      entries.reverse();
    }

    match &self.matches {
      (Some(pat), not) => match Regex::new(pat) {
        Ok(r) => Ok(
          entries
            .into_iter()
            .filter(|e| r.is_match(e.1.command()) != *not)
            .collect(),
        ),
        Err(e) => Err(sherr!(ParseErr, "Invalid regex for --matches: {e}")),
      },
      _ => Ok(entries),
    }
  }

  pub fn from_opts(opts: &[Opt]) -> ShResult<Self> {
    let mut new = Self::new();
    let mut negated = false; // '--not' flag flips this for one argument

    for opt in opts {
      match opt {
        Opt::LongWithArg(name, arg) => match name.as_str() {
          "after" => new.after = (Some(arg.clone()), negated),
          "before" => new.before = (Some(arg.clone()), negated),
          "contains" => new.contains = (Some(arg.clone()), negated),
          "starts-with" => new.starts_with = (Some(arg.clone()), negated),
          "matches" => new.matches = (Some(arg.clone()), negated),
          "duration-gt" => new.duration_gt = (Some(arg.clone()), negated),
          "duration-lt" => new.duration_lt = (Some(arg.clone()), negated),
          "with-token" => new.with_token = (Some(arg.clone()), negated),
          "with-status" => match arg.parse::<i32>() {
            Ok(s) => new.with_status = (Some(s), negated),
            Err(e) => return Err(sherr!(ParseErr, "Invalid status code for {opt}: {e}")),
          },
          "in-dir" => {
            // using canonicalize here allows args like "." to work
            let dir = std::fs::canonicalize(arg)
              .unwrap_or(arg.into())
              .display()
              .to_string();

            new.in_dir = (Some(dir), negated);
          }
          "limit" => new.limit = Some(arg.parse().unwrap_or(usize::MAX)),
          opt @ ("lines-gt" | "lines-lt") => {
            let is_gt = opt == "lines-gt";
            let count = match arg.parse::<usize>() {
              Ok(c) => c,
              Err(e) => return Err(sherr!(ParseErr, "Invalid number for {opt}: {e}")),
            };
            if is_gt {
              new.lines_gt = (Some(count), negated);
            } else {
              new.lines_lt = (Some(count), negated);
            }
          }
          "import" => {
            let path = match arg.as_str() {
              "bash" => {
                let Some(home) = state::get_home() else {
                  return Err(sherr!(
                    ParseErr,
                    "Cannot use {opt} without a valid home directory"
                  ));
                };
                home.join(".bash_history")
              }
              "zsh" => {
                let Some(home) = state::get_home() else {
                  return Err(sherr!(
                    ParseErr,
                    "Cannot use {opt} without a valid home directory"
                  ));
                };
                home.join(".zsh_history")
              }
              "fish" => {
                let Some(home) = state::get_home() else {
                  return Err(sherr!(
                    ParseErr,
                    "Cannot use {opt} without a valid home directory"
                  ));
                };
                let data_dir = dirs::data_dir()
                  .unwrap_or_else(|| PathBuf::from(format!("{}/.local/share", home.display())));
                data_dir.join("fish").join("fish_history")
              }
              _ => PathBuf::from(arg),
            };

            new.import = Some(path.to_string_lossy().to_string());
          }
          _ => {}
        },
        Opt::Long(name) => match name.as_str() {
          "not" => {
            negated = !negated;
            continue;
          }
          "ex" => new.ex_hist = true,
          "count" => new.count = true,
          "delete" => new.delete = true,
          "restore" => new.restore = true,
          "json" => new.json = true,
          _ => {}
        },
        Opt::Short('n') => new.no_numbers = true,
        Opt::Short('r') => new.reverse = true,
        _ => {
          return Err(sherr!(ParseErr, "Unknown option for history: {opt}"));
        }
      }
      negated = false; // reset polarity after each option
    }

    Ok(new)
  }

  pub fn opt_spec() -> [OptSpec; 22] {
    [
      OptSpec {
        opt: Opt::Long("delete".into()),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Long("ex".into()),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Long("restore".into()),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Long("after".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("lines-gt".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("lines-lt".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("before".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("contains".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("starts-with".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("matches".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("duration-gt".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("duration-lt".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("with-status".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("with-token".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("in-dir".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("limit".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("count".into()),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Long("not".into()),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Long("json".into()),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Long("import".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Short('n'),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('r'),
        takes_arg: OptArg::None,
      },
    ]
  }

  pub fn format_entries(&self, entries: &[(i64, HistEntry)]) -> String {
    if self.json {
      let json: serde_json::Value = serde_json::Value::Object(
        entries
          .iter()
          .map(|e| {
            let HistEntry {
              runtime,
              timestamp,
              command,
              cwd,
              status,
              token,
            } = &e.1;
            let mut map = serde_json::Map::new();
            map.insert(
              "runtime".into(),
              serde_json::Value::Number((runtime.as_micros() as i64).into()),
            );
            map.insert(
              "timestamp".into(),
              serde_json::Value::Number(
                (timestamp.duration_since(UNIX_EPOCH).unwrap().as_secs()).into(),
              ),
            );
            map.insert("command".into(), serde_json::Value::String(command.clone()));
            map.insert("cwd".into(), serde_json::Value::String(cwd.clone()));
            map.insert(
              "status".into(),
              serde_json::Value::Number((*status as i64).into()),
            );
            map.insert("token".into(), serde_json::Value::String(token.to_string()));

            (e.0.to_string(), serde_json::Value::Object(map))
          })
          .collect::<serde_json::Map<String, serde_json::Value>>(),
      );

      serde_json::to_string_pretty(&json).unwrap_or_else(|_| {
        let new = Self {
          json: false,
          ..self.clone()
        };
        new.format_entries(entries)
      })
    } else if self.count {
      entries.len().to_string()
    } else {
      entries
        .iter()
        .map(|e| {
          let fmt = if self.no_numbers {
            e.1.command().to_string()
          } else {
            format!("{}\t{}", e.0, e.1.command())
          };
          fmt.replace("\n", "\n\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
    }
  }
}

pub fn hist_builtin(node: Node) -> ShResult<()> {
  let span = node.get_span();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (mut argv, opts) =
    get_opts_from_tokens(argv, &HistQuery::opt_spec()).promote_err(span.clone())?;
  argv.remove(0);
  let mut query = HistQuery::from_opts(&opts).promote_err(span.clone())?;
  let table = if query.ex_hist {
    "ex_history"
  } else {
    "shed_history"
  };
  let hist = History::new(table).promote_err(span.clone())?;

  for (arg, span) in argv {
    let Ok(id) = arg.parse::<i64>() else {
      return Err(sherr!(ParseErr, "Invalid command ID: {arg}").promote(span));
    };
    query.specific_ids.push(id);
  }

  if query.restore {
    let num_restored = hist.restore_backup()?;
    let stderr = borrow_fd(STDERR_FILENO);
    write(
      stderr,
      format!("hist: restored {num_restored} entries from backup.\n").as_bytes(),
    )
    .ok();
    state::set_status(0);
    return Ok(());
  }

  if let Some(ref path) = query.import {
    let entries: Vec<(i64, HistEntry)> = libsh::histimport::import_history(path.into())
      .promote_err(span.clone())?
      .into_iter()
      .enumerate()
      .map(|(i, e)| (i as i64, e))
      .collect();

    let entries_fmt = query.format_entries(&entries);
    let stdout = borrow_fd(STDOUT_FILENO);
    let stderr = borrow_fd(STDERR_FILENO);
    let count = entries.len();

    hist.transaction(|| {
      for (_, entry) in entries {
        hist.push_entry(entry).promote_err(span.clone())?;
      }
      Ok(())
    })?;

    write(stdout, entries_fmt.as_bytes())?;
    write(stdout, b"\n")?;
    write(
      stderr,
      format!("hist: imported {count} entries.\n").as_bytes(),
    )?;

    hist.sort_by_timestamp()?;
    state::set_status(0);
    return Ok(());
  }

  let entries = query.execute(&hist).promote_err(span.clone())?;

  let entries_fmt = query.format_entries(&entries);

  let stdout = borrow_fd(STDOUT_FILENO);

  write(stdout, entries_fmt.as_bytes())?;
  write(stdout, b"\n")?;

  if query.delete {
    let stderr = borrow_fd(STDERR_FILENO);
    let num_deleted = entries.len();
    write(
      stderr,
      format!("hist: deleted {num_deleted} entries.\n").as_bytes(),
    )
    .ok();
  }

  state::set_status(0);
  Ok(())
}
