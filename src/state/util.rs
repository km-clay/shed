use super::*;

use std::{
  collections::{HashMap, VecDeque},
  rc::Rc,
  sync::atomic::Ordering,
};

use nix::unistd::{User, getuid};
use rusqlite::Connection;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
  jobs::Job,
  match_loop,
  parse::{
    execute::exec_nonint,
    lex::{LexFlags, LexStream},
  },
  prelude::*,
  sherr,
  shopt::ShOpts,
  util::{AutoCmdVecUtils, error::ShResult},
};

thread_local! {
  static SHED: Shed = Shed::new();
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
  let expanded = LexStream::new(idx_raw.into(), LexFlags::empty())
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

/*
 * the functions below are some of the most important in the entire codebase
 * it's very important to understand these if you want to get anything done around here.
 *
 * Each one accesses a different part of the shared state (the "Shed" struct),
 * and they take a closure that operates on that part of the state.
 *
 * The main footgun associated with using these is re-entrancy.
 * For instance, If you call write_vars() in a place that can be accessed
 * by write_vars(), (e.g. in the Var table), the shell will crash with a borrow error.
 * Let's not do that!
 *
 * With these, we can access shell state anywhere without threading a state object through every function.
 * However, we must be mindful of what the callstack looks like when we call them, to avoid re-entrancy issues.
 */

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

#[track_caller]
pub fn with_term<T, F: FnOnce(&mut Terminal) -> T>(f: F) -> T {
  let caller = std::panic::Location::caller();
  SHED.with(|shed| {
    let mut term = shed
      .terminal
      .try_borrow_mut()
      .unwrap_or_else(|_| panic!("with_term: RefCell already borrowed (called from {caller})"));
    f(&mut term)
  })
}

#[cfg(test)]
pub fn save_state() {
  SHED.with(|shed| shed.save())
}

#[cfg(test)]
pub fn restore_state() {
  SHED.with(|shed| shed.restore())
}

/// Query the SQLite database.
///
/// Takes a function that returns ShResult<T>, and returns ShResult<Option<T>>.
/// The option is necessary because `Shed.db_conn` can be None. This happens
/// in non-interactive cases, or cases where the database cannot be opened.
///
/// The returns look basically like this:
/// * Ok(None) means "there's no database connection"
/// * Err(e) is your function's ShErr
/// * Ok(Some(T)) means the connection exists and your function succeeded.
pub fn query_db<T, F: FnOnce(Arc<Connection>) -> ShResult<T>>(f: F) -> ShResult<Option<T>> {
  SHED.with(|shed| {
    let Some(Some(conn)) = shed.db_conn.get() else {
      return Ok(None);
    };

    f(Arc::clone(conn)).map(Some)
  })
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
      ("NEW_DIR".into(), dir_raw.as_str()),
      ("OLD_DIR".into(), current_dir.as_str()),
    ],
    || {
      pre_cd.exec();
    },
  );

  env::set_current_dir(dir)?;

  let new_dir_resolved = env::current_dir()?.display().to_string();
  write_vars(|v| {
    v.set_var(
      "OLDPWD",
      VarKind::Str(current_dir.clone()),
      VarFlags::EXPORT,
    )
  })?;
  write_vars(|v| v.set_var("PWD", VarKind::Str(new_dir_resolved), VarFlags::EXPORT))?;

  with_vars(
    [
      ("NEW_DIR".into(), dir_raw.as_str()),
      ("OLD_DIR".into(), current_dir.as_str()),
    ],
    || {
      post_cd.exec();
    },
  );

  Ok(())
}

pub fn get_comp_wordbreaks() -> String {
  env::var("COMP_WORDBREAKS")
    .unwrap_or_else(|_| String::from("\"'><;|=&(:"))
}

/// Get the first char of IFS
///
/// Used mainly for joining strings
pub fn get_separator() -> String {
  env::var("IFS")
    .unwrap_or(String::from(" "))
    .graphemes(true)
    .next()
    .unwrap()
    .to_string()
}

/// Get the entire IFS variable
///
/// Used mainly for splitting strings
pub fn get_separators() -> String {
  env::var("IFS").unwrap_or(String::from(" \t\n"))
}

pub fn get_time_fmt() -> String {
  env::var("TIMEFMT").unwrap_or_else(|_| String::from("\nreal\t%*E\nuser\t%*U\nsys\t%*S"))
}

pub fn get_status() -> i32 {
  super::STATUS_CODE.load(Ordering::Relaxed)
}
pub fn set_status(code: i32) {
  super::STATUS_CODE.store(code, Ordering::Relaxed);
}
pub fn set_status_from_bool(code: bool) {
  super::STATUS_CODE.store(if code { 0 } else { 1 }, Ordering::Relaxed);
}
pub fn set_pipe_status(stats: &[WtStat]) -> ShResult<()> {
  if let Some(pipe_status) = Job::pipe_status(stats) {
    let pipe_status = pipe_status
      .into_iter()
      .map(|s| s.to_string())
      .collect::<VecDeque<String>>();

    write_vars(|v| v.set_var("PIPESTATUS", VarKind::Arr(pipe_status), VarFlags::NONE))?;
  }
  Ok(())
}

pub fn lookup_cmd(cmd: &str) -> Option<PathBuf> {
  if read_shopts(|o| o.set.hashall) {
    which_util(cmd)
      .filter(|u| matches!(u.kind(), UtilKind::Command(_) | UtilKind::File(_)))
      .map(|u| {
        let (UtilKind::Command(path) | UtilKind::File(path)) = u.kind() else {
          unreachable!()
        };
        path.clone()
      })
  } else {
    MetaTab::get_exec_files_in_cwd()
      .into_iter()
      .chain(MetaTab::get_cmds_in_path())
      .find(|u| u.name() == cmd)
      .and_then(|u| match u.kind() {
        UtilKind::Command(path) | UtilKind::File(path) => Some(path.clone()),
        _ => None,
      })
  }
}

