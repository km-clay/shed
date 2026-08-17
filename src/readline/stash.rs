use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

use crate::state::vars::VarStr;

use super::{ShResult, sherr, state};

#[derive(Debug)]
pub(crate) struct StashedCmd {
  pub name: Option<VarStr>,
  pub buffer: VarStr,
  pub cursor_pos: VarStr, // absolute grapheme pos or row:col
}

pub(crate) struct Stash {
  conn: Arc<Mutex<Connection>>,
}

impl Stash {
  pub fn new() -> ShResult<Self> {
    let conn =
      state::util::get_db_conn().ok_or_else(|| sherr!(InternalErr, "database not available"))?;
    Self::init_stash_table(
      &conn
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner),
    )?;
    Ok(Self { conn })
  }

  fn lock(&self) -> MutexGuard<'_, Connection> {
    self
      .conn
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  pub fn init_stash_table(conn: &Connection) -> ShResult<()> {
    conn.execute_batch(
      r"
      CREATE TABLE IF NOT EXISTS stash (
        id	INTEGER PRIMARY KEY,
        name TEXT,
        buffer TEXT NOT NULL,
        cursor TEXT,
        timestamp INTEGER
      );
      ",
    )?;
    Ok(())
  }

  pub fn stack_len(&self) -> usize {
    self
      .lock()
      .query_row("SELECT COUNT(*) FROM stash WHERE name IS NULL", [], |row| {
        row.get(0)
      })
      .unwrap_or(0i64) as usize
  }

  pub fn list(&self, mut named_only: bool, mut stack_only: bool) -> VarStr {
    if named_only && stack_only {
      named_only = false;
      stack_only = false;
    }
    let stack: Vec<String> = self
      .lock()
      .prepare("SELECT buffer FROM stash WHERE name IS NULL ORDER BY timestamp ASC")
      .and_then(|mut stmt| {
        stmt
          .query_map([], |row| row.get::<_, String>(0))?
          .collect::<Result<Vec<_>, _>>()
      })
      .unwrap_or_else(|_| vec![]);
    let named: Vec<(String, String)> = self
      .lock()
      .prepare("SELECT name, buffer FROM stash WHERE name IS NOT NULL ORDER BY timestamp ASC")
      .and_then(|mut stmt| {
        stmt
          .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
          .collect::<Result<Vec<_>, _>>()
      })
      .unwrap_or_else(|_| vec![]);

    let mut output = String::new();
    if !stack.is_empty() && !named_only {
      if stack_only {
        output.push('\n');
      } else {
        output.push_str("Stack:\n");
      }
      output.push_str(
        &stack
          .iter()
          .map(|s| s.replace('\n', "\n\t"))
          .enumerate()
          .map(|(i, s)| format!("[{i}]\t{s}"))
          .collect::<Vec<_>>()
          .join("\n"),
      );
    }

    if !named.is_empty() && !stack_only {
      if !output.is_empty() {
        output.push_str("\n\n");
      }
      if named_only {
        output.push('\n');
      } else {
        output.push_str("Named:\n");
      }
      output.push_str(
        &named
          .iter()
          .map(|(n, b)| format!("{n}\t{}", b.replace('\n', "\n\t")))
          .collect::<Vec<_>>()
          .join("\n"),
      );
    }

    output.into()
  }
  pub fn stash_cmd(&self, cmd: &StashedCmd) -> ShResult<()> {
    if cmd
      .name
      .as_ref()
      .is_some_and(|n| n.to_str_lossy().parse::<usize>().is_ok())
    {
      return Err(sherr!(ParseErr, "stash name cannot be a number"));
    }
    let conn = self.lock();
    if let Some(ref name) = cmd.name {
      conn.execute("DELETE FROM stash WHERE name = ?1", [name])?;
    }
    conn.execute(
      "INSERT INTO stash (name, buffer, cursor, timestamp) VALUES (?1, ?2, ?3, strftime('%s', 'now'))",
      (&cmd.name, &cmd.buffer, cmd.cursor_pos.to_str_lossy().trim())
    )?;
    Ok(())
  }
  pub fn delete_cmd(&self, cmd: &str) -> ShResult<()> {
    let conn = self.lock();
    if let Ok(n) = cmd.parse::<usize>() {
      conn.execute(
        "DELETE FROM stash WHERE name IS NULL AND id IN (SELECT id FROM stash WHERE name IS NULL ORDER BY timestamp ASC LIMIT 1 OFFSET ?1)",
        [n as i64]
      )?;
    } else {
      conn.execute("DELETE FROM stash WHERE name = ?1", [cmd])?;
    }
    Ok(())
  }

  pub fn pop(&self, n: usize) -> ShResult<Option<StashedCmd>> {
    let conn = self.lock();
    let mut stmt = conn.prepare("
      SELECT id, buffer, cursor FROM stash WHERE name IS NULL ORDER BY timestamp ASC LIMIT 1 OFFSET ?1
    ")?;

    let Some((id, cmd)) = stmt
      .query_row([n as i64], |row| {
        Ok((
          row.get::<_, i64>(0)?,
          StashedCmd {
            name: None,
            buffer: row.get(1)?,
            cursor_pos: row.get(2)?,
          },
        ))
      })
      .ok()
    else {
      return Ok(None);
    };

    conn.execute("DELETE FROM stash WHERE id = ?1", [id])?;
    Ok(Some(cmd))
  }

  pub fn push(&self, name: Option<&VarStr>, buffer: &str, cursor: (usize, usize)) -> ShResult<()> {
    let (row, col) = cursor;
    if name
      .as_ref()
      .is_some_and(|n| n.to_str_lossy().parse::<usize>().is_ok())
    {
      return Err(sherr!(ParseErr, "stashed command name cannot be a number"));
    }
    let cursor = format!("{row}:{col}");
    let conn = self.lock();
    if let Some(ref name) = name {
      conn.execute("DELETE FROM stash WHERE name = ?1", [name])?;
    }
    let mut stmt = conn.prepare(
      "
      INSERT INTO stash (name, buffer, cursor, timestamp) VALUES (?1, ?2, ?3, strftime('%s', 'now'))
    ",
    )?;

    stmt.execute((&name, buffer, cursor.trim()))?;
    Ok(())
  }

  pub fn get_index(&self, n: usize) -> ShResult<Option<StashedCmd>> {
    let conn = self.lock();
    let mut stmt = conn.prepare(
      "
      SELECT buffer, cursor FROM stash WHERE name IS NULL ORDER BY timestamp ASC LIMIT 1 OFFSET ?1
    ",
    )?;

    let Some(cmd) = stmt
      .query_row([n as i64], |row| {
        Ok(StashedCmd {
          name: None,
          buffer: row.get(0)?,
          cursor_pos: row.get(1)?,
        })
      })
      .ok()
    else {
      return Ok(None);
    };

    Ok(Some(cmd))
  }

  pub fn get_named(&self, name: &str) -> ShResult<Option<StashedCmd>> {
    let conn = self.lock();
    let mut stmt = conn.prepare(
      "
      SELECT buffer, cursor FROM stash WHERE name = ?1 ORDER BY timestamp ASC LIMIT 1
    ",
    )?;

    let Some(cmd) = stmt
      .query_row([name], |row| {
        Ok(StashedCmd {
          name: Some(name.into()),
          buffer: row.get(0)?,
          cursor_pos: row.get(1)?,
        })
      })
      .ok()
    else {
      return Ok(None);
    };

    Ok(Some(cmd))
  }

  pub fn get(&self, ident: &str) -> ShResult<Option<StashedCmd>> {
    if let Ok(n) = ident.parse::<usize>() {
      self.get_index(n)
    } else {
      self.get_named(ident.trim())
    }
  }
}
