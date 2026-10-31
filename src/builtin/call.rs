use crate::{
  eval::execute::exec_nonint,
  expand::{self, Word, subshell},
  set_var, sherr,
  state::{Shed, meta::MetaTab, vars::VarKind},
  util::{
    self,
    error::{ShErrKind, ShResult},
    guards,
  },
};

use super::ForkBehavior;

pub(super) struct Call;
impl super::Builtin for Call {
  fn fork_behavior(&self) -> ForkBehavior {
    ForkBehavior::Check(|args| {
      let Some(first) = args.first() else {
        return ForkBehavior::dynamic(false);
      };
      let Some(text) = expand::resolve_text(first) else {
        return ForkBehavior::dynamic(true);
      };

      subshell::verdict_for(&text).make_dynamic().into()
    })
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let (mut arg_vec, _opts) = args.take_argv();

    if arg_vec.is_empty() {
      return Err(sherr!(ExecFail @ args.cmd_span(), "expected a command"));
    };

    let lambda            = std::mem::replace(&mut arg_vec[0], Word::from("call"));
    let _arg_guard        = guards::function_scope_guard(Some(arg_vec));
    let _func_depth_guard = Shed::meta_mut(MetaTab::enter_func);

    // getopts OPTIND variable
    // scoped per-script and per-function call
    set_var!(b"OPTIND", VarKind::Int(1); LOCAL).ok();

    match exec_nonint(lambda.content(), None) {
      Ok(()) => Ok(()),
      Err(e) => match e.kind() {
        ShErrKind::FuncReturn(code) => util::with_status(*code),
        _ => Err(e),
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use crate::state::Shed;
  use crate::tests::testutil::{TestGuard, test_input};
  use crate::util::error::ShErrKind;

  crate::test_input! {
    passes_positional_args    : r#"call ^(printf '[%s]' "$@") a b c"#                 => "[a][b][c]";
    sets_argc                 : r#"call ^(printf '%s' $#) a b c"#                     => "3";
    no_args_means_zero_argc   : r#"call ^(printf '%s' $#)"#                           => "0";
    empty_string_is_an_arg    : r#"call ^(printf '%s' $#) """#                        => "1";
    args_keep_their_spaces    : r#"call ^(printf '[%s]' "$@") "a b" "c  d""#          => "[a b][c  d]";
    shell_name_is_untouched   : r#"z="$0"; call ^([ "$0" = "$z" ] && printf same)"#    => "same";
    return_stops_the_body     : r#"call ^(printf a; return; printf b)"#               => "a";
    return_leaves_caller_live : r#"f() { call ^(return 5); printf after; }; f"#        => "after";
    body_scope_is_its_own     : r#"x=outer; f() { call ^(local x=inner; printf '%s' "$x"); printf ' %s' "$x"; }; f"# => "inner outer";
    assignment_reaches_caller : r#"call ^(y=42); printf '%s' "$y""#                   => "42";
    optind_is_fresh_per_call  : r#"f() { getopts "ab" o; printf '%s' "$OPTIND"; call ^(printf '%s' "$OPTIND"); printf '%s' "$OPTIND"; }; f -a -b"# => "212";
    nests                     : r#"call ^(call ^(printf '%s' $#) x y)"#               => "2";
    nesting_restores_args     : r#"call ^(call ^(:) q; printf '%s' $#) a b"#           => "2";
    runs_in_a_pipeline        : r#"call ^(printf hi) | cat"#                          => "hi";
    pipeline_with_external    : r#"call ^(printf hi) | cat | cat"#                    => "hi";
  }

  #[test]
  fn propagates_failure_status() {
    let _g = TestGuard::new();
    test_input("call ^(false)").unwrap();
    assert_eq!(Shed::get_status(), 1);
  }

  #[test]
  fn propagates_success_status() {
    let _g = TestGuard::new();
    test_input("call ^(true)").unwrap();
    assert_eq!(Shed::get_status(), 0);
  }

  #[test]
  fn return_sets_the_call_status() {
    let _g = TestGuard::new();
    test_input("call ^(return 3)").unwrap();
    assert_eq!(Shed::get_status(), 3);
  }

  #[test]
  fn bare_return_inherits_status() {
    let _g = TestGuard::new();
    test_input("call ^(false; return)").unwrap();
    assert_eq!(Shed::get_status(), 1);
  }

  #[test]
  fn exit_in_body_is_not_swallowed() {
    let _g  = TestGuard::new();
    let err = test_input("call ^(exit 9)").expect_err("exit must propagate");
    assert!(matches!(err.kind(), ShErrKind::CleanExit(9)));
  }

  #[test]
  fn raise_in_body_is_not_swallowed() {
    let _g  = TestGuard::new();
    let err = test_input("call ^(raise ExecFail boom)").expect_err("raise must propagate");
    assert!(matches!(err.kind(), ShErrKind::Raised(..)));
  }

  #[test]
  fn missing_command_fails() {
    let _g = TestGuard::new();
    let _  = test_input("call");
    assert_ne!(Shed::get_status(), 0);
  }
}