pub fn which_util(name: &str) -> Option<Rc<Utility>> {
  // Check in shell resolution order: alias > function > builtin > cached command > PATH
  if read_logic(|l| l.get_alias(name).is_some()) {
    return Some(Rc::new(Utility::alias(name.to_string())));
  }
  if read_logic(|l| l.get_func(name).is_some()) {
    return Some(Rc::new(Utility::function(name.to_string())));
  }
  if crate::builtin::lookup_builtin(name).is_some() {
    return Some(Rc::new(Utility::builtin(name.to_string())));
  }
  // For external commands, check cache first, then scan PATH
  read_meta(|m| m.get_cached_cmd(name)).or_else(|| {
    MetaTab::get_cmds_in_path()
      .into_iter()
      .chain(MetaTab::get_exec_files_in_cwd())
      .find(|u| u.name() == name)
      .inspect(|u| write_meta(|m| m.cache_util(Rc::clone(u))))
  })
}

pub fn try_hash() {
  if read_shopts(|o| o.set.hashall) {
    write_meta(|m| m.try_rehash_utils());
  } else {
    write_meta(|m| m.clear_cache());
  }
}

pub fn runtime_files() -> Vec<PathBuf> {
  let mut files = vec![];

  if let Some(home) = get_home() {
    files.push(home.join(".shedrc"));
    files.push(home.join(".shed_profile"));
    files.push(home.join(".shedenv"));
  }

  if let Ok(path) = env::var("SHED_RC") {
    files.push(PathBuf::from(path));
  }
  if let Ok(path) = env::var("SHED_PROFILE") {
    files.push(PathBuf::from(path));
  }
  if let Ok(path) = env::var("SHED_ENV") {
    files.push(PathBuf::from(path));
  }

  files.push(PathBuf::from("/etc/shed/shedrc"));
  files.push(PathBuf::from("/etc/shed/shed_profile"));
  files.push(PathBuf::from("/etc/shed/shedenv"));

  files
}

pub fn rc_file_path() -> Option<PathBuf> {
  if let Ok(path) = env::var("SHED_RC") {
    Some(PathBuf::from(path))
  } else {
    get_home().map(|home| home.join(".shedrc"))
  }
}

pub fn generate_default_rc() -> ShResult<Option<PathBuf>> {
  let rc_path =
    rc_file_path().ok_or_else(|| sherr!(InternalErr, "could not determine rc file path",))?;
  if rc_path.exists() {
    return Ok(None);
  }
  let mut rc_file = OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)
    .open(&rc_path)?;

  let mut lines: Vec<String> = vec![
    "# --- Shed Runtime Commands ---".into(),
    "# This file was automatically generated by shed.".into(),
    "# These are sane defaults for many shed-specific options and features.".into(),
    "# Edit this file to customize, or use it as a reference.".into(),
    "# Refer to the 'help' builtin for information on specific shed features.".into(),
    String::new(),
  ];
  lines.extend(ShOpts::generate_default_rc());
  lines.push(String::new());

  let static_lines = [
    "# -- Tab Completion --",
    "# The 'complete' builtin tells shed how to complete arguments for a command.",
    "complete -d cd     # Only complete directory names",
    "complete -d pushd  # Only complete directory names",
    "complete -d popd   # Only complete directory names",
    "complete -j fg     # Only complete job names",
    "complete -j bg     # Only complete job names",
    "complete -f source # Only complete file names",
    "complete -a alias  # Only complete alias names",
    "",
    "# -- Autocmds --",
    "# Register commands to run on shell lifecycle events.",
    "# Type 'help autocmd' on the prompt for more details.",
    "autocmd 'on-exit' 'echo exit 1>&2' # Print 'exit' when the shell exits",
    "",
    "# -- Keybinds --",
    "# Register commands to run on key presses while on the prompt.",
    "# Type 'help keymap' on the prompt for more advanced usage.",
    "keymap -ie '<C-L>' '<CMD>clear<CR>' # Ctrl+L clears the screen (insert + emacs mode)",
  ];

  for line in &lines {
    writeln!(rc_file, "{}", line)?;
  }
  for line in static_lines {
    writeln!(rc_file, "{}", line)?;
  }

  Ok(Some(rc_path))
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
    home.join(format!(".{name}"))
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
  exec_nonint(buf, None, Some(source_name.into()))?;
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

/// Get a clone of the shared database connection, if available.
pub fn get_db_conn() -> Option<Arc<Connection>> {
  SHED.with(|shed| shed.db_conn.get().cloned().flatten())
}

/// Initialize the shared database connection on the `Shed` struct.
pub fn init_db_conn() {
  SHED.with(|shed| match open_db_conn().ok() {
    Some(conn) => {
      let Ok(_) = conn.execute_batch("PRAGMA journal_mode=WAL") else {
        return;
      };
      let Ok(_) = conn.execute_batch("PRAGMA case_sensitive_like = 1") else {
        return;
      };
      let _ = shed.db_conn.set(Some(conn.into()));
    }
    None => {
      let _ = shed.db_conn.set(None);
    }
  })
}

pub fn open_db_conn() -> ShResult<Connection> {
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

  Ok(Connection::open(&db_path)?)
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
