use crate::{
  HashMap,
  state::{logic::ShFunc, vars::VarStr},
  util::{self, ShErrKind, VarStrDisplay},
  varstr,
};

use super::{Shed, try_var};

use std::{
  fs::OpenOptions,
  io::{Read, Write},
  path::{Path, PathBuf},
  rc::Rc,
  sync::{Arc, Mutex, RwLock},
};

use nix::{
  libc,
  unistd::{User, getuid},
};
use rusqlite::Connection;
use scopeguard::defer;
use unicode_segmentation::UnicodeSegmentation;

use super::{
  ShResult, autocmd,
  eval::{
    execute::exec_nonint,
    lex::{LexFlags, LexStream},
  },
  match_loop,
  meta::{MetaTab, Utility},
  sherr,
  shopt::ShoptSource,
  var,
  vars::{ArrIndex, Var, VarFlags, VarKind},
};

/// Parse `arr[idx]` into (name, `raw_index_expr`). Pure parsing, no expansion.
pub fn parse_arr_bracket(var_name: &str) -> Option<(VarStr, VarStr)> {
  if !var_name.contains('[') {
    return None;
  }
  let mut chars = var_name.chars();
  let mut name = util::scratch_buf();
  let mut idx_raw = util::scratch_buf();
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
    Some((name.into(), idx_raw.into()))
  }
}

/// Expand the raw index expression and parse it into an `ArrIndex`.
pub fn expand_arr_index(idx_raw: &str, allow_side_effects: bool) -> ShResult<ArrIndex> {
  let expanded = LexStream::new(idx_raw.into(), LexFlags::empty())
    .map(|tk| tk.and_then(|tk| tk.expand()).map(|tk| tk.get_words()))
    .try_fold(vec![], |mut acc, wrds| {
      match wrds {
        Ok(wrds) => acc.extend_from_slice(&wrds),
        Err(e) => return Err(e),
      }
      Ok(acc)
    })?
    .into_iter()
    .next()
    .ok_or_else(|| sherr!(ParseErr, "Empty array index"))?;

  ArrIndex::parse(&expanded, allow_side_effects)
    .map_err(|_| sherr!(ParseErr, "Invalid array index: {}", expanded,))
}

/*
 * the functions below are some of the most important in the entire codebase
 * it's very important to understand these if you want to get anything done around here.
 *
 * Each one accesses a different part of the shared state (the "Shed" struct),
 * and they take a closure that operates on that part of the state.
 *
 *
 * With these, we can access shell state anywhere without threading a state object through every function.
 * However, we must be mindful of what the callstack looks like when we call them, to avoid re-entrancy issues.
 */

/// Query the `SQLite` database.
///
/// Takes a function that returns `ShResult<T>`, and returns `ShResult<Option<T>>`.
/// The option is necessary because `Shed.db_conn` can be None. This happens
/// in non-interactive cases, or cases where the database cannot be opened.
///
/// The returns look basically like this:
/// * Ok(None) means "there's no database connection"
/// * Err(e) is your function's `ShErr`
/// * Ok(Some(T)) means the connection exists and your function succeeded.
pub fn query_db<T, F: FnOnce(&Connection) -> ShResult<T>>(f: F) -> ShResult<Option<T>> {
  let Some(conn) = get_db_conn() else {
    return Ok(None);
  };
  let conn = conn.lock().unwrap_or_else(|e| e.into_inner());
  f(&conn).map(Some)
}

pub fn with_vars<F, H, V, T>(vars: H, f: F) -> T
where
  F: FnOnce() -> T,
  H: IntoIterator<Item = (VarStr, V)>,
  V: Into<Var>,
{
  let vars: HashMap<VarStr, V> = vars.into_iter().collect();
  let restores: Vec<(VarStr, Option<(VarKind, VarFlags)>)> = vars
    .keys()
    .map(|k| {
      let prev = Shed::vars(|v| {
        v.try_get_var_meta(k)
          .map(|var| (var.kind().clone(), var.flags()))
      });
      (k.clone(), prev)
    })
    .collect();

  for (name, val) in vars {
    let val = val.into();
    Shed::vars_mut(|v| v.set_var(&name, val.kind().clone(), val.flags()).unwrap());
  }

  let _guard = scopeguard::guard(restores, |restores| {
    Shed::vars_mut(|v| {
      for (name, prev) in restores {
        match prev {
          Some((kind, flags)) => {
            v.set_var(&name, kind, flags).ok();
          }
          None => {
            v.unset_var(&name).ok();
          }
        }
      }
    });
  });
  f()
}

pub fn change_dir<P: AsRef<Path>>(dir: P) -> ShResult<()> {
  change_dir_with_pwd(dir, None)
}

pub fn change_dir_with_pwd<P: AsRef<Path>>(dir: P, logical_pwd: Option<String>) -> ShResult<()> {
  let dir = dir.as_ref();
  let dir_raw = &dir.display().to_string();
  defer!(super::autocmd!(PostChangeDir));

  let current_dir = try_var!("PWD")
    .or_else(|| {
      std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into())
    })
    .unwrap_or_default();

  with_vars(
    [
      ("NEW_DIR".into(), dir_raw.as_str()),
      ("OLD_DIR".into(), current_dir.as_str()),
    ],
    || autocmd!(PreChangeDir),
  );

  std::env::set_current_dir(dir)?;

  let new_pwd = logical_pwd.unwrap_or_else(|| {
    std::env::current_dir()
      .ok()
      .map_or_else(|| dir_raw.clone(), |p| p.display().to_string())
  });

  Shed::vars_mut(|v| {
    v.set_var("OLDPWD", VarKind::Str(current_dir), VarFlags::EXPORT)?;
    v.set_var("PWD", VarKind::string(new_pwd), VarFlags::EXPORT)
  })?;

  Ok(())
}

