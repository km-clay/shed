use std::{
  cmp::Ordering,
  env,
  fmt::Display,
  ops::Deref,
  str::FromStr,
  sync::{Arc, LazyLock, Mutex, MutexGuard, RwLock},
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;

use super::{
  complete::{Candidate, FuzzySelector},
  editcmd::Direction,
  linebuf::{Hint, LineBuf, Lines},
  procio::{MIN_INTERNAL_FD, do_something_that_opens_fds_that_we_cant_access_hack},
  sherr, shopt,
  util::error::ShResult,
};
use crate::{
  HashMap,
  state::{Shed, db},
  util::random::Uuid,
};

#[derive(Debug, Clone)]
pub(crate) struct HistEntry {
  pub runtime: Duration,
  pub timestamp: SystemTime,
  pub command: String,
  pub cwd: String,
  pub status: i32,
  pub token: Uuid,
}

type HistTables = HashMap<CacheKey, Vec<HistEntry>>;

pub(crate) const MAIN_HIST_TABLE_NAME: &str = "shed_history";

static HIST_ENTRIES: LazyLock<Arc<RwLock<HistTables>>> =
  LazyLock::new(|| Arc::new(RwLock::new(HashMap::default())));

static SEARCH_ENTRIES: LazyLock<Arc<RwLock<HistTables>>> =
  LazyLock::new(|| Arc::new(RwLock::new(HashMap::default())));

static SEARCH_WATERMARKS: LazyLock<Arc<RwLock<HashMap<CacheKey, i64>>>> =
  LazyLock::new(|| Arc::new(RwLock::new(HashMap::default())));

fn num_entries(key: &CacheKey) -> usize {
  HIST_ENTRIES
    .read()
    .ok()
    .and_then(|cache| cache.get(key).map(Vec::len))
    .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeResult {
  FastForward,
  UpToDate,
  Merged,
}

fn timestamp_secs(ts: SystemTime) -> i64 {
  ts.duration_since(UNIX_EPOCH)
    .map_or(0, |d| d.as_secs() as i64)
}

impl Default for HistEntry {
  fn default() -> Self {
    Self {
      runtime: Duration::default(),
      timestamp: SystemTime::now(),
      command: String::new(),
      cwd: String::new(),
      status: 0,
      token: Uuid::new_v4(),
    }
  }
}

impl HistEntry {
  pub(crate) fn command(&self) -> &str {
    self.command.as_str()
  }
  /// The raw command bytes (byte-native; preserves non-UTF-8).
  pub(crate) fn command_bytes(&self) -> &[u8] {
    self.command.as_bytes()
  }
}

fn query_since(since_ts: i64, conn: &Connection, table: &Table, branch: &Branch) -> Vec<HistEntry> {
  let sql = format!(
    r"
    WITH RECURSIVE reachable(token) AS (
      SELECT head FROM branches WHERE name = ?1
      UNION
      SELECT CASE k WHEN 0 THEN h.parent ELSE h.joint END
      FROM {table} h
      JOIN reachable r ON h.token = r.token
      CROSS JOIN (SELECT 0 AS k UNION ALL SELECT 1) ks
      WHERE (k = 0 AND h.parent IS NOT NULL)
         OR (k = 1 AND h.joint IS NOT NULL)
    )
    SELECT command, MAX(timestamp) as ts, runtime, cwd, status, token FROM {table}
    WHERE token IN (SELECT token FROM reachable) AND joint IS NULL
    GROUP BY command
    HAVING MAX(timestamp) >= ?2
    ORDER BY ts ASC
    "
  );
  let Ok(mut stmt) = conn.prepare(&sql) else {
    return vec![];
  };
  match stmt.query_map(rusqlite::params![**branch, since_ts], History::row_to_entry) {
    Ok(iter) => iter.filter_map(Result::ok).collect(),
    Err(_) => vec![],
  }
}

fn query_masked(
  prefix: Option<&str>,
  conn: &Connection,
  table: &Table,
  branch: &Branch,
) -> Vec<HistEntry> {
  use std::fmt::Write;
  let mut sql = String::new();

  let _ = write!(
    sql,
    r"
    WITH RECURSIVE reachable(token) AS (
      SELECT head FROM branches WHERE name = ?1
      UNION
      SELECT CASE k WHEN 0 THEN h.parent ELSE h.joint END
      FROM {table} h
      JOIN reachable r ON h.token = r.token
      CROSS JOIN (SELECT 0 AS k UNION ALL SELECT 1) ks
      WHERE (k = 0 AND h.parent IS NOT NULL)
         OR (k = 1 AND h.joint IS NOT NULL)
    )
    SELECT command, MAX(timestamp) as ts, runtime, cwd, status, token FROM {table}
    WHERE token IN (SELECT token FROM reachable) AND joint IS NULL
    "
  );
  if prefix.is_some() {
    sql.push_str(
      "
      AND command LIKE ?2 || '%'
      ",
    );
  }
  sql.push_str(
    "
    GROUP BY command
    ORDER BY ts ASC
    ",
  );

  let Ok(mut stmt) = conn.prepare(&sql) else {
    return vec![];
  };

  let rows = match prefix {
    Some(p) => stmt.query_map(rusqlite::params![**branch, p], History::row_to_entry),
    None => stmt.query_map(rusqlite::params![**branch], History::row_to_entry),
  };

  match rows {
    Ok(iter) => iter.filter_map(Result::ok).collect(),
    Err(_) => vec![],
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CacheKey(String);

impl CacheKey {
  fn new(table: &Table, branch: &Branch) -> Self {
    Self(format!("{table}/{branch}"))
  }

  #[cfg(test)]
  pub(crate) fn dummy(table: &str) -> Self {
    Self::new(&Table(table.to_string()), &Branch("main".to_string()))
  }
}

impl Deref for CacheKey {
  type Target = String;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl Display for CacheKey {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}

/// Thin wrapper newtype over `String`
///
/// Used so that function signatures can cleanly differentiate between a table name and a branch name,
/// even though both are just strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Table(String);

impl Deref for Table {
  type Target = String;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl Display for Table {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}

impl From<&str> for Table {
  fn from(value: &str) -> Self {
    Self(value.to_string())
  }
}

/// Thin wrapper newtype over `String`
///
/// Used so that function signatures can cleanly differentiate between a table name and a branch name,
/// even though both are just strings.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Branch(String);

impl Deref for Branch {
  type Target = String;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl Display for Branch {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}

impl From<&str> for Branch {
  fn from(value: &str) -> Self {
    Self(value.to_string())
  }
}

#[derive(Debug)]
pub(crate) struct History {
  pub pending: Option<LineBuf>,
  pub fuzzy_finder: Option<FuzzySelector>,
  pub cursor: usize,
  pub virt_cursor: usize,

  conn: Arc<Mutex<Connection>>,
  table: Table,
  branch: Branch,
  search_mask: Vec<HistEntry>,
  mask_stale: bool,
  no_matches: bool,
  max_size: Option<u32>,
}

impl History {
  const USER_VERSION: i32 = 5;

  fn lock(&self) -> MutexGuard<'_, Connection> {
    self
      .conn
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn cache_key(&self) -> CacheKey {
    CacheKey::new(&self.table, &self.branch)
  }

  /// Wrap an already-migrated connection for read-only querying, skipping
  /// `init_db` and the background cache loader. Used for forked-child access
  /// (e.g. `hist` in a pipeline) where the inherited connection is fenced off
  /// and migrating or writing isn't possible.
  pub(crate) fn attach(conn: Arc<Mutex<Connection>>, table: &str, branch: &str) -> Self {
    let max_hist = shopt!(history.max_entries);
    let max_size = (max_hist >= 0).then_some(max_hist as u32);
    let table: Table = table.into();
    let branch: Branch = branch.into();

    Self {
      conn,
      table,
      branch,
      pending: None,
      search_mask: vec![],
      mask_stale: true,
      fuzzy_finder: None,
      no_matches: false,
      cursor: 0,
      virt_cursor: 0,
      max_size,
    }
  }

  pub(crate) fn new(conn: Arc<Mutex<Connection>>, table: &str, branch: &str) -> ShResult<Self> {
    let max_hist = shopt!(history.max_entries);
    let table: Table = table.into();
    let branch: Branch = branch.into();

    Self::init_db(
      &conn
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner),
      &table,
    )?;

    // -1 = no limit
    let max_size = (max_hist >= 0).then_some(max_hist as u32);

    let mut hist = Self {
      conn,
      table,
      branch,
      pending: None,
      search_mask: vec![],
      mask_stale: true,
      fuzzy_finder: None,
      no_matches: false,
      cursor: 0,
      virt_cursor: 0,
      max_size,
    };
    let cache_key = hist.cache_key();

    // Ensure cache slots exist so consumers don't see a missing key
    // before the async load finishes.
    if let Ok(mut cache) = HIST_ENTRIES.write() {
      cache.entry(cache_key.clone()).or_default();
    }
    if let Ok(mut cache) = SEARCH_ENTRIES.write() {
      cache.entry(cache_key.clone()).or_default();
    }

    // Load the existing history asynchronously into both HIST_ENTRIES and
    // SEARCH_ENTRIES using a single DB connection. `History::push` can run
    // concurrently and mutate the caches while we're loading; when the load
    // completes we merge by treating any commands already in the cache
    // (added by push during load) as the authoritative newer entry.
    let table = hist.table.clone();
    let branch = hist.branch.clone();
    std::thread::spawn(move || {
      do_something_that_opens_fds_that_we_cant_access_hack(MIN_INTERNAL_FD, || {
        let Some(conn) = db::get_db_conn() else {
          return;
        };
        let loaded = {
          let conn = conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
          query_masked(None, &conn, &table, &branch)
        };

        let max_ts = loaded
          .iter()
          .filter_map(|e| e.timestamp.duration_since(std::time::UNIX_EPOCH).ok())
          .map(|d| d.as_secs() as i64)
          .max()
          .unwrap_or(0);

        // Merge helper: drop loaded entries shadowed by in-session pushes,
        // then prepend the rest so pushes stay at the end (newest).
        let merge = |existing: &mut Vec<HistEntry>, loaded: Vec<HistEntry>| {
          let pushed_cmds: crate::HashSet<String> =
            existing.iter().map(|e| e.command.clone()).collect();
          let mut merged: Vec<HistEntry> = loaded
            .into_iter()
            .filter(|e| !pushed_cmds.contains(&e.command))
            .collect();
          merged.append(existing);
          *existing = merged;
        };

        if let Ok(mut cache) = HIST_ENTRIES.write() {
          merge(cache.entry(cache_key.clone()).or_default(), loaded.clone());
        }
        if let Ok(mut cache) = SEARCH_ENTRIES.write() {
          merge(cache.entry(cache_key.clone()).or_default(), loaded);
        }
        // Initialize watermark; don't overwrite if pushes during load advanced it.
        if let Ok(mut wm) = SEARCH_WATERMARKS.write() {
          let wm_entry = wm.entry(cache_key.clone()).or_insert(0);
          *wm_entry = (*wm_entry).max(max_ts);
        }
      });
    });

    hist.reset();
    Ok(hist)
  }

  pub(crate) fn empty(table: &str, branch: &str) -> Self {
    let conn = Connection::open_in_memory().expect("Failed to open in-memory database");
    let table: Table = table.into();
    let branch: Branch = branch.into();
    Self::init_db(&conn, &table).expect("Failed to initialize in-memory database");

    Self {
      conn: Arc::new(Mutex::new(conn)),
      table: Table(table.to_string()),
      branch: Branch(branch.to_string()),
      pending: None,
      search_mask: vec![],
      mask_stale: true,
      fuzzy_finder: None,
      no_matches: false,
      cursor: 0,
      virt_cursor: 0,
      max_size: None,
    }
  }

  fn init_db(conn: &Connection, table: &Table) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
      r"
			CREATE TABLE IF NOT EXISTS {table} (
				id	INTEGER PRIMARY KEY,
				timestamp	INTEGER NOT NULL,
				runtime	INTEGER NOT NULL DEFAULT 0,
				command TEXT NOT NULL
			);
			-- Composite index supports `query_masked`'s GROUP BY command + MAX(timestamp).
			-- Without it, the planner falls back to full-table scan + hash aggregate +
			-- sort, which dominated CPU profiles on large histories.
			CREATE INDEX IF NOT EXISTS {table}_command_ts_idx
				ON {table}(command, timestamp DESC);
		"
    ))?;

    // Per-table column migrations.
    Self::add_column_if_missing(conn, table, "cwd", "TEXT")?;
    Self::add_column_if_missing(conn, table, "status", "INT DEFAULT 0")?;
    Self::add_column_if_missing(conn, table, "token", "TEXT")?;
    Self::add_column_if_missing(conn, table, "parent", "TEXT")?;
    Self::add_column_if_missing(conn, table, "joint", "TEXT")?;

    conn.execute_batch(&format!(
      "CREATE INDEX IF NOT EXISTS {table}_token_idx ON {table}(token);"
    ))?;

    // Backfill tokens for any rows that predate the token column. A no-op once
    // every row has one, so it is safe to run on every startup.
    let ids: Vec<i64> = {
      let mut stmt = conn.prepare(&format!("SELECT id FROM {table} WHERE token IS NULL"))?;
      stmt
        .query_map([], |r| r.get(0))?
        .filter_map(Result::ok)
        .collect::<Vec<i64>>()
    };
    if !ids.is_empty() {
      conn.execute_batch("BEGIN")?;
      for id in ids {
        let res = conn.execute(
          &format!("UPDATE {table} SET token = ?1 WHERE id = ?2"),
          (Uuid::new_v4().to_string(), id),
        );
        if let Err(e) = res {
          conn.execute_batch("ROLLBACK").ok();
          return Err(e);
        }
      }
      conn.execute_batch("COMMIT")?;
    }

    conn.execute_batch(
      "
      CREATE TABLE IF NOT EXISTS dir_history (
        path        TEXT        PRIMARY KEY NOT NULL,
        visits      INTEGER     NOT NULL DEFAULT 1,
        last_visit  INTEGER     NOT NULL
      );
      ",
    )?;

    conn.execute_batch(
      "
      CREATE TABLE IF NOT EXISTS branches (
        name        TEXT UNIQUE  PRIMARY KEY NOT NULL,
        head        TEXT         NOT NULL
      );
      ",
    )?;

    let has_main: bool = conn.query_row(
      "SELECT EXISTS(SELECT 1 FROM branches WHERE name = 'main')",
      [],
      |r| r.get(0),
    )?;

    if !has_main && **table == MAIN_HIST_TABLE_NAME {
      conn.execute_batch(
        "
        UPDATE shed_history
        SET parent = (
          SELECT token FROM shed_history AS prev
          WHERE prev.id < shed_history.id
          ORDER BY prev.id DESC LIMIT 1
        );

        INSERT INTO branches (name, head)
        SELECT 'main', token FROM shed_history ORDER BY id DESC LIMIT 1;
        ",
      )?;
    }

    conn.execute_batch(&format!("PRAGMA user_version = {}", Self::USER_VERSION))?;

    Ok(())
  }

  /// Adds `column` to `table` only if it does not already exist, so it is safe
  /// to run on both new and pre-existing tables.
  fn add_column_if_missing(
    conn: &Connection,
    table: &Table,
    column: &str,
    decl: &str,
  ) -> rusqlite::Result<()> {
    let count: i64 = conn.query_row(
      &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
      [column],
      |r| r.get(0),
    )?;
    if count == 0 {
      conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
  }
  fn head_entry_conn(
    conn: &Connection,
    table: &Table,
    branch: &Branch,
  ) -> ShResult<Option<HistEntry>> {
    let sql = format!(
      "SELECT command, timestamp, runtime, cwd, status, token FROM {table}
      WHERE token = (SELECT head FROM branches WHERE name = ?1) LIMIT 1"
    );
    let res = conn.query_row(&sql, rusqlite::params![**branch], History::row_to_entry);

    match res {
      Ok(entry) => Ok(Some(entry)),
      Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
      Err(e) => Err(e.into()),
    }
  }
  fn head_conn(conn: &Connection, table: &Table, branch: &Branch) -> ShResult<Option<Uuid>> {
    Ok(Self::head_entry_conn(conn, table, branch)?.map(|e| e.token))
  }
  fn head(&self) -> ShResult<Option<Uuid>> {
    Self::head_conn(&self.lock(), &self.table, &self.branch)
  }
  fn advance_head(conn: &Connection, branch: &Branch, new_token: &str) -> rusqlite::Result<()> {
    conn.execute(
      "
      INSERT INTO branches (name, head) VALUES (?1, ?2)
      ON CONFLICT(name) DO UPDATE SET head = ?2
      ",
      rusqlite::params![**branch, new_token],
    )?;
    Ok(())
  }
  pub(crate) fn branch_exists(&self, name: &str) -> ShResult<bool> {
    Ok(self.lock().query_row(
      "SELECT EXISTS(SELECT 1 FROM branches WHERE name = ?1)",
      rusqlite::params![name],
      |r| r.get(0),
    )?)
  }
  pub(crate) fn list_branches(&self) -> ShResult<Vec<String>> {
    let conn = self.lock();
    Ok(
      conn
        .prepare("SELECT name FROM branches ORDER BY name")?
        .query_map([], |r| r.get(0))?
        .filter_map(Result::ok)
        .collect(),
    )
  }
  pub(crate) fn create_branch(&self, name: &str) -> ShResult<()> {
    let Some(head) = self.head()?.map(|t| t.to_string()) else {
      return Ok(());
    };

    self
      .lock()
      .execute(
        "INSERT INTO branches (name, head) VALUES (?1, ?2)",
        rusqlite::params![name, head],
      )
      .map_err(|_| sherr!(InternalErr, "Branch already exists"))?;
    Ok(())
  }

  /// Whether `target` is reachable from `from` by walking parent/joint edges.
  fn is_reachable(conn: &Connection, table: &Table, from: &str, target: &str) -> ShResult<bool> {
    let sql = format!(
      r"
      WITH RECURSIVE reachable(token) AS (
        SELECT ?1
        UNION
        SELECT CASE k WHEN 0 THEN h.parent ELSE h.joint END
        FROM {table} h
        JOIN reachable r ON h.token = r.token
        CROSS JOIN (SELECT 0 AS k UNION ALL SELECT 1) ks
        WHERE (k = 0 AND h.parent IS NOT NULL)
           OR (k = 1 AND h.joint IS NOT NULL)
      )
      SELECT EXISTS(SELECT 1 FROM reachable WHERE token = ?2)
      "
    );
    Ok(conn.query_row(&sql, rusqlite::params![from, target], |r| r.get(0))?)
  }

  /// Merge `other` into the current branch by inserting a synthetic merge node
  /// (`parent` = our tip, `joint` = `other`'s tip) and advancing our head to it.
  /// Returns `false` if already up to date. The node has no command, so it is
  /// invisible to history listings but is walked for reachability.
  pub(crate) fn merge_branch(&self, other: &str) -> ShResult<MergeResult> {
    let conn = self.lock();

    let other_head: String = match conn.query_row(
      "SELECT head FROM branches WHERE name = ?1",
      rusqlite::params![other],
      |r| r.get(0),
    ) {
      Ok(h) => h,
      Err(rusqlite::Error::QueryReturnedNoRows) => {
        return Err(sherr!(InternalErr, "no such branch: {other}"));
      }
      Err(e) => return Err(e.into()),
    };

    let cur_head = Self::head_conn(&conn, &self.table, &self.branch)?.map(|t| t.to_string());

    if let Some(ref cur) = cur_head {
      if Self::is_reachable(&conn, &self.table, cur, &other_head)? {
        // up to date (we have theirs)
        return Ok(MergeResult::UpToDate);
      }

      if Self::is_reachable(&conn, &self.table, &other_head, cur)? {
        // fast-forward (no divergence)
        Self::advance_head(&conn, &self.branch, &other_head)?;
        return Ok(MergeResult::FastForward);
      }
    }

    let token = Uuid::new_v4().to_string();
    let ts = timestamp_secs(SystemTime::now());
    let new_id = Self::last_id_conn(&conn, &self.table) + 1;
    conn.execute(
      &format!(
        "INSERT INTO {} (id, timestamp, runtime, command, cwd, status, token, parent, joint)
         VALUES (?1, ?2, 0, '', '', 0, ?3, ?4, ?5)",
        self.table
      ),
      rusqlite::params![new_id, ts, token, cur_head, other_head],
    )?;
    Self::advance_head(&conn, &self.branch, &token)?;
    Ok(MergeResult::Merged)
  }

  pub(crate) fn check_branch(&mut self) {
    if *self.table != MAIN_HIST_TABLE_NAME {
      // TODO: maybe add branching for ex history?
      return;
    }

    let current = Shed::hist_branch();
    if *self.branch != current {
      self.branch = Branch(current);
      self.refresh_hist_entries();
      self.mark_mask_stale();
    }
  }
  pub(crate) fn push(&self, command: &str) -> ShResult<Option<Uuid>> {
    if command
      .chars()
      .next()
      .is_none_or(|c| shopt!(history.ignore_space) && c == ' ')
    {
      return Ok(None);
    }
    let cwd = env::current_dir()
      .map(|p| p.to_string_lossy().into())
      .ok()
      .unwrap_or_default();

    self.push_entry(HistEntry {
      runtime: Duration::ZERO,
      timestamp: SystemTime::now(),
      command: command.to_string(),
      cwd,
      status: 0,
      token: Uuid::new_v4(),
    })
  }

  pub(crate) fn set_status(&self, token: Uuid, runtime: Option<Duration>, status: i32) {
    let table = self.table.clone();

    std::thread::spawn(move || {
      do_something_that_opens_fds_that_we_cant_access_hack(MIN_INTERNAL_FD, || {
        let Some(conn) = db::get_db_conn() else {
          return;
        };
        let conn = conn
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
        let micros = runtime.map_or(0, |r| r.as_micros() as i64);
        conn
          .execute(
            &format!("UPDATE {table} SET runtime = ?1, status = ?2 WHERE token = ?3"),
            rusqlite::params![micros, status, token.to_string()],
          )
          .ok();
      });
    });
  }

  fn unique_command_count(&self) -> i64 {
    self
      .lock()
      .query_row(
        &format!("SELECT COUNT(DISTINCT command) FROM {}", self.table),
        [],
        |row| row.get(0),
      )
      .unwrap_or(0)
  }

  fn trim_to_max(&self) {
    let Some(max) = self.max_size else { return };
    let count = self.unique_command_count();
    let excess = count - i64::from(max);
    if excess <= 0 {
      return;
    }
    let table = &self.table;
    let deleted: crate::HashSet<String> = {
      let conn = self.lock();
      let sql = format!(
        "DELETE FROM {table} WHERE command IN (
          SELECT command FROM {table}
          GROUP BY command
          ORDER BY MAX(timestamp) ASC
          LIMIT ?1
        ) RETURNING command"
      );
      match conn.prepare(&sql) {
        Ok(mut stmt) => stmt
          .query_map(rusqlite::params![excess], |row| row.get::<_, String>(0))
          .map(|rows| rows.filter_map(Result::ok).collect())
          .unwrap_or_default(),
        Err(_) => crate::HashSet::default(),
      }
    };

    if deleted.is_empty() {
      return;
    }

    let cache_key = self.cache_key();
    if let Ok(mut cache) = HIST_ENTRIES.write()
      && let Some(entries) = cache.get_mut(&cache_key)
    {
      entries.retain(|e| !deleted.contains(e.command()));
    }

    if let Ok(mut cache) = SEARCH_ENTRIES.write()
      && let Some(entries) = cache.get_mut(&cache_key)
    {
      entries.retain(|e| !deleted.contains(e.command()));
    }
  }

  pub(crate) fn last_id(&self) -> i64 {
    Self::last_id_conn(&self.lock(), &self.table)
  }

  /// `last_id` against an already-held connection, for use inside a lock scope.
  fn last_id_conn(conn: &Connection, table: &Table) -> i64 {
    conn
      .query_row(
        &format!("SELECT id FROM {table} ORDER BY id DESC LIMIT 1"),
        [],
        |row| row.get(0),
      )
      .unwrap_or(0)
  }

  pub(crate) fn delete(
    &self,
    where_clause: &str,
    params: &[&dyn rusqlite::ToSql],
  ) -> ShResult<Vec<(i64, HistEntry)>> {
    let entries = self.query(where_clause, params)?;
    let table = &self.table;

    let table_backup = format!("{table}_backup");
    let table_tmp = format!("{table}_tmp");

    let conn = self.lock();
    let tx = conn.unchecked_transaction()?;

    // gotta un-dangle any branch nodes that depended on the deleted stuff
    // and rebase them to any surviving nodes
    let deleted: crate::HashSet<String> = {
      let mut stmt = tx.prepare(&format!("SELECT token FROM {table} {where_clause}"))?;
      stmt
        .query_map(params, |r| r.get::<_, String>(0))?
        .filter_map(Result::ok)
        .collect()
    };
    let rows: Vec<(String, Option<String>, Option<String>)> = {
      let mut stmt = tx.prepare(&format!("SELECT token, parent, joint FROM {table}"))?;
      stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(Result::ok)
        .collect()
    };
    let parent_of: HashMap<String, Option<String>> = rows
      .iter()
      .map(|(t, p, _)| (t.clone(), p.clone()))
      .collect();
    let resolve = |start: &Option<String>| -> Option<String> {
      let mut cur = start.clone();
      while let Some(tok) = &cur {
        if !deleted.contains(tok) {
          break;
        }
        cur = parent_of.get(tok).cloned().flatten();
      }
      cur
    };
    let remaps: Vec<(String, Option<String>, Option<String>)> = rows
      .iter()
      .filter(|(tok, ..)| !deleted.contains(tok))
      .filter_map(|(tok, parent, joint)| {
        let np = resolve(parent);
        let nj = resolve(joint);
        (&np != parent || &nj != joint).then(|| (tok.clone(), np, nj))
      })
      .collect();
    let branch_repoints: Vec<(String, Option<String>)> = {
      let mut stmt = tx.prepare("SELECT name, head FROM branches")?;
      let all: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(Result::ok)
        .collect();
      all
        .into_iter()
        .filter(|(_, head)| deleted.contains(head))
        .map(|(name, head)| (name, resolve(&Some(head))))
        .collect()
    };

    // rolling backup
    // overwritten on each delete, restorable via `hist --restore`
    tx.execute_batch(&format!(
      "
      DROP TABLE IF EXISTS {table_backup}; \
      CREATE TABLE {table_backup} (
        id INTEGER PRIMARY KEY,
        timestamp INT,
        runtime INT,
        command TEXT,
        cwd TEXT,
        status INT DEFAULT 0,
        token TEXT,
        parent TEXT,
        joint TEXT
      ); \
      INSERT INTO {table_backup} SELECT * FROM {table};
      "
    ))?;
    tx.execute_batch(&format!(
      "
      CREATE TABLE {table_tmp} (
        id INTEGER PRIMARY KEY,
        timestamp INT,
        runtime INT,
        command TEXT,
        cwd TEXT,
        status INT DEFAULT 0,
        token TEXT,
        parent TEXT,
        joint TEXT
      );
      "
    ))?;
    tx.execute(&format!(
      "
      INSERT INTO {table_tmp} (
        id,
        timestamp,
        runtime,
        command,
        cwd,
        status,
        token,
        parent,
        joint
      ) \
      SELECT ROW_NUMBER() OVER (ORDER BY id), timestamp, runtime, command, cwd, status, token, parent, joint
      FROM {table} WHERE id NOT IN (SELECT id FROM {table} {where_clause}) ORDER BY id
      "
		), params)?;
    tx.execute_batch(&format!(
      "DROP TABLE {table}; ALTER TABLE {table_tmp} RENAME TO {table};"
    ))?;

    // Apply the pre-computed edge repairs: survivors whose parent/joint pointed
    // into the deleted set now point at their nearest surviving ancestor.
    for (tok, np, nj) in &remaps {
      tx.execute(
        &format!("UPDATE {table} SET parent = ?1, joint = ?2 WHERE token = ?3"),
        rusqlite::params![np, nj, tok],
      )?;
    }

    // Re-point any branch whose head was deleted to the nearest survivor, or
    // drop it to unborn if nothing of its lineage remains.
    for (name, new_head) in &branch_repoints {
      match new_head {
        Some(head) => {
          tx.execute(
            "UPDATE branches SET head = ?1 WHERE name = ?2",
            rusqlite::params![head, name],
          )?;
        }
        None => {
          tx.execute(
            "DELETE FROM branches WHERE name = ?1",
            rusqlite::params![name],
          )?;
        }
      }
    }

    tx.commit()?;

    Ok(entries)
  }

  /// Deletes exactly the rows with the given ids, reusing [`Self::delete`]'s
  /// backup + rebuild machinery.  Scoping by id lets callers apply non-SQL
  /// filters (e.g. a `--matches` regex) in Rust and then delete precisely the
  /// resolved set, instead of handing an unfiltered/empty WHERE to `delete`
  /// (which would wipe the whole table).
  pub(crate) fn delete_ids(&self, ids: &[i64]) -> ShResult<Vec<(i64, HistEntry)>> {
    if ids.is_empty() {
      return Ok(vec![]);
    }
    let placeholders = (1..=ids.len())
      .map(|i| format!("?{i}"))
      .collect::<Vec<_>>()
      .join(", ");
    let where_clause = format!("WHERE id IN ({placeholders})");
    let params: Vec<&dyn rusqlite::ToSql> =
      ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
    self.delete(&where_clause, &params)
  }

  /// Restores the history table from the rolling backup created by the last delete operation.
  pub(crate) fn restore_backup(&self) -> ShResult<i64> {
    let table = &self.table;
    let table_backup = format!("{table}_backup");
    let table_tmp = format!("{table}_tmp");

    let conn = self.lock();
    let has_backup: bool = conn.query_row(
      "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
      [&table_backup],
      |row| row.get(0),
    )?;
    if !has_backup {
      return Err(sherr!(HistoryReadErr, "no backup table found"));
    }
    let tx = conn.unchecked_transaction()?;
    // index for fast NOT EXISTS lookup during merge
    tx.execute_batch(&format!(
      "CREATE INDEX IF NOT EXISTS {table}_restore_idx ON {table} (command, timestamp);"
    ))?;
    // count how many entries from backup are missing in current table
    let restored: i64 = tx.query_row(
      &format!(
        "
        SELECT COUNT(*) FROM {table_backup} b \
        WHERE NOT EXISTS ( \
          SELECT 1 FROM {table} c \
          WHERE c.command = b.command AND c.timestamp = b.timestamp \
        )
       "
      ),
      [],
      |row| row.get(0),
    )?;
    // merge: insert deleted entries from backup that aren't in the current table
    tx.execute(
      &format!(
        "
        INSERT INTO {table} (
          command,
          timestamp,
          runtime,
          cwd,
          status,
          token,
          parent,
          joint
        ) \
        SELECT b.command, b.timestamp, b.runtime, b.cwd, b.status, b.token, b.parent, b.joint \
        FROM {table_backup} b \
        WHERE NOT EXISTS ( \
          SELECT 1 FROM {table} c \
          WHERE c.command = b.command AND c.timestamp = b.timestamp \
        )
        "
      ),
      [],
    )?;
    // rebuild with contiguous IDs in chronological order
    tx.execute_batch(&format!(
      "
      CREATE TABLE {table_tmp} (
        id INTEGER PRIMARY KEY,
        timestamp INT,
        runtime INT,
        command TEXT,
        cwd TEXT,
        status INT DEFAULT 0,
        token TEXT,
        parent TEXT,
        joint TEXT
      ); \
      INSERT INTO {table_tmp} (
        id,
        timestamp,
        runtime,
        command,
        cwd,
        status,
        token,
        parent,
        joint
      ) \
      SELECT ROW_NUMBER() OVER (ORDER BY timestamp), timestamp, runtime, command, cwd, status, token, parent, joint \
      FROM {table}; \
      DROP TABLE {table}; \
      ALTER TABLE {table_tmp} RENAME TO {table}; \
      DROP TABLE IF EXISTS {table_backup};
      "
    ))?;
    tx.commit()?;
    Ok(restored)
  }

  pub(crate) fn sort_by_timestamp(&self) -> ShResult<()> {
    let table = &self.table;
    let table_tmp = format!("{table}_tmp");

    let conn = self.lock();
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(&format!(
      r"
			CREATE TABLE {table_tmp} (
				id INTEGER PRIMARY KEY,
				timestamp INT,
				runtime INT,
				command TEXT,
				cwd TEXT,
				status INT DEFAULT 0,
				token TEXT,
        parent TEXT,
        joint TEXT
			);
			INSERT INTO {table_tmp} (
        id,
        timestamp,
        runtime,
        command,
        cwd,
        status,
        token,
        parent,
        joint
      )
			SELECT ROW_NUMBER() OVER (ORDER BY timestamp), timestamp, runtime, command, cwd, status, token, parent, joint
			FROM {table};
			DROP TABLE {table};
			ALTER TABLE {table_tmp} RENAME TO {table};
			"
    ))?;
    tx.commit()?;
    Ok(())
  }

  pub(crate) fn transaction<T, F: FnOnce(&Connection) -> ShResult<T>>(&self, f: F) -> ShResult<T> {
    let conn = self.lock();
    conn.execute_batch("BEGIN")?;
    match f(&conn) {
      Ok(val) => {
        conn.execute_batch("COMMIT")?;
        Ok(val)
      }
      Err(e) => {
        conn.execute_batch("ROLLBACK").ok();
        Err(e)
      }
    }
  }

  /// Runs a query on the history table with the given WHERE clause and parameters, returning a vector of (id, `HistEntry`) tuples.
  pub(crate) fn query(
    &self,
    where_clause: &str,
    params: &[&dyn rusqlite::ToSql],
  ) -> ShResult<Vec<(i64, HistEntry)>> {
    let table = &self.table;
    let sql = format!(
      "SELECT command, timestamp, runtime, cwd, status, token, id FROM {table} {where_clause}"
    );
    let conn = self.lock();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params, |row| Ok((row.get(6)?, Self::row_to_entry(row)?)))?;

    Ok(rows.filter_map(Result::ok).collect())
  }

  /// Like [`query`](Self::query), but constrained to the current branch's reachable set.
  ///
  /// * `conditions` are the extra WHERE conditions (no leading `WHERE`)
  /// * `tail` is the `ORDER BY`/`LIMIT` clause.
  pub(crate) fn query_scoped(
    &self,
    conditions: &str,
    tail: &str,
    params: &[&dyn rusqlite::ToSql],
  ) -> ShResult<Vec<(i64, HistEntry)>> {
    let table = &self.table;
    let branch_idx = params.len() + 1;
    let where_ = if conditions.trim().is_empty() {
      "WHERE token IN (SELECT token FROM reachable) AND joint IS NULL".to_string()
    } else {
      format!("WHERE token IN (SELECT token FROM reachable) AND joint IS NULL AND ({conditions})")
    };
    let sql = format!(
      r"
      WITH RECURSIVE reachable(token) AS (
        SELECT head FROM branches WHERE name = ?{branch_idx}
        UNION
        SELECT CASE k WHEN 0 THEN h.parent ELSE h.joint END
        FROM {table} h
        JOIN reachable r ON h.token = r.token
        CROSS JOIN (SELECT 0 AS k UNION ALL SELECT 1) ks
        WHERE (k = 0 AND h.parent IS NOT NULL)
           OR (k = 1 AND h.joint IS NOT NULL)
      )
      SELECT command, timestamp, runtime, cwd, status, token, id
      FROM {table} {where_} {tail}
      "
    );
    let branch = self.branch.to_string();
    let mut all: Vec<&dyn rusqlite::ToSql> = params.to_vec();
    all.push(&branch);

    let conn = self.lock();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(all.as_slice(), |row| {
      Ok((row.get(6)?, Self::row_to_entry(row)?))
    })?;

    Ok(rows.filter_map(Result::ok).collect())
  }

  pub(crate) fn query_range(&self, first: i64, last: i64) -> ShResult<Vec<(i64, HistEntry)>> {
    self.query_scoped(
      "id BETWEEN ?1 AND ?2",
      "ORDER BY id ASC",
      rusqlite::params![first, last],
    )
  }

  pub(crate) fn query_by_prefix(&self, prefix: &str) -> ShResult<Option<(i64, HistEntry)>> {
    Ok(
      self
        .query_scoped(
          "command LIKE ?1 || '%'",
          "ORDER BY id DESC LIMIT 1",
          rusqlite::params![prefix],
        )?
        .into_iter()
        .next(),
    )
  }

  #[cfg_attr(not(test), allow(dead_code))]
  pub(crate) fn push_entry(&self, entry: HistEntry) -> ShResult<Option<Uuid>> {
    let cached = entry.clone();
    let res = Self::push_entry_conn(&self.lock(), &self.table, &self.branch, entry);
    if matches!(res, Ok(Some(_))) {
      self.cache_entry(cached);
    }
    self.trim_to_max();
    res
  }

  fn cache_entry(&self, entry: HistEntry) {
    let key = self.cache_key();
    if let Ok(mut cache) = HIST_ENTRIES.write() {
      let entries = cache.entry(key.clone()).or_default();
      entries.retain(|e| e.command != entry.command);
      entries.push(entry.clone());
    }
    if let Ok(mut cache) = SEARCH_ENTRIES.write() {
      let entries = cache.entry(key).or_default();
      entries.retain(|e| e.command != entry.command);
      entries.push(entry);
    }
  }

  pub(crate) fn push_with(&self, conn: &Connection, entry: HistEntry) -> ShResult<Option<Uuid>> {
    Self::push_entry_conn(conn, &self.table, &self.branch, entry)
  }

  fn push_entry_conn(
    conn: &Connection,
    table: &Table,
    branch: &Branch,
    entry: HistEntry,
  ) -> ShResult<Option<Uuid>> {
    let HistEntry {
      runtime,
      timestamp,
      command,
      cwd,
      status,
      token,
    } = entry;
    let token_raw = token.to_string();

    if command.is_empty() {
      return Ok(None);
    }
    if Self::token_exists(conn, table, token) {
      return Ok(None);
    }
    let parent = Self::head_entry_conn(conn, table, branch)?;
    let parent_token = parent.as_ref().map(|e| e.token.to_string());

    if shopt!(history.ignore_dupes) && parent.as_ref().map(|e| &e.command) == Some(&command) {
      return Ok(None);
    }

    let timestamp = timestamp_secs(timestamp);
    let new_id = Self::last_id_conn(conn, table) + 1;
    conn.execute(
      &format!(
        "
        INSERT INTO {table} (id, timestamp, runtime, command, cwd, status, token, parent)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
      "
      ),
      rusqlite::params![
        new_id,
        timestamp,
        runtime.as_micros() as i64,
        command,
        cwd,
        status,
        &token_raw,
        parent_token
      ],
    )?;

    Self::advance_head(conn, branch, &token_raw)?;

    Ok(Some(token))
  }

  pub(crate) fn token_exists(conn: &Connection, table: &str, token: Uuid) -> bool {
    conn
      .query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE token = ?1)"),
        rusqlite::params![token.to_string()],
        |row| row.get(0),
      )
      .unwrap_or(false)
  }

  pub(crate) fn update_search_mask(&mut self, prefix: Option<&str>) {
    let Some(entries) = HIST_ENTRIES.read().ok() else {
      self.search_mask = vec![];
      return;
    };
    let Some(entry_table) = entries.get(&self.cache_key()) else {
      self.search_mask = vec![];
      return;
    };
    let Some(prefix) = prefix else {
      self.search_mask.clone_from(entry_table);
      return;
    };

    self.search_mask = entry_table
      .iter()
      .filter(|e| e.command().starts_with(prefix))
      .cloned()
      .collect();
  }

  pub(crate) fn reset(&mut self) {
    self.mask_stale = true;
    self.cursor = self.search_mask.len();
    self.virt_cursor = self.cursor;
  }

  pub(crate) fn mark_mask_stale(&mut self) {
    self.mask_stale = true;
  }

  /// Refresh the search mask from the database if stale. Call before
  /// any operation that reads the mask (history scrolling).
  pub(crate) fn ensure_mask_fresh(&mut self) {
    if self.mask_stale {
      let prefix = self.pending.as_ref().map(LineBuf::to_string);
      self.constrain_entries(prefix.as_deref());
      self.mask_stale = false;
    }
  }

  pub(crate) fn constrain_entries(&mut self, prefix: Option<&str>) {
    self.update_search_mask(prefix);
    self.no_matches = self.search_mask.is_empty();
    if self.no_matches {
      self.update_search_mask(None);
    }

    self.cursor = self.search_mask.len();
    self.virt_cursor = self.cursor;
    self.mask_stale = false;
  }

  pub(crate) fn resolve_hist_token(&self, token: &str) -> Option<String> {
    let token = token.strip_prefix('!').unwrap_or(token).to_string();

    // !! → last command verbatim
    if token == "!" {
      return self.last().map(|e| e.command().to_string());
    }
    // !$ → last word of last command
    if token == "$" {
      return self
        .last()
        .and_then(|e| e.command().split_whitespace().last().map(String::from));
    }

    if let Ok(num) = token.parse::<i32>()
      && num != 0
    {
      match num.cmp(&0) {
        // Negative: index from the bottom (!-2 = 2nd from end)
        Ordering::Less => {
          let offset = i64::from(num.unsigned_abs()) - 1;
          self
            .lock()
            .query_row(
              &format!(
                "SELECT command FROM {} ORDER BY id DESC LIMIT 1 OFFSET ?1",
                self.table
              ),
              rusqlite::params![offset],
              |row| row.get(0),
            )
            .ok()
        }
        // Positive: index from the top (!3 = 3rd entry)
        Ordering::Greater => {
          let offset = i64::from(num) - 1;
          self
            .lock()
            .query_row(
              &format!(
                "SELECT command FROM {} ORDER BY id ASC LIMIT 1 OFFSET ?1",
                self.table
              ),
              rusqlite::params![offset],
              |row| row.get(0),
            )
            .ok()
        }
        Ordering::Equal => unreachable!(),
      }
    } else {
      self
        .lock()
        .query_row(
          &format!(
            "SELECT command FROM {} WHERE substr(command, 1, length(?1)) = ?1 ORDER BY id DESC LIMIT 1",
            self.table
          ),
          rusqlite::params![token],
          |row| row.get(0),
        )
        .ok()
    }
  }

  pub(crate) fn row_to_entry(row: &rusqlite::Row) -> Result<HistEntry, rusqlite::Error> {
    Ok(HistEntry {
      command: row.get(0)?,
      timestamp: UNIX_EPOCH + Duration::from_secs(row.get::<_, i64>(1)? as u64),
      runtime: Duration::from_micros(row.get::<_, i64>(2)? as u64),
      cwd: row.get(3).unwrap_or_default(),
      status: row.get(4).unwrap_or(0),
      token: Uuid::from_str(row.get::<_, String>(5)?.as_str()).unwrap_or_default(),
    })
  }

  pub(crate) fn last(&self) -> Option<HistEntry> {
    self
      .lock()
      .query_row(
        &format!(
          "SELECT command, timestamp, runtime, cwd, status, token FROM {} ORDER BY id DESC LIMIT 1",
          self.table
        ),
        [],
        Self::row_to_entry,
      )
      .ok()
  }

  pub(crate) fn update_pending_cmd(&mut self, buf: (&str, usize)) {
    let cmd = buf.0.to_string();
    let cursor_pos = buf.1;

    if !self.at_pending() {
      // we are looking at an old command
      // compare it to the one in history
      // if it's different, reset our cursor and stuff
      let browsed_cmd = self.search_mask.get(self.cursor).map(HistEntry::command);
      if browsed_cmd == Some(cmd.as_str()) {
        return;
      }
      self.reset_to_pending();
    }

    if let Some(pending) = &mut self.pending {
      pending.set_buffer(&cmd);
      pending.set_cursor_from_flat(cursor_pos);
    } else {
      self.pending = Some(LineBuf::new().with_initial(&cmd, cursor_pos));
    }
  }

  pub(crate) fn at_pending(&self) -> bool {
    self.cursor >= self.search_mask.len()
  }

  pub(crate) fn reset_to_pending(&mut self) {
    self.cursor = self.search_mask.len();
    self.virt_cursor = self.cursor;
  }

  #[cfg(test)]
  pub(crate) fn masked_entries(&self) -> &[HistEntry] {
    &self.search_mask
  }

  /// Wipe the cross-test global caches for a given table name. Without this,
  /// tests that push to a shared table (e.g. `shed_history`) see entries
  /// from earlier tests in the same process, breaking single-entry-count
  /// assumptions.
  #[cfg(test)]
  pub(crate) fn clear_global_caches_for_test(table: &CacheKey) {
    if let Ok(mut c) = HIST_ENTRIES.write() {
      c.remove(table);
    }
    if let Ok(mut c) = SEARCH_ENTRIES.write() {
      c.remove(table);
    }
    if let Ok(mut wm) = SEARCH_WATERMARKS.write() {
      wm.remove(table);
    }
  }

  /// Insert a row straight into the DB at an explicit whole-second timestamp,
  /// bypassing the in-memory caches — simulates another session's write that
  /// this session hasn't cached yet.
  #[cfg(test)]
  pub(crate) fn insert_raw_for_test(&self, command: &str, timestamp: i64) {
    let conn = self.lock();
    let new_id = Self::last_id_conn(&conn, &self.table) + 1;
    self.insert_raw_conn(&conn, command, new_id, timestamp);
  }

  /// Insert a row with an explicit id — simulates another session having
  /// committed a specific PRIMARY KEY out of band.
  #[cfg(test)]
  pub(crate) fn insert_raw_with_id_for_test(&self, command: &str, id: i64, timestamp: i64) {
    let conn = self.lock();
    self.insert_raw_conn(&conn, command, id, timestamp);
  }

  /// Chain a raw row onto the current branch tip (parent = head) and advance
  /// the head, mirroring what a real push from another session does — so the
  /// branch-scoped loader can actually reach it.
  #[cfg(test)]
  fn insert_raw_conn(&self, conn: &Connection, command: &str, id: i64, timestamp: i64) {
    let table = &self.table;
    let token = Uuid::new_v4().to_string();
    let parent: Option<String> = conn
      .query_row(
        "SELECT head FROM branches WHERE name = ?1",
        rusqlite::params![*self.branch],
        |r| r.get(0),
      )
      .ok();
    conn
      .execute(
        &format!(
          "INSERT INTO {table} (id, timestamp, runtime, command, cwd, token, parent) VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6)"
        ),
        rusqlite::params![id, timestamp, command, "", token, parent],
      )
      .unwrap();
    Self::advance_head(conn, &self.branch, &token).unwrap();
  }

  #[cfg(test)]
  pub(crate) fn set_max_size_for_test(&mut self, max: u32) {
    self.max_size = Some(max);
  }

  #[cfg(test)]
  pub(crate) fn set_search_watermark_for_test(&self, ts: i64) {
    if let Ok(mut wm) = SEARCH_WATERMARKS.write() {
      wm.insert(self.cache_key(), ts);
    }
  }

  #[cfg(test)]
  pub(crate) fn sync_search_entries_for_test(&self) {
    self.sync_search_entries();
  }

  /// Get a hint by scanning the in-memory cache. No database access.
  pub(crate) fn get_hint(&self) -> Option<Hint> {
    if !self.at_pending() {
      return None;
    }
    let prefix = self.pending.as_ref()?.to_string();
    if prefix.is_empty() {
      return None;
    }
    let entries = HIST_ENTRIES.read().ok()?;
    let table = entries.get(&self.cache_key())?;
    table
      .iter()
      .rev()
      .find(|e| e.command().starts_with(&prefix) && e.command() != prefix)
      .map(|e| Hint::History(Lines::to_lines(e.command())))
  }

  pub(crate) fn refresh_hist_entries(&self) -> usize {
    let cache_key = self.cache_key();
    let num_entries_before = num_entries(&cache_key);
    let entries = query_masked(None, &self.lock(), &self.table, &self.branch);
    let max_ts = entries
      .iter()
      .filter_map(|e| e.timestamp.duration_since(std::time::UNIX_EPOCH).ok())
      .map(|d| d.as_secs() as i64)
      .max()
      .unwrap_or(0);
    if let Ok(mut cache) = HIST_ENTRIES.write() {
      cache.insert(cache_key.clone(), entries.clone());
    }
    if let Ok(mut cache) = SEARCH_ENTRIES.write() {
      cache.insert(cache_key.clone(), entries);
    }
    if let Ok(mut wm) = SEARCH_WATERMARKS.write() {
      wm.insert(cache_key.clone(), max_ts);
    }

    let num_entries_after = num_entries(&cache_key);
    num_entries_after.saturating_sub(num_entries_before)
  }

  #[cfg(test)]
  pub(crate) fn search_cache_commands(&self) -> Vec<String> {
    SEARCH_ENTRIES
      .read()
      .ok()
      .and_then(|c| c.get(&self.cache_key()).cloned())
      .unwrap_or_default()
      .iter()
      .map(|e| e.command().to_string())
      .collect()
  }

  #[cfg(test)]
  pub(crate) fn scroll_cache_commands(&self) -> Vec<String> {
    HIST_ENTRIES
      .read()
      .ok()
      .and_then(|c| c.get(&self.cache_key()).cloned())
      .unwrap_or_default()
      .iter()
      .map(|e| e.command().to_string())
      .collect()
  }

  pub(crate) fn is_virtual_scrolling(&self) -> bool {
    self.virt_cursor != self.cursor
  }

  pub(crate) fn virtual_scroll_direction(&self) -> Option<Direction> {
    match self.virt_cursor.cmp(&self.cursor) {
      Ordering::Greater => Some(Direction::Forward),
      Ordering::Equal => None,
      Ordering::Less => Some(Direction::Backward),
    }
  }

  pub(crate) fn stop_virtual_scroll(&mut self) {
    self.virt_cursor = self.cursor;
  }

  pub(crate) fn scroll(&mut self, offset: isize) -> Option<&HistEntry> {
    self.check_branch();
    self.ensure_mask_fresh();
    self.cursor = self
      .cursor
      .saturating_add_signed(offset)
      .clamp(0, self.search_mask.len());
    self.virt_cursor = self.cursor;

    self.search_mask.get(self.cursor)
  }

  pub(crate) fn scroll_to(&mut self, idx: usize) -> Option<&HistEntry> {
    self.check_branch();
    self.ensure_mask_fresh();
    self.cursor = idx.clamp(0, self.search_mask.len());
    self.virt_cursor = self.cursor;

    self.search_mask.get(self.cursor)
  }

  pub(crate) fn search_mask_count(&self) -> usize {
    self.search_mask.len()
  }

  pub(crate) fn virt_scroll(&mut self, offset: isize) -> Option<&HistEntry> {
    let before = self.virt_cursor;
    if self.is_virtual_scrolling() {
      self.virt_cursor = self
        .virt_cursor
        .saturating_add_signed(offset)
        .clamp(0, self.search_mask.len().saturating_sub(1));
    } else {
      self.virt_cursor = self
        .virt_cursor
        .saturating_add_signed(offset)
        .clamp(0, self.search_mask.len());
    }

    if self.virt_cursor >= self.search_mask.len() {
      self.virt_cursor = before;
    }

    if self.virt_cursor == before {
      // If virt_cursor didn't move, we're at the end of the list and should prevent further scrolling in that direction
      return None;
    }

    log::debug!(
      "Cursor: {}, Virt Cursor: {}, Search Mask Len: {}",
      self.cursor,
      self.virt_cursor,
      self.search_mask.len()
    );

    self.search_mask.get(self.virt_cursor)
  }

  pub(crate) fn merge_search_entries(&mut self) {
    let cache_key = self.cache_key();
    let search = SEARCH_ENTRIES
      .read()
      .ok()
      .and_then(|c| c.get(&cache_key).cloned());
    if let Some(entries) = search
      && let Ok(mut hist) = HIST_ENTRIES.write()
    {
      hist.insert(cache_key, entries);
    }
    self.mark_mask_stale();
  }

  // Fetch any entries from other sessions added after the watermark and merge
  // them into SEARCH_ENTRIES. Runs synchronously since the delta is small.
  fn sync_search_entries(&self) {
    let cache_key = self.cache_key();
    let watermark = SEARCH_WATERMARKS
      .read()
      .ok()
      .and_then(|wm| wm.get(&cache_key).copied())
      .unwrap_or(0);

    let delta = query_since(watermark, &self.lock(), &self.table, &self.branch);
    if delta.is_empty() {
      return;
    }

    let new_watermark = delta
      .iter()
      .filter_map(|e| e.timestamp.duration_since(std::time::UNIX_EPOCH).ok())
      .map(|d| d.as_secs() as i64)
      .max()
      .unwrap_or(watermark);

    if let Ok(mut cache) = SEARCH_ENTRIES.write() {
      let entries = cache.entry(cache_key.clone()).or_default();
      for new_entry in delta {
        entries.retain(|e| e.command != new_entry.command);
        entries.push(new_entry);
      }
      entries.sort_by_key(|e| e.timestamp);
    }
    if let Ok(mut wm) = SEARCH_WATERMARKS.write() {
      let wm_entry = wm.entry(cache_key).or_insert(0);
      *wm_entry = (*wm_entry).max(new_watermark);
    }
  }

  pub(crate) fn start_search(&mut self, initial: &str) -> Option<String> {
    self.sync_search_entries();

    let all_entries = SEARCH_ENTRIES
      .read()
      .ok()
      .and_then(|c| c.get(&self.cache_key()).cloned())
      .unwrap_or_default();

    if all_entries.is_empty() {
      return None;
    }
    if all_entries.len() == 1 {
      return Some(all_entries[0].command().to_string());
    }

    let mut finder = FuzzySelector::new().number_candidates(true);

    let candidates: Vec<Candidate> = all_entries
      .into_iter()
      .enumerate()
      .map(|(i, e)| Candidate::from((i, e.command().to_string())))
      .collect();

    finder.activate(candidates);
    finder.set_query(initial);
    self.fuzzy_finder = Some(finder);
    None
  }

  pub(crate) fn stop_search(&mut self) {
    self.fuzzy_finder = None;
  }

  #[cfg(test)]
  pub(crate) fn entry_count(&self) -> i64 {
    self
      .lock()
      .query_row(&format!("SELECT COUNT(*) FROM {}", self.table), [], |row| {
        row.get(0)
      })
      .unwrap_or(0)
  }
}
