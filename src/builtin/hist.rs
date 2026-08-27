use std::{
  cmp::Ordering,
  convert::Into,
  path::PathBuf,
  sync::{Arc, Mutex},
  time::UNIX_EPOCH,
};

use crate::util::TimeReader;

use crate::{
  HashSet,
  builtin::opt::Opt,
  expand::shell_quote_bytes,
  opt,
  state::{util, vars::VarStr},
  status_msg,
};

use super::{
  Shed, errln,
  opt::OptSpec,
  readline::{HistEntry, History, import_history},
  sherr, state,
  util::{ShResult, ShResultExt, with_status},
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

#[expect(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone)]
pub struct HistQuery {
  after: (Option<VarStr>, bool),
  before: (Option<VarStr>, bool),
  contains: (Option<VarStr>, bool),
  lines_gt: (Option<u64>, bool),
  lines_lt: (Option<u64>, bool),
  starts_with: (Option<VarStr>, bool),
  ends_with: (Option<VarStr>, bool),
  matches: (Option<VarStr>, bool),
  duration_gt: (Option<VarStr>, bool),
  duration_lt: (Option<VarStr>, bool),
  with_status: (Option<i32>, bool),
  with_token: (Option<VarStr>, bool),
  in_dir: (Option<VarStr>, bool),
  limit: Option<u64>,
  specific_ids: Vec<i64>,
  no_numbers: bool,
  no_dupes: bool,
  reverse: bool,
  json: bool,
  quoted: bool,
  pull: bool,
  count: bool,
  delete: bool,
  restore: bool,
  import: Option<VarStr>,
  ex_hist: bool,
}

impl HistQuery {
  pub fn new() -> Self {
    Self::default()
  }

  #[expect(clippy::too_many_lines)]
  pub fn execute(&self, hist: &History) -> ShResult<Vec<(i64, HistEntry)>> {
    let mut conditions: Vec<String> = vec![];
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![];
    let mut idx = 1;

    if let (Some(after), not) = &self.after {
      let ts = TimeReader::interpret(&after.to_str_lossy())
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
      let ts = TimeReader::interpret(&before.to_str_lossy())
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
    if let (Some(prefix), not) = &self.ends_with {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("RTRIM(command) LIKE ?{idx}"),
        format!("%{prefix}")
      );
    }
    if let (Some(contains), not) = &self.contains {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("TRIM(command) LIKE ?{idx}"),
        format!("%{contains}%")
      );
    }
    if let (Some(prefix), not) = &self.starts_with {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("LTRIM(command) LIKE ?{idx}"),
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
        token.clone()
      );
    }
    if let (Some(dir), not) = &self.in_dir {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("cwd LIKE ?{idx}"),
        dir.clone()
      );
    }
    if let (Some(ceiling), not) = &self.lines_lt {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("(LENGTH(command) - LENGTH(REPLACE(command, char(10), ''))) + 1 < ?{idx}"),
        (*ceiling).cast_signed()
      );
    }
    if let (Some(floor), not) = &self.lines_gt {
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("(LENGTH(command) - LENGTH(REPLACE(command, char(10), ''))) + 1 > ?{idx}"),
        (*floor).cast_signed()
      );
    }
    if let (Some(duration), not) = &self.duration_gt {
      let micros = TimeReader::parse_dur(&duration.to_str_lossy())
        .map_err(|e| sherr!(ParseErr, "Failed to parse duration for --longer-than: {e}"))?;
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("runtime >= ?{idx}"),
        micros
      );
    }
    if let (Some(duration), not) = &self.duration_lt {
      let micros = TimeReader::parse_dur(&duration.to_str_lossy())
        .map_err(|e| sherr!(ParseErr, "Failed to parse duration for --shorter-than: {e}"))?;
      cond!(
        not,
        conditions,
        params,
        idx,
        format!("runtime <= ?{idx}"),
        micros
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
      conditions.push(format!("({})", id_strings.join(" OR ")));
    }

    let where_clause = if conditions.is_empty() {
      String::new()
    } else {
      format!("WHERE {}", conditions.join(" AND "))
    };

    let limit = self.limit.map(|n| format!("LIMIT {n}")).unwrap_or_default();

    // hardcoding DESC ordering so that limit always starts from the most recent entry
    let query = format!("{where_clause} ORDER BY id DESC {limit}");

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(AsRef::as_ref).collect();

    let mut entries = hist.query(&query, &param_refs)?;

    if let (Some(pat), not) = &self.matches {
      let re = match Shed::meta_mut(|m| m.get_regex(&pat.to_str_lossy())) {
        Ok(re) => re,
        Err(e) => return Err(sherr!(ParseErr, "{e}")),
      };
      entries.retain(|e| re.is_match(e.1.command()) != *not);
    }

    if self.delete && !entries.is_empty() {
      let ids: Vec<i64> = entries.iter().map(|e| e.0).collect();
      hist.delete_ids(&ids)?;
      hist.refresh_hist_entries();
    }

    // 'self.reverse' means 'print the entries in descending order'
    if !self.reverse {
      // the entries start in descending order. we reverse it
      // so that the more recent ones are at the bottom by default
      entries.reverse();
    }

    Ok(entries)
  }

  pub fn from_opts(opts: &[Opt]) -> ShResult<Self> {
    let mut new = Self::new();
    let mut negated = false; // '--not' flag flips this for one argument
    let value = |opt: &Opt| -> Option<VarStr> { opt.value().ok().map(VarStr::from) };

    for opt in opts {
      match opt.key() {
        "after" => new.after = (value(opt), negated),
        "before" => new.before = (value(opt), negated),
        "contains" => new.contains = (value(opt), negated),
        "starts-with" => new.starts_with = (value(opt), negated),
        "ends-with" => new.ends_with = (value(opt), negated),
        "matches" => new.matches = (value(opt), negated),
        "duration-gt" => new.duration_gt = (value(opt), negated),
        "duration-lt" => new.duration_lt = (value(opt), negated),
        "with-token" => new.with_token = (value(opt), negated),
        "with-status" => {
          let arg = opt.value()?;
          match arg.parse::<i32>() {
            Ok(s) => new.with_status = (Some(s), negated),
            Err(e) => return Err(sherr!(ParseErr, "Invalid status code for {opt}: {e}")),
          }
        }
        "in-dir" => {
          // using canonicalize here allows args like "." to work
          let arg = opt.value()?;
          let dir = std::fs::canonicalize(arg)
            .unwrap_or(arg.into())
            .to_string_lossy()
            .into();

          new.in_dir = (Some(dir), negated);
        }
        "limit" => {
          let arg = opt.value()?;
          new.limit = Some(arg.parse().unwrap_or(u64::MAX));
        }
        opt_key @ ("lines-gt" | "lines-lt") => {
          let is_gt = opt_key == "lines-gt";
          let arg = opt.value()?;
          let count = match arg.parse::<u64>() {
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
          let arg = opt.value()?;
          let path = match arg {
            "bash" => {
              let Some(home) = state::util::get_home() else {
                return Err(sherr!(
                  ParseErr,
                  "Cannot use {opt} without a valid home directory"
                ));
              };
              home.join(".bash_history")
            }
            "zsh" => {
              let Some(home) = state::util::get_home() else {
                return Err(sherr!(
                  ParseErr,
                  "Cannot use {opt} without a valid home directory"
                ));
              };
              home.join(".zsh_history")
            }
            "fish" => {
              let Some(home) = state::util::get_home() else {
                return Err(sherr!(
                  ParseErr,
                  "Cannot use {opt} without a valid home directory"
                ));
              };
              let data_dir = util::data_dir()
                .unwrap_or_else(|| PathBuf::from(format!("{}/.local/share", home.display())));
              data_dir.join("fish").join("fish_history")
            }
            _ => PathBuf::from(arg),
          };

          new.import = Some(path.to_string_lossy().into());
        }
        "not" => {
          negated = !negated;
          continue;
        }
        "ex" => new.ex_hist = true,
        "count" => new.count = true,
        "delete" => new.delete = true,
        "restore" => new.restore = true,
        "json" => new.json = true,
        "quoted" => new.quoted = true,
        "no-dupes" => new.no_dupes = true,
        "pull" => new.pull = true,
        "no-numbers" => new.no_numbers = true,
        "reverse" => new.reverse = true,
        _ => {
          return Err(sherr!(ParseErr, "Unknown option for history: {opt}"));
        }
      }
      negated = false; // reset polarity after each option
    }

    Ok(new)
  }

  pub fn format_entries(
    &self,
    entries: &[(i64, HistEntry)],
    f: &mut impl std::io::Write,
  ) -> std::io::Result<()> {
    // Filters that don't depend on the output format run once, up front, so
    // every renderer below inherits them.
    let entries = self.dedupe(entries);

    if self.count {
      writeln!(f, "{}", entries.len())
    } else if self.json {
      self.format_json(&entries, f)
    } else if self.quoted {
      for (id, entry) in &entries {
        if !self.no_numbers {
          write!(f, "{id} ")?;
        }
        f.write_all(&shell_quote_bytes(entry.command_bytes()))?;
        f.write_all(b"\n")?;
      }

      Ok(())
    } else {
      for (id, entry) in &entries {
        if !self.no_numbers {
          write!(f, "{id}\t")?;
        }
        f.write_all(entry.command_bytes())?;
        f.write_all(b"\n")?;
      }
      Ok(())
    }
  }

  /// Apply `no_dupes`: keep only the most recent entry per command, preserving
  /// chronological order. Just borrows the input when the flag is off.
  fn dedupe<'a>(&self, entries: &'a [(i64, HistEntry)]) -> Vec<&'a (i64, HistEntry)> {
    if !self.no_dupes {
      return entries.iter().collect();
    }
    // Walk newest-first so the kept copy of each command is the latest, then
    // restore chronological order.
    let mut seen: HashSet<&[u8]> = HashSet::default();
    let mut kept: Vec<_> = entries
      .iter()
      .rev()
      .filter(|(_, e)| seen.insert(e.command_bytes()))
      .collect();
    kept.reverse();
    kept
  }

  /// Entries as JSON: an object keyed by id, or (under `no_numbers`, where
  /// there's no id to key on) a plain array of the same objects.
  fn format_json(
    &self,
    entries: &[&(i64, HistEntry)],
    f: &mut impl std::io::Write,
  ) -> std::io::Result<()> {
    use serde_json::Value;
    let entry_obj = |e: &HistEntry| {
      let HistEntry {
        runtime,
        timestamp,
        command,
        cwd,
        status,
        token,
      } = e;
      let mut map = serde_json::Map::new();
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
      map.insert("command".into(), Value::String(command.to_string()));
      map.insert("cwd".into(), Value::String(cwd.to_string()));
      map.insert("status".into(), Value::Number(i64::from(*status).into()));
      map.insert("token".into(), Value::String(token.to_string()));
      Value::Object(map)
    };

    let json = if self.no_numbers {
      Value::Array(entries.iter().map(|(_, e)| entry_obj(e)).collect())
    } else {
      Value::Object(
        entries
          .iter()
          .map(|(id, e)| (id.to_string(), entry_obj(e)))
          .collect(),
      )
    };

    writeln!(f, "{json:#}")
  }
}

