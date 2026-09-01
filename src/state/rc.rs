//! Config-file generation and sourcing
//!
//! Locate ([`rc_file_path`]), generate ([`generate_default_rc`], [`compose_rc`]), and
//! source the shell's rc, profile, and env files.

use std::{
  fs::OpenOptions,
  io::{Read, Write},
  path::PathBuf,
};

use crate::{
  state::{Shed, logic::ShFunc, vars::VarStr},
  try_var,
  util::{error::ShErrKind, strops::VarStrDisplay},
  varstr,
};

use super::{ShResult, eval::execute, meta::MetaTab, paths, sherr, shopt::ShoptSource};

/// Parse `arr[idx]` into (name, `raw_index_expr`). Pure parsing, no expansion.
pub(crate) fn rc_file_path() -> Option<PathBuf> {
  if let Some(p) = try_var!("SHED_RC") {
    return Some(PathBuf::from(p));
  }
  let xdg = paths::config_dir().map(|c| c.join("shed").join("shedrc"));
  let home = paths::get_home().map(|h| h.join(".shedrc"));

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
pub(crate) struct GenRcConfig {
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
  pub(crate) fn first_run() -> Self {
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
    .map(|(name, body)| {
      let mut line = b"alias ".to_vec();
      line.extend_from_slice(&super::vars::display_as_var(name.as_bytes(), body));
      VarStr::from(line)
    })
    .collect()
}

/// Live user function definitions (verbatim source), sorted by name.
fn live_funcs() -> Vec<VarStr> {
  let mut funcs: Vec<(VarStr, VarStr)> = Shed::logic(|l| {
    l.funcs()
      .iter()
      .filter_map(|(name, f)| match f {
        ShFunc::Defined { source, .. } => Some((name.into(), source.as_var_str())),
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
  Shed::logic(|l| l.iter_autocmds().map(VarStrDisplay::to_var_str).collect())
}

/// Live `keymap` registrations, in registration order.
fn live_keymaps() -> Vec<VarStr> {
  Shed::logic(|l| l.keymaps().iter().map(VarStrDisplay::to_var_str).collect())
}

/// Render an rc file to a `Vec<VarStr>` per `config`. Pure — no I/O, no
/// side effects on `Shed` state. Caller decides where the lines go.
pub(crate) fn compose_rc(config: &GenRcConfig) -> Vec<VarStr> {
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
    lines.push(VarStr::default());
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
    lines.push(VarStr::default());
  }

  // Shell options
  if config.include_shopts {
    if comments {
      lines.push("# -- Shell Options --".into());
      lines.push(VarStr::default());
    }
    let mut current_group: Option<&'static str> = None;
    for (_key, group, entry, doc) in Shed::shopts(|o| o.rc_entries(config.source)) {
      if comments && Some(group) != current_group {
        if current_group.is_some() {
          lines.push(VarStr::default());
        }
        lines.push(varstr!("# - {group} -"));
        current_group = Some(group);
      }
      lines.push(match (doc, comments) {
        (Some(d), true) => varstr!("{entry:<50} # {d}"),
        _ => entry,
      });
    }
    lines.push(VarStr::default());
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
  while lines.last().is_some_and(|s| s.is_empty()) {
    lines.pop();
  }
  lines
}

pub(crate) fn generate_default_rc() -> ShResult<Option<PathBuf>> {
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

pub(crate) fn source_runtime_file(name: &str, env_var_name: Option<&str>) -> ShResult<()> {
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
    paths::config_dir()
      .map(|c| c.join("shed").join(name))
      .filter(|p| p.is_file())
      .or_else(|| {
        paths::get_home()
          .map(|h| h.join(format!(".{name}")))
          .filter(|p| p.is_file())
      })
  };

  match user_path {
    Some(path) if path.is_file() => source_file(path),
    _ => Ok(()),
  }
}

pub(crate) fn source_rc() -> ShResult<()> {
  source_runtime_file("shedrc", Some("SHED_RC"))
}

pub(crate) fn source_login() -> ShResult<()> {
  source_runtime_file("shed_profile", Some("SHED_PROFILE"))
}

pub(crate) fn source_env() -> ShResult<()> {
  source_runtime_file("shedenv", Some("SHED_ENV"))
}

pub(crate) fn source_file(path: PathBuf) -> ShResult<()> {
  let source_name = path.to_string_lossy().to_string();
  let source_display = paths::display_path_normalized(source_name);
  let mut file = OpenOptions::new().read(true).open(path)?;

  // Read raw bytes and lossily decode, rather than `read_to_string` which
  // hard-rejects the whole file on a single non-UTF-8 byte. The lexer is
  // `&str`-based (so bytes in string literals still degrade to U+FFFD), but a
  // stray byte in a comment or heredoc no longer aborts sourcing.
  let mut raw = Vec::new();
  file.read_to_end(&mut raw)?;
  let buf = String::from_utf8_lossy(&raw).into_owned();

  // sourced files behave like functions
  // 'return' is valid inside of them, and we also track recursion depth
  let _guard = Shed::meta_mut(MetaTab::enter_func);

  match execute::exec_nonint(buf.into(), Some(source_display.into())) {
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

#[cfg(test)]
mod generate_default_rc_tests {
  use super::*;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::tests::testutil::TestGuard;

  fn set_rc_path(p: &std::path::Path) {
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_RC",
        VarKind::string(p.to_string_lossy().into()),
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
