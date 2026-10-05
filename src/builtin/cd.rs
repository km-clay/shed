//! The `cd` builtin. Used to change the current working directory.
//!
use std::path::{Path, PathBuf};

use crate::{
  procio, sherr,
  state::{cwd, paths},
  try_var,
  util::{self, error::ShResult},
  var,
};

use super::opt::OptSpec;

pub(super) struct Cd;
impl super::Builtin for Cd {
  fn fork_behavior(&self) -> super::ForkBehavior {
    super::ForkBehavior::Subshell
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new("physical").short(b'P'),
      OptSpec::new("logical").short(b'L'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut resolve_syms = false;
    let mut try_cd_path = false;
    let mut print_dir = false;

    for opt in args.options() {
      match opt.key() {
        "physical" => resolve_syms = true,
        "logical" => resolve_syms = false,
        _ => return Err(sherr!(ParseErr @ opt.span(), "Invalid option: {opt}").with_code(2)),
      }
    }

    let (mut new_dir, arg_span) = if let Some((arg, span)) = args.arguments().next() {
      if arg == "-" {
        let old_pwd = get_old_pwd();
        print_dir = true;
        (old_pwd, Some(span))
      } else {
        // we only use cd path if the argument is not absolute or relative (starts with / or .)
        try_cd_path = !arg.to_str_lossy().starts_with(['/', '.']);
        (PathBuf::from(arg), Some(span))
      }
    } else {
      let home_dir = paths::get_home_str().unwrap_or("/".into());
      (PathBuf::from(home_dir), None)
    };

    let span = arg_span.unwrap_or(args.cmd_span());

    if try_cd_path && let Some(found) = search_cd_path(&new_dir) {
      print_dir = true;
      new_dir = found;
    }

    // if resolve_syms is true, we turn symlinks into their canonical paths,
    // which refer to the actual position of the file in the filesystem
    let logical_pwd = if resolve_syms {
      None
    } else {
      let base = if new_dir.is_absolute() {
        PathBuf::new()
      } else {
        try_var!("PWD")
          .map(PathBuf::from)
          .or_else(|| std::env::current_dir().ok())
          .unwrap_or_else(|| PathBuf::from("/"))
      };
      Some(paths::lex_normalize_path(&base.join(&new_dir)))
    };

    let target = if resolve_syms {
      match std::fs::canonicalize(&new_dir) {
        Ok(canon) => canon,
        Err(_) => new_dir,
      }
    } else {
      match logical_pwd.as_deref() {
        Some(logical) => PathBuf::from(logical),
        None => new_dir,
      }
    };

    // handle weird cases
    if !target.exists() {
      return Err(sherr!(ExecFail @ span, "Directory not found: {}", target.display()));
    }
    if !target.is_dir() {
      return Err(sherr!(ExecFail @ span, "Not a directory"));
    }
    if let Err(e) = cwd::change_dir_with_pwd(&target, logical_pwd, true, true) {
      return Err(sherr!(ExecFail @ span, "Failed to change directory: {e}"));
    }

    if print_dir {
      let pwd = PathBuf::from(var!("PWD"));
      procio::outln_bytes(&paths::display_path_bytes(&pwd));
    }

    util::with_status(0)
  }
}

fn search_cd_path(new_dir: impl AsRef<Path>) -> Option<PathBuf> {
  let path = try_var!("CDPATH")?;
  let path = path.to_str_lossy();

  paths::split_path_list(&path)
    .filter(|p| !p.as_os_str().is_empty())
    .find_map(|p| {
      let resolved = p.join(&new_dir);
      resolved.is_dir().then_some(resolved)
    })
}

fn get_old_pwd() -> PathBuf {
  try_var!("OLDPWD")
    .or_else(|| paths::get_home_str().or_else(|| Some("/".into())))
    .map(PathBuf::from)
    .unwrap()
}

#[cfg(test)]
pub(super) mod tests {
  use crate::set_var;
  use std::env;
  use std::fs;

  use tempfile::TempDir;