/// Lexically normalize a path: drop `.` components and resolve `..` against
pub fn lex_normalize_path(path: &Path) -> PathBuf {
  use std::path::Component;
  let mut out: Vec<Component> = Vec::new();
  for comp in path.components() {
    match comp {
      Component::CurDir => {}
      Component::ParentDir => match out.last() {
        Some(Component::Normal(_)) => {
          out.pop();
        }
        Some(Component::RootDir | Component::Prefix(_)) => {}
        Some(Component::ParentDir) | None => out.push(comp),
        Some(Component::CurDir) => unreachable!(),
      },
      _ => out.push(comp),
    }
  }
  if out.is_empty() {
    PathBuf::from(".")
  } else {
    out.iter().collect()
  }
}

pub fn get_comp_wordbreaks() -> VarStr {
  try_var!("COMP_WORDBREAKS").unwrap_or_else(|| "\"'><;|=&(:".into())
}

/// Get the first char of IFS
///
/// Used mainly for joining strings
pub fn get_separator() -> VarStr {
  let separators = get_separators();
  separators.graphemes(true).next().unwrap_or_default().into()
}

/// Get the entire IFS variable
///
/// Used mainly for splitting strings
pub fn get_separators() -> VarStr {
  try_var!("IFS").unwrap_or_else(|| " \t\n".into())
}

pub fn get_time_fmt() -> VarStr {
  try_var!("TIMEFMT").unwrap_or_else(|| "\nreal\t%*E\nuser\t%*U\nsys\t%*S".into())
}

pub fn lookup_cmd(cmd: &str) -> Option<PathBuf> {
  if cmd.contains('/') {
    let p = PathBuf::from(cmd);
    return p.is_file().then_some(p);
  }
  let path = Shed::meta_mut(|m| {
    m.invalidate_path_cache_if_stale();
    if let Some(p) = m.lookup_cached_cmd(cmd) {
      return Some(p.to_path_buf());
    }
    None
  });
  if let Some(p) = path {
    return Some(p);
  }
  let path_env = var!("PATH");
  let resolved = crate::util::resolve_in_path(&path_env, cmd)?;
  Shed::meta_mut(|m| m.cache_cmd(cmd.to_string(), resolved.clone()));
  Some(resolved)
}

pub fn which_util(name: &str) -> Option<Rc<Utility>> {
  if Shed::logic(|l| l.get_alias(name).is_some()) {
    return Some(Rc::new(Utility::alias(name.into())));
  }
  if Shed::logic(|l| l.has_command_func(name)) {
    return Some(Rc::new(Utility::function(name.into())));
  }
  if crate::builtin::lookup_builtin(name).is_some() {
    return Some(Rc::new(Utility::builtin(name.into())));
  }
  let cached = Shed::meta_mut(|m| {
    m.try_rehash_path_cache();
    m.lookup_cached_cmd(name)
      .map(|p| Rc::new(Utility::command(name.into(), p.to_path_buf())))
  });
  cached.or_else(|| {
    // Last resort: an executable file living in $PWD that isn't in $PATH.
    MetaTab::get_exec_files_in_cwd()
      .into_iter()
      .find(|u| u.name() == name)
  })
}

pub fn try_hash() {
  if Shed::shopts(|o| o.set.hashall) {
    Shed::meta_mut(MetaTab::try_rehash_path_cache);
  }
}

pub fn rc_file_path() -> Option<PathBuf> {
  if let Some(p) = try_var!("SHED_RC") {
    return Some(PathBuf::from(p));
  }
  let xdg = xdg_config_home().map(|c| c.join("shed").join("shedrc"));
  let home = get_home().map(|h| h.join(".shedrc"));

  xdg
    .as_ref()
    .filter(|p| p.is_file())
    .cloned()
    .or_else(|| home.as_ref().filter(|p| p.is_file()).cloned())
    .or(xdg)
}

/// Config knobs for `compose_rc`. `Default` reproduces the
/// `genrc` builtin's no-flag behavior: live shopt values, live
/// autocmds + keymaps, header + per-section comments included.
#[derive(Clone, Debug)]
#[expect(clippy::struct_excessive_bools)]
pub struct GenRcConfig {
  pub source: ShoptSource,
  pub include_comments: bool,
  pub include_shopts: bool,
  pub include_aliases: bool,
  pub include_functions: bool,
  pub include_completions: bool,
  pub include_autocmds: bool,
  pub include_keymaps: bool,
}

impl Default for GenRcConfig {
  fn default() -> Self {
    Self {
      source: ShoptSource::Current,
      include_comments: true,
      include_shopts: true,
      include_aliases: true,
      include_functions: true,
      include_completions: true,
      include_autocmds: true,
      include_keymaps: true,
    }
  }
}

impl GenRcConfig {
  pub fn first_run() -> Self {
    Self {
      source: ShoptSource::Defaults,
      ..Self::default()
    }
  }
}

/// Format an rc entry with an aligned trailing doc comment when `comments` is on.
fn rc_entry(entry: &str, doc: &str, comments: bool) -> VarStr {
  if comments {
    varstr!("{entry:<50} # {doc}")
  } else {
    entry.into()
  }
}

/// Live `alias` definitions, sorted by name.
fn live_aliases() -> Vec<VarStr> {
  let mut aliases: Vec<(VarStr, VarStr)> = Shed::logic(|l| {
    l.aliases()
      .iter()
      .map(|(name, a)| (name.into(), a.body()))
      .collect()
  });
  aliases.sort_by(|a, b| a.0.cmp(&b.0));
  aliases
    .into_iter()
    .map(|(name, body)| varstr!("alias {}", super::vars::display_as_var(name, body)))
    .collect()
}

