use super::{
  Shed, sherr,
  util::{ShResult, with_status},
};

pub(super) struct Shift;
impl super::Builtin for Shift {
  fn is_special(&self) -> bool {
    true
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut arg_vec = args.argv.into_iter();

    let count = arg_vec.next().map_or(Ok(1), |(st, sp)| {
      st.parse::<usize>().map_err(|_| {
        sherr!(
          ExecFail @ sp,
          "Expected a number in shift args",
        )
      })
    })?;

    for _ in 0..count {
      Shed::vars_mut(|v| v.sh_argv_scope_mut().fpop_arg());
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  #[test]
  fn shift_in_function() {
    let guard = TestGuard::new();
    test_input("f() { echo $1; shift 1; echo $1; }").unwrap();
    test_input("f a b").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "a");
    assert_eq!(lines[1], "b");
  }

  #[test]
  fn shift_multiple() {
    let guard = TestGuard::new();
    test_input("f() { shift 2; echo $1; }").unwrap();
    test_input("f a b c").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), "c");
  }

  #[test]
  fn shift_all_params() {
    let guard = TestGuard::new();
    test_input("f() { shift 3; echo \"[$1]\"; }").unwrap();
    test_input("f a b c").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), "[]");
  }

  #[test]
  fn shift_non_numeric_fails() {
    let _g = TestGuard::new();
    test_input("shift abc").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn shift_status_zero() {
    let _g = TestGuard::new();
    test_input("f() { shift 1; }").unwrap();
    test_input("f a b").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn shift_to_empty_does_not_panic() {
    // Regression: update_arg_params used to slice [1..] and subtract 1
    // unconditionally, panicking when sh_argv hit length 0. Common path
    // to trigger: getopts in a function with only flag args, followed
    // by `shift $((OPTIND - 1))` consuming the last positional.
    let guard = TestGuard::new();
    test_input(
      r#"
        f() {
          while getopts ":n" opt; do :; done
          shift $((OPTIND - 1))
          echo "count: $#"
          echo "all: [$*]"
        }
        f -n
      "#,
    )
    .unwrap();
    let out = guard.read_output();
    assert_eq!(out, "count: 0\nall: []\n");
  }
}
