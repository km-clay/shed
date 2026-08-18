use std::{env, fs};

use crate::procio::outln_bytes;
use crate::state::util;
use crate::state::vars::VarStr;

use super::{ShResult, opt::OptSpec, sherr, try_var, with_status};

pub(super) struct Pwd;
impl super::Builtin for Pwd {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("logical", 'L'),
      OptSpec::new_short("physical", 'P'),
    ]
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut logical = true;

    for opt in args.options() {
      match opt.key() {
        "logical" => logical = true,
        "physical" => logical = false,
        _ => return Err(sherr!(ParseErr @ opt.span(), "Invalid option: {opt}")),
      }
    }

    if !args.no_arguments() {
      return Err(sherr!(ParseErr @ args.span, "pwd: too many arguments"));
    }

    let dir: Option<VarStr> = if logical {
      try_var!("PWD")
        .filter(|p| is_same_dir_as_cwd(p.as_bytes()))
        .or_else(|| physical_cwd().map(|p| util::path_to_varstr(&p)))
    } else {
      physical_cwd().map(|p| util::path_to_varstr(&p))
    };

    let Some(dir) = dir else {
      return Err(sherr!(
        ExecFail @ args.span,
        "pwd: cannot determine current directory",
      ));
    };

    outln_bytes(dir.as_bytes());
    with_status(0)
  }
}

fn is_same_dir_as_cwd(path: &[u8]) -> bool {
  use std::os::unix::ffi::OsStrExt;
  use std::os::unix::fs::MetadataExt;
  let path = std::path::Path::new(std::ffi::OsStr::from_bytes(path));
  let Ok(p_meta) = fs::metadata(path) else {
    return false;
  };
  let Ok(dot_meta) = fs::metadata(".") else {
    return false;
  };
  p_meta.dev() == dot_meta.dev() && p_meta.ino() == dot_meta.ino()
}

fn physical_cwd() -> Option<std::path::PathBuf> {
  env::current_dir()
    .ok()
    .and_then(|p| fs::canonicalize(&p).ok().or(Some(p)))
}

#[cfg(test)]
mod tests {
  use crate::state::{
    self, Shed,
    vars::{VarFlags, VarKind},
  };
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
    // Default pwd (-L) prints $PWD as-typed. On macOS the tempdir path goes
    // through `/var → /private/var`, so canonicalizing here would mismatch
    // the (correct) -L output.
    assert_eq!(out.trim(), tmp.path().display().to_string());
  }

  #[test]
  fn pwd_status_zero() {
    let _g = TestGuard::new();
    test_input("pwd").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn pwd_p_canonicalizes_through_symlink() {
    let guard = TestGuard::new();
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    let link = tmp.path().join("link");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    test_input(format!("cd -P {}", link.display())).unwrap();
    guard.read_output();
    test_input("pwd -P").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), canon(&real).display().to_string());
  }

  #[test]
  fn pwd_l_uses_pwd_var_when_valid() {
    let guard = TestGuard::new();
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("real");
    let link = tmp.path().join("link");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    // Set $PWD to the symlink path; cwd is the real path. They name the
    // same inode, so -L should print the symlink form.
    env::set_current_dir(&real).unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "PWD",
        VarKind::Str(link.to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();

    test_input("pwd -L").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), link.display().to_string());
  }

  #[test]
  fn pwd_l_falls_back_when_pwd_stale() {
    let guard = TestGuard::new();
    let tmp = TempDir::new().unwrap();

    test_input(format!("cd {}", tmp.path().display())).unwrap();
    guard.read_output();
    // Corrupt $PWD so it doesn't name the current directory.
    Shed::vars_mut(|v| {
      v.set_var(
        "PWD",
        VarKind::Str("/definitely/not/the/cwd".into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();

    test_input("pwd -L").unwrap();
    let out = guard.read_output();
    assert_eq!(out.trim(), canon(tmp.path()).display().to_string());
  }

  #[test]
  fn pwd_rejects_extra_args() {
    let _g = TestGuard::new();
    test_input("pwd extra-arg").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn pwd_rejects_unknown_flag() {
    let _g = TestGuard::new();
    test_input("pwd -X").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
