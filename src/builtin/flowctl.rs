use crate::{
  sherr,
  util::error::{ShErr, ShErrKind, ShResult, ShResultExt},
};

/// A trait for flow control builtins (break, continue, return, exit).
///
/// The way flowctl works in `shed` is by leveraging Rust's error propagation to unwind the call stack until it reaches the appropriate control flow construct (loop, function, or shell exit).
/// This doubles as a true error propagation, if the error created never reaches a context that waits to catch it, it will bubble all the way up to main, where it will be printed.
trait FlowCtl: super::Builtin {
  fn flow_control(&self, code: i32) -> ShErr;
  fn cmd(&self) -> &'static str;
  fn exec_flow_ctl(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let code = args
      .argv
      .into_iter()
      .next()
      .map(|(st, sp)| {
        st.parse::<i32>().map_err(|_| {
          sherr!(
            SyntaxErr @ sp,
            "{}: Expected a number",
            self.cmd(),
          )
        })
      })
      .transpose()?
      .unwrap_or(0);

    Err(self.flow_control(code)).promote_err(args.span)
  }
}

pub(super) struct Return;
impl super::Builtin for Return {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_flow_ctl(args)
  }
}
impl FlowCtl for Return {
  fn cmd(&self) -> &'static str {
    "return"
  }
  fn flow_control(&self, code: i32) -> ShErr {
    ShErr::simple(
      ShErrKind::FuncReturn(code),
      "'return' found outside of function",
    )
  }
}

pub(super) struct Break;
impl super::Builtin for Break {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_flow_ctl(args)
  }
}
impl FlowCtl for Break {
  fn cmd(&self) -> &'static str {
    "break"
  }
  fn flow_control(&self, code: i32) -> ShErr {
    ShErr::simple(ShErrKind::LoopBreak(code), "'break' found outside of loop")
  }
}

pub(super) struct Continue;
impl super::Builtin for Continue {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_flow_ctl(args)
  }
}
impl FlowCtl for Continue {
  fn cmd(&self) -> &'static str {
    "continue"
  }
  fn flow_control(&self, code: i32) -> ShErr {
    ShErr::simple(
      ShErrKind::LoopContinue(code),
      "'continue' found outside of loop",
    )
  }
}

pub(super) struct Exit;
impl super::Builtin for Exit {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_flow_ctl(args)
  }
}
impl FlowCtl for Exit {
  fn cmd(&self) -> &'static str {
    "exit"
  }
  fn flow_control(&self, code: i32) -> ShErr {
    ShErr::simple(ShErrKind::CleanExit(code), "")
  }
}

#[cfg(test)]
mod tests {
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
    test_input("break").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn break_non_numeric_errors() {
    let _g = TestGuard::new();
    test_input("for i in 1; do break abc; done").ok();
    assert_ne!(state::get_status(), 0);
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
    test_input("continue").ok();
    assert_ne!(state::get_status(), 0);
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
    test_input("return").ok();
    assert_ne!(state::get_status(), 0);
  }

  // ===================== exit =====================

  #[test]
  fn exit_returns_clean_exit() {
    let _g = TestGuard::new();
    test_input("exit 0").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn exit_with_code() {
    let _g = TestGuard::new();
    test_input("exit 5").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn exit_non_numeric_errors() {
    let _g = TestGuard::new();
    test_input("exit abc").ok();
    assert_ne!(state::get_status(), 0);
  }
}
