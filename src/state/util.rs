use super::*;

use std::collections::HashMap;

use nix::unistd::{User, getuid};

use crate::{
  exec_input,
  libsh::error::ShResult,
  match_loop,
  parse::lex::{LexFlags, LexStream},
  prelude::*,
  sherr,
  shopt::ShOpts,
};

/// Read from the job table
pub fn read_jobs<T, F: FnOnce(&JobTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.jobs.borrow()))
}

/// Write to the job table
pub fn write_jobs<T, F: FnOnce(&mut JobTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.jobs.borrow_mut()))
}

/// Read from the var scope stack
pub fn read_vars<T, F: FnOnce(&ScopeStack) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.var_scopes.borrow()))
}

/// Write to the variable table
pub fn write_vars<T, F: FnOnce(&mut ScopeStack) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.var_scopes.borrow_mut()))
}

/// Parse `arr[idx]` into (name, raw_index_expr). Pure parsing, no expansion.
pub fn parse_arr_bracket(var_name: &str) -> Option<(String, String)> {
  let mut chars = var_name.chars();
  let mut name = String::new();
  let mut idx_raw = String::new();
  let mut bracket_depth = 0;

  match_loop!(chars.next() => ch, {
    '\\' => {
      chars.next();
    }
    '[' => {
      bracket_depth += 1;
      if bracket_depth > 1 {
        idx_raw.push(ch);
      }
    }
    ']' => {
      if bracket_depth > 0 {
        bracket_depth -= 1;
        if bracket_depth == 0 {
          if idx_raw.is_empty() {
            return None;
          }
          break;
        }
      }
      idx_raw.push(ch);
    }
    _ if bracket_depth > 0 => idx_raw.push(ch),
    _ => name.push(ch),
  });

  if name.is_empty() || idx_raw.is_empty() {
    None
  } else {
    Some((name, idx_raw))
  }
}

/// Expand the raw index expression and parse it into an ArrIndex.
pub fn expand_arr_index(idx_raw: &str) -> ShResult<ArrIndex> {
  let expanded = LexStream::new(Arc::new(idx_raw.to_string()), LexFlags::empty())
    .map(|tk| tk.and_then(|tk| tk.expand()).map(|tk| tk.get_words()))
    .try_fold(vec![], |mut acc, wrds| {
      match wrds {
        Ok(wrds) => acc.extend(wrds),
        Err(e) => return Err(e),
      }
      Ok(acc)
    })?
    .into_iter()
    .next()
    .ok_or_else(|| sherr!(ParseErr, "Empty array index"))?;

  expanded
    .parse::<ArrIndex>()
    .map_err(|_| sherr!(ParseErr, "Invalid array index: {}", expanded,))
}

pub fn read_meta<T, F: FnOnce(&MetaTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.meta.borrow()))
}

/// Write to the meta table
pub fn write_meta<T, F: FnOnce(&mut MetaTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.meta.borrow_mut()))
}

/// Read from the logic table
pub fn read_logic<T, F: FnOnce(&LogTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.logic.borrow()))
}

/// Write to the logic table
pub fn write_logic<T, F: FnOnce(&mut LogTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.logic.borrow_mut()))
}

pub fn read_shopts<T, F: FnOnce(&ShOpts) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.shopts.borrow()))
}

pub fn write_shopts<T, F: FnOnce(&mut ShOpts) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.shopts.borrow_mut()))
}

pub fn descend_scope(argv: Option<Vec<String>>) {
  write_vars(|v| v.descend(argv));
}
pub fn ascend_scope() {
  write_vars(|v| v.ascend());
}

/// This function is used internally and ideally never sees user input
///
/// It will panic if you give it an invalid path.
pub fn get_shopt(path: &str) -> String {
  read_shopts(|s| s.get(path)).unwrap().unwrap()
}

pub fn with_vars<F, H, V, T>(vars: H, f: F) -> T
where
  F: FnOnce() -> T,
  H: Into<HashMap<String, V>>,
  V: Into<Var>,
{
  let snapshot = read_vars(|v| v.clone());
  let vars = vars.into();
  for (name, val) in vars {
    let val = val.into();
    let kind = val.kind().clone();
    let flags = val.flags();
    write_vars(|v| v.set_var(&name, kind, flags).unwrap());
  }
  let _guard = scopeguard::guard(snapshot, |snap| {
    write_vars(|v| *v = snap);
  });
  f()
}

