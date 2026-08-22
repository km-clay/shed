use std::{
  os::unix::fs::PermissionsExt,
  path::{Path, PathBuf},
  time::SystemTime,
};

use crate::{HashMap, state::vars::VarStr, var};

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
  pub fn new(name: String) -> Self {
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
  pub fn update_cache(&mut self) -> bool {
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

pub fn resolve_in_path(path_list: &str, cmd: &str) -> Option<PathBuf> {
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

pub fn split_path_list(path_list: &str) -> impl Iterator<Item = PathBuf> {
  // `split_all_with` calls `build(start, end)` — both byte indices into
  // `path_list`, not (start, length). Naming the second arg `end` keeps the
  // slice expression honest; the previous `start + len` form treated it as
  // a length and ran past the buffer once `cursor` grew large.
  let paths = super::strops::split_all_with(
    path_list.as_bytes(),
    |paths| super::split_at_unescaped(paths, b":"),
    |start, end| path_list[start..end].to_string(),
  );

  paths.into_iter().map(PathBuf::from)
}

pub fn path_entries<P: AsRef<Path>>(path: P) -> impl Iterator<Item = std::fs::DirEntry> {
  path
    .as_ref()
    .read_dir()
    .ok()
    .into_iter()
    .flatten()
    .filter_map(Result::ok)
}

pub fn path_list_entries(path_list: &str) -> impl Iterator<Item = std::fs::DirEntry> {
  let paths = split_path_list(path_list);

  paths.flat_map(|p| {
    p.read_dir()
      .ok()
      .into_iter()
      .flatten()
      .filter_map(Result::ok)
  })
}

pub fn is_executable_file(entry: &std::fs::DirEntry) -> bool {
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
