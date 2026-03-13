use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node, execute::prepare_argv},
  prelude::*,
  state::{self, source_file},
};

pub fn source(node: Node) -> ShResult<()> {
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

  for (arg, span) in argv {
    let path = PathBuf::from(arg);
    if !path.exists() {
      return Err(ShErr::at(
        ShErrKind::ExecFail,
        span,
        format!("source: File '{}' not found", path.display()),
      ));
    }
    if !path.is_file() {
      return Err(ShErr::at(
        ShErrKind::ExecFail,
        span,
        format!("source: Given path '{}' is not a file", path.display()),
      ));
    }
    source_file(path)?;
  }

  state::set_status(0);
  Ok(())
}

#[cfg(test)]
pub mod tests {
  use std::io::Write;

  use crate::state::{self, read_logic, read_vars};
  use crate::testutil::{TestGuard, test_input};
  use tempfile::{NamedTempFile, TempDir};

  #[test]
  fn source_simple() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"some_var=some_val").unwrap();

    test_input(format!("source {path}")).unwrap();
    let var = read_vars(|v| v.get_var("some_var"));
    assert_eq!(var, "some_val".to_string());
  }

  #[test]
  fn source_multiple_commands() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"x=1\ny=2\nz=3").unwrap();

    test_input(format!("source {path}")).unwrap();
    assert_eq!(read_vars(|v| v.get_var("x")), "1");
    assert_eq!(read_vars(|v| v.get_var("y")), "2");
    assert_eq!(read_vars(|v| v.get_var("z")), "3");
  }

  #[test]
  fn source_defines_function() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"greet() { echo hi; }").unwrap();

    test_input(format!("source {path}")).unwrap();
    let func = read_logic(|l| l.get_func("greet"));
    assert!(func.is_some());
  }

  #[test]
  fn source_defines_alias() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"alias ll='ls -la'").unwrap();

    test_input(format!("source {path}")).unwrap();
    let alias = read_logic(|l| l.get_alias("ll"));
    assert!(alias.is_some());
  }

  #[test]
  fn source_output_captured() {
    let guard = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"echo sourced").unwrap();

    test_input(format!("source {path}")).unwrap();
    let out = guard.read_output();
    assert!(out.contains("sourced"));
  }

  #[test]
  fn source_multiple_files() {
    let _g = TestGuard::new();
    let mut file1 = NamedTempFile::new().unwrap();
    let mut file2 = NamedTempFile::new().unwrap();
    let path1 = file1.path().display().to_string();
    let path2 = file2.path().display().to_string();
    file1.write_all(b"a=from_file1").unwrap();
    file2.write_all(b"b=from_file2").unwrap();

    test_input(format!("source {path1} {path2}")).unwrap();
    assert_eq!(read_vars(|v| v.get_var("a")), "from_file1");
    assert_eq!(read_vars(|v| v.get_var("b")), "from_file2");
  }

  // ===================== Dot syntax =====================

  #[test]
  fn source_dot_syntax() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"dot_var=dot_val").unwrap();

    test_input(format!(". {path}")).unwrap();
    assert_eq!(read_vars(|v| v.get_var("dot_var")), "dot_val");
  }

  // ===================== Error cases =====================

  #[test]
  fn source_nonexistent_file() {
    let _g = TestGuard::new();
    let result = test_input("source /tmp/__no_such_file_xyz__");
    assert!(result.is_err());
  }

  #[test]
  fn source_directory_fails() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let result = test_input(format!("source {}", dir.path().display()));
    assert!(result.is_err());
  }

  // ===================== Status =====================

  #[test]
  fn source_status_zero() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"true").unwrap();

    test_input(format!("source {path}")).unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
