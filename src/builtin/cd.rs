use crate::{
  getopt::{Opt, OptSpec},
  prelude::*,
  sherr,
  state::{self, read_vars},
  util::{error::ShResult, with_status, write_ln_out},
};

pub(super) struct Cd;
impl super::Builtin for Cd {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::flag('P'), OptSpec::flag('L')]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut resolve_syms = false;
    let mut try_cd_path = false;
    let mut print_dir = false;

    for opt in &args.opts {
      match opt {
        Opt::Short('P') => resolve_syms = true,
        Opt::Short('L') => resolve_syms = false,
        _ => return Err(sherr!(ParseErr @ args.span, "Invalid option: {opt}")),
      }
    }

    let (mut new_dir, arg_span) = if let Some((arg, span)) = args.argv.into_iter().next() {
      match arg.as_str() {
        "-" => {
          let old_pwd = get_old_pwd();
          print_dir = true;
          (old_pwd, Some(span))
        }
        _ => {
          try_cd_path = !arg.starts_with(['/', '.']);
          (PathBuf::from(arg), Some(span))
        }
      }
    } else {
      (
        PathBuf::from(state::get_home_str().unwrap_or(String::from("/"))),
        None,
      )
    };

    let span = arg_span.unwrap_or(args.span);

    if try_cd_path && let Some(found) = search_cd_path(&new_dir) {
      print_dir = true;
      new_dir = found;
    }

    if resolve_syms && let Ok(canon) = std::fs::canonicalize(&new_dir) {
      new_dir = canon;
    }

    if !new_dir.exists() {
      return Err(sherr!(ExecFail @ span.clone(), "Directory not found"));
    }
    if !new_dir.is_dir() {
      return Err(sherr!(ExecFail @ span.clone(), "Not a directory"));
    }
    if let Err(e) = state::change_dir(new_dir) {
      return Err(sherr!(ExecFail @ span.clone(), "Failed to change directory: {e}"));
    }

    if print_dir {
      let mut dir = env::current_dir()?.display().to_string();
      if let Some(home) = state::get_home_str()
        && let Some(home_dir) = dir.strip_prefix(&home)
      {
        dir = format!("~{home_dir}");
      }

      write_ln_out(dir)?;
    }

    with_status(0)
  }
}

fn search_cd_path(new_dir: impl AsRef<Path>) -> Option<PathBuf> {
  let path = read_vars(|v| v.get_var("CDPATH"));
  let mut paths = path
    .split(':')
    .filter(|p| !p.trim().is_empty())
    .map(PathBuf::from);

  paths.find_map(|p| p.join(&new_dir).is_dir().then(|| p.join(&new_dir)))
}

fn get_old_pwd() -> PathBuf {
  read_vars(|v| v.try_get_var("OLDPWD"))
    .or_else(|| state::get_home_str().or_else(|| Some(String::from("/"))))
    .map(PathBuf::from)
    .unwrap()
}

#[cfg(test)]
pub mod tests {
  use std::env;
  use std::fs;

  use tempfile::TempDir;

  use crate::state::{self, VarFlags, VarKind, read_vars, write_vars};
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
    test_input("cd /nonexistent_path_that_does_not_exist_xyz").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn cd_file_not_directory_fails() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("afile.txt");
    fs::write(&file_path, "hello").unwrap();

    test_input(format!("cd {}", file_path.display())).ok();
    assert_ne!(state::get_status(), 0);
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

  // ===================== OLDPWD / cd - =====================

  #[test]
  fn cd_sets_oldpwd() {
    let _g = TestGuard::new();
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    test_input(format!("cd {}", dir_a.path().display())).unwrap();
    test_input(format!("cd {}", dir_b.path().display())).unwrap();

    let oldpwd = read_vars(|v| v.get_var("OLDPWD"));
    assert_eq!(oldpwd, dir_a.path().display().to_string());
  }

  #[test]
  fn cd_sets_pwd_var() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    let pwd = read_vars(|v| v.get_var("PWD"));
    assert_eq!(pwd, env::current_dir().unwrap().display().to_string());
  }

  #[test]
  fn cd_hyphen_goes_to_oldpwd() {
    let _g = TestGuard::new();
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    test_input(format!("cd {}", dir_a.path().display())).unwrap();
    test_input(format!("cd {}", dir_b.path().display())).unwrap();
    test_input("cd -").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      dir_a.path().display().to_string()
    );
  }

  #[test]
  fn cd_hyphen_toggles() {
    let _g = TestGuard::new();
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    test_input(format!("cd {}", dir_a.path().display())).unwrap();
    test_input(format!("cd {}", dir_b.path().display())).unwrap();
    test_input("cd -").unwrap();
    test_input("cd -").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      dir_b.path().display().to_string()
    );
  }

  // ===================== CDPATH =====================

  #[test]
  fn cd_uses_cdpath() {
    let _g = TestGuard::new();
    let base = TempDir::new().unwrap();
    let target = base.path().join("mydir");
    fs::create_dir(&target).unwrap();

    write_vars(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(base.path().display().to_string()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input("cd mydir").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), target.display().to_string());
  }

  #[test]
  fn cd_cdpath_skips_nonexistent() {
    let _g = TestGuard::new();
    let base = TempDir::new().unwrap();
    let target = base.path().join("realdir");
    fs::create_dir(&target).unwrap();

    write_vars(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(format!("/nonexistent_cdpath_xyz:{}", base.path().display())),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input("cd realdir").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), target.display().to_string());
  }

  #[test]
  fn cd_cdpath_not_used_for_absolute() {
    let _g = TestGuard::new();
    let target = TempDir::new().unwrap();
    let decoy = TempDir::new().unwrap();

    write_vars(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(decoy.path().display().to_string()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input(format!("cd {}", target.path().display())).unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      target.path().display().to_string()
    );
  }

  #[test]
  fn cd_cdpath_not_used_for_dot() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let sub = temp_dir.path().join("child");
    fs::create_dir(&sub).unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    let decoy = TempDir::new().unwrap();
    write_vars(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(decoy.path().display().to_string()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input("cd ./child").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), sub.display().to_string());
  }

  // ===================== -P option =====================

  #[test]
  fn cd_p_resolves_symlinks() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let real_dir = temp_dir.path().join("real");
    let link_dir = temp_dir.path().join("link");
    fs::create_dir(&real_dir).unwrap();
    std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();

    test_input(format!("cd -P {}", link_dir.display())).unwrap();

    let cwd = env::current_dir().unwrap();
    let canonical_real = fs::canonicalize(&real_dir).unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canonical_real.display().to_string()
    );
  }
}