/// Live user function definitions (verbatim source), sorted by name.
fn live_funcs() -> Vec<VarStr> {
  let mut funcs: Vec<(VarStr, VarStr)> = Shed::logic(|l| {
    l.funcs()
      .iter()
      .filter_map(|(name, f)| match f {
        ShFunc::Defined { source, .. } => Some((name.into(), source.as_str().into())),
        ShFunc::Autoload(_) => None,
      })
      .collect()
  });
  funcs.sort_by(|a, b| a.0.cmp(&b.0));
  funcs.into_iter().map(|(_, src)| src).collect()
}

/// Live `complete` registrations (verbatim source), sorted by command.
fn live_completions() -> Vec<VarStr> {
  let mut specs: Vec<(VarStr, VarStr)> = Shed::meta(|m| {
    m.comp_specs()
      .iter()
      .map(|(cmd, spec)| (cmd.clone(), spec.source().into()))
      .collect()
  });
  specs.sort_by(|a, b| a.0.cmp(&b.0));
  specs.into_iter().map(|(_, src)| src).collect()
}

/// Live `autocmd` registrations, in registration order.
fn live_autocmds() -> Vec<VarStr> {
  Shed::logic(|l| l.iter_autocmds().map(|cmd| cmd.to_var_str()).collect())
}

/// Live `keymap` registrations, in registration order.
fn live_keymaps() -> Vec<VarStr> {
  Shed::logic(|l| l.keymaps().iter().map(|km| km.to_var_str()).collect())
}

/// Render an rc file to a `Vec<VarStr>` per `config`. Pure — no I/O, no
/// side effects on `Shed` state. Caller decides where the lines go.
pub fn compose_rc(config: &GenRcConfig) -> Vec<VarStr> {
  use ShoptSource::{Current, Defaults};

  let comments = config.include_comments;
  let mut lines: Vec<VarStr> = vec![];

  // Append a section (header comments + content + trailing blank) only when it
  // is enabled and has content, so empty sections aren't rendered as bare headers.
  let section = |lines: &mut Vec<VarStr>, include: bool, header: &[&str], content: Vec<VarStr>| {
    if !include || content.is_empty() {
      return;
    }
    if comments {
      lines.extend(header.iter().map(|h| VarStr::from(*h)));
    }
    lines.extend(content);
    lines.push(VarStr::new());
  };

  // Content for a user-defined section: live entries, or nothing for the
  // factory defaults (these have no built-in entries).
  let user_section = |live: fn() -> Vec<VarStr>| match config.source {
    Current => live(),
    Defaults => vec![],
  };

  // Content for a section that ships built-in defaults: the hardcoded entries
  // for the factory rc, or the live registrations when regenerating.
  let default_section = |defaults: &[(&str, &str)], live: fn() -> Vec<VarStr>| match config.source {
    Defaults => defaults
      .iter()
      .map(|(e, d)| rc_entry(e, d, comments))
      .collect(),
    Current => live(),
  };

  // Preamble
  if comments {
    lines.push("# --- Shed Runtime Commands ---".into());
    lines.push("# This file was automatically generated by shed.".into());
    lines.push(match config.source {
      Defaults => "# These are sane defaults for many shed-specific options and features.".into(),
      Current => "# Reflects the live shell configuration at generation time.".into(),
    });
    lines.push("# Edit this file to customize, or use it as a reference.".into());
    lines.push("# Refer to the 'help' builtin for information on specific shed features.".into());
    lines.push(VarStr::new());
  }

  // Shell options
  if config.include_shopts {
    if comments {
      lines.push("# -- Shell Options --".into());
      lines.push(VarStr::new());
    }
    let mut current_group: Option<&'static str> = None;
    for (_key, group, entry, doc) in Shed::shopts(|o| o.rc_entries(config.source)) {
      if comments && Some(group) != current_group {
        if current_group.is_some() {
          lines.push(VarStr::new());
        }
        lines.push(varstr!("# - {group} -"));
        current_group = Some(group);
      }
      lines.push(match (doc, comments) {
        (Some(d), true) => varstr!("{entry:<50} # {d}"),
        _ => entry,
      });
    }
    lines.push(VarStr::new());
  }

  // Remaining sections
  section(
    &mut lines,
    config.include_aliases,
    &[
      "# -- Aliases --",
      "# Word-level substitutions applied at the start of a command.",
      "# Type 'help alias' on the prompt for more details.",
    ],
    user_section(live_aliases),
  );

  section(
    &mut lines,
    config.include_functions,
    &[
      "# -- Functions --",
      "# Each function is emitted verbatim from its original definition.",
    ],
    user_section(live_funcs),
  );

  section(
    &mut lines,
    config.include_completions,
    &[
      "# -- Tab Completion --",
      "# The 'complete' builtin tells shed how to complete arguments for a command.",
    ],
    default_section(
      &[
        ("complete -d cd", "Only complete directory names"),
        ("complete -d pushd", "Only complete directory names"),
        ("complete -d popd", "Only complete directory names"),
        ("complete -j fg", "Only complete job names"),
        ("complete -j bg", "Only complete job names"),
        ("complete -f source", "Only complete file names"),
        ("complete -a alias", "Only complete alias names"),
      ],
      live_completions,
    ),
  );

  section(
    &mut lines,
    config.include_autocmds,
    &[
      "# -- Autocmds --",
      "# Register commands to run on shell lifecycle events.",
      "# Type 'help autocmd' on the prompt for more details.",
    ],
    default_section(
      &[(
        "autocmd 'on-exit' 'echo exit 1>&2'",
        "Print 'exit' when the shell exits",
      )],
      live_autocmds,
    ),
  );

  section(
    &mut lines,
    config.include_keymaps,
    &[
      "# -- Keybinds --",
      "# Register commands to run on key presses while on the prompt.",
      "# Type 'help keymap' on the prompt for more advanced usage.",
    ],
    default_section(
      &[(
        "keymap -ie '<C-L>' '<CMD>clear<CR>'",
        "Ctrl+L clears the screen (insert + emacs mode)",
      )],
      live_keymaps,
    ),
  );

  // Trim trailing blank lines so the file doesn't end with extra padding.
  while lines.last().is_some_and(|s| s.as_str().is_empty()) {
    lines.pop();
  }
  lines
}

