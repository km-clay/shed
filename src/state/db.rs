use super::try_var;

use std::{
  path::{Path, PathBuf},
  sync::{
    Arc, Mutex, Once, RwLock,
    atomic::{AtomicBool, Ordering},
  },
};

use nix::libc;
use rusqlite::Connection;

use crate::{
  state::{paths, vars::VarStr},
  util::error::ShResult,
};

/// Parse `arr[idx]` into (name, `raw_index_expr`). Pure parsing, no expansion.
pub(crate) fn query_db<T, F: FnOnce(&Connection) -> ShResult<T>>(f: F) -> ShResult<Option<T>> {
  let Some(conn) = get_db_conn() else {
    return Ok(None);
  };
  let conn = conn
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  f(&conn).map(Some)
}

static DB_CONN: RwLock<Option<Arc<Mutex<Connection>>>> = RwLock::new(None);

pub(crate) static FORKED_CHILD: AtomicBool = AtomicBool::new(false);
static ATFORK_ONCE: Once = Once::new();

extern "C" fn mark_forked_child() {
  FORKED_CHILD.store(true, Ordering::Relaxed);
}

pub(crate) fn register_fork_marker() {
  ATFORK_ONCE.call_once(|| unsafe {
    libc::pthread_atfork(None, None, Some(mark_forked_child));
  });
}

fn open_shared_conn() -> Option<Arc<Mutex<Connection>>> {
  // Idempotent; the primary registration is at startup (`register_fork_marker`
  // in lifecycle::setup). Kept here too so the DB-fencing flag is armed even on
  // paths that open the DB without going through `setup` (e.g. tests).
  register_fork_marker();
  crate::procio::do_something_that_opens_fds_that_we_cant_access_hack(
    crate::procio::MIN_INTERNAL_FD,
    || configure_conn(false),
  )
}

fn configure_conn(is_retry: bool) -> Option<Arc<Mutex<Connection>>> {
  let path = history_db_path();
  let conn = match open_db_conn() {
    Ok(c) => c,
    Err(e) => {
      log::error!("could not open history database at {}: {e}", path.display());
      return None;
    }
  };

  if let Err(e) = conn.busy_timeout(std::time::Duration::from_secs(5)) {
    log::warn!("could not set history database busy timeout: {e}");
  }

  match conn.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0)) {
    Ok(s) if s == "ok" => {}
    Ok(s) => {
      if is_retry {
        log::error!("freshly created history database still fails integrity check: {s}");
        return None;
      }
      log::error!(
        "history database at {} is corrupt ({s}); quarantining it and starting fresh",
        path.display()
      );
      drop(conn);
      quarantine_db(&path);
      return configure_conn(true);
    }
    Err(e) => log::warn!("history database integrity check could not run: {e}"),
  }

  if let Err(e) = conn.execute_batch("PRAGMA journal_mode=DELETE") {
    log::warn!(
      "could not switch history database to rollback-journal mode, continuing in its current mode: {e}"
    );
  }
  if let Err(e) = conn.execute_batch("PRAGMA case_sensitive_like = 1") {
    log::warn!("could not set case_sensitive_like on history database: {e}");
  }

  Some(Arc::new(Mutex::new(conn)))
}

/// Rename a corrupt database and its journal/WAL sidecars aside so a fresh one
/// can take its place.
fn quarantine_db(path: &Path) {
  let stamp = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| d.as_secs());
  for suffix in ["", "-wal", "-shm", "-journal"] {
    let from = PathBuf::from(format!("{}{suffix}", path.display()));
    if !from.exists() {
      continue;
    }
    let to = PathBuf::from(format!("{}{suffix}.corrupt.{stamp}", path.display()));
    match std::fs::rename(&from, &to) {
      Ok(()) => log::error!(
        "quarantined corrupt history file {} -> {}",
        from.display(),
        to.display()
      ),
      Err(e) => log::error!(
        "could not quarantine corrupt history file {}: {e}",
        from.display()
      ),
    }
  }
}

/// Try to obtain a connection to shed's sqlite database
///
/// Returns `None` in forked child processes
pub(crate) fn get_db_conn() -> Option<Arc<Mutex<Connection>>> {
  // A forked child must not use the connection it inherited from the parent.
  if FORKED_CHILD.load(Ordering::Relaxed) {
    return None;
  }
  if let Some(conn) = DB_CONN
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .as_ref()
  {
    return Some(conn.clone());
  }
  let mut guard = DB_CONN
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  if guard.is_none() {
    *guard = open_shared_conn();
  }
  guard.clone()
}