pub(super) struct Hist;
impl super::Builtin for Hist {
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
      opt!("pull"),
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
      opt!("import", 1),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let (arg_vec, opts) = args.take_argv();
    let mut query = HistQuery::from_opts(&opts).promote_err(span.clone())?;
    let table = if query.ex_hist {
      "ex_history"
    } else {
      "shed_history"
    };
    let hist = if let Some(conn) = state::util::get_db_conn() {
      History::new(conn, table).promote_err(span.clone())?
    } else {
      if query.delete || query.pull || query.restore || query.import.is_some() {
        return Err(
          sherr!(
            ExecFail,
            "hist: history can't be modified from a pipeline or subshell"
          )
          .promote(span),
        );
      }
      let conn = state::util::open_db_conn_readonly().promote_err(span.clone())?;
      History::attach(Arc::new(Mutex::new(conn)), table)
    };

    for (arg, span) in arg_vec {
      let Ok(id) = arg.to_str_lossy().parse::<i64>() else {
        Shed::set_status(2);
        return Err(sherr!(ParseErr @ span.clone(), "Invalid command ID: {arg}"));
      };
      query.specific_ids.push(id);
    }

    if query.restore {
      let num_restored = hist.restore_backup()?;
      errln!("hist: restored {num_restored} entries from backup.");

      return with_status(0);
    }