pub fn generate_default_rc() -> ShResult<Option<PathBuf>> {
  let rc_path =
    rc_file_path().ok_or_else(|| sherr!(InternalErr, "could not determine rc file path",))?;
  if rc_path.exists() {
    return Ok(None);
  }
  if let Some(parent) = rc_path.parent() {
    std::fs::create_dir_all(parent)?;
  }

  log::info!("Generating default rc file at {}", rc_path.display());
  let mut rc_file = OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)
    .open(&rc_path)?;

  for line in compose_rc(&GenRcConfig::first_run()) {
    writeln!(rc_file, "{line}")?;
  }

  Ok(Some(rc_path))
}

pub fn source_runtime_file(name: &str, env_var_name: Option<&str>) -> ShResult<()> {
  let etc_path = PathBuf::from(varstr!("/etc/shed/{name}"));
  if etc_path.is_file()
    && let Err(e) = source_file(etc_path)
  {
    e.print_error();
  }

  let user_path = if let Some(n) = env_var_name
    && let Some(p) = try_var!(n)
  {
    Some(PathBuf::from(p))
  } else {
    xdg_config_home()
      .map(|c| c.join("shed").join(name))
      .filter(|p| p.is_file())
      .or_else(|| {
        get_home()
          .map(|h| h.join(format!(".{name}")))
          .filter(|p| p.is_file())
      })
  };

  match user_path {
    Some(path) if path.is_file() => source_file(path),
    _ => Ok(()),
  }
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
  let source_display = display_path_normalized(source_name);
  let mut file = OpenOptions::new().read(true).open(path)?;

  let mut buf = String::new();
  file.read_to_string(&mut buf)?;

  // sourced files behave like functions
  // 'return' is valid inside of them, and we also track recursion depth
  let _guard = Shed::meta_mut(MetaTab::enter_func);

  match exec_nonint(buf.into(), Some(source_display.into())) {
    Ok(()) => Ok(()),
    Err(e) => match e.kind() {
      ShErrKind::FuncReturn(code) => {
        Shed::set_status(*code);
        Ok(())
      }
      _ => Err(e),
    },
  }
}

pub fn display_path<P: AsRef<Path>>(path: P) -> String {
  let s = path.as_ref().to_string_lossy().into_owned();
  if let Some(home) = get_home_str()
    && !home.is_empty()
    && let Some(rest) = s.strip_prefix(&*home)
  {
    format!("~{rest}")
  } else {
    s
  }
}

pub fn display_path_normalized<P: AsRef<Path>>(path: P) -> String {
  display_path(lex_normalize_path(path.as_ref()))
}

pub fn set_ver_info() -> ShResult<()> {
  let version = env!("CARGO_PKG_VERSION");
  let mut semver = version.split('.');
  let major = semver.next().unwrap_or("0");
  let minor = semver.next().unwrap_or("0");
  let patch = semver.next().unwrap_or("0");
  let arch = std::env::consts::ARCH;
  let os = std::env::consts::OS;
  let ver_info = vec![
    ("major".into(), major.into()),
    ("minor".into(), minor.into()),
    ("patch".into(), patch.into()),
    ("arch".into(), arch.into()),
    ("os".into(), os.into()),
  ];

  Shed::vars_mut(|v| {
    v.set_var(
      "SHED_VERSION",
      VarKind::Str(version.into()),
      VarFlags::EXPORT,
    )?;
    v.set_var(
      "SHED_VER_INFO",
      VarKind::AssocArr(ver_info),
      VarFlags::empty(),
    )
  })?;

  Ok(())
}

pub fn set_sh_lvl() -> ShResult<()> {
  // Increment SHLVL, or set to 1 if not present or invalid.
  // This var represents how many nested shell instances we're in
  if let Some(var) = try_var!("SHLVL")
    && let Ok(lvl) = var.parse::<u32>()
  {
    Shed::vars_mut(|v| {
      v.set_var(
        "SHLVL",
        VarKind::string((lvl + 1).to_string()),
        VarFlags::EXPORT,
      )
    })?;
  } else {
    Shed::vars_mut(|v| v.set_var("SHLVL", VarKind::Str("1".into()), VarFlags::EXPORT))?;
  }

  Ok(())
}

/// Get a clone of the shared database connection, if available.
/// The process-wide history database connection.
static DB_CONN: RwLock<Option<Arc<Mutex<Connection>>>> = RwLock::new(None);

fn open_shared_conn() -> Option<Arc<Mutex<Connection>>> {
  crate::procio::do_something_that_opens_fds_that_we_cant_access_hack(
    crate::procio::MIN_INTERNAL_FD,
    || {
      let conn = open_db_conn().ok()?;
      conn.execute_batch("PRAGMA journal_mode=WAL").ok()?;
      // Wait on contention instead of erroring out immediately; matters
      // for the cross-process case (multiple shed sessions share this db).
      conn.busy_timeout(std::time::Duration::from_secs(5)).ok()?;
      conn.execute_batch("PRAGMA case_sensitive_like = 1").ok()?;
      Some(Arc::new(Mutex::new(conn)))
    },
  )
}

