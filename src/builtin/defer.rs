use crate::{builtin::join_raw_args, getopt::{Opt, OptSpec}, outln, state::{read_vars, write_vars}, util::{error::ShResult, with_status}};

pub(super) struct Defer;
impl super::Builtin for Defer {
  fn opts(&self) -> Vec<crate::getopt::OptSpec> {
    vec![
      OptSpec::flag('c')
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> crate::util::error::ShResult<()> {
    if args.argv.is_empty() {
      read_vars(|s| -> ShResult<()> {
        for line in s.cur_scope().display_deferred_cmds().lines() {
          outln!("{line}")?;
        }
        Ok(())
      })?;
      return with_status(0);
    }

    let clear = args.opts.contains(&Opt::Short('c'));

    let command = join_raw_args(args.argv);

    if clear {
      write_vars(|v| v.cur_scope_mut().take_deferred_cmds()); // drops them
    }

    write_vars(|v| v.cur_scope_mut().defer_cmd(command.0));

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::testutil::{TestGuard, test_input};

  // ===================== defer: scope teardown =====================

  #[test]
  fn defer_function_return_lifo() {
    // Defers fire on function return in LIFO order.
    let guard = TestGuard::new();
    test_input("foo() { defer 'echo a'; defer 'echo b'; defer 'echo c'; }").unwrap();
    test_input("foo").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines, vec!["c", "b", "a"]);
  }

  #[test]
  fn defer_brace_group_exit() {
    // Defers fire when a brace group exits.
    let guard = TestGuard::new();
    test_input("{ defer 'echo bye'; echo hi; }").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines, vec!["hi", "bye"]);
  }

  #[test]
  fn defer_brace_group_lifo() {
    let guard = TestGuard::new();
    test_input("{ defer 'echo a'; defer 'echo b'; }").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines, vec!["b", "a"]);
  }

  #[test]
  fn defer_sees_locals_at_run_time() {
    // The defer body is lazy — `$x` resolves at scope-exit, when `local x`
    // is still bound (defer runs before `ascend()`).
    let guard = TestGuard::new();
    test_input("foo() { local x=hello; defer 'echo $x'; }").unwrap();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("hello"), "defer body should see local x; got {out:?}");
  }

  #[test]
  fn defer_eager_substitution_via_command_sub() {
    // Command substitution lets you snapshot a value at defer-registration time.
    let guard = TestGuard::new();
    test_input("foo() { x=before; defer \"echo $(echo $x)\"; x=after; }").unwrap();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("before"),
      "command sub should resolve at registration time; got {out:?}");
  }

  #[test]
  fn defer_clear_flag_drops_existing() {
    // `defer -c cmd` clears all current defers and registers `cmd`.
    let guard = TestGuard::new();
    test_input("{ defer 'echo a'; defer 'echo b'; defer -c 'echo only'; }").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines, vec!["only"]);
  }

  #[test]
  fn defer_no_args_lists_current() {
    // `defer` with no args lists registered defers.
    let guard = TestGuard::new();
    test_input("{ defer 'echo a'; defer 'echo b'; defer; }").unwrap();
    let out = guard.read_output();
    // Output should contain both registered commands. Order/format depends on
    // display_deferred_cmds, but both bodies must appear before they run.
    let listing_idx = out.find("echo a").expect("defer listing should mention 'echo a'");
    assert!(out[listing_idx..].contains("echo b"),
      "defer listing should mention both; got {out:?}");
  }

  #[test]
  fn defer_nested_scope_isolation() {
    // Defers in an inner brace group run when the inner exits, not when the outer does.
    let guard = TestGuard::new();
    test_input("{ defer 'echo outer'; { defer 'echo inner'; }; echo middle; }").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    // Expected: inner brace defer fires when inner closes, then middle prints,
    // then outer defer fires when outer closes.
    assert_eq!(lines, vec!["inner", "middle", "outer"]);
  }

  #[test]
  fn defer_status_zero() {
    let _g = TestGuard::new();
    test_input("defer 'echo unused'").unwrap();
    assert_eq!(crate::state::get_status(), 0);
  }
}