#[cfg(test)]
pub(crate) fn init_test_db_conn() {
  *DB_CONN
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = Connection::open_in_memory()
    .ok()
    .map(|c| Arc::new(Mutex::new(c)));
}

// Functions for history database path migration

/// The default on-disk path of the history database, under `$XDG_STATE_HOME`
/// (`~/.local/state/shed/shed_hist.db`). `None` only if `dirs` cannot resolve a
/// state dir (non-Linux platforms), in which case the `$HOME` fallback is used.
fn default_state_db_path() -> Option<PathBuf> {
  paths::state_dir().map(|p| p.join("shed").join("shed_hist.db"))
}

/// The "old" path to the history database
fn legacy_data_db_path() -> Option<PathBuf> {
  paths::data_dir().map(|p| p.join("shed").join("shed_hist.db"))
}

/// The on-disk path of the history database.
fn history_db_path() -> PathBuf {
  let db_path: VarStr = if let Some(var) = try_var!("SHED_HISTDB") {
    var
  } else {
    let home = try_var!("HOME").unwrap_or_else(|| ".".into());
    default_state_db_path().map_or_else(
      || format!("{home}/.local/state/shed/shed_hist.db").into(),
      |p| p.to_string_lossy().into(),
    )
  };
  PathBuf::from(db_path)
}

/// Migrate history database file from the legacy path to the new one
fn migrate_legacy_history_db(new_path: &Path) {
  // Scope strictly to the default target so a custom SHED_HISTDB is left alone.
  let Some(default_new) = default_state_db_path() else {
    return;
  };
  if new_path != default_new || new_path.exists() {
    return;
  }
  let Some(old_path) = legacy_data_db_path() else {
    return;
  };
  if !old_path.exists() {
    return;
  }

  relocate_history_db(&old_path, new_path);
}

/// Move a history DB and its journal/WAL sidecars from `old_path` to `new_path`,
/// creating `new_path`'s parent. A cross-filesystem `rename` falls back to
/// copy-then-remove. Best-effort: failures are logged, never fatal.
pub(crate) fn relocate_history_db(old_path: &Path, new_path: &Path) {
  if let Some(parent) = new_path.parent()
    && let Err(e) = std::fs::create_dir_all(parent)
  {
    log::warn!(
      "history migration: could not create {}: {e}; leaving legacy DB in place",
      parent.display()
    );
    return;
  }

  for suffix in ["", "-wal", "-shm", "-journal"] {
    let from = PathBuf::from(format!("{}{suffix}", old_path.display()));
    if !from.exists() {
      continue;
    }
    let to = PathBuf::from(format!("{}{suffix}", new_path.display()));
    match std::fs::rename(&from, &to) {
      Ok(()) => log::info!("migrated history {} -> {}", from.display(), to.display()),
      Err(_) => match std::fs::copy(&from, &to).and_then(|_| std::fs::remove_file(&from)) {
        Ok(()) => log::info!(
          "migrated history (copy) {} -> {}",
          from.display(),
          to.display()
        ),
        Err(e) => log::warn!(
          "history migration: could not move {} -> {}: {e}",
          from.display(),
          to.display()
        ),
      },
    }
  }
}

pub(crate) fn open_db_conn() -> ShResult<Connection> {
  let db_path = history_db_path();
  // Relocate a pre-XDG-state (~/.local/share) history DB before we'd otherwise
  // create a fresh empty one at the new location.
  migrate_legacy_history_db(&db_path);
  if let Some(parent) = db_path.parent() {
    std::fs::create_dir_all(parent)?;
  }

  Ok(Connection::open(&db_path)?)
}

/// Open a fresh read-only connection to the history database. Unlike
/// [`get_db_conn`], this is safe to call in a forked child: it's a brand-new
/// handle rather than the fenced inherited one, and being read-only it cannot
/// corrupt the rollback journal. Callers must not rely on it for migrations or
/// writes (the file is expected to already be migrated by the parent).
pub(crate) fn open_db_conn_readonly() -> ShResult<Connection> {
  let db_path = history_db_path();
  let conn = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
  conn.busy_timeout(std::time::Duration::from_secs(5)).ok();
  Ok(conn)
}