pub fn change_dir<P: AsRef<Path>>(dir: P) -> ShResult<()> {
  let dir = dir.as_ref();
  let dir_raw = &dir.display().to_string();
  let pre_cd = read_logic(|l| l.get_autocmds(AutoCmdKind::PreChangeDir));
  let post_cd = read_logic(|l| l.get_autocmds(AutoCmdKind::PostChangeDir));

  let current_dir = env::current_dir()?.display().to_string();
  with_vars(
    [
      ("_NEW_DIR".into(), dir_raw.as_str()),
      ("_OLD_DIR".into(), current_dir.as_str()),
    ],
    || {
      for cmd in pre_cd {
        let AutoCmd {
          command,
          kind: _,
          pattern,
        } = cmd;
        if let Some(pat) = pattern
          && !pat.is_match(dir_raw)
        {
          continue;
        }

        if let Err(e) = exec_input(
          command.clone(),
          None,
          false,
          Some("autocmd (pre-changedir)".to_string()),
        ) {
          e.print_error();
        };
      }
    },
  );

  env::set_current_dir(dir)?;

  with_vars(
    [
      ("_NEW_DIR".into(), dir_raw.as_str()),
      ("_OLD_DIR".into(), current_dir.as_str()),
    ],
    || {
      for cmd in post_cd {
        let AutoCmd {
          command,
          kind: _,
          pattern,
        } = cmd;
        if let Some(pat) = pattern
          && !pat.is_match(dir_raw)
        {
          continue;
        }

        if let Err(e) = exec_input(
          command.clone(),
          None,
          false,
          Some("autocmd (post-changedir)".to_string()),
        ) {
          e.print_error();
        };
      }
    },
  );

  Ok(())
}

pub fn get_separator() -> String {
  env::var("IFS")
    .unwrap_or(String::from(" "))
    .chars()
    .next()
    .unwrap()
    .to_string()
}

pub fn get_status() -> i32 {
  read_vars(|v| v.get_param(ShellParam::Status))
    .parse::<i32>()
    .unwrap()
}
pub fn set_status(code: i32) {
  write_vars(|v| v.set_param(ShellParam::Status, &code.to_string()))
}

pub fn source_runtime_file(name: &str, env_var_name: Option<&str>) -> ShResult<()> {
  let etc_path = PathBuf::from(format!("/etc/shed/{name}"));
  if etc_path.is_file()
    && let Err(e) = source_file(etc_path)
  {
    e.print_error();
  }

  let path = if let Some(name) = env_var_name
    && let Ok(path) = env::var(name)
  {
    PathBuf::from(&path)
  } else if let Some(home) = get_home() {
    home.join(".{name}")
  } else {
    return Err(sherr!(InternalErr, "could not determine home path",));
  };
  if !path.is_file() {
    return Ok(());
  }
  source_file(path)
}

pub fn source_rc() -> ShResult<()> {
  source_runtime_file("shedrc", Some("SHED_RC"))
}

pub fn source_login() -> ShResult<()> {
  source_runtime_file("shed_profile", Some("SHED_PROFILE"))
}

pub fn source_env() -> ShResult<()> {
  source_runtime_file("shedenv", Some("SHED_ENV"))
}

pub fn source_file(path: PathBuf) -> ShResult<()> {
  let source_name = path.to_string_lossy().to_string();
  let mut file = OpenOptions::new().read(true).open(path)?;

  let mut buf = String::new();
  file.read_to_string(&mut buf)?;
  exec_input(buf, None, false, Some(source_name))?;
  Ok(())
}

#[track_caller]
pub fn get_home_unchecked() -> PathBuf {
  if let Some(home) = get_home() {
    home
  } else {
    let caller = std::panic::Location::caller();
    panic!(
      "get_home_unchecked: could not determine home directory (called from {}:{})",
      caller.file(),
      caller.line()
    )
  }
}

#[track_caller]
pub fn get_home_str_unchecked() -> String {
  if let Some(home) = get_home() {
    home.to_string_lossy().to_string()
  } else {
    let caller = std::panic::Location::caller();
    panic!(
      "get_home_str_unchecked: could not determine home directory (called from {}:{})",
      caller.file(),
      caller.line()
    )
  }
}

pub fn get_home() -> Option<PathBuf> {
  env::var("HOME")
    .ok()
    .map(PathBuf::from)
    .or_else(|| User::from_uid(getuid()).ok().flatten().map(|u| u.dir))
}

pub fn get_home_str() -> Option<String> {
  get_home().map(|h| h.to_string_lossy().to_string())
}
