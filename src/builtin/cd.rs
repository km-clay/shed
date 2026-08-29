//! The `cd` builtin. Used to change the current working directory.
//!
//! Also contains `zd`, which uses directory history to jump to
//! partial matches, instead of requiring an absolute path.

use std::{
  path::{Path, PathBuf},
  time::{SystemTime, UNIX_EPOCH},
};

use crate::{
  expand, opt,
  readline::{FuzzyBuilder, fuzzy_best_match, fuzzy_match_score, match_positions},
};

use crate::procio::outln_bytes;

use super::{
  ShResult, Shed,
  opt::OptSpec,
  outln, sherr,
  state::{terminal::Terminal, util},
  try_var, var, with_status,
};

pub(super) struct Cd;
impl super::Builtin for Cd {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new("physical").short(b'P'),
      OptSpec::new("logical").short(b'L'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut resolve_syms = false;
    let mut try_cd_path = false;
    let mut print_dir = false;

    for opt in args.options() {
      match opt.key() {
        "physical" => resolve_syms = true,
        "logical" => resolve_syms = false,
        _ => return Err(sherr!(ParseErr @ opt.span(), "Invalid option: {opt}")),
      }
    }

    let (mut new_dir, arg_span) = if let Some((arg, span)) = args.arguments().next() {
      if arg == "-" {
        let old_pwd = get_old_pwd();
        print_dir = true;
        (old_pwd, Some(span.clone()))
      } else {
        // we only use cd path if the argument is not absolute or relative (starts with / or .)
        try_cd_path = !arg.to_str_lossy().starts_with(['/', '.']);
        (PathBuf::from(arg), Some(span.clone()))
      }
    } else {
      let home_dir = util::get_home_str().unwrap_or("/".into());
      (PathBuf::from(home_dir), None)
    };

    let span = arg_span.unwrap_or(args.cmd_span());

    if try_cd_path && let Some(found) = search_cd_path(&new_dir) {
      print_dir = true;
      new_dir = found;
    }

    // if resolve_syms is true, we turn symlinks into their canonical paths,
    // which refer to the actual position of the file in the filesystem
    let logical_pwd = if resolve_syms {
      None
    } else {
      let base = if new_dir.is_absolute() {
        PathBuf::new()
      } else {
        try_var!("PWD")
          .map(PathBuf::from)
          .or_else(|| std::env::current_dir().ok())
          .unwrap_or_else(|| PathBuf::from("/"))
      };
      Some(util::lex_normalize_path(&base.join(&new_dir)))
    };

    let target = if resolve_syms {
      match std::fs::canonicalize(&new_dir) {
        Ok(canon) => canon,
        Err(_) => new_dir,
      }
    } else {
      match logical_pwd.as_deref() {
        Some(logical) => PathBuf::from(logical),
        None => new_dir,
      }
    };

    // handle weird cases
    if !target.exists() {
      return Err(sherr!(ExecFail @ span.clone(), "Directory not found: {}", target.display()));
    }
    if !target.is_dir() {
      return Err(sherr!(ExecFail @ span.clone(), "Not a directory"));
    }
    if let Err(e) = util::change_dir_with_pwd(&target, logical_pwd) {
      return Err(sherr!(ExecFail @ span.clone(), "Failed to change directory: {e}"));
    }

    if print_dir {
      let pwd = PathBuf::from(var!("PWD"));
      outln_bytes(&util::display_path_bytes(&pwd));
    }

    with_status(0)
  }
}

fn search_cd_path(new_dir: impl AsRef<Path>) -> Option<PathBuf> {
  let path = var!("CDPATH");
  let path = path.to_str_lossy();

  // find the first path that contains a directory matching `new_dir`
  crate::util::split_path_list(&path).find_map(|p| {
    let resolved = p.join(&new_dir);
    resolved.is_dir().then_some(resolved)
  })
}

struct Sort {
  reverse: bool,
  kind: SortKind,
}

#[derive(PartialEq, Eq, Copy, Clone, Debug)]
enum SortKind {
  Frecency,
  Visits,
  Recent,
  Path,
}

