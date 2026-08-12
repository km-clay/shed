use crate::{HashMap, opt, util};
use std::fmt::Write;

use yansi::Paint;

use crate::match_loop;

use super::{
  Shed,
  opt::OptSpec,
  sherr,
  util::{ShErr, ShErrKind, ShResult, ShResultExt},
};

/// A trait for flow control builtins (break, continue, return, exit).
///
/// The way flowctl works in `shed` is by leveraging Rust's error propagation to unwind the call stack until it reaches the appropriate control flow construct (loop, function, or shell exit).
/// This doubles as a true error propagation, if the error created never reaches a context that waits to catch it, it will bubble all the way up to main, where it will be printed.
trait FlowCtl: super::Builtin {
  fn flow_control(&self, code: i32) -> ShErr;
  fn cmd(&self) -> &'static str;
  fn default_code(&self) -> i32 {
    0
  }
  fn exec_flow_ctl(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let code = args
      .arguments()
      .next()
      .map(|(st, sp)| {
        st.parse::<i32>().map_err(|_| {
          sherr!(
            SyntaxErr @ sp.clone(),
            "{}: Expected a number",
            self.cmd(),
          )
        })
      })
      .transpose()?
      .unwrap_or_else(|| self.default_code());

    Err(self.flow_control(code)).promote_err(args.span)
  }
}

pub(super) struct Return;
impl super::Builtin for Return {
  fn is_special(&self) -> bool {
    true
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_flow_ctl(args)
  }
}
impl FlowCtl for Return {
  fn cmd(&self) -> &'static str {
    "return"
  }
  fn default_code(&self) -> i32 {
    Shed::get_status()
  }
  fn flow_control(&self, code: i32) -> ShErr {
    sherr!(FuncReturn(code), "'return' found outside of function",)
  }
}

pub(super) struct Break;
impl super::Builtin for Break {
  fn is_special(&self) -> bool {
    true
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_flow_ctl(args)
  }
}
impl FlowCtl for Break {
  fn cmd(&self) -> &'static str {
    "break"
  }
  /// The "code" for `break` actually means the number of outer loops to break out of
  ///
  /// POSIX 1-indexes this, so we return `1`.
  fn default_code(&self) -> i32 {
    1
  }
  fn flow_control(&self, count: i32) -> ShErr {
    if count <= 0 {
      sherr!(
        ParseErr,
        "'break' count must be a positive integer, got {count}",
      )
    } else {
      sherr!(LoopBreak(count), "'break' found outside of loop",)
    }
  }
}

pub(super) struct Continue;
impl super::Builtin for Continue {
  fn is_special(&self) -> bool {
    true
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_flow_ctl(args)
  }
}
impl FlowCtl for Continue {
  fn cmd(&self) -> &'static str {
    "continue"
  }
  fn default_code(&self) -> i32 {
    1
  }
  fn flow_control(&self, count: i32) -> ShErr {
    if count <= 0 {
      sherr!(
        ParseErr,
        "'continue' count must be a positive integer, got {count}",
      )
    } else {
      sherr!(LoopContinue(count), "'continue' found outside of loop",)
    }
  }
}

pub(super) struct Exit;
impl super::Builtin for Exit {
  fn is_special(&self) -> bool {
    true
  }

  fn always_forks(&self) -> bool {
    true
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_flow_ctl(args)
  }
}
impl FlowCtl for Exit {
  fn cmd(&self) -> &'static str {
    "exit"
  }
  fn default_code(&self) -> i32 {
    Shed::get_status()
  }
  fn flow_control(&self, code: i32) -> ShErr {
    sherr!(CleanExit(code), "",)
  }
}

