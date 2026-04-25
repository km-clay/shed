use std::sync::Arc;

use rusqlite::Connection;

use crate::{
  getopt::{Opt, OptSpec},
  match_loop, outln, sherr, state,
  util::error::{ShResult, ShResultExt},
};

#[derive(Debug)]
pub struct StashedCmd {
  pub name: Option<String>,
  pub buffer: String,
  pub cursor_pos: String, // absolute grapheme pos or row:col
}

#[derive(Debug, Default)]
pub struct StashOpts {
  to_save: Vec<StashedCmd>,
  to_delete: Vec<String>,
  list: bool,
  only_named: bool,
  only_stack: bool,
}

impl StashOpts {
  pub fn from_opts(opts: Vec<Opt>) -> ShResult<Self> {
    let mut new = Self::default();
    let mut opt_iter = opts.into_iter();

    match_loop!(opt_iter.next() => opt, {
      Opt::ShortWithList('s',mut args) => {
        // length of 'args' is enforced by the opt spec
        let cursor = args.pop().unwrap();
        let buffer = args.pop().unwrap();
        let name = args.pop().unwrap();
        new.to_save.push(StashedCmd {
          name: Some(name),
          buffer,
          cursor_pos: cursor,
        });
      }
      Opt::LongWithList(opt, mut args) => {
        let "save" = opt.as_str() else {
          return Err(sherr!(ParseErr, "unexpected option {opt} in stash"))
        };

        // length of 'args' is enforced by the opt spec
        let cursor = args.pop().unwrap();
        let buffer = args.pop().unwrap();
        let name = args.pop().unwrap();
        new.to_save.push(StashedCmd {
          name: Some(name),
          buffer,
          cursor_pos: cursor,
        });
      }
      Opt::ShortWithArg('d', arg) => {
        new.to_delete.push(arg);
      }
      Opt::LongWithArg(opt, arg) => {
        match opt.as_str() {
          "delete" => new.to_delete.push(arg),
          _ => return Err(sherr!(ParseErr, "unexpected option {opt} in stash"))
        }
      }
      Opt::Long(arg) => {
        match arg.as_str() {
          "list" => new.list = true,
          "stack" => new.only_stack = true,
          "named" => new.only_named = true,
          _ => return Err(sherr!(ParseErr, "unexpected option {arg} in stash"))
        }
      }
      _ => return Err(sherr!(ParseErr, "unexpected option {opt} in stash"))
    });

    Ok(new)
  }
}

pub struct Stash {
  conn: Arc<Connection>,
}

impl Stash {
  pub fn new() -> ShResult<Self> {
    let conn = state::get_db_conn().ok_or_else(|| sherr!(InternalErr, "database not available"))?;
    Self::init_stash_table(&conn)?;
    Ok(Self { conn })
  }
  pub fn init_stash_table(conn: &Arc<Connection>) -> ShResult<()> {
    conn.execute_batch(
      r#"
			CREATE TABLE IF NOT EXISTS stash (
				id	INTEGER PRIMARY KEY,
        name TEXT,
        buffer TEXT NOT NULL,
        cursor TEXT,
        timestamp INTEGER
			);
		"#,
    )?;
    Ok(())
  }

  pub fn stack_len(&self) -> usize {
    self
      .conn
      .query_row("SELECT COUNT(*) FROM stash WHERE name IS NULL", [], |row| {
        row.get(0)
      })
      .unwrap_or(0i64) as usize
  }

