use crate::{
  libsh::error::ShResult,
  parse::{
    NdRule, Node,
    execute::{exec_input, prepare_argv},
  },
  state,
};

pub fn eval(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut expanded_argv = prepare_argv(argv)?;
  if !expanded_argv.is_empty() {
    expanded_argv.remove(0);
  }

  if expanded_argv.is_empty() {
    state::set_status(0);
    return Ok(());
  }

  let joined_argv = expanded_argv
    .into_iter()
    .map(|(s, _)| s)
    .collect::<Vec<_>>()
    .join(" ");

  exec_input(joined_argv, None, false, Some("eval".into()))
}

#[cfg(test)]
mod tests {
  use crate::state::{self, VarFlags, VarKind, read_vars, write_vars};
  use crate::testutil::{TestGuard, test_input};

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
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn eval_status_zero() {
    let _g = TestGuard::new();
    test_input("eval true").unwrap();
    assert_eq!(state::get_status(), 0);
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
    write_vars(|v| v.set_var("CMD", VarKind::Str("echo evaluated".into()), VarFlags::NONE))
      .unwrap();

    test_input("eval $CMD").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "evaluated\n");
  }

  #[test]
  fn eval_sets_variable() {
    let _g = TestGuard::new();
    test_input("eval x=42").unwrap();
    let val = read_vars(|v| v.get_var("x"));
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
    let _ = test_input("eval false");
    assert_ne!(state::get_status(), 0);
  }
}