pub fn get_db_conn() -> Option<Arc<Mutex<Connection>>> {
  if let Some(conn) = DB_CONN.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
    return Some(conn.clone());
  }
  let mut guard = DB_CONN.write().unwrap_or_else(|e| e.into_inner());
  if guard.is_none() {
    *guard = open_shared_conn();
  }
  guard.clone()
}

#[cfg(test)]
pub fn init_test_db_conn() {
  *DB_CONN.write().unwrap_or_else(|e| e.into_inner()) = Connection::open_in_memory()
    .ok()
    .map(|c| Arc::new(Mutex::new(c)));
}

pub fn open_db_conn() -> ShResult<Connection> {
  let db_path = if let Some(var) = try_var!("SHED_HISTDB") {
    var
  } else {
    let home = try_var!("HOME").unwrap_or_else(|| ".".into());
    dirs::data_dir().map_or_else(
      || format!("{home}/.local/share/shed/shed_hist.db").into(),
      |p| p.to_string_lossy().into(),
    )
  };

  let db_path = PathBuf::from(db_path);
  if let Some(parent) = db_path.parent() {
    std::fs::create_dir_all(parent)?;
  }

  Ok(Connection::open(&db_path)?)
}

#[cfg(target_os = "android")]
pub fn get_default_path() -> Option<String> {
  // Android does not have conf_str or _CS_PATH
  // So we return None here.
  None
}

#[cfg(not(target_os = "android"))]
pub fn get_default_path() -> Option<String> {
  unsafe {
    let needed = libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0);
    if needed == 0 {
      return None;
    }
    let mut buf = vec![0u8; needed];
    let written = libc::confstr(
      libc::_CS_PATH,
      buf.as_mut_ptr().cast::<std::ffi::c_char>(),
      needed,
    );
    if !(1..=needed).contains(&written) {
      return None;
    }

    // check for null byte
    if buf.ends_with(b"\0") {
      buf.truncate(written - 1);
    }
    String::from_utf8(buf).ok()
  }
}

pub fn xdg_runtime_dir() -> PathBuf {
  if let Some(p) = try_var!("XDG_RUNTIME_DIR") {
    return PathBuf::from(p);
  }
  if let Some(p) = try_var!("TMPDIR") {
    return PathBuf::from(p);
  }
  PathBuf::from(format!("/tmp/shed-{}", getuid()))
}

pub fn xdg_config_home() -> Option<PathBuf> {
  try_var!("XDG_CONFIG_HOME")
    .map(PathBuf::from)
    .or_else(|| get_home().map(|home| home.join(".config")))
}

pub fn get_home() -> Option<PathBuf> {
  try_var!("HOME")
    .map(PathBuf::from)
    .or_else(|| User::from_uid(getuid()).ok().flatten().map(|u| u.dir))
}

pub fn get_home_str() -> Option<VarStr> {
  get_home().map(|h| h.to_string_lossy().into())
}

pub fn get_exec_wrappers() -> Vec<VarStr> {
  let mut wrappers = vec![
    "sudo".into(),
    "doas".into(),
    "pkexec".into(),
    "run0".into(),
    "please".into(),
    "gosu".into(),
    "strace".into(),
    "ltrace".into(),
    "ktrace".into(),
    "valgrind".into(),
    "heaptrack".into(),
    "nohup".into(),
    "nice".into(),
    "ionice".into(),
    "chrt".into(),
    "setsid".into(),
    "setpriv".into(),
    "prlimit".into(),
    "unshare".into(),
    "bwrap".into(),
    "firejail".into(),
    "systemd-run".into(),
    "proot".into(),
    "watch".into(),
    "chronic".into(),
    "parallel".into(),
    "stdbuf".into(),
    "hyperfine".into(),
    "command".into(),
    "builtin".into(),
    "env".into(),
    "exec".into(),
    "defer".into(),
  ];

  // lets users define their own exec wrappers for the highlighter if they want
  // for instance, my personal config has a wrapper function called 'invoke'
  let user_wrappers = Shed::vars(|v| v.get_arr_elems("SHED_EXEC_WRAPPERS"));
  wrappers.extend(user_wrappers);

  wrappers
}

#[cfg(test)]
mod xdg_resolver_tests {
  use super::*;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::tests::testutil::TestGuard;

