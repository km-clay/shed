use std::{
  cmp::Ordering,
  convert::Into,
  fs,
  io::{self, Write},
};

use rusqlite::ToSql;

use crate::{
  HashSet,
  builtin::opt::Opt,
  expand::escape,
  readline::{HistEntry, History},
  sherr,
  state::{Shed, vars::VarStr},
  util::{error::ShResult, strops::TimeReader},
};

use super::entry_obj;

struct WhereBuilder {
  conditions: Vec<String>,
  params: Vec<Box<dyn ToSql>>,
  idx: usize,
}

impl WhereBuilder {
  fn push(&mut self, not: bool, clause: &str, param: impl ToSql + 'static) {
    let mut clause = format!("{clause} ?{}", self.idx);
    if not {
      clause = format!("NOT ({clause})");
    }
    self.conditions.push(clause);
    self.params.push(Box::new(param));
    self.idx += 1;
  }
  fn build(self) -> (String, Vec<Box<dyn ToSql>>) {
    (self.conditions.join(" AND "), self.params)
  }
}

#[expect(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone)]
pub(super) struct HistQuery {
  // 456 bytes by the way lol 8D
  pub(super) after: (Option<VarStr>, bool),
  pub(super) before: (Option<VarStr>, bool),
  pub(super) contains: (Option<VarStr>, bool),
  pub(super) lines_gt: (Option<u64>, bool),
  pub(super) lines_lt: (Option<u64>, bool),
  pub(super) starts_with: (Option<VarStr>, bool),
  pub(super) ends_with: (Option<VarStr>, bool),
  pub(super) matches: (Option<VarStr>, bool),
  pub(super) duration_gt: (Option<VarStr>, bool),
  pub(super) duration_lt: (Option<VarStr>, bool),
  pub(super) with_status: (Option<i32>, bool),
  pub(super) with_token: (Option<VarStr>, bool),
  pub(super) in_dir: (Option<VarStr>, bool),
  pub(super) limit: Option<u64>,
  pub(super) specific_ids: Vec<i64>,
  pub(super) no_numbers: bool,
  pub(super) no_dupes: bool,
  pub(super) reverse: bool,
  pub(super) json: bool,
  pub(super) quoted: bool,
  pub(super) count: bool,
  pub(super) delete: bool,
  pub(super) restore: bool,
  pub(super) ex_hist: bool,
}

impl HistQuery {
  pub(super) fn new() -> Self {
    Self::default()
  }

  pub(super) fn execute(&self, hist: &History) -> ShResult<Vec<(i64, HistEntry)>> {
    let b = self.build_conditions(hist)?;

    let (conditions, params) = b.build();

    let limit = self.limit.map(|n| format!("LIMIT {n}")).unwrap_or_default();

    // hardcoding DESC ordering so that limit always starts from the most recent entry
    let tail = format!("ORDER BY id DESC {limit}");

    let param_refs: Vec<&dyn ToSql> = params.iter().map(AsRef::as_ref).collect();

    let mut entries = hist.query_scoped(&conditions, &tail, &param_refs)?;

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
    // so '!self.reverse' means to go in ascending order instead
    if !self.reverse {
      // the entries start in descending order. we reverse it
      // so that the more recent ones are at the bottom by default
      entries.reverse();
    }

    Ok(entries)
  }

  fn build_conditions(&self, hist: &History) -> ShResult<WhereBuilder> {
    let mut b = WhereBuilder {
      conditions: vec![],
      params: vec![],
      idx: 1,
    };

    if let (Some(after), not) = &self.after {
      let ts = TimeReader::interpret(&after.to_str_lossy())
        .map_err(|e| sherr!(ParseErr, "Failed to parse date for --after: {e}"))?;

      b.push(*not, "timestamp >=", ts.timestamp());
    }

    if let (Some(before), not) = &self.before {
      let ts = TimeReader::interpret(&before.to_str_lossy())
        .map_err(|e| sherr!(ParseErr, "Failed to parse date for --before: {e}"))?;

      b.push(*not, "timestamp <=", ts.timestamp());
    }

    if let (Some(prefix), not) = &self.ends_with {
      b.push(*not, "RTRIM(command) LIKE", format!("%{prefix}"));
    }

    if let (Some(contains), not) = &self.contains {
      b.push(*not, "TRIM(command) LIKE", format!("%{contains}%"));
    }

    if let (Some(prefix), not) = &self.starts_with {
      b.push(*not, "LTRIM(command) LIKE", format!("{prefix}%"));
    }

    if let (Some(status), not) = &self.with_status {
      b.push(*not, "status =", *status);
    }

    if let (Some(token), not) = &self.with_token {
      b.push(*not, "token =", token.clone());
    }

    if let (Some(dir), not) = &self.in_dir {
      b.push(*not, "cwd LIKE", dir.clone());
    }

    if let (Some(ceiling), not) = &self.lines_lt {
      b.push(
        *not,
        "(LENGTH(command) - LENGTH(REPLACE(command, char(10), ''))) + 1 <",
        (*ceiling).cast_signed(),
      );
    }

    if let (Some(floor), not) = &self.lines_gt {
      b.push(
        *not,
        "(LENGTH(command) - LENGTH(REPLACE(command, char(10), ''))) + 1 >",
        (*floor).cast_signed(),
      );
    }

    if let (Some(duration), not) = &self.duration_gt {
      let micros = TimeReader::parse_dur(&duration.to_str_lossy())
        .map_err(|e| sherr!(ParseErr, "Failed to parse duration for --longer-than: {e}"))?;

      b.push(*not, "runtime >=", micros);
    }

    if let (Some(duration), not) = &self.duration_lt {
      let micros = TimeReader::parse_dur(&duration.to_str_lossy())
        .map_err(|e| sherr!(ParseErr, "Failed to parse duration for --shorter-than: {e}"))?;
      b.push(*not, "runtime <=", micros);
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

        id_strings.push(format!("id = ?{}", b.idx));
        b.params.push(Box::new(id));
        b.idx += 1;
      }
      b.conditions.push(format!("({})", id_strings.join(" OR ")));
    }

    Ok(b)
  }

  pub(super) fn from_opts(opts: &[Opt]) -> ShResult<Self> {
    let mut new = Self::new();
    let mut negated = false; // '--not' flag flips this for one argument
    let value = |opt: &Opt| -> Option<VarStr> { opt.value().ok() };

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
            Err(a) => {
              return Err(sherr!(ParseErr, "Invalid status code for {opt}: {a}",));
            }
          }
        }
        "in-dir" => {
          // using canonicalize here allows args like "." to work
          let arg = opt.value()?;
          let dir = fs::canonicalize(&arg)
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
          let count = arg
            .parse::<u64>()
            .map_err(|v| sherr!(ParseErr, "Invalid number for {opt}: {v}"))?;

          if is_gt {
            new.lines_gt = (Some(count), negated);
          } else {
            new.lines_lt = (Some(count), negated);
          }
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
        "no-numbers" => new.no_numbers = true,
        "reverse" => new.reverse = true,
        _ => {
          return Err(sherr!(ParseErr, "Unknown option for history: {opt}").with_code(2));
        }
      }
      negated = false; // reset polarity after each option
    }

    Ok(new)
  }

  pub(super) fn format_entries(
    &self,
    entries: &[(i64, HistEntry)],
    f: &mut impl Write,
  ) -> io::Result<()> {
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
        f.write_all(&escape::shell_quote_bytes(entry.command_bytes()))?;
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
  fn format_json(&self, entries: &[&(i64, HistEntry)], f: &mut impl Write) -> io::Result<()> {
    use serde_json::Value;

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
