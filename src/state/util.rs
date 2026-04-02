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
	let rc_path = rc_file_path().ok_or_else(|| sherr!(InternalErr, "could not determine rc file path",))?;
	if rc_path.exists() {
		return Ok(None)
	}
	let mut rc_file = OpenOptions::new()
		.write(true)
		.create(true)
		.truncate(true)
		.open(&rc_path)?;

	let lines = [
		"# --- Shed Runtime Commands ---",
		"# This file was automatically generated by shed.",
		"# These are sane defaults for many shed-specific options and features.",
		"# Edit this file to customize, or use it as a reference.",
		"",
		"# -- Shell Options --",
		"",
		"# - Core -",
		"shopt core.dotglob=false             # Include hidden files in glob expansion",
		"shopt core.autocd=false              # Executing a directory name moves to that directory",
		"shopt core.hist_ignore_dupes=true    # Don't add duplicate entries to history",
		"shopt core.max_hist=10000            # Max number of history entries to save",
		"shopt core.interactive_comments=true # Allow comments in interactive line editing",
		"shopt core.auto_hist=true            # Automatically add commands to history",
		"shopt core.bell_enabled=true         # Certain shell events will ring the terminal bell",
		"shopt core.max_recurse_depth=1000    # Max depth for recursive shell function calls",
		"shopt core.xpg_echo=false            # Whether echo expands escape sequences by default",
		"",
		"# - Prompt -",
		"shopt prompt.highlight=true               # Enable syntax highlighting on the prompt",
		"shopt prompt.auto_indent=true             # Intelligently indent new-lines based on nesting depth",
		"shopt prompt.line_numbers=true            # Show line numbers on the prompt",
		"shopt prompt.comp_limit=100               # Max completion candidates before asking for confirmation",
		"shopt prompt.completion_ignore_case=false # Ignore case when completing file and directory names",
		"shopt prompt.trunc_prompt_path=4          # Truncate paths to this many trailing components",
		"shopt prompt.hist_cat=true                # History concatenation with Shift+Up/Down. Try it out!",
		"shopt prompt.screensaver_cmd=\"\"           # Command to run after idle timeout (empty = disabled)",
		"shopt prompt.screensaver_idle_time=600    # Seconds of inactivity before running screensaver_cmd",
		"",
		"# The character referred to by <leader> in keymaps.",
		"# Default is space, so <leader>f means Space followed by f.",
		"shopt prompt.leader=\" \"",
		"",
		"# - Line Editor -",
		"shopt line.viewport_height=\"50%\" # Viewport height: absolute lines (e.g. 20) or percent (e.g. 50%)",
		"shopt line.scroll_offset=2         # Lines of context to keep when the viewport scrolls",
		"",
		"# - POSIX Set Options -",
		"# These are the options normally toggled with 'set' in other shells.",
		"# In shed, 'set' still works, but you can also use 'shopt' directly.",
		"shopt set.hashall=true    # Cache command locations; false = search $PATH every time",
		"shopt set.vi=false        # Vi keybindings (false = Emacs mode)",
		"shopt set.allexport=false # Auto-export all variable assignments",
		"shopt set.errexit=false   # Exit immediately on non-zero command status",
		"shopt set.noclobber=false # Prevent '>' from overwriting existing files",
		"shopt set.monitor=true    # Enable job control and background processes",
		"shopt set.noglob=false    # Disable globbing (filename expansion)",
		"shopt set.noexec=false    # Parse but don't execute commands (syntax checking)",
		"shopt set.nolog=false     # Don't save function definitions to history",
		"shopt set.notify=false    # Print job status asynchronously on exit/stop",
		"shopt set.nounset=false   # Error on expansion of unset variables",
		"shopt set.verbose=false   # Print shell input to stderr as it is read",
		"shopt set.xtrace=false    # Print commands after expansion, before execution",
		"",
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
		"# Type ':h autocmd' on the prompt for more details.",
		"autocmd 'on-exit' 'echo exit 1>&2' # Print 'exit' when the shell exits",
		"",
		"# -- Keybinds --",
		"# Register commands to run on key presses while on the prompt.",
		"# Type ':h keymap' on the prompt for more advanced usage.",
		"keymap -ie '<C-L>' '<CMD>clear<CR>' # Ctrl+L clears the screen (insert + emacs mode)",
	];

	for line in lines {
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
