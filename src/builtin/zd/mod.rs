//! The `zd` builtin. Jumps to directories by partial match, using
//! the directory history database.

use std::{
  fs,
  path::Path,
  time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;

use crate::{
  readline::{self, Candidate},
  state::{db, paths},
  sub_command,
};

use super::{Builtin, BuiltinRouter, SubCommand};

#[cfg(test)]
mod tests;

mod add;
mod clean;
mod jump;
mod list;
mod remove;

use add::ZdAdd;
use clean::ZdClean;
use jump::ZdJump;
use list::ZdList;
use remove::ZdRemove;

pub(super) struct Zd;
impl BuiltinRouter for Zd {
  fn name(&self) -> &'static str {
    "zd"
  }

  fn default_sub(&self) -> Option<&'static dyn Builtin> {
    Some(&ZdJump)
  }
  fn sub_commands(&self) -> &'static [SubCommand] {
    const SUB_COMMANDS: &[SubCommand] = &[
      sub_command!(
        &ZdAdd, "add",
        "[-r] <dirs...>",
        "add directories to dir history database"
      ),
      sub_command!(
        &ZdRemove, "remove",
        "[-r] <dirs...>",
        "remove specific directories from dir history database"
      ),
      sub_command!(
        &ZdClean, "clean",
        "prune dead directories from dir history database"
      ),
      sub_command!(
        &ZdList, "list", "[--json|--quoted] [--reverse] [--sort <kind>] [<query>]",
        "list entries from the dir history database"
      ),
    ];
    SUB_COMMANDS
  }
}

pub(super) struct DirStat {
  path      : String,
  visits    : i64,
  last_visit: i64,
  frecency  : i32,
}

pub(super) fn query_dir_stats(conn: &Connection) -> Vec<DirStat> {
  let Ok(mut stmt) = conn.prepare("SELECT path, visits, last_visit FROM dir_history") else {
    return vec![];
  };

  let now = now_secs();
  let Ok(rows) = stmt.query_map([], |r| {
    Ok((
      r.get::<_, String>(0)?, // path
      r.get::<_, i64>(1)?,    // visits
      r.get::<_, i64>(2)?,    // last_visit seconds
    ))
  }) else {
    return vec![];
  };

  rows
    .flatten()
    .map(|(path, visits, last_visit)| DirStat {
      path,
      visits,
      last_visit,
      frecency: dir_frecency(visits, now - last_visit),
    })
    .collect()
}

pub(super) fn load_dir_stats() -> Vec<DirStat> {
  if let Some(shared) = db::get_db_conn() {
    let Ok(conn) = shared.try_lock() else {
      return vec![];
    };
    query_dir_stats(&conn)
  } else if let Ok(conn) = db::open_db_conn_readonly() {
    query_dir_stats(&conn)
  } else {
    vec![]
  }
}

/// Highlight the basename match, mirroring how `fuzzy_score_dir` rewards it, so
/// the underline lands on (e.g.) the "dev" in ".../dev-shells" rather than being
/// smeared across parent segments. Falls back to the default full-path match.
pub(super) fn highlight_dir(display: &str, query: &str) -> Option<Vec<usize>> {
  let base      = Path::new(display).file_name()?.to_str()?;
  // char offset of the basename within the display string (positions are chars).
  let offset    = display.chars().count() - base.chars().count();
  let positions = readline::match_positions(base, query);
  (!positions.is_empty()).then(|| positions.into_iter().map(|p| p + offset).collect())
}

pub(super) fn fuzzy_score_dir(cand: &Candidate, chars: &[char], penalize_len_diff: bool) -> i32 {
  let content = cand.content();
  let path    = Path::new(content);

  // An exact path match is unambiguous, so it always wins. This breaks ties like
  // "/home/me" vs "/home/me/projects" for the query "/home/me", where the matched
  // prefix otherwise scores identically for both.
  if chars.iter().copied().eq(content.chars()) {
    return i32::MAX;
  }

  // Otherwise, add the basename's own score on top of the full-path score, so a
  // match on the final segment ("fer" -> ".../fern") outranks one smeared across
  // parent directories. Double-counting the basename is the point.
  if let Some(base) = path.file_name().and_then(|b| b.to_str()) {
    let base_score = readline::fuzzy_match_score(&base.into(), chars, penalize_len_diff);
    let full       = readline::fuzzy_match_score(cand, chars, penalize_len_diff);
    if base_score > i32::MIN && full > i32::MIN {
      return full.saturating_add(base_score);
    }
  }

  readline::fuzzy_match_score(cand, chars, penalize_len_diff)
}

pub(super) fn now_secs() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |d| d.as_secs() as i64)
}

/// Recursively collect canonical paths of `root` and its subdirectories,
/// skipping hidden directories and symlinks (avoids `.git` clutter and loops).
/// `depth` caps how many levels below `root` to descend: `Some(0)` adds only
/// `root`, `None` recurses without limit.
pub(super) fn collect_subdirs(root: &Path, depth: Option<usize>, out: &mut Vec<String>) {
  if let Ok(canon) = root.canonicalize() {
    out.push(canon.to_string_lossy().into_owned());
  }
  if depth == Some(0) {
    return;
  }
  let Ok(entries) = fs::read_dir(root) else {
    return;
  };
  let next = depth.map(|d| d - 1);
  for entry in entries.flatten() {
    let path = entry.path();
    let hidden = path
      .file_name()
      .and_then(|n| n.to_str())
      .is_some_and(|n| n.starts_with('.'));
    if hidden || path.is_symlink() || !path.is_dir() {
      continue;
    }
    collect_subdirs(&path, next, out);
  }
}

/// Frecency weight from visit count and seconds since last visit. Recent and
/// frequent directories rank highest; old ones keep a small baseline weight.
pub(super) fn dir_frecency(visits: i64, age_secs: i64) -> i32 {
  let factor = match age_secs {
    s if s < 3_600   => 4, // within the hour
    s if s < 86_400  => 3, // within the day
    s if s < 604_800 => 2, // within the week
    _ => 1,
  };
  visits.saturating_mul(factor).clamp(0, i64::from(i32::MAX)) as i32
}

pub(super) fn load_dir_entries() -> Vec<(String, i32)> {
  load_dir_entries_inner(false)
}

pub(super) fn load_abbreviated_dirs() -> Vec<(String, i32)> {
  load_dir_entries_inner(true)
}

/// Load visited directories as `(path, frecency weight)`, skipping any that no
/// longer exist on disk.
pub(super) fn load_dir_entries_inner(format_paths: bool) -> Vec<(String, i32)> {
  let Some(conn) = db::get_db_conn() else {
    return vec![];
  };
  let Ok(conn) = conn.try_lock() else {
    return vec![];
  };
  let Ok(mut stmt) = conn.prepare("SELECT path, visits, last_visit FROM dir_history") else {
    return vec![];
  };
  let now = now_secs();
  let Ok(rows) = stmt.query_map([], |r| {
    Ok((
      r.get::<_, String>(0)?,
      r.get::<_, i64>(1)?,
      r.get::<_, i64>(2)?,
    ))
  }) else {
    return vec![];
  };
  rows
    .flatten()
    .filter(|(path, ..)| Path::new(path).is_dir())
    .map(|(path, visits, last_visit)| {
      let path = if format_paths {
        paths::display_path(path)
      } else {
        path
      };
      (path, dir_frecency(visits, now - last_visit))
    })
    .collect()
}
