use std::{path::PathBuf, rc::Rc};

use crate::{
  HashSet,
  builtin::{self, BUILTIN_NAMES},
  state::{
    Shed,
    meta::{MetaTab, Utility},
    paths,
    vars::VarStr,
  },
  util::strops::{self},
  var,
};

/// Parse `arr[idx]` into (name, `raw_index_expr`). Pure parsing, no expansion.
pub(crate) fn lookup_cmd(cmd: &str) -> Option<PathBuf> {
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
  let resolved = paths::resolve_in_path(&path_env.to_str_lossy(), cmd)?;
  Shed::meta_mut(|m| m.cache_cmd(cmd.to_string(), resolved.clone()));
  Some(resolved)
}

pub(crate) fn which_util(name: &str) -> Option<Rc<Utility>> {
  if Shed::logic(|l| l.get_alias(name).is_some()) {
    return Some(Rc::new(Utility::alias(name.into())));
  }
  if Shed::logic(|l| l.has_command_func(name)) {
    return Some(Rc::new(Utility::function(name.into())));
  }
  if builtin::lookup_builtin(name.as_bytes()).is_some() {
    return Some(Rc::new(Utility::builtin(name.into())));
  }
  if let Some(p) = lookup_cmd(name) {
    return Some(Rc::new(Utility::command(name.into(), p)));
  }
  // Last resort: an executable file living in $PWD that isn't in $PATH.
  MetaTab::get_exec_files_in_cwd()
    .into_iter()
    .find(|u| u.name() == name)
}

pub(crate) fn try_hash() {
  if Shed::shopts(|o| o.set.hashall) {
    Shed::meta_mut(MetaTab::try_rehash_path_cache);
  }
}

pub(crate) fn list_util_names() -> HashSet<VarStr> {
  let mut cmd_names = HashSet::default();
  let path_cmds = super::meta::MetaTab::get_cmds_in_path()
    .into_iter()
    .map(|u| u.name());
  let builtin_names = BUILTIN_NAMES.iter().map(|n| VarStr::from(*n));
  let func_names = Shed::logic(|l| l.funcs().keys().map(VarStr::from).collect::<Vec<_>>());
  let aliases = Shed::logic(|l| l.aliases().keys().map(VarStr::from).collect::<Vec<_>>());

  cmd_names.extend(path_cmds);
  cmd_names.extend(builtin_names);
  cmd_names.extend(func_names);
  cmd_names.extend(aliases);

  cmd_names
}

/// Check if a command name is a likely typo of a known hashed utility.
///
/// # Panics
/// This calls [`list_util_names()`], which calls [`Shed::meta()`] internally.
/// Calling this from inside of a [`Shed::meta()`] closure is a `RefCell` panic.
pub(crate) fn check_typo(cmd: &[u8]) -> Vec<VarStr> {
  let max_edits = (cmd.len() / 3).clamp(1, 2);
  let max_dist = max_edits * strops::EDIT_WEIGHT;

  let mut matches = list_util_names()
    .into_iter()
    .filter(|n| n.len().abs_diff(cmd.len()) <= max_dist) // cheap prune
    .filter_map(|n| {
      // compute all distances
      let d = strops::levenshtein(cmd, n.as_bytes());
      (1..=max_dist).contains(&d).then_some((d, n))
    })
    .collect::<Vec<_>>();

  matches.sort_by_key(|(d, n)| (*d, n.as_bytes().first() != cmd.first()));

  if let Some(&(best, _)) = matches.first() {
    matches.retain(|(d, _)| *d == best);
  }
  matches.truncate(3);

  matches.into_iter().map(|(_, n)| n).collect()
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
    try_hash();
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
mod check_typo_tests {
  //! `check_typo` suggests likely-typo corrections for an unknown command. It
  //! draws candidates from the live command set ($PATH walk + builtins + funcs
  //! + aliases via [`list_util_names`]), so these tests register their own
  //!   distinctively-named aliases and query with strings gibberish enough that
  //!   no real $PATH binary can plausibly fall within the edit threshold — that
  //!   keeps the assertions deterministic regardless of the host's $PATH.
  use super::*;
  use crate::eval::lex::Span;
  use crate::tests::testutil::TestGuard;

  fn alias(name: &str) {
    Shed::logic_mut(|l| l.insert_alias(name, &"echo".into(), Span::default()));
  }

  fn suggestions(cmd: &str) -> Vec<String> {
    check_typo(cmd.as_bytes())
      .iter()
      .map(ToString::to_string)
      .collect()
  }

  #[test]
  fn suggests_a_close_command() {
    let _g = TestGuard::new();
    alias("qzxwvbn");
    // one inserted byte from the alias → a single ordinary edit, well within
    // the threshold for an 8-byte command.
    let out = suggestions("qzxwvbnq");
    assert!(
      out.iter().any(|n| n == "qzxwvbn"),
      "expected 'qzxwvbn' among suggestions, got {out:?}"
    );
  }

  #[test]
  fn exact_match_is_not_suggested() {
    let _g = TestGuard::new();
    alias("qzxwvbn");
    // The command exists verbatim (distance 0); check_typo excludes distance 0,
    // and nothing else is near this gibberish, so there is nothing to suggest.
    assert!(
      suggestions("qzxwvbn").is_empty(),
      "an exact match must not be offered as a typo"
    );
  }

  #[test]
  fn distant_command_yields_nothing() {
    let _g = TestGuard::new();
    alias("qzxwvbn");
    // Far outside the edit threshold from the alias (and from any real binary).
    assert!(
      suggestions("totally_unrelated_string").is_empty(),
      "a far-off command should produce no suggestions"
    );
  }

  #[test]
  fn caps_suggestions_at_three() {
    let _g = TestGuard::new();
    // Four equidistant candidates (last byte substituted); the best-distance
    // tier holds all four, but the result is capped at three.
    for name in ["qzjxvbwa", "qzjxvbwb", "qzjxvbwc", "qzjxvbwd"] {
      alias(name);
    }
    let out = suggestions("qzjxvbwe");
    assert_eq!(out.len(), 3, "cap-at-3 violated, got {out:?}");
    assert!(
      out.iter().all(|n| n.starts_with("qzjxvbw")),
      "unexpected non-registered suggestion in {out:?}"
    );
  }

  #[test]
  fn prefers_the_closest_over_a_farther_match() {
    let _g = TestGuard::new();
    // Both candidates are within the threshold for `qzjxvbw` (max distance 4):
    // `qzjxvbq` is one substitution away (distance 2), `qzjxvpq` is two subs
    // away (distance 4). Only the closer one survives the best-distance tier.
    alias("qzjxvbq");
    alias("qzjxvpq");
    let out = suggestions("qzjxvbw");
    assert_eq!(out, vec!["qzjxvbq".to_string()], "got {out:?}");
  }
}
