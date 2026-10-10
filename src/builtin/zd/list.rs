use bstr::ByteSlice;
use serde_json::{Map, Value};

use crate::{
  expand::escape,
  opt, outln, sherr,
  util::{self, error::ShResult},
};

use super::super::{Builtin, BuiltinArgs, opt::OptSpec};

use super::{DirStat, load_dir_stats};

struct Sort {
  reverse: bool,
  kind   : SortKind,
}

#[derive(PartialEq, Eq, Copy, Clone, Debug)]
enum SortKind {
  Frecency,
  Visits,
  Recent,
  Path,
}

pub(super) struct ZdList;
impl Builtin for ZdList {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("json"),
      opt!("quoted"),
      opt!("reverse" | b'r'),
      opt!("sort", 1),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut quoted = false;
    let mut json   = false;

    let mut sort = Sort {
      reverse: false,
      kind   : SortKind::Frecency,
    };

    for opt in args.options() {
      match opt.key() {
        // 'reverse' and 'recursive' both share the '-r' shorthand,
        // it means different things on different subcommands.
        "reverse" => sort.reverse = true,
        "json"    => json = true,
        "quoted"  => quoted = true,

        "sort" => match &*opt.value()? {
          b"frecency" => sort.kind = SortKind::Frecency,
          b"visits"   => sort.kind = SortKind::Visits,
          b"recent"   => sort.kind = SortKind::Recent,
          b"path"     => sort.kind = SortKind::Path,
          val => {
            return Err(sherr!(ParseErr @ opt.span(), "invalid sort kind: {}", val.to_str_lossy()));
          }
        },
        _ => return Err(sherr!(ParseErr @ opt.span(), "invalid option: {opt}").with_code(2)),
      }
    }
    if json && quoted {
      return Err(
        sherr!(ParseErr @ args.cmd_span(), "--json and --quoted are mutually exclusive")
          .with_code(2),
      );
    }

    let query = args
      .arguments()
      .map(|(a, _)| a.to_str_lossy())
      .collect::<String>();

    let mut rows = load_dir_stats();

    if rows.is_empty() {
      return Err(sherr!(ExecFail @ args.span(), "no directory history yet"));
    }

    if !query.is_empty() {
      rows.retain(|r| r.path.contains(&query));
    }

    let default_desc = !matches!(sort.kind, SortKind::Path);
    let descending   = default_desc != sort.reverse;
    rows.sort_by(|a, b| {
      let ord = match sort.kind {
        SortKind::Frecency => a.frecency.cmp(&b.frecency),
        SortKind::Visits   => a.visits.cmp(&b.visits),
        SortKind::Recent   => a.last_visit.cmp(&b.last_visit),
        SortKind::Path     => a.path.cmp(&b.path),
      }
      .then_with(|| a.path.cmp(&b.path)); // tie-breaker

      if descending { ord.reverse() } else { ord }
    });

    let output = if quoted {
      Self::fmt_entries_quoted(&rows)
    } else if json {
      Self::fmt_entries_json(&rows)
    } else {
      Self::fmt_entries(&rows)
    };

    outln!("{output}");

    util::with_status(0)
  }
}

impl ZdList {
  fn fmt_entries(rows: &[DirStat]) -> String {
    // no format specified, use tab-separated values
    let mut entries = vec![];

    for row in rows {
      let mut entry = vec![];
      let DirStat {
        path,
        visits,
        last_visit,
        frecency,
      } = row;

      entry.push(visits.to_string());
      entry.push(last_visit.to_string());
      entry.push(frecency.to_string());
      entry.push(path.clone()); // push the path last because it can be anything
      // the previous stuff all follows a specific pattern (numbers)
      // but the path can throw off cut/awk parsers if it's in the middle

      entries.push(entry.join("\t"));
    }

    // we don't need to care about making sure the fields don't contain our separators here
    // since --json and --quoted are used for that. so let's just naively separate by tabs and newlines
    // when neither of those is passed.
    entries.join("\n")
  }

  fn fmt_entries_json(rows: &[DirStat]) -> String {
    // JSON formatted output
    let mut entries = vec![];

    for row in rows {
      let mut map = Map::new();
      let DirStat {
        path,
        visits,
        last_visit,
        frecency,
      } = row;

      map.insert("path".to_string(), Value::String(path.clone()));
      map.insert("visits".to_string(), Value::Number((*visits).into()));
      map.insert(
        "last_visit".to_string(),
        Value::Number((*last_visit).into()),
      );
      map.insert("frecency".to_string(), Value::Number((*frecency).into()));

      entries.push(Value::Object(map));
    }

    let json_arr = Value::Array(entries);
    serde_json::to_string_pretty(&json_arr).unwrap()
  }

  fn fmt_entries_quoted(rows: &[DirStat]) -> String {
    // SQR serialization
    let mut entries = vec![];

    for row in rows {
      let mut entry = vec![];
      let DirStat {
        path,
        visits,
        last_visit,
        frecency,
      } = row;

      // Same column order as the bare output (path last), just shell-quoted.
      entry.push(escape::shell_quote(&visits.to_string()));
      entry.push(escape::shell_quote(&last_visit.to_string()));
      entry.push(escape::shell_quote(&frecency.to_string()));
      entry.push(escape::shell_quote(path));

      entries.push(entry.join(" ")); // SQR fields are separated by spaces
    }

    let output = entries.join("\n"); // SQR rows are separated by newlines

    output
  }
}
