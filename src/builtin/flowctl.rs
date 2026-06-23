use crate::{HashMap, util};
use std::fmt::Write;

use yansi::Paint;

use crate::{builtin::getopt::OptSpec, match_loop};

use super::{
  Shed,
  getopt::Opt,
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
    ShErr::simple(
      ShErrKind::FuncReturn(code),
      "'return' found outside of function",
    )
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
  fn flow_control(&self, code: i32) -> ShErr {
    ShErr::simple(ShErrKind::LoopBreak(code), "'break' found outside of loop")
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
  fn flow_control(&self, code: i32) -> ShErr {
    ShErr::simple(
      ShErrKind::LoopContinue(code),
      "'continue' found outside of loop",
    )
  }
}

pub(super) struct Exit;
impl super::Builtin for Exit {
  fn is_special(&self) -> bool {
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
    ShErr::simple(ShErrKind::CleanExit(code), "")
  }
}

pub(super) struct Raise;
impl super::Builtin for Raise {
  fn is_special(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<super::getopt::OptSpec> {
    vec![
      OptSpec::single_arg('c'),
      OptSpec::single_arg("code"),
      OptSpec::single_arg('k'),
      OptSpec::single_arg("kind"),
      OptSpec::single_arg('n'),
      OptSpec::single_arg("note"),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut code = 1;
    let mut kind = None;
    let mut notes = vec![];
    let span = args.cmd_span();

    for opt in &args.opts {
      match opt {
        Opt::LongWithArg(name, arg) => match name.as_str() {
          "code" => {
            let Ok(code_arg) = arg.parse::<i32>() else {
              return Err(sherr!(
                  SyntaxErr @ args.span(),
                  "Invalid exit code: expected a number, got '{arg}'",
              ));
            };
            code = code_arg;
          }
          "kind" => {
            kind = Some(arg.clone());
          }
          "note" => {
            notes.push(arg.clone());
          }
          _ => {
            return Err(sherr!(
              SyntaxErr @ args.span(),
              "Unknown option '--{name}'",
            ));
          }
        },

        Opt::ShortWithArg('k', kind_arg) => {
          kind = Some(kind_arg.clone());
        }

        Opt::ShortWithArg('n', note_arg) => {
          notes.push(note_arg.clone());
        }

        Opt::ShortWithArg('c', code_arg) => {
          let Ok(code_arg) = code_arg.parse::<i32>() else {
            return Err(sherr!(
              SyntaxErr @ args.span(),
              "Invalid exit code: expected a number, got '{code_arg}'",
            ));
          };
          code = code_arg;
        }
        opt => {
          return Err(sherr!(
            SyntaxErr @ args.span(),
            "Unknown option '{opt}'"
          ));
        }
      }
    }

    let mut message_parts = vec![];
    let mut part = String::new();
    let mut color_map: HashMap<u32, yansi::Color> = HashMap::default();
    let mut arg_iter = args.argv.into_iter();

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
                  SyntaxErr @ span,
                  "missing format arg for '%{color_id}'",
                ));
              };

              let color = color_map.get(&color_id).unwrap();
              let painted = arg.paint(*color);

              write!(&mut part, "{painted}").ok();
            }
            _ => {
              return Err(sherr!(
                SyntaxErr @ span,
                "Invalid format specifier: '%{n_ch}'",
              ).with_note("'raise' only takes digits or '%' after '%'").with_note("to include a literal '%', use '%%'"));
            }
          }
        }
        _ => part.push(ch),
      });
      message_parts.push(std::mem::take(&mut part));
    }

    let message = message_parts.join(" ");
    let mut error = ShErr::at(ShErrKind::Raised(kind, code), span, message);

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
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn exit_with_code() {
    let _g = TestGuard::new();
    test_input("exit 5").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn exit_non_numeric_errors() {
    let _g = TestGuard::new();
    test_input("exit abc").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
