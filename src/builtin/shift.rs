use crate::{
  sherr,
  state::write_vars,
  util::{error::ShResult, with_status},
};

pub(super) struct Shift;
impl super::Builtin for Shift {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut argv = args.argv.into_iter();

    let count = argv
      .next()
      .map(|(st, sp)| {
        st.parse::<usize>().map_err(|_| {
          sherr!(
            ExecFail @ sp,
            "Expected a number in shift args",
          )
        })
      })
      .unwrap_or(Ok(1))?;

    for _ in 0..count {
      write_vars(|v| v.cur_scope_mut().fpop_arg());
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::testutil::{TestGuard, test_input};

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
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn shift_status_zero() {
    let _g = TestGuard::new();
    test_input("f() { shift 1; }").unwrap();
    test_input("f a b").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
