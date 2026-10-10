use std::{convert::Into, time::UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::{
  builtin::BuiltinArgs,
  outln,
  readline::{HistDump, ReflogEntry},
  sherr,
  util::{self, error::ShResult},
};

use super::super::Builtin;

use super::{entry_obj, open_history};

pub(super) struct HistExport;
impl Builtin for HistExport {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let hist = open_history(args.span(), false, false)?;
    let HistDump {
      entries,
      branches,
      reflog,
    } = hist.dump_all()?;

    let mut map          = Map::new();

    let mut json_entries = vec![];
    for entry in entries {
      let (ent, parent, joint) = entry;
      let Value::Object(mut ent_json) = entry_obj(&ent) else {
        unreachable!()
      };

      ent_json.insert("parent".into(), Value::from(parent));
      ent_json.insert("joint".into(), Value::from(joint));

      json_entries.push(Value::Object(ent_json));
    }
    map.insert("entries".into(), Value::Array(json_entries));

    let mut json_branches = vec![];
    for branch in branches {
      let     (name, head) = branch;
      let mut branch_map   = Map::new();

      branch_map.insert("name".into(), Value::String(name));
      branch_map.insert("head".into(), Value::from(head));
      json_branches.push(Value::Object(branch_map));
    }
    map.insert("branches".into(), Value::Array(json_branches));

    let mut json_reflog = vec![];
    for ent in reflog {
      let ReflogEntry {
        table,
        branch,
        old_head,
        new_head,
        op,
        timestamp,
      } = ent;
      let ts = timestamp
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
      let mut map = Map::new();

      map.insert("table".into(), Value::String((*table).clone()));
      map.insert("branch".into(), Value::String((*branch).clone()));
      map.insert("old_head".into(), Value::from(old_head));
      map.insert("new_head".into(), Value::from(new_head));
      map.insert("op".into(), Value::String(op));
      map.insert("timestamp".into(), Value::from(ts));

      json_reflog.push(Value::Object(map));
    }
    map.insert("reflog".into(), Value::Array(json_reflog));

    map.insert("version".into(), Value::from(1));

    let raw = match serde_json::to_string_pretty(&map) {
      Ok(r) => r,
      Err(e) => {
        return Err(sherr!(ExecFail @ args.span(), "Failed to serialize history to JSON: {e}"));
      }
    };

    outln!("{raw}");

    util::with_status(0)
  }
}
