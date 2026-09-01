//! Working-directory changes
//!
//! Change the process working directory while keeping `$PWD`/`$OLDPWD` in sync
//! ([`change_dir`], [`change_dir_with_pwd`]).

use std::{
  path::{Path, PathBuf},
  time::SystemTime,
};

use crate::{
  defer, sherr,
  state::{db, params, paths, terminal::Terminal},
  try_var,
  util::error::ShResult,
};

use super::{
  Shed, autocmd,
  meta::MetaTab,
  vars::{VarFlags, VarKind},
};

/// Change the working directory to the next directory in the jump table.
pub(crate) fn next_dir() -> ShResult<()> {
  let Some(target) = Shed::meta(MetaTab::peek_fwd) else {
    return Err(sherr!(ExecFail, "nextd: no next directory"));
  };
  change_dir_with_pwd(target.as_path(), None, false)?;
  Shed::meta_mut(MetaTab::commit_fwd);
  Ok(())
}

/// Change the working directory to the previous directory in the jump table.
pub(crate) fn prev_dir() -> ShResult<()> {
  let Some(target) = Shed::meta(MetaTab::peek_back) else {
    return Err(sherr!(ExecFail, "prevd: no previous directory"));
  };
  change_dir_with_pwd(target.as_path(), None, false)?;
  Shed::meta_mut(MetaTab::commit_back);
  Ok(())
}

/// Parse `arr[idx]` into (name, `raw_index_expr`). Pure parsing, no expansion.
pub(crate) fn change_dir<P: AsRef<Path>>(dir: P) -> ShResult<()> {
  change_dir_with_pwd(dir, None, true)
}

/// Change the working directory and update `$PWD`/`$OLDPWD`.
///
/// The `is_new_dir` parameter is false if we got here from the `prevd`/`nextd` builtins
pub(crate) fn change_dir_with_pwd<P: AsRef<Path>>(
  dir: P,
  logical_pwd: Option<PathBuf>,
  is_new_dir: bool,
) -> ShResult<()> {
  let dir = dir.as_ref();
  let dir_raw = paths::path_to_varstr(dir);
  defer!(super::autocmd!(PostChangeDir));

  let current_dir = try_var!("PWD")
    .or_else(|| {
      std::env::current_dir()
        .ok()
        .map(|p| paths::path_to_varstr(&p))
    })
    .unwrap_or_default();

  params::with_vars(
    [
      ("NEW_DIR".into(), dir_raw.clone()),
      ("OLD_DIR".into(), current_dir.clone()),
    ],
    || autocmd!(PreChangeDir),
  );

  std::env::set_current_dir(dir)?;

  let new_pwd = logical_pwd.map_or_else(
    || {
      std::env::current_dir()
        .ok()
        .map_or_else(|| dir_raw.clone(), |p| paths::path_to_varstr(&p))
    },
    |p| paths::path_to_varstr(&p),
  );

  if Shed::meta(MetaTab::interactive_shell)
    && Shed::term(Terminal::interactive)
    && let Ok(dir) = std::env::current_dir()
    && let Some(conn) = db::get_db_conn()
    && let Ok(conn) = conn.try_lock()
  {
    let dir_str = dir.to_string_lossy();
    let timestamp = SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_secs() as i64;

    conn
      .execute(
        "INSERT INTO dir_history (path, visits, last_visit) VALUES (?1, 1, ?2)
       ON CONFLICT(path) DO UPDATE SET visits = visits + 1, last_visit = ?2",
        rusqlite::params![dir_str.as_ref(), timestamp],
      )
      .ok();
  }

  if is_new_dir {
    Shed::meta_mut(|m| m.new_dir(dir.to_path_buf()));
  }

  Shed::vars_mut(|v| {
    v.set_var("OLDPWD", VarKind::Str(current_dir), VarFlags::EXPORT)?;
    v.set_var("PWD", VarKind::string(new_pwd), VarFlags::EXPORT)
  })?;

  Ok(())
}