  fn set_var(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name, VarKind::Str(val.into()), VarFlags::EXPORT)
        .unwrap();
    });
  }
  fn unset_var(name: &str) {
    Shed::vars_mut(|v| {
      v.unset_var(name).ok();
    });
  }

  // ─── xdg_config_home ──────────────────────────────────────────────

  #[test]
  fn xdg_config_home_uses_env_var_when_set() {
    let _g = TestGuard::new();
    set_var("XDG_CONFIG_HOME", "/explicit/xdg/config");
    assert_eq!(
      xdg_config_home(),
      Some(PathBuf::from("/explicit/xdg/config"))
    );
  }

  #[test]
  fn xdg_config_home_falls_back_to_home_dot_config() {
    let _g = TestGuard::new();
    unset_var("XDG_CONFIG_HOME");
    set_var("HOME", "/some/home");
    assert_eq!(xdg_config_home(), Some(PathBuf::from("/some/home/.config")));
  }

  // ─── xdg_runtime_dir ──────────────────────────────────────────────

  #[test]
  fn xdg_runtime_dir_prefers_env_var() {
    let _g = TestGuard::new();
    set_var("XDG_RUNTIME_DIR", "/run/user/1000");
    assert_eq!(xdg_runtime_dir(), PathBuf::from("/run/user/1000"));
  }

  #[test]
  fn xdg_runtime_dir_falls_back_to_tmpdir() {
    let _g = TestGuard::new();
    unset_var("XDG_RUNTIME_DIR");
    set_var("TMPDIR", "/custom/tmp");
    assert_eq!(xdg_runtime_dir(), PathBuf::from("/custom/tmp"));
  }

  #[test]
  fn xdg_runtime_dir_falls_back_to_tmp_uid_when_none_set() {
    let _g = TestGuard::new();
    unset_var("XDG_RUNTIME_DIR");
    unset_var("TMPDIR");
    let expected = PathBuf::from(format!("/tmp/shed-{}", getuid()));
    assert_eq!(xdg_runtime_dir(), expected);
  }

  // ─── rc_file_path ─────────────────────────────────────────────────

  #[test]
  fn rc_file_path_shed_rc_env_var_overrides_everything() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("explicit.rc");
    std::fs::write(&path, "").unwrap();
    set_var("SHED_RC", &path.to_string_lossy());

    // Even with XDG file present, SHED_RC wins
    let xdg_dir = tempfile::TempDir::new().unwrap();
    set_var("XDG_CONFIG_HOME", &xdg_dir.path().to_string_lossy());
    std::fs::create_dir_all(xdg_dir.path().join("shed")).unwrap();
    std::fs::write(xdg_dir.path().join("shed").join("shedrc"), "").unwrap();

    assert_eq!(rc_file_path(), Some(path));
  }

  #[test]
  fn rc_file_path_prefers_xdg_over_legacy_when_both_exist() {
    let _g = TestGuard::new();
    unset_var("SHED_RC");
    let home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    set_var("HOME", &home.path().to_string_lossy());
    set_var("XDG_CONFIG_HOME", &xdg.path().to_string_lossy());

    std::fs::write(home.path().join(".shedrc"), "").unwrap();
    std::fs::create_dir_all(xdg.path().join("shed")).unwrap();
    let xdg_rc = xdg.path().join("shed").join("shedrc");
    std::fs::write(&xdg_rc, "").unwrap();

    assert_eq!(rc_file_path(), Some(xdg_rc));
  }

  #[test]
  fn rc_file_path_falls_back_to_legacy_when_xdg_does_not_exist() {
    let _g = TestGuard::new();
    unset_var("SHED_RC");
    let home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    set_var("HOME", &home.path().to_string_lossy());
    set_var("XDG_CONFIG_HOME", &xdg.path().to_string_lossy());

    let legacy = home.path().join(".shedrc");
    std::fs::write(&legacy, "").unwrap();

    assert_eq!(rc_file_path(), Some(legacy));
  }

  #[test]
  fn rc_file_path_returns_xdg_path_for_creation_when_neither_exists() {
    let _g = TestGuard::new();
    unset_var("SHED_RC");
    let home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    set_var("HOME", &home.path().to_string_lossy());
    set_var("XDG_CONFIG_HOME", &xdg.path().to_string_lossy());

    let expected = xdg.path().join("shed").join("shedrc");
    assert_eq!(rc_file_path(), Some(expected));
  }
}

#[cfg(test)]
mod generate_default_rc_tests {
  use super::*;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::tests::testutil::TestGuard;

  fn set_rc_path(p: &std::path::Path) {
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_RC",
        VarKind::string(p.to_string_lossy()),
        VarFlags::empty(),
      )
      .unwrap();
    });
  }

  // ─── creates file when missing ──────────────────────────────────

  #[test]
  fn creates_file_when_not_present() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let rc = dir.path().join("test.shedrc");
    set_rc_path(&rc);
    assert!(!rc.exists());
    let result = generate_default_rc().unwrap();
    assert_eq!(result, Some(rc.clone()));
    assert!(rc.exists());
  }

  // ─── doesn't overwrite an existing file ─────────────────────────

  #[test]
  fn does_not_overwrite_existing_file() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let rc = dir.path().join("existing.shedrc");
    std::fs::write(&rc, "USER_CONTENT_MARKER").unwrap();
    set_rc_path(&rc);
    let result = generate_default_rc().unwrap();
    assert_eq!(result, None);
    // File still has user content.
    let content = std::fs::read_to_string(&rc).unwrap();
    assert_eq!(content, "USER_CONTENT_MARKER");
  }

  // ─── file content contains expected sections ────────────────────

  #[test]
  fn generated_file_contains_default_shopt_lines() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let rc = dir.path().join("rc_with_shopts.shedrc");
    set_rc_path(&rc);
    generate_default_rc().unwrap();
    let content = std::fs::read_to_string(&rc).unwrap();
    // Header marker.
    assert!(
      content.contains("Shed Runtime Commands"),
      "got: {content:?}"
    );
    // ShOpts::generate_default_rc should produce `shopt set ...` lines
    // for known group names. We check a few representative ones.
    assert!(content.contains("core."), "missing core shopt lines");
    assert!(content.contains("prompt."), "missing prompt shopt lines");
    assert!(content.contains("line."), "missing line shopt lines");
  }

  #[test]
  fn generated_file_contains_static_helper_section() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let rc = dir.path().join("rc_with_static.shedrc");
    set_rc_path(&rc);
    generate_default_rc().unwrap();
    let content = std::fs::read_to_string(&rc).unwrap();
    assert!(content.contains("complete -d cd"), "got: {content:?}");
    assert!(content.contains("autocmd"), "got: {content:?}");
    assert!(content.contains("keymap"), "got: {content:?}");
  }

  #[test]
  fn creates_parent_dir_when_missing() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    // Nested path whose parent directories don't exist yet
    let rc = dir.path().join("nested").join("shed").join("shedrc");
    assert!(!rc.parent().unwrap().exists());
    set_rc_path(&rc);
    let result = generate_default_rc().unwrap();
    assert_eq!(result, Some(rc.clone()));
    assert!(rc.exists());
    assert!(rc.parent().unwrap().is_dir());
  }

  // The "no rc path resolvable" error path is essentially unreachable
  // in practice: `get_home` falls back to passwd-uid lookup, so even
  // with HOME unset rc_file_path returns Some. Not tested here.
}

