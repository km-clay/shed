//! Forget specific aspects of shell state.
//!
//! Allows the user to selectively forget any of the following:
//! * `Variables`
//! * `Traps`
//! * `Aliases`
//! * `Functions`
//! * `Shopts`
//! * `Keymaps`
//! * `Completions`
//! * `Autocmds`
//! * `Deferred` commands
//!
//! The [`genrc`](super::genrc::Genrc) builtin can be used to dump shell state for round trip sourcing.

use crate::{
  opt, sherr,
  state::{ForgetFlags, ForgetSpec, Shed},
  util::{self, error::ShResult},
};

use super::{ForkBehavior, opt::OptSpec};

pub(super) struct Forget;
impl super::Builtin for Forget {
  fn fork_behavior(&self) -> ForkBehavior {
    ForkBehavior::Subshell
  }
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("vars" | b'v'),
      opt!("traps" | b't'),
      opt!("aliases" | b'a'),
      opt!("functions" | b'f'),
      opt!("shopts" | b's'),
      opt!("keymaps" | b'k'),
      opt!("comps" | b'c'),
      opt!("autocmds" | b'A'),
      opt!("deferred" | b'd'),
      opt!("except"),
      opt!("exclude" | b'x').argc(1),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if !args.no_arguments() {
      let (_, span) = super::argv::join_raw_arg_iter(args.arguments());
      return Err(
        sherr!(
          ParseErr @ span,
          "unexpected argument(s)"
        )
        .with_note("'forget' does not take positional arguments. see 'help builtin-forget'".into()),
      );
    }
    if args.no_options() {
      // user gave no flags, forget everything
      Shed::forget(&ForgetSpec::all());
      return util::with_status(0);
    }

    let mut spec = ForgetSpec::new();
    let mut excepted = false;

    for opt in args.options() {
      match opt.key() {
        "vars"/*------*/=> spec |= ForgetFlags::VARS,
        "traps"/*-----*/=> spec |= ForgetFlags::TRAPS,
        "aliases"/*---*/=> spec |= ForgetFlags::ALIASES,
        "functions"/*-*/=> spec |= ForgetFlags::FUNCS,
        "shopts"/*----*/=> spec |= ForgetFlags::SHOPTS,
        "keymaps"/*---*/=> spec |= ForgetFlags::KEYMAPS,
        "comps"/*-----*/=> spec |= ForgetFlags::COMPS,
        "autocmds"/*--*/=> spec |= ForgetFlags::AUTOCMDS,
        "deferred"/*--*/=> spec |= ForgetFlags::DEFERRED,
        "except"/*----*/=> excepted = true,
        "exclude"/*---*/=> {
          let Ok(var) = opt.value() else {
            return Err(sherr!(ParseErr @ opt.span(), "missing argument for '{}'", opt));
          };
          spec.exclude(var);
        }
        _ => return Err(sherr!(ParseErr @ opt.span(), "unrecognized option '{}'", opt)),
      }
    }

    if excepted {
      spec.except();
    }
    if spec.flags().is_empty() {
      spec |= ForgetFlags::all();
    }

    Shed::forget(&spec);

    util::with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{Shed, logic::TrapTarget};
  use crate::tests::testutil::{TestGuard, test_input};

  // ===================== vars: reset to launch baseline =====================

