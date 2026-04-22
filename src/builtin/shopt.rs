use crate::{
  util::error::{ShResult, ShResultExt},
  parse::{NdRule, Node, execute::prepare_argv},
  prelude::*,
  procio::borrow_fd,
  state::{self, write_shopts},
};

pub fn shopt(node: Node) -> ShResult<()> {
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

  if argv.is_empty() {
    let mut output = write_shopts(|s| s.display_opts())?;

    let output_channel = borrow_fd(STDOUT_FILENO);
    output.push('\n');

    write(output_channel, output.as_bytes())?;
    state::set_status(0);
    return Ok(());
  }

  for (arg, span) in argv {
    let Some(mut output) = write_shopts(|s| s.query(&arg)).promote_err(span)? else {
      continue;
    };

    let output_channel = borrow_fd(STDOUT_FILENO);
    output.push('\n');

    write(output_channel, output.as_bytes())?;
  }

  state::set_status(0);
  Ok(())
}

#[cfg(test)]
mod tests {
  use crate::state::{self, read_shopts};
  use crate::testutil::{TestGuard, test_input};

  // ===================== Display =====================

  #[test]
  fn shopt_no_args_displays_all() {
    let guard = TestGuard::new();
    test_input("shopt").unwrap();
    let out = guard.read_output();
    assert!(out.contains("dotglob"));
    assert!(out.contains("autocd"));
    assert!(out.contains("max_hist"));
    assert!(out.contains("comp_limit"));
  }

  #[test]
  fn shopt_query_category() {
    let guard = TestGuard::new();
    test_input("shopt core").unwrap();
    let out = guard.read_output();
    assert!(out.contains("dotglob"));
    assert!(out.contains("autocd"));
    // Should not contain prompt opts
    assert!(!out.contains("comp_limit"));
  }

  #[test]
  fn shopt_query_single_opt() {
    let guard = TestGuard::new();
    test_input("shopt core.dotglob").unwrap();
    let out = guard.read_output();
    assert!(out.contains("false"));
  }

  // ===================== Set =====================

  #[test]
  fn shopt_set_bool() {
    let _g = TestGuard::new();
    test_input("shopt core.dotglob=true").unwrap();
    assert!(read_shopts(|o| o.core.dotglob));
  }

  #[test]
  fn shopt_set_int() {
    let _g = TestGuard::new();
    test_input("shopt core.max_hist=500").unwrap();
    assert_eq!(read_shopts(|o| o.core.max_hist), 500);
  }

  #[test]
  fn shopt_set_string() {
    let _g = TestGuard::new();
    test_input("shopt prompt.leader=space").unwrap();
    assert_eq!(read_shopts(|o| o.prompt.leader.clone()), "space");
  }

  #[test]
  fn shopt_set_completion_ignore_case() {
    let _g = TestGuard::new();
    test_input("shopt prompt.completion_ignore_case=true").unwrap();
    assert!(read_shopts(|o| o.prompt.completion_ignore_case));
  }

  // ===================== Error cases =====================

  #[test]
  fn shopt_invalid_category() {
    let _g = TestGuard::new();
    let result = test_input("shopt bogus.dotglob");
    assert!(result.is_err());
  }

  #[test]
  fn shopt_invalid_option() {
    let _g = TestGuard::new();
    let result = test_input("shopt core.nonexistent");
    assert!(result.is_err());
  }

  #[test]
  fn shopt_invalid_value() {
    let _g = TestGuard::new();
    let result = test_input("shopt core.dotglob=notabool");
    assert!(result.is_err());
  }

  // ===================== Status =====================

  #[test]
  fn shopt_status_zero() {
    let _g = TestGuard::new();
    test_input("shopt core.autocd=true").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
