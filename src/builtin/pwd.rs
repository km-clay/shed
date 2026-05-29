use std::{
  env,
  path::{Component, Path, PathBuf},
};

use super::{
  ShResult,
  getopt::{Opt, OptSpec},
  outln, sherr, try_var, with_status,
};

pub(super) struct Pwd;
impl super::Builtin for Pwd {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::flag('P'), OptSpec::flag('L')]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut use_pwd = true; // whether to use $PWD first (-L)

    for opt in &args.opts {
      match opt {
        Opt::Short('P') => use_pwd = false,
        Opt::Short('L') => use_pwd = true,
        _ => return Err(sherr!(ParseErr @ args.span, "Invalid option: {opt}")),
      }
    }
    if use_pwd {
      // -L
      let pwd = try_var!("PWD").map(PathBuf::from).unwrap_or("".into());
      if is_clean_absolute_path(&pwd) {
        let pwd = pwd.display();
        outln!("{pwd}");
      } else {
        use_pwd = false; // behaves like -P in this case
      }
    }
    if !use_pwd {
      // -P
      let cwd = env::current_dir()?;
      let cwd = cwd.display();
      outln!("{cwd}");
    }
    with_status(0)
  }
}

/// whether path is (absolute and not contain ., ..)
fn is_clean_absolute_path(path: &Path) -> bool {
  path.is_absolute()
    && !path
      .components()
      .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::tests::testutil::{TestGuard, canon, test_input};
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
    assert_eq!(out.trim(), canon(tmp.path()).display().to_string());
  }

  #[test]
  fn pwd_status_zero() {
    let _g = TestGuard::new();
    test_input("pwd").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }
}
