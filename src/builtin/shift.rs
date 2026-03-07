use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node, execute::prepare_argv},
  state::{self, write_vars},
};

pub fn shift(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }
  let mut argv = argv.into_iter();

  if let Some((arg, span)) = argv.next() {
    let Ok(count) = arg.parse::<usize>() else {
      return Err(ShErr::at(
        ShErrKind::ExecFail,
        span,
        "Expected a number in shift args",
      ));
    };
    for _ in 0..count {
      write_vars(|v| v.cur_scope_mut().fpop_arg());
    }
  }

  state::set_status(0);
  Ok(())
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
    let result = test_input("shift abc");
    assert!(result.is_err());
  }

  #[test]
  fn shift_status_zero() {
    let _g = TestGuard::new();
    test_input("f() { shift 1; }").unwrap();
    test_input("f a b").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
