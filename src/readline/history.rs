use std::{
  cmp::Ordering,
  env,
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
use crate::{HashMap, state::db, util::random::Uuid};

#[derive(Debug, Clone)]
pub struct HistEntry {
  pub runtime: Duration,
  pub timestamp: SystemTime,
  pub command: String,
  pub cwd: String,
  pub status: i32,
  pub token: Uuid,
}

type HistTables = HashMap<String, Vec<HistEntry>>;

static HIST_ENTRIES: LazyLock<Arc<RwLock<HistTables>>> =
  LazyLock::new(|| Arc::new(RwLock::new(HashMap::default())));

static SEARCH_ENTRIES: LazyLock<Arc<RwLock<HistTables>>> =
  LazyLock::new(|| Arc::new(RwLock::new(HashMap::default())));

static SEARCH_WATERMARKS: LazyLock<Arc<RwLock<HashMap<String, i64>>>> =
  LazyLock::new(|| Arc::new(RwLock::new(HashMap::default())));

fn num_entries(table: &str) -> usize {
  HIST_ENTRIES
    .read()
    .ok()
    .and_then(|cache| cache.get(table).map(Vec::len))
    .unwrap_or(0)
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
  pub fn command(&self) -> &str {
    self.command.as_str()
  }
  /// The raw command bytes (byte-native; preserves non-UTF-8).
  pub fn command_bytes(&self) -> &[u8] {
    self.command.as_bytes()
  }
}

fn query_since(since_ts: i64, conn: &Connection, table: &str) -> Vec<HistEntry> {
  let sql = format!(
    r"
    SELECT command, MAX(timestamp) as ts, runtime, cwd, status, token FROM {table}
    GROUP BY command
    HAVING MAX(timestamp) >= ?1
    ORDER BY ts ASC
    "
  );
  let Ok(mut stmt) = conn.prepare(&sql) else {
    return vec![];
  };
  match stmt.query_map(rusqlite::params![since_ts], History::row_to_entry) {
    Ok(iter) => iter.filter_map(Result::ok).collect(),
    Err(_) => vec![],
  }
}

fn query_masked(prefix: Option<&str>, conn: &Connection, table: &str) -> Vec<HistEntry> {
  let sql = match prefix {
    Some(_) => format!(
      r"
      SELECT command, MAX(timestamp) as ts, runtime, cwd, status, token FROM {table}
      WHERE command LIKE ?1 || '%'
      GROUP BY command
      ORDER BY ts ASC
      "
    ),
    None => format!(
      r"
      SELECT command, MAX(timestamp) as ts, runtime, cwd, status, token FROM {table}
      GROUP BY command
      ORDER BY ts ASC
      "
    ),
  };
  let Ok(mut stmt) = conn.prepare(&sql) else {
    return vec![];
  };
  let rows = match prefix {
    Some(p) => stmt.query_map(rusqlite::params![p], History::row_to_entry),
    None => stmt.query_map([], History::row_to_entry),
  };

  match rows {
    Ok(iter) => iter.filter_map(Result::ok).collect(),
    Err(_) => vec![],
  }
}

#[derive(Debug)]
pub struct History {
  pub pending: Option<LineBuf>,
  pub fuzzy_finder: Option<FuzzySelector>,
  pub cursor: usize,
  pub virt_cursor: usize,

  conn: Arc<Mutex<Connection>>,
  table: String,
  search_mask: Vec<HistEntry>,
  mask_stale: bool,
  no_matches: bool,
  max_size: Option<u32>,
}

impl History {
  const USER_VERSION: i32 = 4;

  fn lock(&self) -> MutexGuard<'_, Connection> {
    self
      .conn
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// Wrap an already-migrated connection for read-only querying, skipping
  /// `init_db` and the background cache loader. Used for forked-child access
  /// (e.g. `hist` in a pipeline) where the inherited connection is fenced off
  /// and migrating or writing isn't possible.
  pub fn attach(conn: Arc<Mutex<Connection>>, table: &str) -> Self {
    let max_hist = shopt!(history.max_entries);
    let max_size = (max_hist >= 0).then_some(max_hist as u32);
    Self {
      conn,
      table: table.to_string(),
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

  pub fn new(conn: Arc<Mutex<Connection>>, table: &str) -> ShResult<Self> {
    let max_hist = shopt!(history.max_entries);

    Self::init_db(
      &conn
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner),
      table,
    )?;

    let max_size = (max_hist >= 0).then_some(max_hist as u32);
    let mut hist = Self {
      conn,
      table: table.to_string(),
      pending: None,
      search_mask: vec![],
      mask_stale: true,
      fuzzy_finder: None,
      no_matches: false,
      cursor: 0,
      virt_cursor: 0,
      max_size,
    };
    // Ensure cache slots exist so consumers don't see a missing key
    // before the async load finishes.
    if let Ok(mut cache) = HIST_ENTRIES.write() {
      cache.entry(hist.table.clone()).or_default();
    }
    if let Ok(mut cache) = SEARCH_ENTRIES.write() {
      cache.entry(hist.table.clone()).or_default();
    }

    // Load the existing history asynchronously into both HIST_ENTRIES and
    // SEARCH_ENTRIES using a single DB connection. `History::push` can run
    // concurrently and mutate the caches while we're loading; when the load
    // completes we merge by treating any commands already in the cache
    // (added by push during load) as the authoritative newer entry.
    let table_name = hist.table.clone();
    std::thread::spawn(move || {
      do_something_that_opens_fds_that_we_cant_access_hack(MIN_INTERNAL_FD, || {
        let Some(conn) = db::get_db_conn() else {
          return;
        };
        let loaded = {
          let conn = conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
          query_masked(None, &conn, &table_name)
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
          merge(cache.entry(table_name.clone()).or_default(), loaded.clone());
        }
        if let Ok(mut cache) = SEARCH_ENTRIES.write() {
          merge(cache.entry(table_name.clone()).or_default(), loaded);
        }
        // Initialize watermark; don't overwrite if pushes during load advanced it.
        if let Ok(mut wm) = SEARCH_WATERMARKS.write() {
          let wm_entry = wm.entry(table_name).or_insert(0);
          *wm_entry = (*wm_entry).max(max_ts);
        }
      });
    });

    hist.reset();
    Ok(hist)
  }

  pub fn empty(table: &str) -> Self {
    let conn = Connection::open_in_memory().expect("Failed to open in-memory database");
    Self::init_db(&conn, table).expect("Failed to initialize in-memory database");
    Self {
      conn: Arc::new(Mutex::new(conn)),
      table: table.to_string(),
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

  fn init_db(conn: &Connection, table: &str) -> rusqlite::Result<()> {
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

    // Per-table column migrations. These are gated on whether the column
    // actually exists (not on the DB-wide `user_version`), so they are safe to
    // run against both a brand-new table (columns already present -> skipped)
    // and an old one carried over from a pre-token schema.
    Self::add_column_if_missing(conn, table, "cwd", "TEXT")?;
    Self::add_column_if_missing(conn, table, "status", "INT DEFAULT 0")?;
    // 'token' acts as an absolute identifier since the id field actually shifts
    // after commands are deleted.
    Self::add_column_if_missing(conn, table, "token", "TEXT")?;

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

    conn.execute_batch(&format!("PRAGMA user_version = {}", Self::USER_VERSION))?;

    Ok(())
  }

  /// Adds `column` to `table` only if it does not already exist, so it is safe
  /// to run on both new and pre-existing tables. `table` is always an internal
  /// constant (never user input), so interpolating it is safe.
  fn add_column_if_missing(
    conn: &Connection,
    table: &str,
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
  pub fn push(&self, command: &str) -> ShResult<Option<Uuid>> {
    if command.is_empty() {
      return Ok(None);
    }
    let table = &self.table;
    let timestamp = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .as_secs() as i64;
    let cwd: Option<String> = env::current_dir().map(|p| p.to_string_lossy().into()).ok();
    let token = Uuid::new_v4();

    {
      let conn = self.lock();

      if shopt!(history.ignore_space) && command.starts_with(' ') {
        return Ok(None);
      }

      if shopt!(history.ignore_dupes) {
        let last: Option<String> = conn
          .query_row(
            &format!("SELECT command FROM {table} ORDER BY id DESC LIMIT 1"),
            [],
            |row| row.get(0),
          )
          .ok();
        if last.as_deref() == Some(command) {
          return Ok(None);
        }
      }
      conn.execute(
        &format!(
          "INSERT INTO {table} (id, timestamp, runtime, command, cwd, token)
           SELECT COALESCE(MAX(id), 0) + 1, ?1, 0, ?2, ?3, ?4 FROM {table}"
        ),
        rusqlite::params![timestamp, command, cwd, token.to_string()],
      )?;
    }

    // Incremental cache update: the new entry supersedes any prior entry with
    // the same command (matching `query_masked`'s GROUP BY semantics).
    // Avoids the post-command SQLite re-query that dominated CPU profiles.
    let entry = HistEntry {
      runtime: Duration::default(),
      timestamp: SystemTime::now(),
      command: command.into(),
      cwd: cwd.unwrap_or_default(),
      status: 0,
      token,
    };
    if let Ok(mut cache) = HIST_ENTRIES.write() {
      let table_entries = cache.entry(self.table.clone()).or_default();
      table_entries.retain(|e| e.command != command);
      table_entries.push(entry.clone());
    }
    if let Ok(mut cache) = SEARCH_ENTRIES.write() {
      let table_entries = cache.entry(self.table.clone()).or_default();
      table_entries.retain(|e| e.command != command);
      table_entries.push(entry);
    }
    self.trim_to_max();
    Ok(Some(token))
  }

  pub fn set_status(&self, token: Uuid, runtime: Option<Duration>, status: i32) {
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

    if let Ok(mut cache) = HIST_ENTRIES.write()
      && let Some(entries) = cache.get_mut(table.as_str())
    {
      entries.retain(|e| !deleted.contains(e.command()));
    }

    if let Ok(mut cache) = SEARCH_ENTRIES.write()
      && let Some(entries) = cache.get_mut(table.as_str())
    {
      entries.retain(|e| !deleted.contains(e.command()));
    }
  }

  pub fn last_id(&self) -> i64 {
    Self::last_id_conn(&self.lock(), &self.table)
  }

  /// `last_id` against an already-held connection, for use inside a lock scope.
  fn last_id_conn(conn: &Connection, table: &str) -> i64 {
    conn
      .query_row(
        &format!("SELECT id FROM {table} ORDER BY id DESC LIMIT 1"),
        [],
        |row| row.get(0),
      )
      .unwrap_or(0)
  }

  pub fn delete(
    &self,
    where_clause: &str,
    params: &[&dyn rusqlite::ToSql],
  ) -> ShResult<Vec<(i64, HistEntry)>> {
    let entries = self.query(where_clause, params)?;
    let table = &self.table;

    let conn = self.lock();
    let tx = conn.unchecked_transaction()?;
    // rolling backup - overwritten on each delete, restorable via `hist --restore`
    tx.execute_batch(&format!(
      "DROP TABLE IF EXISTS {table}_backup; \
       CREATE TABLE {table}_backup (id INTEGER PRIMARY KEY, timestamp INT, runtime INT, command TEXT, cwd TEXT, status INT DEFAULT 0, token TEXT); \
       INSERT INTO {table}_backup SELECT * FROM {table};"
    ))?;
    tx.execute_batch(&format!(
      "CREATE TABLE {table}_tmp (id INTEGER PRIMARY KEY, timestamp INT, runtime INT, command TEXT, cwd TEXT, status INT DEFAULT 0, token TEXT);"
    ))?;
    tx.execute(&format!(
				"INSERT INTO {table}_tmp (id, timestamp, runtime, command, cwd, status, token) \
				 SELECT ROW_NUMBER() OVER (ORDER BY id), timestamp, runtime, command, cwd, status, token FROM {table} WHERE id NOT IN (SELECT id FROM {table}
		{where_clause}) ORDER BY id"
		), params)?;
    tx.execute_batch(&format!(
      "DROP TABLE {table}; ALTER TABLE {table}_tmp RENAME TO {table};"
    ))?;
    tx.commit()?;

    Ok(entries)
  }

  /// Deletes exactly the rows with the given ids, reusing [`Self::delete`]'s
  /// backup + rebuild machinery. Scoping by id lets callers apply non-SQL
  /// filters (e.g. a `--matches` regex) in Rust and then delete precisely the
  /// resolved set, instead of handing an unfiltered/empty WHERE to `delete`
  /// (which would wipe the whole table).
  pub fn delete_ids(&self, ids: &[i64]) -> ShResult<Vec<(i64, HistEntry)>> {
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
  pub fn restore_backup(&self) -> ShResult<i64> {
    let table = &self.table;
    let conn = self.lock();
    let has_backup: bool = conn.query_row(
      "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
      [&format!("{table}_backup")],
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
        "SELECT COUNT(*) FROM {table}_backup b \
       WHERE NOT EXISTS ( \
         SELECT 1 FROM {table} c \
         WHERE c.command = b.command AND c.timestamp = b.timestamp \
       )"
      ),
      [],
      |row| row.get(0),
    )?;
    // merge: insert deleted entries from backup that aren't in the current table
    tx.execute(
      &format!(
        "INSERT INTO {table} (command, timestamp, runtime, cwd, status, token) \
       SELECT b.command, b.timestamp, b.runtime, b.cwd, b.status, b.token \
       FROM {table}_backup b \
       WHERE NOT EXISTS ( \
         SELECT 1 FROM {table} c \
         WHERE c.command = b.command AND c.timestamp = b.timestamp \
       )"
      ),
      [],
    )?;
    // rebuild with contiguous IDs in chronological order
    tx.execute_batch(&format!(
      "CREATE TABLE {table}_tmp (id INTEGER PRIMARY KEY, timestamp INT, runtime INT, command TEXT, cwd TEXT, status INT DEFAULT 0, token TEXT); \
       INSERT INTO {table}_tmp (id, timestamp, runtime, command, cwd, status, token) \
       SELECT ROW_NUMBER() OVER (ORDER BY timestamp), timestamp, runtime, command, cwd, status, token \
       FROM {table}; \
       DROP TABLE {table}; \
       ALTER TABLE {table}_tmp RENAME TO {table}; \
       DROP TABLE IF EXISTS {table}_backup;"
    ))?;
    tx.commit()?;
    Ok(restored)
  }

  pub fn sort_by_timestamp(&self) -> ShResult<()> {
    let table = &self.table;
    let conn = self.lock();
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(&format!(
      r"
			CREATE TABLE {table}_tmp (
				id INTEGER PRIMARY KEY,
				timestamp INT,
				runtime INT,
				command TEXT,
				cwd TEXT,
				status INT DEFAULT 0,
				token TEXT
			);
			INSERT INTO {table}_tmp (id, timestamp, runtime, command, cwd, status, token)
			SELECT ROW_NUMBER() OVER (ORDER BY timestamp), timestamp, runtime, command, cwd, status, token
			FROM {table};
			DROP TABLE {table};
			ALTER TABLE {table}_tmp RENAME TO {table};
			"
    ))?;
    tx.commit()?;
    Ok(())
  }

  pub fn transaction<T, F: FnOnce(&Connection) -> ShResult<T>>(&self, f: F) -> ShResult<T> {
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
  pub fn query(
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

  pub fn query_range(&self, first: i64, last: i64) -> ShResult<Vec<(i64, HistEntry)>> {
    let where_clause = r"
			WHERE id BETWEEN ?1 AND ?2
			ORDER BY id ASC
		"
    .to_string();
    self.query(&where_clause, rusqlite::params![first, last])
  }

  pub fn query_by_prefix(&self, prefix: &str) -> ShResult<Option<(i64, HistEntry)>> {
    let where_clause = r"
			WHERE command LIKE ?1 || '%'
			ORDER BY id DESC
			LIMIT 1
		"
    .to_string();
    Ok(
      self
        .query(&where_clause, rusqlite::params![prefix])?
        .into_iter()
        .next(),
    )
  }

  #[cfg_attr(not(test), allow(dead_code))]
  pub fn push_entry(&self, entry: HistEntry) -> ShResult<bool> {
    Self::push_entry_conn(&self.lock(), &self.table, entry)
  }

  pub fn push_with(&self, conn: &Connection, entry: HistEntry) -> ShResult<bool> {
    Self::push_entry_conn(conn, &self.table, entry)
  }

  fn push_entry_conn(conn: &Connection, table: &str, entry: HistEntry) -> ShResult<bool> {
    let HistEntry {
      runtime,
      timestamp,
      command,
      cwd,
      status,
      token,
    } = entry;
    if command.is_empty() {
      return Ok(false);
    }
    if Self::token_exists(conn, table, token) {
      return Ok(false);
    }
    if shopt!(history.ignore_dupes) {
      let last: Option<String> = conn
        .query_row(
          &format!("SELECT command FROM {table} ORDER BY id DESC LIMIT 1"),
          [],
          |row| row.get(0),
        )
        .ok();
      if last.as_deref() == Some(command.as_ref()) {
        return Ok(false);
      }
    }

    let timestamp = timestamp.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let new_id = Self::last_id_conn(conn, table) + 1;
    conn.execute(
      &format!("INSERT INTO {table} (id, timestamp, runtime, command, cwd, status, token) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"),
      rusqlite::params![new_id, timestamp, runtime.as_micros() as i64, command, cwd, status, token.to_string()],
    )?;
    Ok(true)
  }

  pub fn token_exists(conn: &Connection, table: &str, token: Uuid) -> bool {
    conn
      .query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE token = ?1)"),
        rusqlite::params![token.to_string()],
        |row| row.get(0),
      )
      .unwrap_or(false)
  }

  pub fn update_search_mask(&mut self, prefix: Option<&str>) {
    let Some(entries) = HIST_ENTRIES.read().ok() else {
      self.search_mask = vec![];
      return;
    };
    let Some(entry_table) = entries.get(&self.table) else {
      self.search_mask = vec![];
      return;
    };
    let Some(prefix) = prefix else {
      self.search_mask = entry_table.clone();
      return;
    };

    self.search_mask = entry_table
      .iter()
      .filter(|e| e.command().starts_with(prefix))
      .cloned()
      .collect();
  }

  pub fn reset(&mut self) {
    self.mask_stale = true;
    self.cursor = self.search_mask.len();
    self.virt_cursor = self.cursor;
  }

  pub fn mark_mask_stale(&mut self) {
    self.mask_stale = true;
  }

  /// Refresh the search mask from the database if stale. Call before
  /// any operation that reads the mask (history scrolling).
  pub fn ensure_mask_fresh(&mut self) {
    if self.mask_stale {
      let prefix = self.pending.as_ref().map(LineBuf::to_string);
      self.constrain_entries(prefix.as_deref());
      self.mask_stale = false;
    }
  }

  pub fn constrain_entries(&mut self, prefix: Option<&str>) {
    self.update_search_mask(prefix);
    self.no_matches = self.search_mask.is_empty();
    if self.no_matches {
      self.update_search_mask(None);
    }

    self.cursor = self.search_mask.len();
    self.virt_cursor = self.cursor;
    self.mask_stale = false;
  }

  pub fn resolve_hist_token(&self, token: &str) -> Option<String> {
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

  pub fn row_to_entry(row: &rusqlite::Row) -> Result<HistEntry, rusqlite::Error> {
    Ok(HistEntry {
      command: row.get(0)?,
      timestamp: UNIX_EPOCH + Duration::from_secs(row.get::<_, i64>(1)? as u64),
      runtime: Duration::from_micros(row.get::<_, i64>(2)? as u64),
      cwd: row.get(3).unwrap_or_default(),
      status: row.get(4).unwrap_or(0),
      token: Uuid::from_str(row.get::<_, String>(5)?.as_str()).unwrap_or_default(),
    })
  }

  pub fn last(&self) -> Option<HistEntry> {
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

  pub fn update_pending_cmd(&mut self, buf: (&str, usize)) {
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

  pub fn at_pending(&self) -> bool {
    self.cursor >= self.search_mask.len()
  }

  pub fn reset_to_pending(&mut self) {
    self.cursor = self.search_mask.len();
    self.virt_cursor = self.cursor;
  }

  #[cfg(test)]
  pub fn masked_entries(&self) -> &[HistEntry] {
    &self.search_mask
  }

  /// Wipe the cross-test global caches for a given table name. Without this,
  /// tests that push to a shared table (e.g. `shed_history`) see entries
  /// from earlier tests in the same process, breaking single-entry-count
  /// assumptions.
  #[cfg(test)]
  pub fn clear_global_caches_for_test(table: &str) {
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
  pub fn insert_raw_for_test(&self, command: &str, timestamp: i64) {
    let table = &self.table;
    let conn = self.lock();
    let new_id = Self::last_id_conn(&conn, table) + 1;
    conn
      .execute(
        &format!(
          "INSERT INTO {table} (id, timestamp, runtime, command, cwd, token) VALUES (?1, ?2, 0, ?3, ?4, ?5)"
        ),
        rusqlite::params![new_id, timestamp, command, "", Uuid::new_v4().to_string()],
      )
      .unwrap();
  }

  /// Insert a row with an explicit id — simulates another session having
  /// committed a specific PRIMARY KEY out of band.
  #[cfg(test)]
  pub fn insert_raw_with_id_for_test(&self, command: &str, id: i64, timestamp: i64) {
    let table = &self.table;
    let conn = self.lock();
    conn
      .execute(
        &format!(
          "INSERT INTO {table} (id, timestamp, runtime, command, cwd, token) VALUES (?1, ?2, 0, ?3, ?4, ?5)"
        ),
        rusqlite::params![id, timestamp, command, "", Uuid::new_v4().to_string()],
      )
      .unwrap();
  }

  #[cfg(test)]
  pub fn set_max_size_for_test(&mut self, max: u32) {
    self.max_size = Some(max);
  }

  #[cfg(test)]
  pub fn set_search_watermark_for_test(&self, ts: i64) {
    if let Ok(mut wm) = SEARCH_WATERMARKS.write() {
      wm.insert(self.table.clone(), ts);
    }
  }

  #[cfg(test)]
  pub fn sync_search_entries_for_test(&self) {
    self.sync_search_entries();
  }

  /// Get a hint by scanning the in-memory cache. No database access.
  pub fn get_hint(&self) -> Option<Hint> {
    if !self.at_pending() {
      return None;
    }
    let prefix = self.pending.as_ref()?.to_string();
    if prefix.is_empty() {
      return None;
    }
    let entries = HIST_ENTRIES.read().ok()?;
    let table = entries.get(&self.table)?;
    table
      .iter()
      .rev()
      .find(|e| e.command().starts_with(&prefix) && e.command() != prefix)
      .map(|e| Hint::History(Lines::to_lines(e.command())))
  }

  pub fn refresh_hist_entries(&self) -> usize {
    let num_entries_before = num_entries(&self.table);
    let entries = query_masked(None, &self.lock(), &self.table);
    let max_ts = entries
      .iter()
      .filter_map(|e| e.timestamp.duration_since(std::time::UNIX_EPOCH).ok())
      .map(|d| d.as_secs() as i64)
      .max()
      .unwrap_or(0);
    if let Ok(mut cache) = HIST_ENTRIES.write() {
      cache.insert(self.table.clone(), entries.clone());
    }
    if let Ok(mut cache) = SEARCH_ENTRIES.write() {
      cache.insert(self.table.clone(), entries);
    }
    if let Ok(mut wm) = SEARCH_WATERMARKS.write() {
      wm.insert(self.table.clone(), max_ts);
    }

    let num_entries_after = num_entries(&self.table);
    num_entries_after.saturating_sub(num_entries_before)
  }

  #[cfg(test)]
  pub fn search_cache_commands(&self) -> Vec<String> {
    SEARCH_ENTRIES
      .read()
      .ok()
      .and_then(|c| c.get(&self.table).cloned())
      .unwrap_or_default()
      .iter()
      .map(|e| e.command().to_string())
      .collect()
  }

  #[cfg(test)]
  pub fn scroll_cache_commands(&self) -> Vec<String> {
    HIST_ENTRIES
      .read()
      .ok()
      .and_then(|c| c.get(&self.table).cloned())
      .unwrap_or_default()
      .iter()
      .map(|e| e.command().to_string())
      .collect()
  }

  pub fn is_virtual_scrolling(&self) -> bool {
    self.virt_cursor != self.cursor
  }

  pub fn virtual_scroll_direction(&self) -> Option<Direction> {
    match self.virt_cursor.cmp(&self.cursor) {
      Ordering::Greater => Some(Direction::Forward),
      Ordering::Equal => None,
      Ordering::Less => Some(Direction::Backward),
    }
  }

  pub fn stop_virtual_scroll(&mut self) {
    self.virt_cursor = self.cursor;
  }

  pub fn scroll(&mut self, offset: isize) -> Option<&HistEntry> {
    self.ensure_mask_fresh();
    self.cursor = self
      .cursor
      .saturating_add_signed(offset)
      .clamp(0, self.search_mask.len());
    self.virt_cursor = self.cursor;

    self.search_mask.get(self.cursor)
  }

  pub fn scroll_to(&mut self, idx: usize) -> Option<&HistEntry> {
    self.ensure_mask_fresh();
    self.cursor = idx.clamp(0, self.search_mask.len());
    self.virt_cursor = self.cursor;

    self.search_mask.get(self.cursor)
  }

  pub fn search_mask_count(&self) -> usize {
    self.search_mask.len()
  }

  pub fn virt_scroll(&mut self, offset: isize) -> Option<&HistEntry> {
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

  pub fn merge_search_entries(&mut self) {
    let search = SEARCH_ENTRIES
      .read()
      .ok()
      .and_then(|c| c.get(&self.table).cloned());
    if let Some(entries) = search
      && let Ok(mut hist) = HIST_ENTRIES.write()
    {
      hist.insert(self.table.clone(), entries);
    }
    self.mark_mask_stale();
  }

  // Fetch any entries from other sessions added after the watermark and merge
  // them into SEARCH_ENTRIES. Runs synchronously since the delta is small.
  fn sync_search_entries(&self) {
    let watermark = SEARCH_WATERMARKS
      .read()
      .ok()
      .and_then(|wm| wm.get(&self.table).copied())
      .unwrap_or(0);

    let delta = query_since(watermark, &self.lock(), &self.table);
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
      let entries = cache.entry(self.table.clone()).or_default();
      for new_entry in delta {
        entries.retain(|e| e.command != new_entry.command);
        entries.push(new_entry);
      }
      entries.sort_by_key(|e| e.timestamp);
    }
    if let Ok(mut wm) = SEARCH_WATERMARKS.write() {
      let wm_entry = wm.entry(self.table.clone()).or_insert(0);
      *wm_entry = (*wm_entry).max(new_watermark);
    }
  }

  pub fn start_search(&mut self, initial: &str) -> Option<String> {
    self.sync_search_entries();

    let all_entries = SEARCH_ENTRIES
      .read()
      .ok()
      .and_then(|c| c.get(&self.table).cloned())
      .unwrap_or_default();

    if all_entries.is_empty() {
      return None;
    }
    if all_entries.len() == 1 {
      return Some(all_entries[0].command().to_string());
    }

    let mut finder = FuzzySelector::new("History").number_candidates(true);

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

  pub fn stop_search(&mut self) {
    self.fuzzy_finder = None;
  }

  #[cfg(test)]
  pub fn entry_count(&self) -> i64 {
    self
      .lock()
      .query_row(&format!("SELECT COUNT(*) FROM {}", self.table), [], |row| {
        row.get(0)
      })
      .unwrap_or(0)
  }
}