#[cfg(test)]
mod source_runtime_file_tests {
  use super::*;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::tests::testutil::TestGuard;
  use crate::var;

  fn set_var(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name, VarKind::Str(val.into()), VarFlags::empty())
        .unwrap();
    });
  }

  #[test]
  fn env_var_pointed_file_gets_sourced() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("source.sh");
    std::fs::write(&path, "MARKER_VAR=set_by_source\n").unwrap();
    set_var("TEST_RC_VAR", &path.to_string_lossy());
    source_runtime_file("testrc", Some("TEST_RC_VAR")).unwrap();
    assert_eq!(var!("MARKER_VAR"), "set_by_source");
  }

  #[test]
  fn missing_target_file_is_no_op() {
    let _g = TestGuard::new();
    set_var("TEST_RC_NONEXISTENT", "/path/that/should/never/exist/zzz");
    let res = source_runtime_file("nonexistent", Some("TEST_RC_NONEXISTENT"));
    assert!(res.is_ok());
  }

  #[test]
  fn env_var_unset_and_no_home_file_no_op() {
    let _g = TestGuard::new();
    // Point HOME to a tempdir with no matching file.
    let dir = tempfile::TempDir::new().unwrap();
    set_var("HOME", &dir.path().to_string_lossy());
    Shed::vars_mut(|v| v.unset_var("TEST_NOTHING").ok());
    let res = source_runtime_file("nothing", Some("TEST_NOTHING"));
    assert!(res.is_ok());
  }

  #[test]
  fn falls_back_to_home_dot_file_when_env_unset() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let dotfile = dir.path().join(".my_test_rc");
    std::fs::write(&dotfile, "HOME_FALLBACK_MARKER=via_home\n").unwrap();
    set_var("HOME", &dir.path().to_string_lossy());
    Shed::vars_mut(|v| v.unset_var("ENV_NAME_NOT_SET").ok());
    source_runtime_file("my_test_rc", Some("ENV_NAME_NOT_SET")).unwrap();
    assert_eq!(var!("HOME_FALLBACK_MARKER"), "via_home");
  }

  #[test]
  fn prefers_xdg_over_legacy_when_both_exist() {
    let _g = TestGuard::new();
    let home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    set_var("HOME", &home.path().to_string_lossy());
    set_var("XDG_CONFIG_HOME", &xdg.path().to_string_lossy());
    Shed::vars_mut(|v| v.unset_var("XDG_OVER_LEGACY_ENV").ok());

    // legacy ~/.over_legacy_rc
    std::fs::write(
      home.path().join(".over_legacy_rc"),
      "OVER_LEGACY_SRC=legacy\n",
    )
    .unwrap();
    // xdg ~/.config/shed/over_legacy_rc
    std::fs::create_dir_all(xdg.path().join("shed")).unwrap();
    std::fs::write(
      xdg.path().join("shed").join("over_legacy_rc"),
      "OVER_LEGACY_SRC=xdg\n",
    )
    .unwrap();

    source_runtime_file("over_legacy_rc", Some("XDG_OVER_LEGACY_ENV")).unwrap();
    assert_eq!(var!("OVER_LEGACY_SRC"), "xdg");
  }

  #[test]
  fn uses_xdg_when_only_it_exists() {
    let _g = TestGuard::new();
    let home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    set_var("HOME", &home.path().to_string_lossy());
    set_var("XDG_CONFIG_HOME", &xdg.path().to_string_lossy());
    Shed::vars_mut(|v| v.unset_var("XDG_ONLY_ENV").ok());

    std::fs::create_dir_all(xdg.path().join("shed")).unwrap();
    std::fs::write(
      xdg.path().join("shed").join("xdg_only_rc"),
      "XDG_ONLY_MARKER=from_xdg\n",
    )
    .unwrap();

    source_runtime_file("xdg_only_rc", Some("XDG_ONLY_ENV")).unwrap();
    assert_eq!(var!("XDG_ONLY_MARKER"), "from_xdg");
  }

  #[test]
  fn env_var_overrides_both_xdg_and_legacy() {
    let _g = TestGuard::new();
    let home = tempfile::TempDir::new().unwrap();
    let xdg = tempfile::TempDir::new().unwrap();
    let explicit_dir = tempfile::TempDir::new().unwrap();
    set_var("HOME", &home.path().to_string_lossy());
    set_var("XDG_CONFIG_HOME", &xdg.path().to_string_lossy());

    // All three locations have a file; env-var wins
    std::fs::write(home.path().join(".triple_rc"), "TRIPLE_SRC=legacy\n").unwrap();
    std::fs::create_dir_all(xdg.path().join("shed")).unwrap();
    std::fs::write(
      xdg.path().join("shed").join("triple_rc"),
      "TRIPLE_SRC=xdg\n",
    )
    .unwrap();
    let explicit = explicit_dir.path().join("explicit_rc");
    std::fs::write(&explicit, "TRIPLE_SRC=explicit\n").unwrap();
    set_var("TRIPLE_RC_ENV", &explicit.to_string_lossy());

    source_runtime_file("triple_rc", Some("TRIPLE_RC_ENV")).unwrap();
    assert_eq!(var!("TRIPLE_SRC"), "explicit");
  }
}

