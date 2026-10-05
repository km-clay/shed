use std::path::Path;

use super::super::{Builtin, BuiltinArgs};

use crate::{
  outln,
  state::db,
  util::{self, error::ShResult},
};

pub(super) struct ZdClean;
impl Builtin for ZdClean {
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    let Some(conn) = db::get_db_conn() else {
      return util::with_status(0);
    };
    let Ok(conn) = conn.try_lock() else {
      return util::with_status(0);
    };

    let dead: Vec<String> = {
      let Ok(mut stmt) = conn.prepare("SELECT path FROM dir_history") else {
        return util::with_status(0);
      };
      let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return util::with_status(0);
      };
      rows.flatten().filter(|p| !Path::new(p).is_dir()).collect()
    };

    let mut removed = 0;
    for path in &dead {
      removed += conn
        .execute(
          "DELETE FROM dir_history WHERE path = ?1",
          rusqlite::params![path],
        )
        .unwrap_or(0);
    }

    outln!(
      "zd: pruned {removed} dead {}",
      if removed == 1 {
        "directory"
      } else {
        "directories"
      }
    );
    util::with_status(0)
  }
}
