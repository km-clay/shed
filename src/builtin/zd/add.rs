use std::{env, path::PathBuf};

use crate::{
  opt, sherr,
  state::db,
  util::{self, error::ShResult},
};

use super::super::{Builtin, BuiltinArgs, opt::OptSpec};

use super::{collect_subdirs, now_secs};

pub(super) struct ZdAdd;
impl Builtin for ZdAdd {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("recursive" | b'r'), opt!("depth" | b'd', 1)]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let depth = match args.opt_value("depth") {
      Some(n) => match n.to_str_lossy().parse::<usize>() {
        Ok(n)  => Some(n),
        Err(_) => return Err(sherr!(ParseErr @ args.span(), "invalid depth: {n}")),
      },
      None => None,
    };

    // a depth cap only makes sense recursively, so it implies -r.
    let recursive = depth.is_some() || args.has_opt("recursive");

    let mut dirs: Vec<PathBuf> = args
      .arguments()
      .map(|(a, _)| PathBuf::from(a.clone()))
      .collect();
    if dirs.is_empty()
      && let Ok(cwd) = env::current_dir()
    {
      dirs.push(cwd);
    }

    let mut paths = Vec::new();
    for dir in dirs {
      if !dir.is_dir() {
        return Err(sherr!(ExecFail @ args.span(), "not a directory: {}", dir.display()));
      }
      if recursive {
        collect_subdirs(&dir, depth, &mut paths);
      } else if let Ok(canon) = dir.canonicalize() {
        paths.push(canon.to_string_lossy().into_owned());
      }
    }

    let Some(conn) = db::get_db_conn() else {
      return util::with_status(0);
    };
    let Ok(conn) = conn.try_lock() else {
      return util::with_status(0);
    };
    let now = now_secs();
    conn.execute_batch("BEGIN").ok();
    for path in &paths {
      conn
        .execute(
          "INSERT INTO dir_history (path, visits, last_visit) VALUES (?1, 1, ?2)
           ON CONFLICT(path) DO NOTHING",
          rusqlite::params![path, now],
        )
        .ok();
    }
    conn.execute_batch("COMMIT").ok();
    util::with_status(0)
  }
}
