use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node, execute::prepare_argv},
};

/// Returns a ShErr that signals a control flow change (break, continue, return, or exit) with an optional status code.
/// The reason we return an Error on what is technically the "happy path" is because this is how we can unwind the call stack to the appropriate control flow construct (loop, function, or shell exit).
/// The error bubbles up until it is caught by a context that waits to catch it.
/// If the error bubbles all the way up to main, the error is printed and the status code is set to 1.
pub fn flowctl(node: Node, kind: ShErrKind) -> ShResult<()> {
	use ShErrKind as K;
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };
  let mut code = 0;

  let mut argv = prepare_argv(argv)?;
  let cmd = argv.remove(0).0;

  if !argv.is_empty() {
    let (arg, span) = argv.into_iter().next().unwrap();

    let Ok(status) = arg.parse::<i32>() else {
      return Err(ShErr::at(
        K::SyntaxErr,
        span,
        format!("{cmd}: Expected a number"),
      ));
    };

    code = status;
  }

  let (kind, message) = match kind {
    K::LoopContinue(_) => (K::LoopContinue(code), "'continue' found outside of loop"),
    K::LoopBreak(_) => (K::LoopBreak(code), "'break' found outside of loop"),
    K::FuncReturn(_) => (K::FuncReturn(code), "'return' found outside of function"),
    K::CleanExit(_) => (K::CleanExit(code), ""),
    _ => unreachable!(),
  };

  Err(ShErr::simple(kind, message))
}

#[cfg(test)]
mod tests {
  use crate::libsh::error::ShErrKind;
  use crate::state;
  use crate::testutil::{TestGuard, test_input};

  // ===================== break =====================

  #[test]
  fn break_exits_loop() {
    let guard = TestGuard::new();
    test_input("for i in 1 2 3; do echo $i; break; done").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), "1");
  }

  #[test]
  fn break_outside_loop_errors() {
    let _g = TestGuard::new();
    let result = test_input("break");
    assert!(result.is_err());
  }

  #[test]
  fn break_non_numeric_errors() {
    let _g = TestGuard::new();
    let result = test_input("for i in 1; do break abc; done");
    assert!(result.is_err());
  }

  // ===================== continue =====================

  #[test]
  fn continue_skips_iteration() {
    let guard = TestGuard::new();
    test_input("for i in 1 2 3; do if [[ $i == 2 ]]; then continue; fi; echo $i; done").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["1", "3"]);
  }

  #[test]
  fn continue_outside_loop_errors() {
    let _g = TestGuard::new();
    let result = test_input("continue");
    assert!(result.is_err());
  }

  // ===================== return =====================

  #[test]
  fn return_exits_function() {
    let guard = TestGuard::new();
    test_input("f() { echo before; return; echo after; }").unwrap();
    test_input("f").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), "before");
  }

  #[test]
  fn return_with_status() {
    let _g = TestGuard::new();
    test_input("f() { return 42; }").unwrap();
    test_input("f").unwrap();
    assert_eq!(state::get_status(), 42);
  }

  #[test]
  fn return_outside_function_errors() {
    let _g = TestGuard::new();
    let result = test_input("return");
    assert!(result.is_err());
  }

  // ===================== exit =====================

  #[test]
  fn exit_returns_clean_exit() {
    let _g = TestGuard::new();
    let result = test_input("exit 0");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err.kind(), ShErrKind::CleanExit(0)));
  }

  #[test]
  fn exit_with_code() {
    let _g = TestGuard::new();
    let result = test_input("exit 5");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err.kind(), ShErrKind::CleanExit(5)));
  }

  #[test]
  fn exit_non_numeric_errors() {
    let _g = TestGuard::new();
    let result = test_input("exit abc");
    assert!(result.is_err());
  }
}
