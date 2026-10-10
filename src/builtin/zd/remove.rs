use std::fs;

use crate::{
  opt, sherr,
  state::db,
  util::{self, error::ShResult},
};

use super::super::{Builtin, BuiltinArgs, opt::OptSpec};

pub(super) struct ZdRemove;
impl Builtin for ZdRemove {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("recursive" | b'r')]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let recursive            = args.has_opt("recursive");
    let targets: Vec<String> = args.arguments().map(|(a, _)| a.to_string()).collect();

    if targets.is_empty() {
      return Err(sherr!(ExecFail @ args.span(), "remove requires a directory").with_code(2));
    }

    let Some(conn) = db::get_db_conn() else {
      return util::with_status(0);
    };
    let Ok(conn) = conn.try_lock() else {
      return util::with_status(0);
    };

    let mut removed = 0;
    for target in &targets {
      let canon = fs::canonicalize(target)
        .map_or_else(|_| target.clone(), |c| c.to_string_lossy().into_owned());

      removed += if recursive {
        conn
          .execute(
            "DELETE FROM dir_history WHERE path = ?1 OR path GLOB ?1 || '/*'",
            rusqlite::params![canon],
          )
          .unwrap_or(0)
      } else {
        conn
          .execute(
            "DELETE FROM dir_history WHERE path = ?1 OR path = ?2",
            rusqlite::params![canon, target],
          )
          .unwrap_or(0)
      };
    }
    util::with_status(i32::from(removed == 0))
  }
}
