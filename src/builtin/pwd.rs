use crate::{
  util::error::ShResult,
  parse::{NdRule, Node},
  prelude::*,
  procio::borrow_fd,
  state,
};

pub fn pwd(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv: _,
  } = node.class
  else {
    unreachable!()
  };

  let stdout = borrow_fd(STDOUT_FILENO);

  let mut curr_dir = env::current_dir().unwrap().to_str().unwrap().to_string();
  curr_dir.push('\n');
  write(stdout, curr_dir.as_bytes())?;

  state::set_status(0);
  Ok(())
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::testutil::{TestGuard, test_input};
  use std::env;
  use tempfile::TempDir;

  #[test]
  fn pwd_prints_cwd() {
    let guard = TestGuard::new();
    let cwd = env::current_dir().unwrap();

    test_input("pwd").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), cwd.display().to_string());
  }

  #[test]
  fn pwd_after_cd() {
    let guard = TestGuard::new();
    let tmp = TempDir::new().unwrap();

    test_input(format!("cd {}", tmp.path().display())).unwrap();
    guard.read_output();

    test_input("pwd").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), tmp.path().display().to_string());
  }

  #[test]
  fn pwd_status_zero() {
    let _g = TestGuard::new();
    test_input("pwd").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
