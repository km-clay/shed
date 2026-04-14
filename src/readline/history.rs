use std::{
  cmp::Ordering,
  env,
  path::PathBuf,
  sync::Arc,
  time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;
use uuid::Uuid;

use crate::{
  libsh::error::ShResult,
  readline::{
    complete::{Candidate, FuzzySelector},
    editcmd::Direction,
    linebuf::LineBuf,
  },
  sherr,
  state::read_shopts,
};

#[derive(Debug, Clone)]
pub struct HistEntry {
  pub runtime: Duration,
  pub timestamp: SystemTime,
  pub command: String,
  pub cwd: String,
  pub status: i32,
  pub token: Uuid,
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
  pub fn runtime(&self) -> Duration {
    self.runtime
  }
  pub fn timestamp(&self) -> SystemTime {
    self.timestamp
  }
  pub fn command(&self) -> &str {
    &self.command
  }
}

#[derive(Debug, Clone)]
pub struct History {
  pub pending: Option<LineBuf>,
  pub fuzzy_finder: FuzzySelector,
  pub cursor: usize,
  pub virt_cursor: usize,

  conn: Arc<Connection>,
  table: String,
  search_mask: Vec<HistEntry>,
  no_matches: bool,
  max_size: Option<u32>,
}

impl History {
  const USER_VERSION: i32 = 2;
  fn init_db(conn: &Connection, table: &str) -> rusqlite::Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL")?;
    conn.execute_batch(&format!(
      r#"
			CREATE TABLE IF NOT EXISTS {table} (
				id	INTEGER PRIMARY KEY,
				timestamp	INTEGER NOT NULL,
				runtime	INTEGER NOT NULL DEFAULT 0,
				command TEXT NOT NULL
			);
		"#
    ))?;

    let mut user_version = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))?;
    while user_version < Self::USER_VERSION {
      match user_version {
        0 => {
          conn.execute_batch(&format!(
            r#"
						ALTER TABLE {table} ADD COLUMN cwd TEXT;
						ALTER TABLE {table} ADD COLUMN status INT DEFAULT 0;
						"#
          ))?;
          conn.execute_batch("PRAGMA user_version = 1")?;
        }
        1 => {
          // add the token field here.
          // 'token' acts as an absolute identifier since
          // the id field actually shifts after commands are deleted
          conn
            .execute_batch(&format!(
              r#"
						ALTER TABLE {table} ADD COLUMN token TEXT;
						"#
            ))
            .ok();

          let mut stmt = conn.prepare(&format!("SELECT id FROM {table} WHERE token IS NULL"))?;
          let ids: Vec<i64> = stmt
            .query_map([], |r| r.get(0))?
            .filter_map(|r| r.ok())
            .collect();

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

          conn.execute_batch("PRAGMA user_version = 2")?;
        }
        _ => {}
      }
      user_version = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i32>(0))?;
    }

    Ok(())
  }
  pub fn new(table: &str) -> ShResult<Self> {
    let max_hist = read_shopts(|o| o.core.max_hist);

    let db_path = if let Ok(var) = env::var("SHED_HISTDB") {
      var
    } else {
      let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
      dirs::data_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| format!("{home}/.local/share/shed/shed_hist.db"))
    };

    let db_path = PathBuf::from(db_path);
    if let Some(parent) = db_path.parent() {
      std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(&db_path)?;
    Self::init_db(&conn, table)?;

    let max_size = (max_hist >= 0).then_some(max_hist as u32);
    let mut hist = Self {
      conn: conn.into(),
      table: table.to_string(),
      pending: None,
      search_mask: vec![],
      fuzzy_finder: FuzzySelector::new("History").number_candidates(true),
      no_matches: false,
      cursor: 0,
      virt_cursor: 0,
      max_size,
    };
    hist.reset();
    Ok(hist)
  }

  pub fn empty(table: &str) -> Self {
    let conn = Connection::open_in_memory().expect("Failed to open in-memory database");
    Self::init_db(&conn, table).expect("Failed to initialize in-memory database");
    Self {
      conn: conn.into(),
      table: table.to_string(),
      pending: None,
      search_mask: vec![],
      fuzzy_finder: FuzzySelector::new("History").number_candidates(true),
      no_matches: false,
      cursor: 0,
      virt_cursor: 0,
      max_size: None,
    }
  }

  pub fn push(&self, command: String) -> ShResult<Option<Uuid>> {
    if command.is_empty() {
      return Ok(None);
    }
    if read_shopts(|o| o.core.hist_ignore_dupes) {
      let last: Option<String> = self
        .conn
        .query_row(
          &format!(
            "SELECT command FROM {} ORDER BY id DESC LIMIT 1",
            self.table
          ),
          [],
          |row| row.get(0),
        )
        .ok();
      if last.as_deref() == Some(&command) {
        return Ok(None);
      }
    }
    let table = &self.table;
    let timestamp = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .as_secs() as i64;
    let new_id = self.last_id() + 1;
    let cwd = env::current_dir()
      .map(|p| p.to_string_lossy().to_string())
      .ok();
    let token = Uuid::new_v4();

    self.conn.execute(
      &format!("INSERT INTO {table} (id, timestamp, runtime, command, cwd, token) VALUES (?1, ?2, 0, ?3, ?4, ?5)"),
      rusqlite::params![new_id, timestamp, command, cwd, token.to_string()],
    )?;
    Ok(Some(token))
  }

  pub fn set_status(&self, token: Uuid, runtime: Option<Duration>, status: i32) -> ShResult<()> {
    let table = &self.table;
    let micros = runtime.map(|r| r.as_micros() as i64).unwrap_or(0);
    self.conn.execute(
      &format!("UPDATE {table} SET runtime = ?1, status = ?2 WHERE token = ?3"),
      rusqlite::params![micros, status, token.to_string()],
    )?;

    Ok(())
  }

  pub fn entry_count(&self) -> i64 {
    self
      .conn
      .query_row(&format!("SELECT COUNT(*) FROM {}", self.table), [], |row| {
        row.get(0)
      })
      .unwrap_or(0)
  }

  pub fn last_id(&self) -> i64 {
    self
      .conn
      .query_row(
        &format!("SELECT id FROM {} ORDER BY id DESC LIMIT 1", self.table),
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

    let tx = self.conn.unchecked_transaction()?;
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

  /// Restores the history table from the rolling backup created by the last delete operation.
  pub fn restore_backup(&self) -> ShResult<i64> {
    let table = &self.table;
    let has_backup: bool = self.conn.query_row(
      "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
      [&format!("{table}_backup")],
      |row| row.get(0),
    )?;
    if !has_backup {
      return Err(sherr!(HistoryReadErr, "no backup table found"));
    }
    let tx = self.conn.unchecked_transaction()?;
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
    let tx = self.conn.unchecked_transaction()?;
    tx.execute_batch(&format!(
      r#"
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
			"#
    ))?;
    tx.commit()?;
    Ok(())
  }

  pub fn transaction<T, F: FnOnce() -> ShResult<T>>(&self, f: F) -> ShResult<T> {
    self.conn.execute_batch("BEGIN")?;
    match f() {
      Ok(val) => {
        self.conn.execute_batch("COMMIT")?;
        Ok(val)
      }
      Err(e) => {
        self.conn.execute_batch("ROLLBACK").ok();
        Err(e)
      }
    }
  }

  /// Runs a query on the history table with the given WHERE clause and parameters, returning a vector of (id, HistEntry) tuples.
  pub fn query(
    &self,
    where_clause: &str,
    params: &[&dyn rusqlite::ToSql],
  ) -> ShResult<Vec<(i64, HistEntry)>> {
    let table = &self.table;
    let sql = format!(
      "SELECT command, timestamp, runtime, cwd, status, token, id FROM {table} {where_clause}"
    );
    let mut stmt = self.conn.prepare(&sql)?;
    let rows = stmt.query_map(params, |row| Ok((row.get(6)?, Self::row_to_entry(row)?)))?;

    Ok(rows.filter_map(Result::ok).collect())
  }

  pub fn query_range(&self, first: i64, last: i64) -> ShResult<Vec<(i64, HistEntry)>> {
    let where_clause = r##"
			WHERE id BETWEEN ?1 AND ?2
			ORDER BY id ASC
		"##
      .to_string();
    self.query(&where_clause, rusqlite::params![first, last])
  }

  pub fn query_by_prefix(&self, prefix: &str) -> ShResult<Option<(i64, HistEntry)>> {
    let where_clause = r##"
			WHERE command LIKE ?1 || '%'
			ORDER BY id DESC
			LIMIT 1
		"##
      .to_string();
    Ok(
      self
        .query(&where_clause, rusqlite::params![prefix])?
        .into_iter()
        .next(),
    )
  }

  pub fn push_entry(&self, entry: HistEntry) -> ShResult<()> {
    let HistEntry {
      runtime,
      timestamp,
      command,
      cwd,
      status,
      token,
    } = entry;
    if command.is_empty() {
      return Ok(());
    }
    if read_shopts(|o| o.core.hist_ignore_dupes) {
      let last: Option<String> = self
        .conn
        .query_row(
          &format!(
            "SELECT command FROM {} ORDER BY id DESC LIMIT 1",
            self.table
          ),
          [],
          |row| row.get(0),
        )
        .ok();
      if last.as_deref() == Some(&command) {
        return Ok(());
      }
    }
    let table = &self.table;
    let timestamp = timestamp.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let new_id = self.last_id() + 1;
    self.conn.execute(
      &format!("INSERT INTO {table} (id, timestamp, runtime, command, cwd, status, token) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"),
      rusqlite::params![new_id, timestamp, runtime.as_micros() as i64, command, cwd, status, token.to_string()],
    )?;
    Ok(())
  }

  pub fn query_masked(&self, prefix: Option<&str>) -> Vec<HistEntry> {
    let table = &self.table;
    let sql = match prefix {
      Some(_) => format!(
        r##"
				SELECT command, MAX(timestamp) as ts, runtime, cwd, status, token FROM {table}
				WHERE command LIKE ?1 || '%'
				GROUP BY command
				ORDER BY ts ASC
			"##
      ),
      None => format!(
        r##"
				SELECT command, MAX(timestamp) as ts, runtime, cwd, status, token FROM {table}
				GROUP BY command
				ORDER BY ts ASC
			"##
      ),
    };
    let mut stmt = match self.conn.prepare(&sql) {
      Ok(s) => s,
      Err(_) => return vec![],
    };
    let rows = match prefix {
      Some(p) => stmt.query_map(rusqlite::params![p], Self::row_to_entry),
      None => stmt.query_map([], Self::row_to_entry),
    };

    match rows {
      Ok(iter) => iter.filter_map(Result::ok).collect(),
      Err(_) => vec![],
    }
  }

  pub fn update_search_mask(&mut self, prefix: Option<&str>) {
    self.search_mask = self.query_masked(prefix);
  }

  pub fn reset(&mut self) {
    self.update_search_mask(None);
    self.cursor = self.search_mask.len();
    self.virt_cursor = self.cursor;
  }

  pub fn constrain_entries(&mut self, prefix: Option<&str>) {
    self.update_search_mask(prefix);
    self.no_matches = self.search_mask.is_empty();
    if self.no_matches {
      self.update_search_mask(None);
    }

    self.cursor = self.search_mask.len();
    self.virt_cursor = self.cursor;
  }

  pub fn resolve_hist_token(&self, token: &str) -> Option<String> {
    let token = token.strip_prefix('!').unwrap_or(token).to_string();
    if let Ok(num) = token.parse::<i32>()
      && num != 0
    {
      match num.cmp(&0) {
        // Negative: index from the bottom (!-2 = 2nd from end)
        Ordering::Less => {
          let offset = num.unsigned_abs() as i64 - 1;
          self
            .conn
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
          let offset = num as i64 - 1;
          self
            .conn
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
        _ => unreachable!(),
      }
    } else {
      self
        .conn
        .query_row(
          &format!(
            "SELECT command FROM {} WHERE command LIKE ?1 || '%' ORDER BY id DESC LIMIT 1",
            self.table
          ),
          rusqlite::params![token],
          |row| row.get(0),
        )
        .ok()
    }
  }

  fn row_to_entry(row: &rusqlite::Row) -> Result<HistEntry, rusqlite::Error> {
    Ok(HistEntry {
      command: row.get(0)?,
      timestamp: UNIX_EPOCH + Duration::from_secs(row.get::<_, i64>(1)? as u64),
      runtime: Duration::from_micros(row.get::<_, i64>(2)? as u64),
      cwd: row.get(3).unwrap_or_else(|_| "".to_string()),
      status: row.get(4).unwrap_or(0),
      token: Uuid::parse_str(row.get::<_, String>(5)?.as_str()).unwrap_or_default(),
    })
  }

  pub fn last(&self) -> Option<HistEntry> {
    self
      .conn
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
    let cursor_pos = if let Some(pending) = &self.pending {
      pending.cursor_to_flat()
    } else {
      buf.1
    };
    let cmd = buf.0.to_string();

    if let Some(pending) = &mut self.pending {
      pending.set_buffer(cmd.clone());
      pending.set_cursor_from_flat(cursor_pos);
    } else {
      self.pending = Some(LineBuf::new().with_initial(&cmd, cursor_pos));
    }
    self.constrain_entries(Some(&cmd));
  }

  pub fn max_hist_size(&mut self, size: Option<u32>) {
    self.max_size = size
  }

  pub fn masked_entries(&self) -> &[HistEntry] {
    &self.search_mask
  }

  pub fn cursor_entry(&self) -> Option<&HistEntry> {
    self.search_mask.get(self.cursor)
  }

  pub fn at_pending(&self) -> bool {
    self.cursor >= self.search_mask.len()
  }

  pub fn reset_to_pending(&mut self) {
    self.cursor = self.search_mask.len();
    self.virt_cursor = self.cursor;
  }

  pub fn hint_entry(&self) -> Option<&HistEntry> {
    if self.no_matches {
      return None;
    };
    self.search_mask.last()
  }

  pub fn get_hint(&self) -> Option<String> {
    if self.at_pending()
      && self
        .pending
        .as_ref()
        .is_some_and(|p| !p.joined().is_empty())
    {
      let entry = self.hint_entry()?;
      Some(entry.command().to_string())
    } else {
      None
    }
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
    self.cursor = self
      .cursor
      .saturating_add_signed(offset)
      .clamp(0, self.search_mask.len());
    self.virt_cursor = self.cursor;

    self.search_mask.get(self.cursor)
  }

  pub fn scroll_to(&mut self, idx: usize) -> Option<&HistEntry> {
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

  pub fn start_search(&mut self, initial: &str) -> Option<String> {
    if self.search_mask.is_empty() {
      None
    } else if self.search_mask.len() == 1 {
      Some(self.search_mask[0].command().to_string())
    } else {
      self.update_search_mask(Some(initial));
      self.fuzzy_finder.set_query(initial.to_string());
      let raw_entries = self
        .search_mask
        .clone()
        .into_iter()
        .enumerate()
        .map(|(i, ent)| Candidate::from((i, ent.command().to_string())));
      self.fuzzy_finder.activate(raw_entries.collect());
      None
    }
  }
}
