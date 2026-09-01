//! `$PATH` scanning and caching
//!
//! Utilities for interacting with paths, like [`path_list_entries`], along with caching utilities for
//! path-list-style env vars like `$SHED_HPATH`.

use crate::{HashMap, state::vars::VarStr, util::strops};

use super::try_var;

use std::{
  os::unix::fs::PermissionsExt,
  path::{Path, PathBuf},
  time::SystemTime,
};

use nix::unistd::{User, getuid};

use super::var;

/// Caches the current state of a path-list-style env var (e.g. `$SHED_HPATH`)
/// so consumers can cheaply detect when either the var's value or any of the
/// referenced files have changed.
///
/// For directory entries in the var, both the directory's own mtime and every
/// contained file's mtime are tracked. On refresh, the directory mtime is
/// checked first: if it changed (entry add, remove, or rename), the directory
/// is re-walked. If it didn't, only existing files' content mtimes are
/// checked. The common "nothing changed" case costs one stat per directory
/// plus one stat per cached file.
pub(crate) struct PathCache {
  name: String,
  path_raw: VarStr,
  entries: HashMap<PathBuf, EntryCache>,
}

/// Cached state for one entry from the path list. Directory entries hold a
/// directory mtime plus an mtime for each file directly inside. Subdirectories
/// are not recursed into; the help system's HPATH layout is one level deep.
enum EntryCache {
  Dir {
    dir_mtime: SystemTime,
    files: HashMap<PathBuf, SystemTime>,
  },
  File(SystemTime),
}

pub(crate) fn path_from_bytes(bytes: &[u8]) -> &Path {
  use std::os::unix::ffi::OsStrExt;
  std::ffi::OsStr::from_bytes(bytes).as_ref()
}