pub(super) struct Raise;
impl super::Builtin for Raise {
  fn is_special(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("code" | 'c', 1),
      opt!("kind" | 'k', 1),
      opt!("note" | 'n', 1),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut code = 1;
    let mut kind = None;
    let mut notes = vec![];
    let span = args.cmd_span();

    for opt in args.options() {
      match opt.key() {
        "code" => {
          let Some(c) = opt.value() else {
            return Err(sherr!(
              SyntaxErr @ opt.span(),
              "Option '--code' requires an argument",
            ));
          };
          let Ok(code_arg) = c.parse::<i32>() else {
            return Err(sherr!(
              SyntaxErr @ opt.span(),
              "Invalid exit code: expected a number, got '{code}'",
            ));
          };

          code = code_arg;
        }
        "kind" => {
          let Some(k) = opt.value() else {
            return Err(sherr!(
              SyntaxErr @ opt.span(),
              "Option '--kind' requires an argument",
            ));
          };
          kind = Some(k.into());
        }
        "note" => {
          let Some(n) = opt.value() else {
            return Err(sherr!(
              SyntaxErr @ opt.span(),
              "Option '--note' requires an argument",
            ));
          };
          notes.push(n.into());
        }
        _ => {
          return Err(sherr!(
            SyntaxErr @ opt.span(),
            "Unknown option '{opt}'"
          ));
        }
      }
    }

    let mut message_parts = vec![];
    let mut part = util::scratch_buf();
    let mut color_map: HashMap<u32, yansi::Color> = HashMap::default();
    let mut arg_iter = args.arguments();

    while let Some((arg, span)) = arg_iter.next() {
      let mut chars = arg.chars().peekable();
      match_loop!(chars.next() => ch, {
        '%' => {
          let Some(n_ch) = chars.next() else {
            part.push('%');
            break;
          };
          let mut color_id = util::scratch_buf();
          match n_ch {
            '%' => part.push('%'),
            _ if n_ch.is_ascii_digit() => {
              color_id.push(n_ch);

              while let Some(&next_ch) = chars.peek()
                && next_ch.is_ascii_digit()
              {
                chars.next();
                color_id.push(next_ch);
              }

              let color_id = color_id.parse::<u32>().map_err(|_| {
                sherr!(
                  SyntaxErr @ span.clone(),
                  "Invalid color code: expected a number, got '{color_id}'",
                )
              })?;
              color_map.entry(color_id).or_insert_with(crate::util::error::next_color);

              let Some((arg,_)) = arg_iter.next() else {
                return Err(sherr!(
                  SyntaxErr @ span.clone(),
                  "missing format arg for '%{color_id}'",
                ));
              };

              let color = color_map.get(&color_id).unwrap();
              let painted = arg.paint(*color);

              write!(&mut part, "{painted}").ok();
            }
            _ => {
              return Err(sherr!(
                SyntaxErr @ span.clone(),
                "Invalid format specifier: '%{n_ch}'",
              ).with_note("'raise' only takes digits or '%' after '%'".into()).with_note("to include a literal '%', use '%%'".into()));
            }
          }
        }
        _ => part.push(ch),
      });
      message_parts.push(std::mem::take(&mut part));
    }

    let message = message_parts.join(" ");
    let mut error = ShErr::at(ShErrKind::Raised(kind, code), span, message.into());

    for note in notes {
      error = error.with_note(note);
    }

    Err(error)
  }
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  // ===================== return/exit status masking =====================