  pub fn list(&self, mut named_only: bool, mut stack_only: bool) -> String {
    if named_only && stack_only {
      named_only = false;
      stack_only = false;
    }
    let stack: Vec<String> = self
      .conn
      .prepare("SELECT buffer FROM stash WHERE name IS NULL ORDER BY timestamp ASC")
      .and_then(|mut stmt| {
        stmt
          .query_map([], |row| row.get::<_, String>(0))?
          .collect::<Result<Vec<_>, _>>()
      })
      .unwrap_or_else(|_| vec![]);
    let named: Vec<(String, String)> = self
      .conn
      .prepare("SELECT name, buffer FROM stash WHERE name IS NOT NULL ORDER BY timestamp ASC")
      .and_then(|mut stmt| {
        stmt
          .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
          .collect::<Result<Vec<_>, _>>()
      })
      .unwrap_or_else(|_| vec![]);

    let mut output = String::new();
    if !stack.is_empty() && !named_only {
      if !stack_only {
        output.push_str("Stack:\n");
      } else {
        output.push('\n');
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
      if !named_only {
        output.push_str("Named:\n");
      } else {
        output.push('\n');
      }
      output.push_str(
        &named
          .iter()
          .map(|(n, b)| format!("{n}\t{}", b.replace('\n', "\n\t")))
          .collect::<Vec<_>>()
          .join("\n"),
      );
    }

    output
  }
  pub fn stash_cmd(&self, cmd: StashedCmd) -> ShResult<()> {
    if cmd
      .name
      .as_ref()
      .is_some_and(|n| n.parse::<usize>().is_ok())
    {
      return Err(sherr!(ParseErr, "stash name cannot be a number"));
    }
    if let Some(ref name) = cmd.name {
      self
        .conn
        .execute("DELETE FROM stash WHERE name = ?1", [name])?;
    }
    self.conn.execute(
      "INSERT INTO stash (name, buffer, cursor, timestamp) VALUES (?1, ?2, ?3, strftime('%s', 'now'))",
      (&cmd.name, &cmd.buffer, cmd.cursor_pos.trim())
    )?;
    Ok(())
  }
  pub fn delete_cmd(&self, cmd: &str) -> ShResult<()> {
    if let Ok(n) = cmd.parse::<usize>() {
      self.conn.execute(
        "DELETE FROM stash WHERE name IS NULL AND id IN (SELECT id FROM stash WHERE name IS NULL ORDER BY timestamp ASC LIMIT 1 OFFSET ?1)",
        [n as i64]
      )?;
    } else {
      self
        .conn
        .execute("DELETE FROM stash WHERE name = ?1", [cmd])?;
    }
    Ok(())
  }

  pub fn pop(&self, n: usize) -> ShResult<Option<StashedCmd>> {
    let mut stmt = self.conn.prepare("
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

    self.conn.execute("DELETE FROM stash WHERE id = ?1", [id])?;
    Ok(Some(cmd))
  }

  pub fn push(&self, name: Option<String>, buffer: &str, cursor: (usize, usize)) -> ShResult<()> {
    let (row, col) = cursor;
    if name.as_ref().is_some_and(|n| n.parse::<usize>().is_ok()) {
      return Err(sherr!(ParseErr, "stashed command name cannot be a number"));
    }
    let cursor = format!("{row}:{col}");
    if let Some(ref name) = name {
      self
        .conn
        .execute("DELETE FROM stash WHERE name = ?1", [name])?;
    }
    let mut stmt = self.conn.prepare(
      "
      INSERT INTO stash (name, buffer, cursor, timestamp) VALUES (?1, ?2, ?3, strftime('%s', 'now'))
    ",
    )?;

    stmt.execute((&name, buffer, cursor.trim()))?;
    Ok(())
  }

  pub fn get_index(&self, n: usize) -> ShResult<Option<StashedCmd>> {
    let mut stmt = self.conn.prepare(
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
    let mut stmt = self.conn.prepare(
      "
      SELECT buffer, cursor FROM stash WHERE name LIKE ?1 ORDER BY timestamp ASC LIMIT 1
    ",
    )?;

    let Some(cmd) = stmt
      .query_row([name], |row| {
        Ok(StashedCmd {
          name: Some(name.to_string()),
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

pub(super) struct StashBuiltin;
impl super::Builtin for StashBuiltin {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::exact_args('s', 3),
      OptSpec::exact_args("save", 3),
      OptSpec::single_arg('d'),
      OptSpec::single_arg("delete"),
      OptSpec::flag('l'),
      OptSpec::flag("list"),
      OptSpec::flag("stack"),
      OptSpec::flag("named"),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let is_empty = args.opts.is_empty();
    let stash_opts = StashOpts::from_opts(args.opts).promote_err(span.clone())?;
    let stash = Stash::new().promote_err(span.clone())?;

    for cmd in stash_opts.to_save {
      stash.stash_cmd(cmd).promote_err(span.clone())?;
    }

    for cmd in stash_opts.to_delete {
      stash.delete_cmd(&cmd).promote_err(span.clone())?;
    }

    if stash_opts.list || is_empty {
      let output = stash.list(stash_opts.only_named, stash_opts.only_stack);
      outln!("{output}")?;
    }

    Ok(())
  }
}