    if query.pull {
      let pulled = hist.refresh_hist_entries();
      status_msg!("hist: pulled {pulled} commands");

      return with_status(0);
    }

    if let Some(ref path) = query.import {
      let entries: Vec<(i64, HistEntry)> = import_history(path)
        .promote_err(span.clone())?
        .into_iter()
        .enumerate()
        .map(|(i, e)| ((i as u64).cast_signed(), e))
        .collect();

      Shed::sinks(|s| query.format_entries(&entries, s)).ok();
      let mut count = 0;

      hist.transaction(|conn| {
        for (_, entry) in entries {
          let pushed = hist.push_with(conn, entry).promote_err(span.clone())?;
          count += i32::from(pushed);
        }
        Ok(())
      })?;

      errln!("hist: imported {count} entries.");

      hist.sort_by_timestamp()?;
      return with_status(0);
    }

    let entries = query.execute(&hist).promote_err(span.clone())?;
    Shed::sinks(|s| query.format_entries(&entries, s)).ok();

    if query.delete {
      let num_deleted = entries.len();
      errln!("hist: deleted {num_deleted} entries.");
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{tests::testutil::TestGuard, util};

  fn parse(opts: &[Opt]) -> HistQuery {
    HistQuery::from_opts(opts).expect("from_opts should succeed")
  }

  // ─── Options with an argument → field assignments ────────────────────

  #[test]
  fn opts_after() {
    let q = parse(&[Opt::for_test("after", &["yesterday"])]);
    assert_eq!(q.after, (Some("yesterday".into()), false));
  }

  #[test]
  fn opts_before() {
    let q = parse(&[Opt::for_test("before", &["tomorrow"])]);
    assert_eq!(q.before, (Some("tomorrow".into()), false));
  }

  #[test]
  fn opts_contains() {
    let q = parse(&[Opt::for_test("contains", &["grep"])]);
    assert_eq!(q.contains, (Some("grep".into()), false));
  }

  #[test]
  fn opts_starts_with() {
    let q = parse(&[Opt::for_test("starts-with", &["git"])]);
    assert_eq!(q.starts_with, (Some("git".into()), false));
  }

  #[test]
  fn opts_ends_with() {
    let q = parse(&[Opt::for_test("ends-with", &[".log"])]);
    assert_eq!(q.ends_with, (Some(".log".into()), false));
  }

  #[test]
  fn opts_matches_regex() {
    let q = parse(&[Opt::for_test("matches", &["^cargo"])]);
    assert_eq!(q.matches, (Some("^cargo".into()), false));
  }

  #[test]
  fn opts_duration_gt_lt() {
    let q = parse(&[
      Opt::for_test("duration-gt", &["1s"]),
      Opt::for_test("duration-lt", &["1h"]),
    ]);
    assert_eq!(q.duration_gt, (Some("1s".into()), false));
    assert_eq!(q.duration_lt, (Some("1h".into()), false));
  }

  #[test]
  fn opts_with_token() {
    let q = parse(&[Opt::for_test("with-token", &["abcd-1234"])]);
    assert_eq!(q.with_token, (Some("abcd-1234".into()), false));
  }

  #[test]
  fn opts_with_status_parses_integer() {
    let q = parse(&[Opt::for_test("with-status", &["127"])]);
    assert_eq!(q.with_status, (Some(127), false));
  }

  #[test]
  fn opts_with_status_invalid_errors() {
    let result = HistQuery::from_opts(&[Opt::for_test("with-status", &["notanumber"])]);
    assert!(result.is_err());
  }

  #[test]
  fn opts_lines_gt_lt() {
    let q = parse(&[
      Opt::for_test("lines-gt", &["5"]),
      Opt::for_test("lines-lt", &["20"]),
    ]);
    assert_eq!(q.lines_gt, (Some(5), false));
    assert_eq!(q.lines_lt, (Some(20), false));
  }

  #[test]
  fn opts_lines_gt_invalid_errors() {
    let result = HistQuery::from_opts(&[Opt::for_test("lines-gt", &["abc"])]);
    assert!(result.is_err());
  }

  #[test]
  fn opts_limit() {
    let q = parse(&[Opt::for_test("limit", &["50"])]);
    assert_eq!(q.limit, Some(50));
  }

  #[test]
  fn opts_limit_invalid_falls_back_to_max() {
    // The code uses unwrap_or(u64::MAX) for limit specifically.
    let q = parse(&[Opt::for_test("limit", &["abc"])]);
    assert_eq!(q.limit, Some(u64::MAX));
  }

  #[test]
  fn opts_in_dir_uses_arg_when_not_canonicalizable() {
    let _g = TestGuard::new();
    // A clearly non-existent path falls back to the literal arg.
    let q = parse(&[Opt::for_test(
      "in-dir",
      &["/definitely/not/a/real/dir/xyz123"],
    )]);
    assert_eq!(
      q.in_dir,
      (Some("/definitely/not/a/real/dir/xyz123".into()), false)
    );
  }

  // ─── Flags (no arg) → bool ───────────────────────────────────────────

  #[test]
  fn opts_ex_hist_flag() {
    let q = parse(&[Opt::for_test("ex", &[])]);
    assert!(q.ex_hist);
  }

  #[test]
  fn opts_count_flag() {
    let q = parse(&[Opt::for_test("count", &[])]);
    assert!(q.count);
  }

  #[test]
  fn opts_delete_flag() {
    let q = parse(&[Opt::for_test("delete", &[])]);
    assert!(q.delete);
  }

  #[test]
  fn opts_restore_flag() {
    let q = parse(&[Opt::for_test("restore", &[])]);
    assert!(q.restore);
  }

  #[test]
  fn opts_json_flag() {
    let q = parse(&[Opt::for_test("json", &[])]);
    assert!(q.json);
  }

  #[test]
  fn opts_pull_flag() {
    let q = parse(&[Opt::for_test("pull", &[])]);
    assert!(q.pull);
  }

  // ─── Short flags ─────────────────────────────────────────────────────

  #[test]
  fn opts_short_n_disables_numbers() {
    // `-n` resolves to the "no-numbers" key.
    let q = parse(&[Opt::for_test("no-numbers", &[])]);
    assert!(q.no_numbers);
  }

  #[test]
  fn opts_short_r_reverses() {
    // `-r` resolves to the "reverse" key.
    let q = parse(&[Opt::for_test("reverse", &[])]);
    assert!(q.reverse);
  }

  // ─── --not polarity ──────────────────────────────────────────────────

  #[test]
  fn opts_not_flips_polarity_for_next_arg() {
    let q = parse(&[
      Opt::for_test("not", &[]),
      Opt::for_test("contains", &["rm -rf"]),
    ]);
    assert_eq!(q.contains, (Some("rm -rf".into()), true));
  }

  #[test]
  fn opts_not_only_applies_to_next_arg_then_resets() {
    let q = parse(&[
      Opt::for_test("not", &[]),
      Opt::for_test("contains", &["danger"]),
      Opt::for_test("after", &["yesterday"]),
    ]);
    assert_eq!(q.contains, (Some("danger".into()), true));
    // 'after' should NOT be negated — polarity reset after 'contains'.
    assert_eq!(q.after, (Some("yesterday".into()), false));
  }

  #[test]
  fn opts_double_not_cancels_polarity() {
    let q = parse(&[
      Opt::for_test("not", &[]),
      Opt::for_test("not", &[]),
      Opt::for_test("contains", &["x"]),
    ]);
    assert_eq!(q.contains, (Some("x".into()), false));
  }

  // ─── --import path resolution ────────────────────────────────────────

  fn set_shed_home(path: &str) {
    use crate::state::vars::{VarFlags, VarKind};
    Shed::vars_mut(|v| v.set_var("HOME", VarKind::Str(path.into()), VarFlags::EXPORT)).unwrap();
  }

  #[test]
  fn opts_import_bash_resolves_to_home_bash_history() {
    let _g = TestGuard::new();
    set_shed_home("/tmp/some_home");
    let q = parse(&[Opt::for_test("import", &["bash"])]);
    assert_eq!(q.import, Some("/tmp/some_home/.bash_history".into()));
  }

  #[test]
  fn opts_import_zsh_resolves_to_home_zsh_history() {
    let _g = TestGuard::new();
    set_shed_home("/tmp/some_home");
    let q = parse(&[Opt::for_test("import", &["zsh"])]);
    assert_eq!(q.import, Some("/tmp/some_home/.zsh_history".into()));
  }

  #[test]
  fn opts_import_arbitrary_path_passed_through() {
    let _g = TestGuard::new();
    let q = parse(&[Opt::for_test("import", &["/etc/some.history"])]);
    assert_eq!(q.import, Some("/etc/some.history".into()));
  }

  // ─── Unknown / error handling ────────────────────────────────────────
  //
  // In practice only recognized keys reach `from_opts` (the option parser
  // filters the rest), but the catch-all arm defensively errors on anything
  // it doesn't recognize.

  #[test]
  fn opts_unknown_long_errors() {
    let result = HistQuery::from_opts(&[Opt::for_test("totally-made-up", &["x"])]);
    assert!(result.is_err());
  }

  #[test]
  fn opts_unknown_short_errors() {
    let result = HistQuery::from_opts(&[Opt::for_test("x", &["val"])]);
    assert!(result.is_err());
  }

  // ─── Combined / multi-opt sanity check ───────────────────────────────

  #[test]
  fn opts_multiple_fields_compose() {
    let q = parse(&[
      Opt::for_test("reverse", &[]),
      Opt::for_test("json", &[]),
      Opt::for_test("contains", &["cargo"]),
      Opt::for_test("limit", &["10"]),
      Opt::for_test("not", &[]),
      Opt::for_test("in-dir", &["/nonexistent/zzz"]),
    ]);
    assert!(q.reverse);
    assert!(q.json);
    assert_eq!(q.contains, (Some("cargo".into()), false));
    assert_eq!(q.limit, Some(10));
    assert_eq!(q.in_dir, (Some("/nonexistent/zzz".into()), true));
  }

  // ─── HistQuery::execute ──────────────────────────────────────────────
  //
  // Each test builds a fresh in-memory History, seeds it with known
  // entries, then runs a HistQuery and checks the result. The test
  // table name varies per test so the LazyLock cache in history.rs
  // doesn't bleed entries across cases.

  use crate::readline::HistEntry;
  use std::time::{Duration as StdDuration, UNIX_EPOCH};

  /// Build a `HistEntry` with the given command and the rest filled in
  /// from defaults. Timestamp is fixed (NOT `now()`) so cross-runs are
  /// deterministic where they need to be.
  fn entry(cmd: &str) -> HistEntry {
    HistEntry {
      runtime: StdDuration::from_micros(0),
      timestamp: UNIX_EPOCH + StdDuration::from_secs(1_700_000_000),
      command: cmd.into(),
      cwd: "/tmp".into(),
      status: 0,
      token: util::random::Uuid::new_v4(),
    }
  }

  fn entry_full(
    cmd: &str,
    cwd: &str,
    status: i32,
    runtime_micros: u64,
    secs_since_epoch: u64,
  ) -> HistEntry {
    HistEntry {
      runtime: StdDuration::from_micros(runtime_micros),
      timestamp: UNIX_EPOCH + StdDuration::from_secs(secs_since_epoch),
      command: cmd.into(),
      cwd: cwd.into(),
      status,
      token: util::random::Uuid::new_v4(),
    }
  }

  /// Create a History with a unique per-test table name and seed it with
  /// the given entries (oldest first).
  fn hist_with(name: &str, entries: Vec<HistEntry>) -> crate::readline::History {
    let h = crate::readline::History::empty(name);
    for e in entries {
      h.push_entry(e).unwrap();
    }
    h
  }

  // ─── No filters ─────────────────────────────────────────────────────

  #[test]
  fn execute_no_conditions_returns_all_entries() {
    let _g = TestGuard::new();
    let h = hist_with("exec_all", vec![entry("a"), entry("b"), entry("c")]);
    let q = HistQuery::new();
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 3);
  }