  use crate::var;
  use crate::{
    state::{
      self, Shed,
      vars::{VarFlags, VarKind},
    },
    tests::testutil::{TestGuard, canon, test_input},
  };

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
      canon(temp_dir.path()).display().to_string()
    );
  }

  #[test]
  fn cd_logical_keeps_kernel_cwd_in_sync_across_symlink() {
    // Regression: in logical mode (-L, the default), `cd link; cd ..` must
    // chdir the *logical* path so the kernel cwd matches $PWD (POSIX cd -L).
    // Previously `..` was resolved physically, desyncing the kernel cwd from
    // $PWD when the symlink's parent differed from its target's parent.
    let _g = TestGuard::new();
    let base = TempDir::new().unwrap();
    // Canonicalize up front so /tmp being a symlink (e.g. on macOS) can't skew
    // the comparison.
    let root = canon(base.path());
    let parent_a = root.join("A");
    let target = root.join("B").join("target");
    fs::create_dir_all(&parent_a).unwrap();
    fs::create_dir_all(&target).unwrap();
    let link = parent_a.join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    test_input(format!("cd {}", link.display())).unwrap();
    test_input("cd ..").unwrap();

    let kernel = env::current_dir().unwrap().display().to_string();
    let pwd = var!("PWD").to_string();
    // The core of the bug: kernel cwd and $PWD must agree.
    assert_eq!(kernel, pwd, "kernel cwd must match $PWD (no desync)");
    // And both must be the logical parent A, not the target's physical parent B.
    assert_eq!(
      pwd,
      parent_a.display().to_string(),
      "expected logical parent A"
    );
  }

  #[test]
  fn cd_no_args_goes_home() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "HOME",
        VarKind::Str(temp_dir.path().display().to_string().into()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    test_input("cd").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(temp_dir.path()).display().to_string()
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
    assert_eq!(cwd.display().to_string(), canon(&sub).display().to_string());
  }

  // ===================== Environment =====================

  #[test]
  fn cd_status_zero_on_success() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Error Cases =====================

  #[test]
  fn cd_nonexistent_dir_fails() {
    let _g = TestGuard::new();
    test_input("cd /nonexistent_path_that_does_not_exist_xyz").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn cd_file_not_directory_fails() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("afile.txt");
    fs::write(&file_path, "hello").unwrap();

    test_input(format!("cd {}", file_path.display())).ok();
    assert_ne!(state::Shed::get_status(), 0);
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
      canon(dir_a.path()).display().to_string()
    );

    test_input(format!("cd {}", dir_b.path().display())).unwrap();
    assert_eq!(
      env::current_dir().unwrap().display().to_string(),
      canon(dir_b.path()).display().to_string()
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
      canon(&deep).display().to_string()
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

    // -L semantics: OLDPWD preserves the path the user typed, not the
    // canonical form. On macOS `/var/folders/...` is a symlink to
    // `/private/var/folders/...` so comparing against `canon(...)` would
    // wrongly canonicalize it.
    let oldpwd = var!("OLDPWD");
    assert_eq!(oldpwd, dir_a.path().display().to_string());
  }

  #[test]
  fn cd_sets_pwd_var() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    // -L semantics: $PWD reflects what the user typed, not the canonical
    // kernel cwd. The kernel cwd can differ if any component of the input
    // path is a symlink (e.g. macOS's `/var` → `/private/var`).
    let pwd = var!("PWD");
    assert_eq!(pwd, temp_dir.path().display().to_string());
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
      canon(dir_a.path()).display().to_string()
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
      canon(dir_b.path()).display().to_string()
    );
  }

  // ===================== CDPATH =====================

  #[test]
  fn cd_uses_cdpath() {
    let _g = TestGuard::new();
    let base = TempDir::new().unwrap();
    let target = base.path().join("mydir");
    fs::create_dir(&target).unwrap();

    Shed::vars_mut(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(base.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input("cd mydir").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(&target).display().to_string()
    );
  }

  #[test]
  fn cd_cdpath_skips_nonexistent() {
    let _g = TestGuard::new();
    let base = TempDir::new().unwrap();
    let target = base.path().join("realdir");
    fs::create_dir(&target).unwrap();

    Shed::vars_mut(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(format!("/nonexistent_cdpath_xyz:{}", base.path().to_string_lossy()).into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input("cd realdir").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(&target).display().to_string()
    );
  }

  #[test]
  fn cd_cdpath_not_used_for_absolute() {
    let _g = TestGuard::new();
    let target = TempDir::new().unwrap();
    let decoy = TempDir::new().unwrap();

    Shed::vars_mut(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(decoy.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input(format!("cd {}", target.path().display())).unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(
      cwd.display().to_string(),
      canon(target.path()).display().to_string()
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
    Shed::vars_mut(|v| {
      v.set_var(
        "CDPATH",
        VarKind::Str(decoy.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    test_input("cd ./child").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), canon(&sub).display().to_string());
  }

  #[test]
  fn cd_empty_cdpath_does_not_print() {
    let g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let sub = temp_dir.path().join("child");
    fs::create_dir(&sub).unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    set_var!("CDPATH", VarKind::Str("".into()); EXPORT).unwrap();
    test_input("cd child").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), canon(&sub).display().to_string());
    assert_eq!(
      g.read_output(),
      "",
      "empty CDPATH must not trigger the print"
    );
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

  // ===================== -L (default) symlink preservation =====================

  #[test]
  fn cd_l_preserves_symlink_in_pwd() {
    // The bug from #73: by default `cd` should NOT resolve symlinks when
    // setting $PWD. The kernel cwd is canonical (no avoiding that), but
    // $PWD should reflect what the user typed.
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let real_dir = temp_dir.path().join("real");
    let link_dir = temp_dir.path().join("link");
    fs::create_dir(&real_dir).unwrap();
    std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();

    test_input(format!("cd {}", link_dir.display())).unwrap();

    let pwd = var!("PWD");
    assert_eq!(pwd, link_dir.display().to_string());
  }

  #[test]
  fn cd_l_dotdot_pops_lexically() {
    // After `cd /a/symlink-to-b`, `cd ..` with -L should land in /a (the
    // parent of the symlink path), not in the parent of the real dir.
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let real = temp_dir.path().join("real");
    let link = temp_dir.path().join("link");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    test_input(format!("cd {}", link.display())).unwrap();
    test_input("cd ..").unwrap();

    let pwd = var!("PWD");
    assert_eq!(pwd, temp_dir.path().display().to_string());
  }

  #[test]
  fn cd_l_normalizes_dotdot_in_input() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let sub = temp_dir.path().join("sub");
    fs::create_dir(&sub).unwrap();

    let weird = format!("{}/sub/../sub", temp_dir.path().display());
    test_input(format!("cd {weird}")).unwrap();

    let pwd = var!("PWD");
    assert_eq!(pwd, sub.display().to_string());
  }

  #[test]
  fn cd_p_pwd_is_canonical() {
    // Sanity: with -P, $PWD matches the kernel cwd (symlinks resolved).
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let real = temp_dir.path().join("real");
    let link = temp_dir.path().join("link");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    test_input(format!("cd -P {}", link.display())).unwrap();

    let pwd = var!("PWD");
    assert_eq!(pwd, fs::canonicalize(&real).unwrap().display().to_string());
  }
}
