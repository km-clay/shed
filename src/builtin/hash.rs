use super::{
  Shed,
  expand::shell_quote,
  opt::OptSpec,
  outln, sherr,
  state::meta::MetaTab,
  util::{ShResult, with_status},
};

pub(super) struct Hash;
impl super::Builtin for Hash {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("clear", 'r'),
      OptSpec::new_long("refresh"),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut refresh = false;
    let mut clear = false;

    for opt in args.options() {
      match opt.key() {
        "clear" => clear = true,
        "refresh" => refresh = true,
        _ => {
          return Err(
            sherr!(ParseErr @ opt.span(), "Invalid hash option: {opt}").promote(args.span()),
          );
        }
      }
    }

    if args.no_arguments() && args.no_options() {
      let entries: Vec<(String, std::path::PathBuf)> = Shed::meta(|m| {
        m.path_cache()
          .entries()
          .map(|(name, path)| (name.clone(), path.clone()))
          .collect()
      });
      for (name, path) in entries {
        let path = shell_quote(&path.to_string_lossy());
        outln!("{name}={path}");
      }
    }

    Shed::meta_mut(|m| {
      if clear {
        m.clear_path_cache();
      }
      if refresh {
        m.rehash_path_cache();
      }
    });

    if !args.no_arguments() {
      Shed::meta_mut(MetaTab::rehash_path_cache);
      Shed::meta(|m| -> ShResult<()> {
        for (arg, span) in args.arguments() {
          if m.lookup_cached_cmd(&arg.to_str_lossy()).is_none() {
            return Err(sherr!(NotFound, "Command not found: {arg}").promote(span.clone()));
          }
        }
        Ok(())
      })?;
    }

    with_status(0)
  }
}

#[cfg(test)]
mod hash_tests {
  use crate::state::meta::MetaTab;
  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, has_cmd, test_input};

  /// Strip cached utilities so each test starts from a known state.
  fn clear_cache() {
    Shed::meta_mut(MetaTab::clear_path_cache);
  }

  // ─── no args, no opts → list cached commands ────────────────────

  #[test]
  fn hash_with_no_args_lists_cached_commands() {
    if !has_cmd("cat") {
      return;
    }
    let g = TestGuard::new();
    clear_cache();
    // Hash a known command first so there's something to list.
    test_input("hash cat").unwrap();
    g.read_output();
    test_input("hash").unwrap();
    let out = g.read_output();
    assert!(out.contains("cat="), "got: {out:?}");
  }

  // ─── hash <cmd> → caches command ────────────────────────────────

  #[test]
  fn hash_specific_command_succeeds() {
    if !has_cmd("cat") {
      return;
    }
    let _g = TestGuard::new();
    clear_cache();
    test_input("hash cat").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── unknown command → NotFound ─────────────────────────────────

  #[test]
  fn hash_unknown_command_errors() {
    let _g = TestGuard::new();
    clear_cache();
    test_input("hash definitely_not_a_real_cmd_xyzzy").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── -r clears cache ────────────────────────────────────────────

  #[test]
  fn hash_dash_r_clears_cache() {
    if !has_cmd("cat") {
      return;
    }
    let g = TestGuard::new();
    clear_cache();
    test_input("hash cat").unwrap();
    g.read_output();
    test_input("hash -r").unwrap();
    g.read_output();
    // After clearing, `hash` should produce no output.
    test_input("hash").unwrap();
    let out = g.read_output();
    assert!(!out.contains("cat="), "cache should be empty: {out:?}");
  }

  // ─── --refresh re-discovers PATH commands ───────────────────────

  #[test]
  fn hash_refresh_succeeds() {
    let _g = TestGuard::new();
    clear_cache();
    test_input("hash --refresh").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── unknown opt → ParseErr ─────────────────────────────────────

  #[test]
  fn hash_unknown_opt_errors() {
    let _g = TestGuard::new();
    clear_cache();
    test_input("hash --not-a-real-flag").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