fn mtime_of(path: &Path) -> SystemTime {
  std::fs::metadata(path)
    .and_then(|m| m.modified())
    .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn collect_files_in_dir(path: &Path) -> HashMap<PathBuf, SystemTime> {
  let mut files = HashMap::default();
  if let Ok(read) = std::fs::read_dir(path) {
    for entry in read.flatten() {
      let p = entry.path();
      let m = entry
        .metadata()
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
      files.insert(p, m);
    }
  }
  files
}

fn build_entry(path: &Path) -> EntryCache {
  if path.is_dir() {
    EntryCache::Dir {
      dir_mtime: mtime_of(path),
      files: collect_files_in_dir(path),
    }
  } else {
    EntryCache::File(mtime_of(path))
  }
}

impl PathCache {
  pub(crate) fn new(name: String) -> Self {
    let path_raw = var!(&name);
    let entries = Self::build_entries(&path_raw.to_str_lossy());
    Self {
      name,
      path_raw,
      entries,
    }
  }

  fn build_entries(path_raw: &str) -> HashMap<PathBuf, EntryCache> {
    split_path_list(path_raw)
      .map(|p| {
        let entry = build_entry(&p);
        (p, entry)
      })
      .collect()
  }

  /// Refreshes the cache against current disk state. Returns `true` if
  /// anything changed (var content, any directory's contents, or any file's
  /// mtime); `false` if everything is identical to the cached state.
  pub(crate) fn update_cache(&mut self) -> bool {
    let path_raw = var!(&self.name);
    if path_raw != self.path_raw {
      self.path_raw = path_raw;
      self.entries = Self::build_entries(&self.path_raw.to_str_lossy());
      return true;
    }

    let mut changed = false;
    for (path, entry) in &mut self.entries {
      let current_top_mtime = mtime_of(path);

      match entry {
        EntryCache::Dir { dir_mtime, files } => {
          if current_top_mtime == *dir_mtime {
            for (file_path, file_mtime) in files.iter_mut() {
              let current = mtime_of(file_path);
              if current != *file_mtime {
                *file_mtime = current;
                changed = true;
              }
            }
          } else {
            // dir mtime moved, so an entry was added, removed, or renamed.
            // re-walk picks up fresh mtimes for every file at once, so we
            // skip the per-file check below.
            *dir_mtime = current_top_mtime;
            *files = collect_files_in_dir(path);
            changed = true;
          }
        }
        EntryCache::File(mtime) => {
          if current_top_mtime != *mtime {
            *mtime = current_top_mtime;
            changed = true;
          }
        }
      }
    }

    changed
  }
}

pub(crate) fn resolve_in_path(path_list: &str, cmd: &str) -> Option<PathBuf> {
  for dir in split_path_list(path_list) {
    let candidate = dir.join(cmd);
    if let Ok(meta) = std::fs::metadata(&candidate)
      && meta.is_file()
      && meta.permissions().mode() & 0o111 != 0
    {
      return Some(candidate);
    }
  }
  None
}

/// Split a POSIX path-list-style string (colon separated paths) into an iterator of `PathBuf`s.
pub(crate) fn split_path_list(path_list: &str) -> impl Iterator<Item = PathBuf> {
  let paths = strops::split_all_with(
    path_list.as_bytes(),
    |paths| strops::split_at_unescaped(paths, b":"),
    |start, end| path_list[start..end].to_string(),
  );

  paths.into_iter().map(PathBuf::from)
}

pub(crate) fn path_entries<P: AsRef<Path>>(path: P) -> impl Iterator<Item = std::fs::DirEntry> {
  path
    .as_ref()
    .read_dir()
    .ok()
    .into_iter()
    .flatten()
    .filter_map(Result::ok)
}

pub(crate) fn path_list_entries(path_list: &str) -> impl Iterator<Item = std::fs::DirEntry> {
  let paths = split_path_list(path_list);

  paths.flat_map(|p| {
    p.read_dir()
      .ok()
      .into_iter()
      .flatten()
      .filter_map(Result::ok)
  })
}

pub(crate) fn is_executable_file(entry: &std::fs::DirEntry) -> bool {
  let ft = entry.file_type().ok();
  let is_symlink = ft.is_some_and(|t| t.is_symlink());
  let meta = if is_symlink {
    std::fs::metadata(entry.path())
  } else {
    entry.metadata()
  };
  let Ok(meta) = meta else {
    return false;
  };
  meta.is_file() && meta.permissions().mode() & 0o111 != 0
}

/// Parse `arr[idx]` into (name, `raw_index_expr`). Pure parsing, no expansion.
pub(crate) fn lex_normalize_path(path: &Path) -> PathBuf {
  use std::path::Component;
  let mut out: Vec<Component> = Vec::new();
  for comp in path.components() {
    match comp {
      Component::CurDir => {}
      Component::ParentDir => match out.last() {
        Some(Component::Normal(_)) => {
          out.pop();
        }
        Some(Component::RootDir | Component::Prefix(_)) => {}
        Some(Component::ParentDir) | None => out.push(comp),
        Some(Component::CurDir) => unreachable!(),
      },
      _ => out.push(comp),
    }
  }
  if out.is_empty() {
    PathBuf::from(".")
  } else {
    out.iter().collect()
  }
}

pub(crate) fn display_path<P: AsRef<Path>>(path: P) -> String {
  let s = path.as_ref().to_string_lossy().into_owned();
  if let Some(home) = get_home_str()
    && !home.is_empty()
    && let Some(rest) = s.strip_prefix(&*home.to_str_lossy())
  {
    format!("~{rest}")
  } else {
    s
  }
}

pub(crate) fn display_path_normalized<P: AsRef<Path>>(path: P) -> String {
  display_path(lex_normalize_path(path.as_ref()))
}

/// A filesystem path's exact bytes as a `VarStr`. Unix paths are arbitrary
/// bytes, so this avoids the lossy UTF-8 step of `display()`/`to_string_lossy`.
pub(crate) fn path_to_varstr(path: &Path) -> VarStr {
  use std::os::unix::ffi::OsStrExt;
  VarStr::from(path.as_os_str().as_bytes())
}

/// Byte-native counterpart to [`display_path`]: collapse a leading `$HOME` to
/// `~`, preserving arbitrary path bytes rather than laundering them.
pub(crate) fn display_path_bytes(path: &Path) -> Vec<u8> {
  use std::os::unix::ffi::OsStrExt;
  let bytes = path.as_os_str().as_bytes();
  if let Some(home) = get_home()
    && !home.as_os_str().is_empty()
    && let Some(rest) = bytes.strip_prefix(home.as_os_str().as_bytes())
  {
    let mut out = Vec::with_capacity(rest.len() + 1);
    out.push(b'~');
    out.extend_from_slice(rest);
    out
  } else {
    bytes.to_vec()
  }
}

#[derive(Debug, Clone, Copy)]
enum Fallback {
  Dir(&'static str),
  Var(&'static str),
}

fn get_dir(env: &str) -> Option<PathBuf> {
  try_var!(env).map(PathBuf::from).filter(|p| p.is_absolute())
}

fn xdg_dir(env: &str, fallback: Fallback) -> Option<PathBuf> {
  get_dir(env).or_else(|| match fallback {
    Fallback::Dir(d) => get_home().map(|h| h.join(d)),
    Fallback::Var(v) => get_dir(v),
  })
}

pub(crate) fn data_dir() -> Option<PathBuf> {
  xdg_dir("XDG_DATA_HOME", Fallback::Dir(".local/share"))
}
pub(crate) fn state_dir() -> Option<PathBuf> {
  xdg_dir("XDG_STATE_HOME", Fallback::Dir(".local/state"))
}
pub(crate) fn config_dir() -> Option<PathBuf> {
  xdg_dir("XDG_CONFIG_HOME", Fallback::Dir(".config"))
}
pub(crate) fn runtime_dir() -> Option<PathBuf> {
  xdg_dir("XDG_RUNTIME_DIR", Fallback::Var("TMPDIR"))
}

pub(crate) fn get_home() -> Option<PathBuf> {
  try_var!("HOME")
    .map(PathBuf::from)
    .or_else(|| User::from_uid(getuid()).ok().flatten().map(|u| u.dir))
}

pub(crate) fn get_home_str() -> Option<VarStr> {
  get_home().map(|h| h.to_string_lossy().into())
}

#[cfg(test)]
mod xdg_resolver_tests {
  use super::*;
  use crate::state::rc::rc_file_path;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::state::{Shed, db};
  use crate::tests::testutil::TestGuard;

  fn set_var(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name, VarKind::Str(val.into()), VarFlags::EXPORT)
        .unwrap();
    });
  }
  fn unset_var(name: &str) {
    Shed::vars_mut(|v| {
      v.unset_var(name).ok();
    });
  }

  // ─── xdg_config_home ──────────────────────────────────────────────

  #[test]
  fn xdg_config_home_uses_env_var_when_set() {
    let _g = TestGuard::new();
    set_var("XDG_CONFIG_HOME", "/explicit/xdg/config");
    assert_eq!(config_dir(), Some(PathBuf::from("/explicit/xdg/config")));
  }

  #[test]
  fn xdg_config_home_falls_back_to_home_dot_config() {
    let _g = TestGuard::new();
    unset_var("XDG_CONFIG_HOME");
    set_var("HOME", "/some/home");
    assert_eq!(config_dir(), Some(PathBuf::from("/some/home/.config")));
  }

  // ─── history DB migration (share -> state) ────────────────────────

  #[test]
  fn relocate_history_db_moves_db_and_sidecars() {
    use std::fs;
    let old_dir = tempfile::TempDir::new().unwrap();
    let new_dir = tempfile::TempDir::new().unwrap();
    let old_db = old_dir.path().join("shed").join("shed_hist.db");
    // New location's parent doesn't exist yet — relocate must create it.
    let new_db = new_dir.path().join("shed").join("shed_hist.db");

    fs::create_dir_all(old_db.parent().unwrap()).unwrap();
    fs::write(&old_db, b"legacy-db-contents").unwrap();
    fs::write(format!("{}-journal", old_db.display()), b"j").unwrap();

    db::relocate_history_db(&old_db, &new_db);

    assert!(!old_db.exists(), "legacy DB should have been moved away");
    assert_eq!(fs::read(&new_db).unwrap(), b"legacy-db-contents");
    assert!(
      new_dir
        .path()
        .join("shed")
        .join("shed_hist.db-journal")
        .exists(),
      "journal sidecar should have moved too"
    );
  }

  // ─── xdg_runtime_dir ──────────────────────────────────────────────

  #[test]
  fn xdg_runtime_dir_prefers_env_var() {
    let _g = TestGuard::new();
    set_var("XDG_RUNTIME_DIR", "/run/user/1000");
    assert_eq!(runtime_dir(), Some(PathBuf::from("/run/user/1000")));
  }

  #[test]
  fn xdg_runtime_dir_falls_back_to_tmpdir() {
    let _g = TestGuard::new();
    unset_var("XDG_RUNTIME_DIR");
    set_var("TMPDIR", "/custom/tmp");
    assert_eq!(runtime_dir(), Some(PathBuf::from("/custom/tmp")));
  }

  #[test]
  fn xdg_runtime_dir_none_when_no_runtime_dir_set() {
    let _g = TestGuard::new();
    unset_var("XDG_RUNTIME_DIR");
    unset_var("TMPDIR");
    // No invented /tmp fallback: the socket simply becomes unavailable.
    assert_eq!(runtime_dir(), None);
  }

  // ─── rc_file_path ─────────────────────────────────────────────────

  #[test]
  fn rc_file_path_shed_rc_env_var_overrides_everything() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("explicit.rc");
    std::fs::write(&path, "").unwrap();
    set_var("SHED_RC", &path.to_string_lossy());

    // Even with XDG file present, SHED_RC wins
    let xdg_dir = tempfile::TempDir::new().unwrap();
    set_var("XDG_CONFIG_HOME", &xdg_dir.path().to_string_lossy());
    std::fs::create_dir_all(xdg_dir.path().join("shed")).unwrap();
    std::fs::write(xdg_dir.path().join("shed").join("shedrc"), "").unwrap();

    assert_eq!(rc_file_path(), Some(path));
  }

  #[test]
  fn rc_file_path_prefers_xdg_over_legacy_when_both_exist() {
    let _g = TestGuard::new();
    unset_var("SHED_RC");
    let home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    set_var("HOME", &home.path().to_string_lossy());
    set_var("XDG_CONFIG_HOME", &xdg.path().to_string_lossy());

    std::fs::write(home.path().join(".shedrc"), "").unwrap();
    std::fs::create_dir_all(xdg.path().join("shed")).unwrap();
    let xdg_rc = xdg.path().join("shed").join("shedrc");
    std::fs::write(&xdg_rc, "").unwrap();

    assert_eq!(rc_file_path(), Some(xdg_rc));
  }

  #[test]
  fn rc_file_path_falls_back_to_legacy_when_xdg_does_not_exist() {
    let _g = TestGuard::new();
    unset_var("SHED_RC");
    let home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    set_var("HOME", &home.path().to_string_lossy());
    set_var("XDG_CONFIG_HOME", &xdg.path().to_string_lossy());

    let legacy = home.path().join(".shedrc");
    std::fs::write(&legacy, "").unwrap();

    assert_eq!(rc_file_path(), Some(legacy));
  }

  #[test]
  fn rc_file_path_returns_xdg_path_for_creation_when_neither_exists() {
    let _g = TestGuard::new();
    unset_var("SHED_RC");
    let home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    set_var("HOME", &home.path().to_string_lossy());
    set_var("XDG_CONFIG_HOME", &xdg.path().to_string_lossy());

    let expected = xdg.path().join("shed").join("shedrc");
    assert_eq!(rc_file_path(), Some(expected));
  }
}
