use crate::{
  getopt::{Opt, OptArg, OptSpec, get_opts_from_tokens}, libsh::error::ShResult, match_loop, parse::{NdRule, Node}, prelude::*, procio::borrow_fd, sherr, state::{self, read_vars}
};

fn cd_opt_spec() -> [OptSpec; 2] {
	[
		OptSpec {
			opt: Opt::Short('P'),
			takes_arg: OptArg::None
		},
		OptSpec {
			opt: Opt::Short('L'),
			takes_arg: OptArg::None
		}
	]
}

struct CdOpts {
	resolve_syms: bool
}

impl CdOpts {
	pub fn from_opts(opts: &[Opt]) -> ShResult<Self> {
		let mut new = Self { resolve_syms: false };
		let mut opts = opts.iter();

		match_loop!(opts.next() => opt, {
			Opt::Short('P') => new.resolve_syms = true,
			Opt::Short('L') => new.resolve_syms = false,
			_ => return Err(sherr!(ParseErr, "Invalid option: {opt}"))
		});

		Ok(new)
	}
}

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

  let (mut argv,opts) = get_opts_from_tokens(argv, &cd_opt_spec())?;
	let cd_opts = CdOpts::from_opts(&opts)?;
  if !argv.is_empty() {
    argv.remove(0);
  }

	let mut try_cd_path = false;
	let mut print_dir = false;

  let (mut new_dir, arg_span) = if let Some((arg, span)) = argv.into_iter().next() {
		match arg.as_str() {
			"-" => {
				let old_pwd = read_vars(|v| v.try_get_var("OLDPWD"))
					.unwrap_or_else(|| state::get_home_str().unwrap_or(String::from("/")));
				print_dir = true;
				(PathBuf::from(old_pwd), Some(span))
			}
			_ => {
				try_cd_path = !(arg.starts_with(['/', '.']) || arg.starts_with(".."));
				(PathBuf::from(arg), Some(span))
			}
		}
  } else {
    (PathBuf::from(state::get_home_str().unwrap_or(String::from("/"))), None)
  };

	if try_cd_path {
		let path = read_vars(|v| v.get_var("CDPATH"));
		let paths = path.split(':').map(|p| if p.is_empty() { "." } else { p }).map(PathBuf::from);
		for path in paths {
			let joined = path.join(&new_dir);
			if joined.is_dir() {
				new_dir = joined;
				break
			}
		}
	}

	if cd_opts.resolve_syms {
		new_dir = std::fs::canonicalize(&new_dir).unwrap_or(new_dir);
	}

  if !new_dir.exists() {
		if let Some(arg_span) = arg_span {
			return Err(sherr!(ExecFail @ arg_span.clone(), "Failed to change directory"));
		} else {
			return Err(sherr!(ExecFail @ span.clone(), "Failed to change directory"));
		}
  }

  if !new_dir.is_dir() {
		if let Some(arg_span) = arg_span {
			return Err(sherr!(ExecFail @ arg_span.clone(), "Not a directory"));
		} else {
			return Err(sherr!(ExecFail @ span.clone(), "Not a directory"));
		}
  }

  if let Err(e) = state::change_dir(new_dir) {
    return Err(sherr!(ExecFail @ cd_span.clone(), "Failed to change directory: {e}"));
  }

	if print_dir {
		let mut dir = env::current_dir()?.display().to_string();
		if let Some(home) = state::get_home_str()
		&& let Some(home_dir) = dir.strip_prefix(&home) {
			dir = format!("~{home_dir}");
		}

		let stdout = borrow_fd(STDOUT_FILENO);
		write(stdout, dir.as_bytes())?;
		write(stdout, b"\n")?;
	}

  state::set_status(0);
  Ok(())
}

#[cfg(test)]
pub mod tests {
  use std::env;
  use std::fs;

  use tempfile::TempDir;

  use crate::state::{self, read_vars, write_vars, VarFlags, VarKind};
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
    assert_eq!(cwd.display().to_string(), dir_a.path().display().to_string());
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
    assert_eq!(cwd.display().to_string(), dir_b.path().display().to_string());
  }

  // ===================== CDPATH =====================

  #[test]
  fn cd_uses_cdpath() {
    let _g = TestGuard::new();
    let base = TempDir::new().unwrap();
    let target = base.path().join("mydir");
    fs::create_dir(&target).unwrap();

    write_vars(|v| v.set_var("CDPATH", VarKind::Str(base.path().display().to_string()), VarFlags::EXPORT)).unwrap();
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

    write_vars(|v| v.set_var(
      "CDPATH",
      VarKind::Str(format!("/nonexistent_cdpath_xyz:{}", base.path().display())),
      VarFlags::EXPORT,
    )).unwrap();
    test_input("cd realdir").unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), target.display().to_string());
  }

  #[test]
  fn cd_cdpath_not_used_for_absolute() {
    let _g = TestGuard::new();
    let target = TempDir::new().unwrap();
    let decoy = TempDir::new().unwrap();

    write_vars(|v| v.set_var("CDPATH", VarKind::Str(decoy.path().display().to_string()), VarFlags::EXPORT)).unwrap();
    test_input(format!("cd {}", target.path().display())).unwrap();

    let cwd = env::current_dir().unwrap();
    assert_eq!(cwd.display().to_string(), target.path().display().to_string());
  }

  #[test]
  fn cd_cdpath_not_used_for_dot() {
    let _g = TestGuard::new();
    let temp_dir = TempDir::new().unwrap();
    let sub = temp_dir.path().join("child");
    fs::create_dir(&sub).unwrap();

    test_input(format!("cd {}", temp_dir.path().display())).unwrap();

    let decoy = TempDir::new().unwrap();
    write_vars(|v| v.set_var("CDPATH", VarKind::Str(decoy.path().display().to_string()), VarFlags::EXPORT)).unwrap();
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
    assert_eq!(cwd.display().to_string(), canonical_real.display().to_string());
  }
}