  #[test]
  fn forget_drops_user_vars() {
    let g = TestGuard::new();
    test_input(r#"myvar=hello; forget; printf '[%s]' "$myvar""#).unwrap();
    assert_eq!(g.read_output(), "[]");
  }

  #[test]
  fn forget_keeps_launch_env() {
    let g = TestGuard::new();
    test_input(r#"myvar=x; forget; printf '%s' "${HOME:+set}""#).unwrap();
    assert_eq!(g.read_output(), "set");
  }

  #[test]
  fn forget_resets_modified_launch_var() {
    let g = TestGuard::new();
    test_input(r#"IFS=X; forget; [ "$IFS" = X ] && printf notreset || printf reset"#).unwrap();
    assert_eq!(g.read_output(), "reset");
  }

  #[test]
  fn forget_keeps_pwd_live() {
    let g = TestGuard::new();
    test_input(r#"cd /; forget; printf '%s' "$PWD""#).unwrap();
    assert_eq!(g.read_output(), "/");
  }

  // ===================== per-category isolation =====================

  #[test]
  fn forget_aliases_keeps_functions() {
    let _g = TestGuard::new();
    test_input("alias al=echo; f() { :; }; forget -a").unwrap();
    assert!(
      Shed::logic(|l| l.get_alias("al").is_none()),
      "alias dropped"
    );
    assert!(
      Shed::logic(|l| l.get_func("f").is_some()),
      "function survives -a"
    );
  }

  #[test]
  fn forget_functions_keeps_aliases() {
    let _g = TestGuard::new();
    test_input("alias al=echo; f() { :; }; forget -f").unwrap();
    assert!(
      Shed::logic(|l| l.get_func("f").is_none()),
      "function dropped"
    );
    assert!(
      Shed::logic(|l| l.get_alias("al").is_some()),
      "alias survives -f"
    );
  }

  #[test]
  fn forget_functions_keeps_bundled_autoloads() {
    let _g = TestGuard::new();
    test_input("f() { :; }; forget -f").unwrap();
    assert!(
      Shed::logic(|l| l.get_func("f").is_none()),
      "user function dropped"
    );
    assert!(
      !Shed::logic(|l| l.funcs().is_empty()),
      "bundled autoloads preserved through forget -f"
    );
  }

  #[test]
  fn forget_clears_traps() {
    let _g = TestGuard::new();
    test_input("trap 'echo x' EXIT; forget -t").unwrap();
    assert!(Shed::logic(|l| l.get_trap(TrapTarget::Exit).is_none()));
  }

  #[test]
  fn forget_shopts_resets_to_default() {
    let _g = TestGuard::new();
    Shed::shopts_mut(|o| o.core.autocd = true);
    test_input("forget -s").unwrap();
    assert!(!Shed::shopts(|o| o.core.autocd), "shopt reset to default");
  }

  // ===================== deferred: current scope only =====================

  #[test]
  fn forget_deferred_cancels_current_scope() {
    let g = TestGuard::new();
    test_input(r"f() { defer printf cleanup; forget -d; }; f; printf done").unwrap();
    assert_eq!(g.read_output(), "done");
  }

  #[test]
  fn deferred_runs_without_forget() {
    let g = TestGuard::new();
    test_input(r"f() { defer printf cleanup; }; f; printf done").unwrap();
    assert_eq!(g.read_output(), "cleanupdone");
  }

  // ===================== no flags = forget everything =====================

  #[test]
  fn forget_no_flags_clears_all() {
    let g = TestGuard::new();
    test_input(r#"myvar=x; alias al=echo; f() { :; }; forget; printf '[%s]' "$myvar""#).unwrap();
    assert_eq!(g.read_output(), "[]", "user var gone");
    assert!(Shed::logic(|l| l.get_alias("al").is_none()), "alias gone");
    assert!(Shed::logic(|l| l.get_func("f").is_none()), "function gone");
  }

  // ===================== except / exclude =====================

  #[test]
  fn forget_exclude_keeps_named_var() {
    let g = TestGuard::new();
    test_input(r#"gone=x; keep=y; forget -x keep; printf '[%s][%s]' "$gone" "$keep""#).unwrap();
    assert_eq!(g.read_output(), "[][y]");
  }

  #[test]
  fn forget_except_inverts_categories() {
    let _g = TestGuard::new();
    test_input("alias al=echo; f() { :; }; forget --except -f").unwrap();
    assert!(
      Shed::logic(|l| l.get_func("f").is_some()),
      "function kept by --except -f"
    );
    assert!(
      Shed::logic(|l| l.get_alias("al").is_none()),
      "alias forgotten by --except -f"
    );
  }

  #[test]
  fn forget_dump_round_trip() {
    let g = TestGuard::new();
    test_input(
      r#"a=1; b=2; dump=$(genrc --vars); forget -x dump; eval "$dump"; printf '[%s][%s]' "$a" "$b""#,
    )
    .unwrap();
    assert_eq!(g.read_output(), "[1][2]");
  }

  // ===================== input validation =====================

  #[test]
  fn forget_unknown_flag_errors_and_forgets_nothing() {
    let g = TestGuard::new();
    test_input(r#"keep=yes; forget -z; [ $? -ne 0 ] && printf err; printf '[%s]' "$keep""#)
      .unwrap();
    assert!(g.read_output().ends_with("err[yes]"));
  }

  #[test]
  fn forget_positional_arg_errors_and_forgets_nothing() {
    let g = TestGuard::new();
    test_input(r#"keep=yes; forget vars; [ $? -ne 0 ] && printf err; printf '[%s]' "$keep""#)
      .unwrap();
    assert!(g.read_output().ends_with("err[yes]"));
  }
}