  #[test]
  fn return_status_masked_to_byte() {
    // `$?` is an 8-bit value; `return N` uses `N & 255` (get_status masks).
    let _g = TestGuard::new();
    for (arg, expect) in [(300, 44), (256, 0), (257, 1), (-1, 255), (-2, 254)] {
      test_input(format!("f() {{ return {arg}; }}; f")).unwrap();
      assert_eq!(
        state::Shed::get_status(),
        expect,
        "return {arg} should give $?={expect}"
      );
    }
  }

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
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn break_non_numeric_errors() {
    let _g = TestGuard::new();
    test_input("for i in 1; do break abc; done").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn break_one_is_innermost_only() {
    // POSIX `break 1` is equivalent to bare `break`: it exits only the
    // innermost loop, so the outer loop keeps iterating.
    let guard = TestGuard::new();
    test_input("for i in 1 2; do for j in a b; do echo $i$j; break 1; done; done").unwrap();
    assert_eq!(
      guard.read_output().split_whitespace().collect::<Vec<_>>(),
      ["1a", "2a"]
    );
  }

  #[test]
  fn break_n_exits_n_enclosing_loops() {
    // `break 2` escapes both loops, so the outer body after the inner loop
    // and the outer loop's remaining iterations are skipped.
    let guard = TestGuard::new();
    test_input(
      "for i in 1 2; do for j in a b; do echo $i$j; break 2; done; echo in$i; done; echo end",
    )
    .unwrap();
    assert_eq!(
      guard.read_output().split_whitespace().collect::<Vec<_>>(),
      ["1a", "end"]
    );
  }

  #[test]
  fn break_count_exceeding_nesting_clamps_to_outermost() {
    // POSIX: if n is greater than the number of enclosing loops, the outermost
    // loop is exited (not an error). `break 5` inside 2 loops == `break 2`.
    let guard = TestGuard::new();
    test_input(
      "for i in 1 2; do for j in a b; do echo $i$j; break 5; done; echo in$i; done; echo end",
    )
    .unwrap();
    assert_eq!(
      guard.read_output().split_whitespace().collect::<Vec<_>>(),
      ["1a", "end"]
    );
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn break_zero_is_rejected() {
    // POSIX requires n >= 1; `break 0` is an error and must not break.
    let _g = TestGuard::new();
    test_input("for i in 1; do break 0; done").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn break_leaves_status_zero() {
    // `break` is a successful builtin (exit status 0), so it does not carry
    // the loop body's prior status out of the loop.
    let _g = TestGuard::new();
    test_input("for i in 1; do false; break; done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
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
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn continue_one_is_innermost_only() {
    // `continue 1` == bare `continue`: it resumes the innermost loop, so every
    // inner iteration still runs.
    let guard = TestGuard::new();
    test_input("for i in 1 2; do for j in a b; do echo $i$j; continue 1; echo un; done; done")
      .unwrap();
    assert_eq!(
      guard.read_output().split_whitespace().collect::<Vec<_>>(),
      ["1a", "1b", "2a", "2b"]
    );
  }

  #[test]
  fn continue_n_resumes_nth_enclosing_loop() {
    // `continue 2` abandons the inner loop and resumes the OUTER loop's next
    // iteration, so `echo un` and the rest of the inner loop are skipped.
    let guard = TestGuard::new();
    test_input("for i in 1 2; do for j in a b; do echo $i$j; continue 2; echo un; done; echo in$i; done; echo end")
      .unwrap();
    assert_eq!(
      guard.read_output().split_whitespace().collect::<Vec<_>>(),
      ["1a", "2a", "end"]
    );
  }

  #[test]
  fn continue_count_exceeding_nesting_clamps_to_outermost() {
    // POSIX: n greater than the nesting resumes the outermost loop.
    // `continue 5` inside 2 loops == `continue 2`.
    let guard = TestGuard::new();
    test_input("for i in 1 2; do for j in a b; do echo $i$j; continue 5; echo un; done; echo in$i; done; echo end")
      .unwrap();
    assert_eq!(
      guard.read_output().split_whitespace().collect::<Vec<_>>(),
      ["1a", "2a", "end"]
    );
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn continue_zero_is_rejected() {
    let _g = TestGuard::new();
    test_input("for i in 1; do continue 0; done").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn continue_leaves_status_zero() {
    // `continue` is a successful builtin (exit status 0).
    let _g = TestGuard::new();
    test_input("for i in 1; do false; continue; done").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
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
    assert_eq!(state::Shed::get_status(), 42);
  }

  #[test]
  fn bare_return_propagates_last_status() {
    // POSIX: `return` with no argument exits with the status of the last
    // command executed, not 0.
    let _g = TestGuard::new();
    test_input("f() { false; return; }").unwrap();
    test_input("f").unwrap();
    assert_eq!(state::Shed::get_status(), 1);

    test_input("g() { true; return; }").unwrap();
    test_input("g").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn return_outside_function_errors() {
    let _g = TestGuard::new();
    test_input("return").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn return_in_piped_function() {
    let _g = TestGuard::new();
    // Regression: `return` inside a function on the LHS of a pipeline used
    // to surface as 'return found outside of function' because exec_func's
    // FuncReturn catch ran in the parent while the function body ran in a
    // fork via FORK_BUILTINS. Now exec_func forks itself when the flag is
    // set so the catch is in the same process as the body.
    test_input("piped_ret() { return 42; }").unwrap();
    test_input("piped_ret | cat").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn early_return_in_piped_function_works() {
    let guard = TestGuard::new();
    // Sanity: function with conditional return on LHS of pipeline,
    // verify the output (not just exit status) is right.
    test_input("early_ret() { echo before; return; echo after; }").unwrap();
    test_input("early_ret | cat").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), "before");
  }

  // ===================== exit =====================

  #[test]
  fn exit_returns_clean_exit() {
    let _g = TestGuard::new();
    test_input("exit 0").ok();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn exit_with_code() {
    let _g = TestGuard::new();
    test_input("exit 5").ok();
    assert_eq!(state::Shed::get_status(), 5);
  }

  #[test]
  fn exit_non_numeric_errors() {
    let _g = TestGuard::new();
    test_input("exit abc").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
