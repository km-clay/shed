use std::time::{Duration, UNIX_EPOCH};

use chrono::Utc;
use chrono_english::{Dialect, Interval, parse_date_string};
use nix::{
  libc::{STDERR_FILENO, STDOUT_FILENO},
  unistd::write,
};
use regex::Regex;

use crate::{
  getopt::{Opt, OptArg, OptSpec, get_opts_from_tokens},
  libsh::error::{ShResult, ShResultExt},
  parse::{NdRule, Node},
  procio::borrow_fd,
  readline::history::{HistEntry, History},
  sherr,
  state::{self},
};

#[derive(Debug, Default, Clone)]
pub struct HistQuery {
  after: Option<String>,
  before: Option<String>,
  contains: Option<String>,
  lines_gt: Option<usize>,
  lines_lt: Option<usize>,
  starts_with: Option<String>,
  matches: Option<String>,
  duration_gt: Option<String>,
  duration_lt: Option<String>,
  limit: Option<usize>,
  specific_ids: Vec<i64>,
  no_numbers: bool,
  reverse: bool,
  json: bool,
  count: bool,
  delete: bool,
  restore: bool,
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

    if let Some(after) = &self.after {
      let ts = parse_date_string(after, Utc::now(), Dialect::Us)
        .map_err(|e| sherr!(ParseErr, "Failed to parse date for --after: {e}"))?;
      conditions.push(format!("timestamp >= ?{idx}"));
      params.push(Box::new(ts.timestamp()));
      idx += 1;
    }
    if let Some(before) = &self.before {
      let ts = parse_date_string(before, Utc::now(), Dialect::Us)
        .map_err(|e| sherr!(ParseErr, "Failed to parse date for --before: {e}"))?;
      conditions.push(format!("timestamp <= ?{idx}"));
      params.push(Box::new(ts.timestamp()));
      idx += 1;
    }
    if let Some(contains) = &self.contains {
      conditions.push(format!("command LIKE ?{idx}"));
      params.push(Box::new(format!("%{contains}%")));
      idx += 1;
    }
    if let Some(prefix) = &self.starts_with {
      conditions.push(format!("command LIKE ?{idx}"));
      params.push(Box::new(format!("{prefix}%")));
      idx += 1;
    }
    if let Some(ceiling) = &self.lines_lt {
      conditions.push(format!(
        "(LENGTH(command) - LENGTH(REPLACE(command, char(10), ''))) + 1 < ?{idx}"
      ));
      params.push(Box::new(*ceiling as i64));
      idx += 1;
    }
    if let Some(floor) = &self.lines_gt {
      conditions.push(format!(
        "(LENGTH(command) - LENGTH(REPLACE(command, char(10), ''))) + 1 > ?{idx}"
      ));
      params.push(Box::new(*floor as i64));
      idx += 1;
    }
    if let Some(duration) = &self.duration_gt {
      let secs = chrono_english::parse_duration(duration)
        .map_err(|e| sherr!(ParseErr, "Failed to parse duration for --longer-than: {e}"))?;
      conditions.push(format!("runtime >= ?{idx}"));
      match secs {
        Interval::Seconds(n) => {
          let dur = Duration::from_secs(n as u64).as_micros();
          params.push(Box::new(dur as i64));
        }
        Interval::Days(n) => {
          let hours = n * 24;
          let dur = Duration::from_secs(hours as u64 * 3600).as_micros();
          params.push(Box::new(dur as i64));
        }
        Interval::Months(n) => {
          let hours = n * 30 * 24;
          let dur = Duration::from_secs(hours as u64 * 3600).as_micros();
          params.push(Box::new(dur as i64));
        }
      }
      idx += 1;
    }
    if let Some(duration) = &self.duration_lt {
      let secs = chrono_english::parse_duration(duration)
        .map_err(|e| sherr!(ParseErr, "Failed to parse duration for --shorter-than: {e}"))?;
      conditions.push(format!("runtime <= ?{idx}"));
      match secs {
        Interval::Seconds(n) => {
          let dur = Duration::from_secs(n as u64).as_micros();
          params.push(Box::new(dur as i64));
        }
        Interval::Days(n) => {
          let hours = n * 24;
          let dur = Duration::from_secs(hours as u64 * 3600).as_micros();
          params.push(Box::new(dur as i64));
        }
        Interval::Months(n) => {
          let hours = n * 30 * 24;
          let dur = Duration::from_secs(hours as u64 * 3600).as_micros();
          params.push(Box::new(dur as i64));
        }
      }
      idx += 1;
    }
    if !self.specific_ids.is_empty() {
      let mut id_strings = vec![];
      for id in &self.specific_ids {
        id_strings.push(format!("id = ?{idx}"));
        params.push(Box::new(*id));
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

    if let Some(pat) = &self.matches {
      match Regex::new(pat) {
        Ok(r) => Ok(
          entries
            .into_iter()
            .filter(|e| r.is_match(e.1.command()))
            .collect(),
        ),
        Err(e) => Err(sherr!(ParseErr, "Invalid regex for --matches: {e}")),
      }
    } else {
      Ok(entries)
    }
  }

  pub fn from_opts(opts: &[Opt]) -> ShResult<Self> {
    let mut new = Self::new();

    for opt in opts {
      match opt {
        Opt::LongWithArg(name, arg) => match name.as_str() {
          "after" => new.after = Some(arg.clone()),
          "before" => new.before = Some(arg.clone()),
          "contains" => new.contains = Some(arg.clone()),
          "starts-with" => new.starts_with = Some(arg.clone()),
          "matches" => new.matches = Some(arg.clone()),
          "duration-gt" => new.duration_gt = Some(arg.clone()),
          "duration-lt" => new.duration_lt = Some(arg.clone()),
          "limit" => new.limit = Some(arg.parse().unwrap_or(usize::MAX)),
          opt @ ("lines-gt" | "lines-lt") => {
            let is_gt = opt == "lines-gt";
            let count = match arg.parse::<usize>() {
              Ok(c) => c,
              Err(e) => return Err(sherr!(ParseErr, "Invalid number for {opt}: {e}")),
            };
            if is_gt {
              new.lines_gt = Some(count);
            } else {
              new.lines_lt = Some(count);
            }
          }
          _ => {}
        },
        Opt::Long(name) => match name.as_str() {
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
    }

    Ok(new)
  }

  pub fn opt_spec() -> [OptSpec; 17] {
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
        opt: Opt::Long("limit".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("count".into()),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Long("json".into()),
        takes_arg: OptArg::None,
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