  // ─── Substring / prefix / suffix filters ────────────────────────────

  #[test]
  fn execute_contains_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_contains",
      vec![entry("ls -la"), entry("echo hello"), entry("cat foo")],
    );
    let mut q = HistQuery::new();
    q.contains = (Some("echo".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "echo hello");
  }

  #[test]
  fn execute_starts_with_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_starts",
      vec![entry("git status"), entry("git log"), entry("ls")],
    );
    let mut q = HistQuery::new();
    q.starts_with = (Some("git".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn execute_ends_with_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_ends",
      vec![entry("touch a.log"), entry("rm b.log"), entry("vi c.txt")],
    );
    let mut q = HistQuery::new();
    q.ends_with = (Some(".log".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  // ─── Status / token / dir filters ───────────────────────────────────

  #[test]
  fn execute_with_status_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_status",
      vec![
        entry_full("ok", "/tmp", 0, 0, 100),
        entry_full("fail", "/tmp", 1, 0, 200),
        entry_full("notfound", "/tmp", 127, 0, 300),
      ],
    );
    let mut q = HistQuery::new();
    q.with_status = (Some(127), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "notfound");
  }

  #[test]
  fn execute_in_dir_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_dir",
      vec![
        entry_full("a", "/home/u", 0, 0, 100),
        entry_full("b", "/tmp", 0, 0, 200),
        entry_full("c", "/home/u", 0, 0, 300),
      ],
    );
    let mut q = HistQuery::new();
    q.in_dir = (Some("/home/u".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  // ─── Line count filters ─────────────────────────────────────────────

  #[test]
  fn execute_lines_gt_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_lines_gt",
      vec![
        entry("one"),
        entry("one\ntwo\nthree"),       // 3 lines
        entry("one\ntwo\nthree\nfour"), // 4 lines
      ],
    );
    let mut q = HistQuery::new();
    q.lines_gt = (Some(2), false); // strictly greater than 2
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn execute_lines_lt_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_lines_lt",
      vec![entry("one"), entry("one\ntwo"), entry("a\nb\nc\nd")],
    );
    let mut q = HistQuery::new();
    q.lines_lt = (Some(3), false); // strictly less than 3
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  // ─── Duration filters ───────────────────────────────────────────────

  #[test]
  fn execute_duration_gt_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_dur_gt",
      vec![
        entry_full("fast", "/", 0, 1, 100),           // 1us
        entry_full("medium", "/", 0, 1_000_000, 200), // 1s
        entry_full("slow", "/", 0, 10_000_000, 300),  // 10s
      ],
    );
    let mut q = HistQuery::new();
    q.duration_gt = (Some("5s".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "slow");
  }

  #[test]
  fn execute_duration_lt_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_dur_lt",
      vec![
        entry_full("fast", "/", 0, 1, 100),
        entry_full("slow", "/", 0, 10_000_000, 200),
      ],
    );
    let mut q = HistQuery::new();
    q.duration_lt = (Some("1s".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "fast");
  }

  #[test]
  fn execute_duration_invalid_errors() {
    let _g = TestGuard::new();
    let h = hist_with("exec_dur_bad", vec![entry("x")]);
    let mut q = HistQuery::new();
    q.duration_gt = (Some("not-a-duration".into()), false);
    let result = q.execute(&h);
    assert!(result.is_err());
  }

  // ─── Limit / specific IDs ───────────────────────────────────────────

  #[test]
  fn execute_limit_caps_result_count() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_limit",
      vec![entry("a"), entry("b"), entry("c"), entry("d")],
    );
    let mut q = HistQuery::new();
    q.limit = Some(2);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn execute_specific_id_positive() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_id",
      vec![entry("first"), entry("second"), entry("third")],
    );
    let mut q = HistQuery::new();
    q.specific_ids = vec![2]; // literal id=2 (second entry, since ids start at 1)
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "second");
  }

  #[test]
  fn execute_specific_id_negative_is_relative_to_end() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_id_neg",
      vec![entry("first"), entry("second"), entry("third")],
    );
    let mut q = HistQuery::new();
    q.specific_ids = vec![-1]; // -1 → second-newest entry
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "second");
  }

  // ─── --not negation ─────────────────────────────────────────────────

  #[test]
  fn execute_negated_contains_excludes_matches() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_not",
      vec![
        entry("danger_rm_command"),
        entry("safe_ls"),
        entry("also_safe"),
      ],
    );
    let mut q = HistQuery::new();
    q.contains = (Some("danger".into()), true); // NOT contains
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
    for r in &results {
      assert!(!r.1.command.to_str_lossy().contains("danger"));
    }
  }

  // ─── matches (regex, applied post-query) ────────────────────────────

  #[test]
  fn execute_matches_regex_post_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_regex",
      vec![entry("cargo build"), entry("cargo test"), entry("git log")],
    );
    let mut q = HistQuery::new();
    q.matches = (Some("^cargo".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn execute_matches_regex_negated() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_regex_neg",
      vec![entry("cargo build"), entry("cargo test"), entry("git log")],
    );
    let mut q = HistQuery::new();
    q.matches = (Some("^cargo".into()), true);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "git log");
  }

  // ─── Ordering ───────────────────────────────────────────────────────

  #[test]
  fn execute_default_returns_oldest_first_after_reverse_default() {
    // execute() pulls DESC from sqlite, then reverses (since
    // self.reverse defaults to false). End result: oldest at index 0,
    // newest at the end.
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_order",
      vec![entry("one"), entry("two"), entry("three")],
    );
    let q = HistQuery::new();
    let results = q.execute(&h).unwrap();
    assert_eq!(results[0].1.command, "one");
    assert_eq!(results[2].1.command, "three");
  }

  #[test]
  fn execute_reverse_keeps_desc_order() {
    let _g = TestGuard::new();
    let h = hist_with("exec_rev", vec![entry("one"), entry("two"), entry("three")]);
    let mut q = HistQuery::new();
    q.reverse = true;
    let results = q.execute(&h).unwrap();
    assert_eq!(results[0].1.command, "three");
    assert_eq!(results[2].1.command, "one");
  }

  // ─── Combined filters ──────────────────────────────────────────────

  #[test]
  fn execute_combined_status_and_starts_with() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_combo",
      vec![
        entry_full("git push", "/", 0, 0, 100),
        entry_full("git push --force", "/", 128, 0, 200),
        entry_full("ls -la", "/", 0, 0, 300),
      ],
    );
    let mut q = HistQuery::new();
    q.starts_with = (Some("git".into()), false);
    q.with_status = (Some(0), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "git push");
  }

  // ─── Bad date input ─────────────────────────────────────────────────

  #[test]
  fn execute_invalid_after_date_errors() {
    let _g = TestGuard::new();
    let h = hist_with("exec_bad_date", vec![entry("x")]);
    let mut q = HistQuery::new();
    q.after = (Some("not-a-real-date-zzz".into()), false);
    let result = q.execute(&h);
    assert!(result.is_err());
  }
}