/// The `zd` builtin. Uses directory history to jump to partial matches, instead of requiring an absolute path.
pub(super) struct Zd;
impl super::Builtin for Zd {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("recursive" | b'r'),
      opt!("depth" | b'd', 1),
      opt!("print" | b'p'),
      opt!("json"),
      opt!("quoted"),
      opt!("reverse"),
      opt!("sort"),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let first = args.arguments().next().map(|(s, _)| s).cloned();
    let first = first.as_ref().map(|s| s.to_str_lossy());
    match first.as_deref() {
      // check subcommands
      Some("add") => Self::add(args),
      Some("remove") => Self::remove(args),
      Some("clean") => Self::clean(),
      Some("list") => Self::list(args),

      // normal case, querying for a directory
      _ => Self::query(args),
    }
  }
}

impl Zd {
  /// zd add [-r] [dirs...] - add directories
  fn add(args: super::BuiltinArgs) -> ShResult<()> {
    let depth = match args.options().find_map(|o| match o.key() {
      "depth" => o.value().ok(),
      _ => None,
    }) {
      Some(n) => match n.parse::<usize>() {
        Ok(n) => Some(n),
        Err(_) => return Err(sherr!(ParseErr @ args.span.clone(), "zd: invalid depth: {n}")),
      },
      None => None,
    };
    // a depth cap only makes sense recursively, so it implies -r.
    let recursive = depth.is_some() || args.options().any(|o| o.key() == "recursive");

    let mut dirs: Vec<PathBuf> = args
      .arguments()
      .skip(1) // the "add" subcommand itself
      .map(|(a, _)| PathBuf::from(a.clone()))
      .collect();
    if dirs.is_empty()
      && let Ok(cwd) = std::env::current_dir()
    {
      dirs.push(cwd);
    }

    let mut paths = Vec::new();
    for dir in dirs {
      if !dir.is_dir() {
        return Err(sherr!(ExecFail @ args.span, "zd: not a directory: {}", dir.display()));
      }
      if recursive {
        collect_subdirs(&dir, depth, &mut paths);
      } else if let Ok(canon) = dir.canonicalize() {
        paths.push(canon.to_string_lossy().into_owned());
      }
    }

    let Some(conn) = util::get_db_conn() else {
      return with_status(0);
    };
    let Ok(conn) = conn.try_lock() else {
      return with_status(0);
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
    with_status(0)
  }

  /// zd add [-r] <dirs...> - remove directories
  fn remove(args: super::BuiltinArgs) -> ShResult<()> {
    let recursive = args.options().any(|o| o.key() == "recursive");
    let targets: Vec<String> = args
      .arguments()
      .skip(1) // the "remove" subcommand itself
      .map(|(a, _)| a.to_string())
      .collect();
    if targets.is_empty() {
      return Err(sherr!(ExecFail @ args.span, "zd: remove requires a directory"));
    }

    let Some(conn) = util::get_db_conn() else {
      return with_status(0);
    };
    let Ok(conn) = conn.try_lock() else {
      return with_status(0);
    };
    let mut removed = 0;
    for target in &targets {
      let canon = std::fs::canonicalize(target)
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
    with_status(i32::from(removed == 0))
  }

  fn list(args: super::BuiltinArgs) -> ShResult<()> {
    let mut quoted = false;
    let mut json = false;

    let mut sort = Sort {
      reverse: false,
      kind: SortKind::Frecency,
    };

    for opt in args.options() {
      match opt.key() {
        // 'reverse' and 'recursive' both share the '-r' shorthand,
        // it means different things on different subcommands.
        "reverse" | "recursive" => sort.reverse = true,
        "json" => json = true,
        "quoted" => quoted = true,

        "sort" => match opt.value()? {
          "frecency" => sort.kind = SortKind::Frecency,
          "visits" => sort.kind = SortKind::Visits,
          "recent" => sort.kind = SortKind::Recent,
          "path" => sort.kind = SortKind::Path,
          val => return Err(sherr!(ParseErr @ opt.span(), "invalid sort kind: {val}")),
        },
        _ => return Err(sherr!(ParseErr @ opt.span(), "invalid option: {opt}")),
      }
    }
    if json && quoted {
      return Err(sherr!(ParseErr @ args.span, "--json and --quoted are mutually exclusive"));
    }

    let query = args
      .arguments()
      .skip(1)
      .map(|(a, _)| a.to_str_lossy())
      .collect::<String>();

    let mut rows = load_dir_stats();

    if rows.is_empty() {
      return Err(sherr!(ExecFail @ args.span, "zd: no directory history yet"));
    }

    if !query.is_empty() {
      rows.retain(|r| r.path.contains(&query));
    }

    let default_desc = !matches!(sort.kind, SortKind::Path);
    let descending = default_desc != sort.reverse;
    rows.sort_by(|a, b| {
      let ord = match sort.kind {
        SortKind::Frecency => a.frecency.cmp(&b.frecency),
        SortKind::Visits => a.visits.cmp(&b.visits),
        SortKind::Recent => a.last_visit.cmp(&b.last_visit),
        SortKind::Path => a.path.cmp(&b.path),
      }
      .then_with(|| a.path.cmp(&b.path)); // tie-breaker

      if descending { ord.reverse() } else { ord }
    });

    if quoted {
      // SQR serialization
      let mut entries = vec![];

      for row in &rows {
        let mut entry = vec![];
        let DirStat {
          path,
          visits,
          last_visit,
          frecency,
        } = row;

        // Same column order as the bare output (path last), just shell-quoted.
        entry.push(expand::shell_quote(&visits.to_string()));
        entry.push(expand::shell_quote(&last_visit.to_string()));
        entry.push(expand::shell_quote(&frecency.to_string()));
        entry.push(expand::shell_quote(path));

        entries.push(entry.join(" ")); // SQR fields are separated by spaces
      }

      let output = entries.join("\n"); // SQR rows are separated by newlines

      outln!("{output}");
    } else if json {
      // JSON formatted output
      let mut entries = vec![];

      for row in &rows {
        let mut map = serde_json::Map::new();
        let DirStat {
          path,
          visits,
          last_visit,
          frecency,
        } = row;

        map.insert("path".to_string(), serde_json::Value::String(path.clone()));
        map.insert(
          "visits".to_string(),
          serde_json::Value::Number((*visits).into()),
        );
        map.insert(
          "last_visit".to_string(),
          serde_json::Value::Number((*last_visit).into()),
        );
        map.insert(
          "frecency".to_string(),
          serde_json::Value::Number((*frecency).into()),
        );

        entries.push(serde_json::Value::Object(map));
      }

      let json_arr = serde_json::Value::Array(entries);
      let output = serde_json::to_string_pretty(&json_arr).unwrap();

      outln!("{output}");
    } else {
      // no format specified, use tab-separated values
      let mut entries = vec![];

      for row in &rows {
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
      let output = entries.join("\n");

      outln!("{output}");
    }

    with_status(0)
  }

  /// `zd clean` - prune entries whose directory no longer exists.
  fn clean() -> ShResult<()> {
    let Some(conn) = util::get_db_conn() else {
      return with_status(0);
    };
    let Ok(conn) = conn.try_lock() else {
      return with_status(0);
    };
    let dead: Vec<String> = {
      let Ok(mut stmt) = conn.prepare("SELECT path FROM dir_history") else {
        return with_status(0);
      };
      let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return with_status(0);
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
    with_status(0)
  }

  fn query(args: super::BuiltinArgs) -> ShResult<()> {
    // every positional is concatenated into one subsequence query, so
    // `zd pro fern` still finds `~/projects/fern`.
    let query: String = args.arguments().map(|(a, _)| a.to_str_lossy()).collect();
    let print_dir = args.options().any(|o| o.key() == "print");

    let entries = if query.is_empty() {
      load_abbreviated_dirs()
    } else {
      load_dir_entries()
    };
    if entries.is_empty() {
      return Err(sherr!(ExecFail @ args.span, "zd: no directory history yet"));
    }

    let mut target = if query.is_empty() {
      if !Shed::term(Terminal::interactive) {
        return Err(
          sherr!(ExecFail @ args.span, "zd: a directory query is required when non-interactive"),
        );
      }
      // no argument, let's open the fuzzy finder
      let selector = FuzzyBuilder::new()
        .with_entries(entries)
        .with_placeholder("pick a directory (type to filter, enter selects, esc cancels)")
        .with_score_cb(fuzzy_score_dir)
        .with_highlight_cb(highlight_dir);

      selector.pick()?
    } else {
      fuzzy_best_match(&query, entries, Some(fuzzy_score_dir), None)
    };

    if let Some(target) = target.as_mut()
      && target.starts_with('~')
      && let Some(home) = util::get_home_str()
    {
      *target = target.replacen('~', &home.to_str_lossy(), 1);
    }

    match target {
      Some(path) => {
        if print_dir {
          outln!("{path}");
        } else if let Err(e) = util::change_dir(&path) {
          return Err(sherr!(ExecFail @ args.span, "zd: could not change directory: {e}"));
        }
        with_status(0)
      }
      // cancelled, or nothing matched the query
      None => with_status(1),
    }
  }
}

struct DirStat {
  path: String,
  visits: i64,
  last_visit: i64,
  frecency: i32,
}

fn query_dir_stats(conn: &rusqlite::Connection) -> Vec<DirStat> {
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

fn load_dir_stats() -> Vec<DirStat> {
  if let Some(shared) = util::get_db_conn() {
    let Ok(conn) = shared.try_lock() else {
      return vec![];
    };
    query_dir_stats(&conn)
  } else if let Ok(conn) = util::open_db_conn_readonly() {
    query_dir_stats(&conn)
  } else {
    vec![]
  }
}

/// Highlight the basename match, mirroring how `fuzzy_score_dir` rewards it, so
/// the underline lands on (e.g.) the "dev" in ".../dev-shells" rather than being
/// smeared across parent segments. Falls back to the default full-path match.
fn highlight_dir(display: &str, query: &str) -> Option<Vec<usize>> {
  let base = Path::new(display).file_name()?.to_str()?;
  // char offset of the basename within the display string (positions are chars).
  let offset = display.chars().count() - base.chars().count();
  let positions = match_positions(base, query);
  (!positions.is_empty()).then(|| positions.into_iter().map(|p| p + offset).collect())
}

fn fuzzy_score_dir(cand: &str, chars: &[char], penalize_len_diff: bool) -> i32 {
  let path = Path::new(cand);

  // An exact path match is unambiguous, so it always wins. This breaks ties like
  // "/home/me" vs "/home/me/projects" for the query "/home/me", where the matched
  // prefix otherwise scores identically for both.
  if chars.iter().copied().eq(cand.chars()) {
    return i32::MAX;
  }

  // Otherwise, add the basename's own score on top of the full-path score, so a
  // match on the final segment ("fer" -> ".../fern") outranks one smeared across
  // parent directories. Double-counting the basename is the point.
  if let Some(base) = path.file_name().and_then(|b| b.to_str()) {
    let base_score = fuzzy_match_score(base, chars, penalize_len_diff);
    let full = fuzzy_match_score(cand, chars, penalize_len_diff);
    if base_score > i32::MIN && full > i32::MIN {
      return full.saturating_add(base_score);
    }
  }

  fuzzy_match_score(cand, chars, penalize_len_diff)
}

fn now_secs() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |d| d.as_secs() as i64)
}

/// Recursively collect canonical paths of `root` and its subdirectories,
/// skipping hidden directories and symlinks (avoids `.git` clutter and loops).
/// `depth` caps how many levels below `root` to descend: `Some(0)` adds only
/// `root`, `None` recurses without limit.
fn collect_subdirs(root: &Path, depth: Option<usize>, out: &mut Vec<String>) {
  if let Ok(canon) = root.canonicalize() {
    out.push(canon.to_string_lossy().into_owned());
  }
  if depth == Some(0) {
    return;
  }
  let Ok(entries) = std::fs::read_dir(root) else {
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
fn dir_frecency(visits: i64, age_secs: i64) -> i32 {
  let factor = match age_secs {
    s if s < 3_600 => 4,   // within the hour
    s if s < 86_400 => 3,  // within the day
    s if s < 604_800 => 2, // within the week
    _ => 1,
  };
  visits.saturating_mul(factor).clamp(0, i64::from(i32::MAX)) as i32
}

fn load_dir_entries() -> Vec<(String, i32)> {
  load_dir_entries_inner(false)
}

fn load_abbreviated_dirs() -> Vec<(String, i32)> {
  load_dir_entries_inner(true)
}

/// Load visited directories as `(path, frecency weight)`, skipping any that no
/// longer exist on disk.
fn load_dir_entries_inner(format_paths: bool) -> Vec<(String, i32)> {
  let Some(conn) = util::get_db_conn() else {
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
        util::display_path(path)
      } else {
        path
      };
      (path, dir_frecency(visits, now - last_visit))
    })
    .collect()
}

fn get_old_pwd() -> PathBuf {
  try_var!("OLDPWD")
    .or_else(|| util::get_home_str().or_else(|| Some("/".into())))
    .map(PathBuf::from)
    .unwrap()
}

#[cfg(test)]
pub mod tests {
  use std::env;
  use std::fs;

  use tempfile::TempDir;

  use crate::var;
  use crate::{
    state::{
      self, Shed,
      vars::{VarFlags, VarKind},
    },
    tests::testutil::{TestGuard, canon, test_input},
  };

  // ===================== Basic Navigation =====================

  #[test]
  fn cd_simple() {
    let _g = TestGuard::new();
    let old_dir = env::current_dir().unwrap();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    let new_dir = env::current_dir().unwrap();
    assert_ne!(old_dir, new_dir);

    assert_eq!(
      new_dir.display().to_string(),
      canon(temp_dir.path()).display().to_string()
    );
  }

  #[test]
  fn cd_logical_keeps_kernel_cwd_in_sync_across_symlink() {
    // Regression: in logical mode (-L, the default), `cd link; cd ..` must
    // chdir the *logical* path so the kernel cwd matches $PWD (POSIX cd -L).
    // Previously `..` was resolved physically, desyncing the kernel cwd from
    // $PWD when the symlink's parent differed from its target's parent.
    let _g = TestGuard::new();
    let base = TempDir::new().unwrap();
    // Canonicalize up front so /tmp being a symlink (e.g. on macOS) can't skew
    // the comparison.
    let root = canon(base.path());
    let parent_a = root.join("A");
    let target = root.join("B").join("target");
    fs::create_dir_all(&parent_a).unwrap();
    fs::create_dir_all(&target).unwrap();
    let link = parent_a.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    test_input(format!("cd {}", link.display())).unwrap();
    test_input("cd ..").unwrap();

    let kernel = env::current_dir().unwrap().display().to_string();
    let pwd = var!("PWD").to_string();
    // The core of the bug: kernel cwd and $PWD must agree.
    assert_eq!(kernel, pwd, "kernel cwd must match $PWD (no desync)");
    // And both must be the logical parent A, not the target's physical parent B.
    assert_eq!(
      pwd,
      parent_a.display().to_string(),
      "expected logical parent A"
    );
  }

  #[test]
  fn cd_no_args_goes_home() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "HOME",
        VarKind::Str(temp_dir.path().display().to_string().into()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    test_input("cd").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(temp_dir.path()).display().to_string()
    );
  }

  #[test]
  fn cd_relative_path() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let sub = temp_dir.path().join("child");
    fs::create_dir(&sub).unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();
    test_input("cd child").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), canon(&sub).display().to_string());
  }

  // ===================== Environment =====================

  #[test]
  fn cd_status_zero_on_success() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Error Cases =====================

  #[test]
  fn cd_nonexistent_dir_fails() {
    let _g = TestGuard::new();
    test_input("cd /nonexistent_path_that_does_not_exist_xyz").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn cd_file_not_directory_fails() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("afile.txt");
    fs::write(&file_path, "hello").unwrap();

    test_input(format!("cd {}", file_path.display())).ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Multiple cd =====================

  #[test]
  fn cd_multiple_times() {
    let _g = TestGuard::new();
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    test_input(format!("cd {}", dir_a.path().display())).unwrap();
    assert_eq!(
      env::current_dir().unwrap().display().to_string(),
      canon(dir_a.path()).display().to_string()
    );

    test_input(format!("cd {}", dir_b.path().display())).unwrap();
    assert_eq!(
      env::current_dir().unwrap().display().to_string(),
      canon(dir_b.path()).display().to_string()
    );
  }

  #[test]
  fn cd_nested_subdirectories() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let deep = temp_dir.path().join("a").join("b").join("c");
    fs::create_dir_all(&deep).unwrap();

    test_input(format!("cd {}", deep.display())).unwrap();
    assert_eq!(
      env::current_dir().unwrap().display().to_string(),
      canon(&deep).display().to_string()
    );
  }

  // ===================== Autocmd Integration =====================

  #[test]
  fn cd_fires_post_change_dir_autocmd() {
    let guard = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input("autocmd post-change-dir 'echo cd-hook-fired'").unwrap();
    guard.read_output();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();
    let out = guard.read_output();
    assert!(out.contains("cd-hook-fired"));
  }

  #[test]
  fn cd_fires_pre_change_dir_autocmd() {
    let guard = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input("autocmd pre-change-dir 'echo pre-cd'").unwrap();
    guard.read_output();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();
    let out = guard.read_output();
    assert!(out.contains("pre-cd"));
  }

  // ===================== OLDPWD / cd - =====================

  #[test]
  fn cd_sets_oldpwd() {
    let _g = TestGuard::new();
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    test_input(format!("cd {}", dir_a.path().display())).unwrap();
    test_input(format!("cd {}", dir_b.path().display())).unwrap();

    // -L semantics: OLDPWD preserves the path the user typed, not the
    // canonical form. On macOS `/var/folders/...` is a symlink to
    // `/private/var/folders/...` so comparing against `canon(...)` would
    // wrongly canonicalize it.
    let oldpwd = var!("OLDPWD");
    assert_eq!(oldpwd, dir_a.path().display().to_string());
  }

  #[test]
  fn cd_sets_pwd_var() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    // -L semantics: $PWD reflects what the user typed, not the canonical
    // kernel cwd. The kernel cwd can differ if any component of the input
    // path is a symlink (e.g. macOS's `/var` → `/private/var`).
    let pwd = var!("PWD");
    assert_eq!(pwd, temp_dir.path().display().to_string());
  }

  #[test]
  fn cd_hyphen_goes_to_oldpwd() {
    let _g = TestGuard::new();
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    test_input(format!("cd {}", dir_a.path().display())).unwrap();
    test_input(format!("cd {}", dir_b.path().display())).unwrap();
    test_input("cd -").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(dir_a.path()).display().to_string()
    );
  }

  #[test]
  fn cd_hyphen_toggles() {
    let _g = TestGuard::new();
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    test_input(format!("cd {}", dir_a.path().display())).unwrap();
    test_input(format!("cd {}", dir_b.path().display())).unwrap();
    test_input("cd -").unwrap();
    test_input("cd -").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(dir_b.path()).display().to_string()
    );
  }

  // ===================== CDPATH =====================

  #[test]
  fn cd_uses_cdpath() {
    let _g = TestGuard::new();
    let base = TempDir::new().unwrap();
    let target = base.path().join("mydir");
    fs::create_dir(&target).unwrap();

    Shed::vars_mut(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(base.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input("cd mydir").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(&target).display().to_string()
    );
  }

  #[test]
  fn cd_cdpath_skips_nonexistent() {
    let _g = TestGuard::new();
    let base = TempDir::new().unwrap();
    let target = base.path().join("realdir");
    fs::create_dir(&target).unwrap();

    Shed::vars_mut(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(format!("/nonexistent_cdpath_xyz:{}", base.path().to_string_lossy()).into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input("cd realdir").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(&target).display().to_string()
    );
  }

  #[test]
  fn cd_cdpath_not_used_for_absolute() {
    let _g = TestGuard::new();
    let target = TempDir::new().unwrap();
    let decoy = TempDir::new().unwrap();

    Shed::vars_mut(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(decoy.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input(format!("cd {}", target.path().display())).unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(target.path()).display().to_string()
    );
  }

  #[test]
  fn cd_cdpath_not_used_for_dot() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let sub = temp_dir.path().join("child");
    fs::create_dir(&sub).unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    let decoy = TempDir::new().unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(decoy.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input("cd ./child").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), canon(&sub).display().to_string());
  }

  // ===================== -P option =====================

  #[test]
  fn cd_p_resolves_symlinks() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let real_dir = temp_dir.path().join("real");
    let link_dir = temp_dir.path().join("link");
    fs::create_dir(&real_dir).unwrap();
    std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();

    test_input(format!("cd -P {}", link_dir.display())).unwrap();

    let cwd = env::current_dir().unwrap();
    let canonical_real = fs::canonicalize(&real_dir).unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canonical_real.display().to_string()
    );
  }

  // ===================== -L (default) symlink preservation =====================

  #[test]
  fn cd_l_preserves_symlink_in_pwd() {
    // The bug from #73: by default `cd` should NOT resolve symlinks when
    // setting $PWD. The kernel cwd is canonical (no avoiding that), but
    // $PWD should reflect what the user typed.
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let real_dir = temp_dir.path().join("real");
    let link_dir = temp_dir.path().join("link");
    fs::create_dir(&real_dir).unwrap();
    std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();

    test_input(format!("cd {}", link_dir.display())).unwrap();

    let pwd = var!("PWD");
    assert_eq!(pwd, link_dir.display().to_string());
  }

  #[test]
  fn cd_l_dotdot_pops_lexically() {
    // After `cd /a/symlink-to-b`, `cd ..` with -L should land in /a (the
    // parent of the symlink path), not in the parent of the real dir.
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let real = temp_dir.path().join("real");
    let link = temp_dir.path().join("link");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    test_input(format!("cd {}", link.display())).unwrap();
    test_input("cd ..").unwrap();

    let pwd = var!("PWD");
    assert_eq!(pwd, temp_dir.path().display().to_string());
  }

  #[test]
  fn cd_l_normalizes_dotdot_in_input() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let sub = temp_dir.path().join("sub");
    fs::create_dir(&sub).unwrap();

    let weird = format!("{}/sub/../sub", temp_dir.path().display());
    test_input(format!("cd {weird}")).unwrap();

    let pwd = var!("PWD");
    assert_eq!(pwd, sub.display().to_string());
  }

  #[test]
  fn cd_p_pwd_is_canonical() {
    // Sanity: with -P, $PWD matches the kernel cwd (symlinks resolved).
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let real = temp_dir.path().join("real");
    let link = temp_dir.path().join("link");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    test_input(format!("cd -P {}", link.display())).unwrap();

    let pwd = var!("PWD");
    assert_eq!(pwd, fs::canonicalize(&real).unwrap().display().to_string());
  }

  // ===================== zd: frecency =====================

  #[test]
  fn frecency_recent_outranks_old() {
    // Same visit count, more recent wins.
    assert!(super::dir_frecency(3, 60) > super::dir_frecency(3, 60 * 60 * 24 * 30));
  }

  #[test]
  fn frecency_more_visits_outranks_fewer() {
    // Same age bucket, more visits wins.
    assert!(super::dir_frecency(10, 60) > super::dir_frecency(1, 60));
  }

  #[test]
  fn frecency_saturates_without_overflow() {
    assert_eq!(super::dir_frecency(i64::MAX, 60), i32::MAX);
  }

  // ===================== zd: directory scoring =====================

  fn qchars(s: &str) -> Vec<char> {
    s.chars().collect()
  }

  #[test]
  fn score_dir_exact_path_wins() {
    let q = qchars("/home/me");
    let exact = super::fuzzy_score_dir("/home/me", &q, false);
    let longer = super::fuzzy_score_dir("/home/me/projects", &q, false);
    assert_eq!(exact, i32::MAX);
    assert!(
      exact > longer,
      "exact path must outrank a longer prefix match"
    );
  }

  #[test]
  fn score_dir_basename_outranks_smeared() {
    let q = qchars("dev");
    // Basename "dev" matches cleanly; the other only matches across parent segments.
    let basename = super::fuzzy_score_dir("/a/b/dev", &q, false);
    let smeared = super::fuzzy_score_dir("/d/e/v/zzz", &q, false);
    assert!(basename > smeared);
  }

  #[test]
  fn score_dir_no_match_is_min() {
    let q = qchars("zzz");
    assert_eq!(
      super::fuzzy_score_dir("/home/me/projects", &q, false),
      i32::MIN
    );
  }

  // ===================== zd: highlighting =====================

  #[test]
  fn highlight_dir_marks_basename_with_offset() {
    // "/home/me/" is 9 chars, so the basename match lands at 9,10,11.
    let pos = super::highlight_dir("/home/me/dev-shells", "dev").unwrap();
    assert_eq!(pos, vec![9, 10, 11]);
  }

  #[test]
  fn highlight_dir_falls_back_when_basename_unmatched() {
    // Query matches only the parents → None, so the caller uses the full-path match.
    assert!(super::highlight_dir("/home/dev/xyz", "dev").is_none());
  }

  #[test]
  fn highlight_dir_none_without_basename() {
    assert!(super::highlight_dir("/", "x").is_none());
  }

  // ===================== zd: dir_history DB =====================

  fn fresh_dir_history() {
    let conn = state::util::get_db_conn().expect("test db");
    conn
      .lock()
      .unwrap()
      .execute_batch(
        "CREATE TABLE IF NOT EXISTS dir_history (
           path        TEXT     PRIMARY KEY NOT NULL,
           visits      INTEGER  NOT NULL DEFAULT 1,
           last_visit  INTEGER  NOT NULL
         );
         DELETE FROM dir_history;",
      )
      .unwrap();
  }

  fn dir_visits(path: &str) -> Option<i64> {
    let conn = state::util::get_db_conn().unwrap();
    let conn = conn.lock().unwrap();
    conn
      .query_row(
        "SELECT visits FROM dir_history WHERE path = ?1",
        [path],
        |r| r.get(0),
      )
      .ok()
  }

  fn insert_dir(path: &str, visits: i64, last_visit: i64) {
    let conn = state::util::get_db_conn().unwrap();
    conn
      .lock()
      .unwrap()
      .execute(
        "INSERT OR REPLACE INTO dir_history (path, visits, last_visit) VALUES (?1, ?2, ?3)",
        rusqlite::params![path, visits, last_visit],
      )
      .unwrap();
  }

  #[test]
  fn zd_add_inserts_canonical_path() {
    let _g = TestGuard::new();
    fresh_dir_history();
    let dir = TempDir::new().unwrap();
    test_input(format!("zd add {}", dir.path().display())).unwrap();
    let canon = fs::canonicalize(dir.path()).unwrap().display().to_string();
    assert_eq!(dir_visits(&canon), Some(1));
  }

  #[test]
  fn zd_add_is_idempotent() {
    let _g = TestGuard::new();
    fresh_dir_history();
    let dir = TempDir::new().unwrap();
    let cmd = format!("zd add {}", dir.path().display());
    test_input(&cmd).unwrap();
    test_input(&cmd).unwrap();
    let canon = fs::canonicalize(dir.path()).unwrap().display().to_string();
    // ON CONFLICT DO NOTHING: re-adding must not inflate the visit count.
    assert_eq!(dir_visits(&canon), Some(1));
  }

  #[test]
  fn zd_remove_deletes_entry() {
    let _g = TestGuard::new();
    fresh_dir_history();
    let dir = TempDir::new().unwrap();
    let canon = fs::canonicalize(dir.path()).unwrap().display().to_string();
    insert_dir(&canon, 5, 1000);
    test_input(format!("zd remove {}", dir.path().display())).unwrap();
    assert_eq!(dir_visits(&canon), None);
  }

  #[test]
  fn zd_clean_prunes_only_dead_dirs() {
    let _g = TestGuard::new();
    fresh_dir_history();
    let live = TempDir::new().unwrap();
    let live_canon = fs::canonicalize(live.path()).unwrap().display().to_string();
    insert_dir(&live_canon, 1, 1000);
    insert_dir("/nonexistent_zz_dir_12345", 1, 1000);
    test_input("zd clean").unwrap();
    assert!(
      dir_visits(&live_canon).is_some(),
      "existing dir must be kept"
    );
    assert_eq!(
      dir_visits("/nonexistent_zz_dir_12345"),
      None,
      "dead dir must be pruned"
    );
  }

  #[test]
  fn load_dir_entries_skips_missing_dirs() {
    let _g = TestGuard::new();
    fresh_dir_history();
    let live = TempDir::new().unwrap();
    let live_canon = fs::canonicalize(live.path()).unwrap().display().to_string();
    insert_dir(&live_canon, 3, 1000);
    insert_dir("/nonexistent_zz_dir_98765", 9, 9999);
    let entries = super::load_dir_entries();
    assert!(entries.iter().any(|(p, _)| p == &live_canon));
    assert!(!entries.iter().any(|(p, _)| p.starts_with("/nonexistent")));
  }
}