#[cfg(test)]
mod source_wrapper_tests {
  //! Thin one-liner wrappers that delegate to `source_runtime_file`
  //! with hardcoded (name, `env_var`) pairs. The tests verify that each
  //! wrapper uses the right env-var name — if any pair gets swapped,
  //! the assertion fails.

  use super::*;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::tests::testutil::TestGuard;
  use crate::var;

  fn set_var(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name, VarKind::Str(val.into()), VarFlags::empty())
        .unwrap();
    });
  }

  #[test]
  fn source_rc_uses_shed_rc_env_var() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("rc.sh");
    std::fs::write(&path, "SOURCE_RC_MARKER=fired\n").unwrap();
    set_var("SHED_RC", &path.to_string_lossy());
    source_rc().unwrap();
    assert_eq!(var!("SOURCE_RC_MARKER"), "fired");
  }

  #[test]
  fn source_login_uses_shed_profile_env_var() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("profile.sh");
    std::fs::write(&path, "SOURCE_LOGIN_MARKER=fired\n").unwrap();
    set_var("SHED_PROFILE", &path.to_string_lossy());
    source_login().unwrap();
    assert_eq!(var!("SOURCE_LOGIN_MARKER"), "fired");
  }

  #[test]
  fn source_env_uses_shed_env_env_var() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("env.sh");
    std::fs::write(&path, "SOURCE_ENV_MARKER=fired\n").unwrap();
    set_var("SHED_ENV", &path.to_string_lossy());
    source_env().unwrap();
    assert_eq!(var!("SOURCE_ENV_MARKER"), "fired");
  }
}

#[cfg(test)]
mod lookup_cmd_tests {
  use super::*;
  use crate::tests::testutil::{TestGuard, has_cmd};

  #[test]
  fn lookup_returns_path_for_known_binary_with_hashall() {
    if !has_cmd("ls") {
      return;
    }
    let _g = TestGuard::new();
    crate::shopt_mut!(set.hashall = true);
    crate::state::util::try_hash();
    let path = lookup_cmd("ls");
    assert!(path.is_some(), "expected Some(path) for 'ls'");
    // Whatever the path is, it should end with "ls".
    let path = path.unwrap();
    assert_eq!(path.file_name().unwrap().to_string_lossy(), "ls");
  }

  #[test]
  fn lookup_returns_path_for_known_binary_without_hashall() {
    if !has_cmd("ls") {
      return;
    }
    let _g = TestGuard::new();
    crate::shopt_mut!(set.hashall = false);
    let path = lookup_cmd("ls");
    assert!(path.is_some(), "expected Some(path) for 'ls'");
  }

  #[test]
  fn lookup_returns_none_for_unknown_command() {
    let _g = TestGuard::new();
    assert!(lookup_cmd("definitely_not_a_real_binary_zzzqqq").is_none());
  }
}

#[cfg(test)]
mod set_ver_info_tests {
  //! `set_ver_info` populates two shell vars from compile-time
  //! constants: `SHED_VERSION` (the Cargo.toml version string) and
  //! `SHED_VER_INFO` (an `AssocArr` with major/minor/patch/arch/os keys).
  //! The tests pin the structure rather than specific values, so they
  //! don't churn each version bump.

  use super::*;
  use crate::tests::testutil::TestGuard;
  use crate::var;

  #[test]
  fn sets_shed_version_to_cargo_pkg_version() {
    let _g = TestGuard::new();
    set_ver_info().unwrap();
    assert_eq!(var!("SHED_VERSION"), env!("CARGO_PKG_VERSION"));
  }

  #[test]
  fn ver_info_is_assoc_array_with_five_keys() {
    let _g = TestGuard::new();
    set_ver_info().unwrap();
    let kind = Shed::vars(|v| v.try_get_var_kind("SHED_VER_INFO"));
    match kind {
      Some(VarKind::AssocArr(items)) => {
        let keys: crate::HashSet<&VarStr> = items.iter().map(|(k, _)| k).collect();
        assert_eq!(keys.len(), 5, "got: {keys:?}");
        for expected in ["major", "minor", "patch", "arch", "os"] {
          assert!(
            keys.contains(&VarStr::from(expected)),
            "missing key {expected}, got: {keys:?}"
          );
        }
      }
      other => panic!("expected AssocArr, got {other:?}"),
    }
  }

  #[test]
  fn ver_info_arch_and_os_match_compile_time_consts() {
    let _g = TestGuard::new();
    set_ver_info().unwrap();
    let items = match Shed::vars(|v| v.try_get_var_kind("SHED_VER_INFO")) {
      Some(VarKind::AssocArr(items)) => items,
      other => panic!("expected AssocArr, got {other:?}"),
    };
    let get = |k: &str| {
      items
        .iter()
        .find(|(key, _)| key == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
    };
    assert_eq!(get("arch"), std::env::consts::ARCH);
    assert_eq!(get("os"), std::env::consts::OS);
  }

  #[test]
  fn ver_info_semver_components_match_cargo_pkg_version() {
    let _g = TestGuard::new();
    set_ver_info().unwrap();
    let items = match Shed::vars(|v| v.try_get_var_kind("SHED_VER_INFO")) {
      Some(VarKind::AssocArr(items)) => items,
      other => panic!("expected AssocArr, got {other:?}"),
    };
    let get = |k: &str| {
      items
        .iter()
        .find(|(key, _)| key == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
    };
    let expected: Vec<&str> = env!("CARGO_PKG_VERSION").split('.').collect();
    assert_eq!(get("major"), expected[0]);
    assert_eq!(get("minor"), expected[1]);
    assert_eq!(get("patch"), expected[2]);
  }
}
