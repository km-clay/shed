use ariadne::Fmt;
use yansi::Color;

use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult, next_color},
  parse::{NdRule, Node, execute::prepare_argv},
  prelude::*,
  sherr,
  state::{self},
};

pub fn cd(node: Node) -> ShResult<()> {
  let span = node.get_span();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };
  let cd_span = argv.first().unwrap().span.clone();

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }

  let (new_dir, arg_span) = if let Some((arg, span)) = argv.into_iter().next() {
    (PathBuf::from(arg), Some(span))
  } else {
    (PathBuf::from(env::var("HOME").unwrap()), None)
  };

  if !new_dir.exists() {
    let mut err = sherr!(ExecFail @ span.clone(), "Failed to change directory");
    if let Some(span) = arg_span {
      err = err.labeled(
        span,
        format!(
          "No such file or directory '{}'",
          new_dir.display().fg(next_color())
        ),
      );
    }
    return Err(err);
  }

  if !new_dir.is_dir() {
    return Err(ShErr::new(ShErrKind::ExecFail, span.clone()).labeled(
      cd_span.clone(),
      format!(
        "cd: Not a directory '{}'",
        new_dir.display().fg(next_color())
      ),
    ));
  }

  if let Err(e) = state::change_dir(new_dir) {
    return Err(ShErr::new(ShErrKind::ExecFail, span.clone()).labeled(
      cd_span.clone(),
      format!("cd: Failed to change directory: '{}'", e.fg(Color::Red)),
    ));
  }
  let new_dir = env::current_dir().map_err(|e| {
    ShErr::new(ShErrKind::ExecFail, span.clone()).labeled(
      cd_span.clone(),
      format!(
        "cd: Failed to get current directory: '{}'",
        e.fg(Color::Red)
      ),
    )
  })?;
  unsafe { env::set_var("PWD", new_dir) };

  state::set_status(0);
  Ok(())
}

#[cfg(test)]
pub mod tests {
  use std::env;
  use std::fs;

  use tempfile::TempDir;

  use crate::state;
  use crate::testutil::{TestGuard, test_input};

  // ===================== Basic Navigation =====================

  #[test]
  fn cd_simple() {
    let _g = TestGuard::new();
    let old_dir = env::current_dir().unwrap();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    let new_dir = env::current_dir().unwrap();
    assert_ne!(old_dir, new_dir);

    assert_eq!(
      new_dir.display().to_string(),
      temp_dir.path().display().to_string()
    );
  }

  #[test]
  fn cd_no_args_goes_home() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    unsafe { env::set_var("HOME", temp_dir.path()) };

    test_input("cd").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      temp_dir.path().display().to_string()
    );
  }

  #[test]
  fn cd_relative_path() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let sub = temp_dir.path().join("child");
    fs::create_dir(&sub).unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();
    test_input("cd child").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), sub.display().to_string());
  }

  // ===================== Environment =====================

  #[test]
  fn cd_sets_pwd_env() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    let pwd = env::var("PWD").unwrap();
    assert_eq!(pwd, env::current_dir().unwrap().display().to_string());
  }

  #[test]
  fn cd_status_zero_on_success() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    assert_eq!(state::get_status(), 0);
  }

  // ===================== Error Cases =====================

  #[test]
  fn cd_nonexistent_dir_fails() {
    let _g = TestGuard::new();
    let result = test_input("cd /nonexistent_path_that_does_not_exist_xyz");
    assert!(result.is_err());
  }

  #[test]
  fn cd_file_not_directory_fails() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("afile.txt");
    fs::write(&file_path, "hello").unwrap();

    let result = test_input(format!("cd {}", file_path.display()));
    assert!(result.is_err());
  }

  // ===================== Multiple cd =====================

  #[test]
  fn cd_multiple_times() {
    let _g = TestGuard::new();
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    test_input(format!("cd {}", dir_a.path().display())).unwrap();
    assert_eq!(
      env::current_dir().unwrap().display().to_string(),
      dir_a.path().display().to_string()
    );

    test_input(format!("cd {}", dir_b.path().display())).unwrap();
    assert_eq!(
      env::current_dir().unwrap().display().to_string(),
      dir_b.path().display().to_string()
    );
  }

  #[test]
  fn cd_nested_subdirectories() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let deep = temp_dir.path().join("a").join("b").join("c");
    fs::create_dir_all(&deep).unwrap();

    test_input(format!("cd {}", deep.display())).unwrap();
    assert_eq!(
      env::current_dir().unwrap().display().to_string(),
      deep.display().to_string()
    );
  }

  // ===================== Autocmd Integration =====================

  #[test]
  fn cd_fires_post_change_dir_autocmd() {
    let guard = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input("autocmd post-change-dir 'echo cd-hook-fired'").unwrap();
    guard.read_output();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();
    let out = guard.read_output();
    assert!(out.contains("cd-hook-fired"));
  }

  #[test]
  fn cd_fires_pre_change_dir_autocmd() {
    let guard = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input("autocmd pre-change-dir 'echo pre-cd'").unwrap();
    guard.read_output();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();
    let out = guard.read_output();
    assert!(out.contains("pre-cd"));
  }
}
