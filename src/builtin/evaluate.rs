use crate::{
  eval::execute,
  expand::{self, Word, subshell},
  state::{Shed, meta::MetaTab, params, vars::ShStrSliceExt},
  util::{self, error::ShResult},
};

use super::ForkBehavior;

pub(super) struct Eval;
impl super::Builtin for Eval {
  fn is_special(&self) -> bool {
    true
  }

  fn fork_behavior(&self) -> ForkBehavior {
    ForkBehavior::Check(|args| match args {
      [only] => {
        // we can only reliably avoid forking if there is a single word.
        // we join the arguments with IFS, which can change inbetween invocations
        // and that can cause issues with the fork report cache
        let Some(text) = expand::resolve_text(only) else {
          return ForkBehavior::dynamic(true);
        };

        subshell::verdict_for(&text).make_dynamic().into()
      }
      [] => ForkBehavior::dynamic(false),
      _  => ForkBehavior::dynamic(true),
    })
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.arguments().count() == 0 {
      return util::with_status(0);
    }
    let sep = params::get_separator();
    let command = args
      .arguments()
      .map(Word::content)
      .collect::<Vec<_>>()
      .join_with(&sep);

    // eval adds an xtrace layer
    let _xtrace = Shed::meta_mut(MetaTab::xtrace_descend);

    execute::exec_nonint(command, Some("eval".into()))
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{self, Shed, vars::VarFlags, vars::VarKind};
  use crate::tests::testutil::{TestGuard, test_input};
  use crate::var;

  // ===================== Basic =====================

  #[test]
  fn eval_simple_command() {
    let guard = TestGuard::new();
    test_input("eval echo hello").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\n");
  }

  #[test]
  fn eval_no_args_succeeds() {
    let _g = TestGuard::new();
    test_input("eval").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn eval_status_zero() {
    let _g = TestGuard::new();
    test_input("eval true").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Joins args =====================

  #[test]
  fn eval_joins_args() {
    let guard = TestGuard::new();
    // eval receives "echo" "hello" "world" as separate args, joins to "echo hello world"
    test_input("eval echo          hello         world").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello world\n");
  }

  // ===================== Re-evaluation =====================

  #[test]
  fn eval_expands_variable() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        b"CMD",
        VarKind::Str("echo evaluated".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    test_input("eval $CMD").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "evaluated\n");
  }

  #[test]
  fn eval_sets_variable() {
    let _g = TestGuard::new();
    test_input("eval x=42").unwrap();
    let val = var!(b"x");
    assert_eq!(val, "42");
  }

  #[test]
  fn eval_pipeline() {
    let guard = TestGuard::new();
    test_input("eval 'echo hello | cat'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\n");
  }

  #[test]
  fn eval_compound_command() {
    let guard = TestGuard::new();
    test_input("eval 'echo first; echo second'").unwrap();
    let out = guard.read_output();
    assert!(out.contains("first"));
    assert!(out.contains("second"));
  }

  // ===================== Status propagation =====================

  #[test]
  fn eval_propagates_failure_status() {
    let _g = TestGuard::new();
    let _  = test_input("eval false");
    assert_ne!(state::Shed::get_status(), 0);
  }
}