#[cfg(test)]
mod hist_builtin_execute_tests {
  //! Tests for the `Hist` builtin's `execute()` itself — covering the
  //! `hist` command end-to-end via `test_input`. The mod above (`tests`)
  //! exercises `HistQuery` directly; this one exercises argument
  //! dispatch, table selection, output formatting, and the restore/pull/
  //! import branches.

  use crate::readline::History;
  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, test_input};

  /// Drop and re-init the named table on the shared in-memory conn so
  /// each test starts with a clean slate. Returns a History handle for
  /// seeding entries.
  fn fresh_history(table: &str) -> History {
    let conn = state::util::get_db_conn().expect("test db conn");
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch(&format!("DROP TABLE IF EXISTS {table}"));
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch(&format!("DROP TABLE IF EXISTS {table}_backup"));
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch("PRAGMA user_version = 0");
    History::new(conn, table).expect("history init")
  }

  // ─── default listing / filtering ───────────────────────────────────

  #[test]
  fn hist_lists_pushed_entries() {
    let g = TestGuard::new();
    let h = fresh_history("shed_history");
    h.push(": alpha").unwrap();
    h.push(": beta").unwrap();
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": alpha"), "got: {out:?}");
    assert!(out.contains(": beta"), "got: {out:?}");
    assert_eq!(Shed::get_status(), 0);
  }

  #[test]
  fn hist_n_flag_omits_ids() {
    let g = TestGuard::new();
    let h = fresh_history("shed_history");
    h.push(": only-entry").unwrap();
    // With -n, lines should NOT start with the id\t prefix.
    test_input("hist -n").unwrap();
    let out = g.read_output();
    assert!(out.contains(": only-entry"));
    // The id form would be "1\t: only-entry". With -n we just have the cmd.
    assert!(!out.contains("1\t"), "got: {out:?}");
  }

  #[test]
  fn hist_count_outputs_entry_count() {
    let g = TestGuard::new();
    let h = fresh_history("shed_history");
    h.push(": a").unwrap();
    h.push(": b").unwrap();
    h.push(": c").unwrap();
    test_input("hist --count").unwrap();
    let out = g.read_output();
    assert!(out.trim_end().ends_with('3'), "got: {out:?}");
  }

  #[test]
  fn hist_json_outputs_json_object() {
    let g = TestGuard::new();
    let h = fresh_history("shed_history");
    h.push(": json-entry").unwrap();
    test_input("hist --json").unwrap();
    let out = g.read_output();
    // serde_json::to_string_pretty produces newlines and a {…} wrapper.
    assert!(out.contains("\"command\""), "got: {out:?}");
    assert!(out.contains(": json-entry"), "got: {out:?}");
  }

  // ─── --ex selects ex_history table ─────────────────────────────────

  #[test]
  fn hist_ex_uses_ex_history_table() {
    let g = TestGuard::new();
    let normal = fresh_history("shed_history");
    let ex = fresh_history("ex_history");
    normal.push(": normal-entry").unwrap();
    ex.push(": ex-entry").unwrap();
    test_input("hist --ex").unwrap();
    let out = g.read_output();
    assert!(out.contains(": ex-entry"), "got: {out:?}");
    assert!(!out.contains(": normal-entry"), "got: {out:?}");
  }

  // ─── --delete and --restore ────────────────────────────────────────

  #[test]
  fn hist_delete_by_id_removes_entry() {
    let g = TestGuard::new();
    let h = fresh_history("shed_history");
    h.push(": kept").unwrap();
    h.push(": doomed").unwrap();
    // Delete the second entry by id.
    test_input("hist --delete 2").unwrap();
    g.read_output(); // drain --delete output
    // Now re-list; the doomed entry should be gone.
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": kept"), "got: {out:?}");
    assert!(!out.contains(": doomed"), "got: {out:?}");
  }

  #[test]
  fn hist_delete_matches_only_removes_matching_entries() {
    // Regression: `--delete --matches <regex>` used to run the delete on the
    // SQL WHERE (empty when --matches is the only filter) and apply the regex
    // only to the displayed list — wiping the ENTIRE table. It must now delete
    // exactly the regex-matched rows.
    let g = TestGuard::new();
    let h = fresh_history("shed_history");
    h.push(": cargo build").unwrap();
    h.push(": cargo test").unwrap();
    h.push(": git status").unwrap();
    test_input("hist --delete --matches '^: cargo'").unwrap();
    g.read_output(); // drain --delete output
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      out.contains(": git status"),
      "non-matching entry wiped: {out:?}"
    );
    assert!(
      !out.contains(": cargo build"),
      "matching entry survived: {out:?}"
    );
    assert!(
      !out.contains(": cargo test"),
      "matching entry survived: {out:?}"
    );
  }

  #[test]
  fn hist_delete_matches_none_keeps_all_entries() {
    // A regex that matches nothing must delete nothing (must NOT fall through
    // to an empty WHERE and wipe the table).
    let g = TestGuard::new();
    let h = fresh_history("shed_history");
    h.push(": alpha").unwrap();
    h.push(": beta").unwrap();
    test_input("hist --delete --matches 'zzz-no-match'").unwrap();
    g.read_output();
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": alpha"), "entry wiped: {out:?}");
    assert!(out.contains(": beta"), "entry wiped: {out:?}");
  }

  #[test]
  fn init_db_creates_all_tables_on_shared_connection() {
    // Regression: init_db keyed its early-return on the DB-wide `user_version`,
    // so once the first table bumped it to USER_VERSION, every later table on
    // the same connection skipped its CREATE TABLE and silently never
    // persisted. Simulate startup order on one shared connection.
    let _g = TestGuard::new();
    let conn = state::util::get_db_conn().expect("test db conn");
    {
      let c = conn.lock().unwrap();
      c.execute_batch("DROP TABLE IF EXISTS shed_history").ok();
      c.execute_batch("DROP TABLE IF EXISTS ex_history").ok();
      // Fresh-ish DB so the FIRST init succeeds; the SECOND then hits the
      // (formerly buggy) `user_version == USER_VERSION` early-return.
      c.execute_batch("PRAGMA user_version = 0").ok();
    }

    // First table bumps user_version to USER_VERSION.
    let first = History::new(conn.clone(), "shed_history").expect("init first table");
    first.push(": first-table-entry").unwrap();

    // Second table on the same connection, user_version now == USER_VERSION.
    // Before the fix its CREATE TABLE was skipped, so this INSERT would fail.
    let second = History::new(conn.clone(), "ex_history").expect("init second table");
    second
      .push(": second-table-entry")
      .expect("second table must exist and be writable");

    let entries = second.query("", &[]).expect("query second table");
    assert!(
      entries
        .iter()
        .any(|(_, e)| e.command() == ": second-table-entry"),
      "second table did not persist its entry: {entries:?}"
    );
  }

  #[test]
  fn hist_restore_brings_back_deleted_entries() {
    let g = TestGuard::new();
    let h = fresh_history("shed_history");
    h.push(": one").unwrap();
    h.push(": two").unwrap();
    // Delete both — creates the backup table.
    test_input("hist --delete --contains :").unwrap();
    g.read_output();
    // Now restore.
    test_input("hist --restore").unwrap();
    g.read_output();
    // Re-list: both entries should reappear.
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": one"), "got: {out:?}");
    assert!(out.contains(": two"), "got: {out:?}");
  }

  #[test]
  fn hist_restore_with_no_backup_errors() {
    let _g = TestGuard::new();
    let _h = fresh_history("shed_history");
    // No prior --delete → no backup table → restore fails.
    test_input("hist --restore").ok();
    assert_ne!(Shed::get_status(), 0);
  }

  // ─── --pull just refreshes caches ──────────────────────────────────

  #[test]
  fn hist_pull_returns_ok() {
    let _g = TestGuard::new();
    let _h = fresh_history("shed_history");
    test_input("hist --pull").unwrap();
    assert_eq!(Shed::get_status(), 0);
  }

  // ─── --import reads a file and pushes entries ──────────────────────

  #[test]
  fn hist_import_adds_entries_from_bash_format_file() {
    let g = TestGuard::new();
    let _h = fresh_history("shed_history");
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join(".bash_history");
    std::fs::write(
      &path,
      "#1700000000\n: imported-one\n#1700000001\n: imported-two\n",
    )
    .unwrap();
    test_input(format!("hist --import {}", path.display())).unwrap();
    g.read_output(); // drain "imported N" + entries dump
    // Verify the entries are queryable via a follow-up list.
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": imported-one"), "got: {out:?}");
    assert!(out.contains(": imported-two"), "got: {out:?}");
  }
}
