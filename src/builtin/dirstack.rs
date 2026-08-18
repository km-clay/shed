use std::{env, path::PathBuf};

use crate::procio::{out_bytes, outln_bytes};

use super::{
  super::state::meta::MetaTab,
  ShResult, Shed, Span,
  opt::OptSpec,
  sherr,
  state::util::{change_dir, display_path_bytes},
  util::ShResultExt,
  with_status,
};

fn is_index_arg(arg: &str) -> bool {
  arg.starts_with('+')
    || (arg.starts_with('-') && arg.len() > 1 && arg.as_bytes()[1].is_ascii_digit())
}

struct DirStackArgs {
  no_cd: bool,
  index: Option<StackIdx>,
  dir: Option<PathBuf>,
}

fn parse_dirstack_args(args: &super::BuiltinArgs, cmd: &str) -> ShResult<DirStackArgs> {
  let no_cd = args.options().any(|o| o.key() == "no_cd");
  let mut index = None;
  let mut dir = None;

  for (arg, _) in args.arguments() {
    if is_index_arg(&arg.to_str_lossy()) {
      index = Some(parse_stack_idx(&arg.to_str_lossy(), args.span(), cmd)?);
    } else if arg.to_str_lossy().starts_with('-') {
      return Err(sherr!(
        ExecFail @ args.span(),
        "{cmd}: invalid option: '{arg}'",
      ));
    } else {
      if dir.is_some() {
        return Err(sherr!(
          ExecFail @ args.span(),
          "{cmd}: too many arguments",
        ));
      }
      let target = PathBuf::from(arg);
      if !target.is_dir() {
        return Err(sherr!(
          ExecFail @ args.span(),
          "{cmd}: not a directory: '{}'",
          target.display(),
        ));
      }
      dir = Some(target);
    }
  }

  Ok(DirStackArgs { no_cd, index, dir })
}

pub(super) struct PushDir;
impl super::Builtin for PushDir {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::new_short("no_cd", 'n')]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let blame = args.span();
    let parsed = parse_dirstack_args(&args, "pushd")?;

    if let Some(idx) = parsed.index {
      let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
      // Rotate a *copy* of the visible stack (`[cwd] + deque`); the real stack
      // is only committed after a successful cd, so a failed cd (e.g. the
      // target was removed from disk) leaves it untouched rather than dropping
      // an entry.
      let mut stack = Shed::meta(|m| m.dirs().clone());
      stack.push_front(cwd);
      let (StackIdx::FromTop(n) | StackIdx::FromBottom(n)) = idx;
      if n >= stack.len() {
        let sign = if matches!(idx, StackIdx::FromTop(_)) {
          '+'
        } else {
          '-'
        };
        return Err(sherr!(
          ExecFail @ blame,
          "pushd: directory index out of range: {sign}{n}",
        ));
      }
      match idx {
        StackIdx::FromTop(n) => stack.rotate_left(n),
        StackIdx::FromBottom(n) => stack.rotate_right(n + 1),
      }
      // The rotated top becomes the new cwd (and is dropped from the stack when
      // we actually cd; for `-n` it's discarded and cwd stays put, matching
      // bash's rotate-then-keep-cwd behavior).
      let new_cwd = stack.pop_front();

      if let Some(dir) = &new_cwd
        && !parsed.no_cd
      {
        change_dir(dir).promote_err(blame)?;
      }
      Shed::meta_mut(|m| *m.dirs_mut() = stack);
      print_dirs()?;
    } else if let Some(dir) = parsed.dir {
      if parsed.no_cd {
        // `pushd -n dir`: add {dir} to the stack just below the current dir,
        // without changing directory (bash). The old code pushed the *cwd*
        // instead of {dir}, so the target was silently dropped.
        Shed::meta_mut(|m| m.push_dir(dir));
        print_dirs()?;
        return with_status(0);
      }

      let old_dir = env::current_dir()?;
      if old_dir != dir {
        Shed::meta_mut(|m| m.push_dir(old_dir));
      }

      change_dir(&dir).promote_err(blame)?;
      print_dirs()?;
    }

    with_status(0)
  }
}

pub(super) struct PopDir;
impl super::Builtin for PopDir {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::new_short("no_cd", 'n')]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let blame = args.span();
    let parsed = parse_dirstack_args(&args, "popd")?;

    if let Some(idx) = parsed.index {
      match idx {
        StackIdx::FromTop(0) => {
          // +0 is same as plain popd: pop top, cd to it
          let dir = Shed::meta_mut(MetaTab::pop_dir);
          if !parsed.no_cd {
            if let Some(dir) = dir {
              change_dir(&dir).promote_err(blame)?;
            } else {
              return Err(sherr!(
                ExecFail @ blame,
                "popd: directory stack empty",
              ));
            }
          }
        }
        StackIdx::FromTop(n) => {
          // +N (N>0): remove (N-1)th stored entry, no cd
          Shed::meta_mut(|m| {
            let dirs = m.dirs_mut();
            let idx = n - 1;
            if idx >= dirs.len() {
              return Err(sherr!(
                ExecFail @ blame.clone(),
                "popd: directory index out of range: +{n}",
              ));
            }
            dirs.remove(idx);
            Ok(())
          })?;
        }
        StackIdx::FromBottom(n) => {
          Shed::meta_mut(|m| -> ShResult<()> {
            let dirs = m.dirs_mut();
            let actual = dirs.len().checked_sub(n + 1).ok_or_else(|| {
              sherr!(
                ExecFail @ blame.clone(),
                "popd: directory index out of range: -{n}",
              )
            })?;
            dirs.remove(actual);
            Ok(())
          })?;
        }
      }
      print_dirs()?;
    } else {
      let dir = Shed::meta_mut(super::super::state::meta::MetaTab::pop_dir);

      if parsed.no_cd {
        return with_status(0);
      }

      if let Some(dir) = dir {
        change_dir(&dir).promote_err(blame)?;
        print_dirs()?;
      } else {
        return Err(sherr!(
          ExecFail @ blame,
          "popd: directory stack empty",
        ));
      }
    }

    with_status(0)
  }
}

pub(super) struct Dirs;
impl super::Builtin for Dirs {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("one_per_line", 'p'),
      OptSpec::new_short("one_per_line_indexed", 'v'),
      OptSpec::new_short("clear_stack", 'c'),
      OptSpec::new_short("no_home_truncation", 'l'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut abbreviate_home = true;
    let mut one_per_line = false;
    let mut one_per_line_indexed = false;
    let mut clear_stack = false;
    let mut target_idx: Option<StackIdx> = None;
    let blame = args.span();

    for opt in args.options() {
      match opt.key() {
        "one_per_line" => one_per_line = true,
        "one_per_line_indexed" => one_per_line_indexed = true,
        "clear_stack" => clear_stack = true,
        "no_home_truncation" => abbreviate_home = false,
        _ => {}
      }
    }

    for (arg, _) in args.arguments() {
      match arg.to_str_lossy().as_ref() {
        _ if is_index_arg(&arg.to_str_lossy()) => {
          target_idx = Some(parse_stack_idx(&arg.to_str_lossy(), blame.clone(), "dirs")?);
        }
        _ if arg.to_str_lossy().starts_with('-') => {
          return Err(sherr!(
            ExecFail @ blame,
            "dirs: invalid option: '{arg}'",
          ));
        }
        _ => {
          return Err(sherr!(
            ExecFail @ blame,
            "dirs: unexpected argument: '{arg}'",
          ));
        }
      }
    }

    if clear_stack {
      Shed::meta_mut(|m| m.dirs_mut().clear());
      return Ok(());
    }

    let mut dirs: Vec<Vec<u8>> = Shed::meta(|m| {
      let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
      let stack = [current_dir].into_iter().chain(m.dirs().clone());

      if abbreviate_home {
        stack.map(|d| display_path_bytes(&d)).collect()
      } else {
        stack.map(|d| path_bytes(&d)).collect()
      }
    });

    let indexed = target_idx.is_some();
    if let Some(idx) = target_idx {
      let target = match idx {
        StackIdx::FromTop(n) => dirs.get(n),
        StackIdx::FromBottom(n) => dirs.get(dirs.len().saturating_sub(n + 1)),
      };

      if let Some(dir) = target {
        dirs = vec![dir.clone()];
      } else {
        return Err(sherr!(
          ExecFail @ blame,
          "dirs: directory index out of range: {}",
          match idx {
            StackIdx::FromTop(n) => format!("+{n}"),
            StackIdx::FromBottom(n) => format!("-{n}"),
          }
        ));
      }
    }

    if one_per_line {
      out_bytes(&join_bytes(&dirs, b"\n"));
    } else if one_per_line_indexed {
      let indexed_lines: Vec<Vec<u8>> = dirs
        .iter()
        .enumerate()
        .map(|(i, dir)| {
          let mut line = format!("{i}\t").into_bytes();
          line.extend_from_slice(dir);
          line
        })
        .collect();
      out_bytes(&join_bytes(&indexed_lines, b"\n"));
      out_bytes(b"\n");
    } else if indexed {
      // An index was supplied: print just the selected entry (`dirs` was
      // narrowed above), not the whole stack.
      out_bytes(&join_bytes(&dirs, b" "));
      out_bytes(b"\n");
    } else {
      print_dirs()?;
    }

    with_status(0)
  }
}

/// A path's raw bytes (Unix: exact, no lossy step).
fn path_bytes(path: &std::path::Path) -> Vec<u8> {
  use std::os::unix::ffi::OsStrExt;
  path.as_os_str().as_bytes().to_vec()
}

/// Join byte slices with `sep` (no trailing separator).
fn join_bytes(parts: &[Vec<u8>], sep: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  for (i, part) in parts.iter().enumerate() {
    if i > 0 {
      out.extend_from_slice(sep);
    }
    out.extend_from_slice(part);
  }
  out
}

enum StackIdx {
  FromTop(usize),
  FromBottom(usize),
}

fn print_dirs() -> ShResult<()> {
  let current_dir = env::current_dir()?;
  let dirs_iter = Shed::meta(|m| m.dirs().clone().into_iter());
  let all_dirs: Vec<Vec<u8>> = [current_dir]
    .into_iter()
    .chain(dirs_iter)
    .map(|d| display_path_bytes(&d))
    .collect();

  outln_bytes(&join_bytes(&all_dirs, b" "));

  Ok(())
}

/// Collapse a leading `$HOME` in a path to `~` (test helper mirroring the
/// byte-native [`display_path_bytes`]).
#[cfg(test)]
pub fn truncate_home_path(path: &str) -> String {
  String::from_utf8_lossy(&display_path_bytes(std::path::Path::new(path))).into_owned()
}

fn parse_stack_idx(arg: &str, blame: Span, cmd: &str) -> ShResult<StackIdx> {
  let (from_top, digits) = if let Some(rest) = arg.strip_prefix('+') {
    (true, rest)
  } else if let Some(rest) = arg.strip_prefix('-') {
    (false, rest)
  } else {
    unreachable!()
  };

  if digits.is_empty() {
    return Err(sherr!(
      ExecFail @ blame,

      "{cmd}: missing index after '{}'",
      if from_top { "+" } else { "-" }
      ,
    ));
  }

  for ch in digits.chars() {
    if !ch.is_ascii_digit() {
      return Err(sherr!(
        ExecFail @ blame,
        "{cmd}: invalid argument: '{arg}'",
      ));
    }
  }

  let n = digits.parse::<usize>().map_err(|e| {
    sherr!(
      ExecFail @ blame,
      "{cmd}: invalid index: '{e}'",
    )
  })?;

  if from_top {
    Ok(StackIdx::FromTop(n))
  } else {
    Ok(StackIdx::FromBottom(n))
  }
}

#[cfg(test)]
pub mod tests {
  use crate::{
    Shed, state,
    tests::testutil::{TestGuard, canon, test_input},
  };
  use pretty_assertions::{assert_eq, assert_ne};
  use std::{env, path::PathBuf};
  use tempfile::TempDir;

  #[test]
  fn test_pushd_interactive() {
    let g = TestGuard::new();
    let current_dir = env::current_dir().unwrap();

    test_input("pushd /tmp").unwrap();

    let new_dir = env::current_dir().unwrap();

    assert_ne!(new_dir, current_dir);
    assert_eq!(new_dir, canon(PathBuf::from("/tmp")));

    let dir_stack = Shed::meta(|m| m.dirs().clone());
    assert_eq!(dir_stack.len(), 1);
    assert_eq!(dir_stack[0], current_dir);

    let out = g.read_output();
    let path = super::truncate_home_path(&current_dir.to_string_lossy());
    let tmp_canon = canon(PathBuf::from("/tmp")).to_string_lossy().to_string();
    assert_eq!(out, format!("{tmp_canon} {path}\n"));
  }

  #[test]
  fn test_popd_interactive() {
    let g = TestGuard::new();
    let current_dir = env::current_dir().unwrap();
    let tempdir = TempDir::new().unwrap();
    let tempdir_raw = tempdir.path().to_path_buf().to_string_lossy().to_string();

    test_input(format!("pushd {tempdir_raw}")).unwrap();

    let dir_stack = Shed::meta(|m| m.dirs().clone());
    assert_eq!(dir_stack.len(), 1);
    assert_eq!(dir_stack[0], current_dir);

    assert_eq!(env::current_dir().unwrap(), canon(tempdir.path()));
    g.read_output(); // consume output of pushd

    test_input("popd").unwrap();

    assert_eq!(env::current_dir().unwrap(), current_dir);
    let out = g.read_output();
    let path = super::truncate_home_path(&current_dir.to_string_lossy());
    assert_eq!(out, format!("{path}\n"));
  }

  #[test]
  fn test_popd_empty_stack() {
    let _g = TestGuard::new();

    test_input("popd").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_pushd_multiple_then_popd() {
    let g = TestGuard::new();
    let original = env::current_dir().unwrap();
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();
    let path1 = canon(tmp1.path());
    let path2 = canon(tmp2.path());

    test_input(format!("pushd {}", path1.display())).unwrap();
    test_input(format!("pushd {}", path2.display())).unwrap();
    g.read_output();

    assert_eq!(env::current_dir().unwrap(), path2);
    let stack = Shed::meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 2);
    assert_eq!(stack[0], path1);
    assert_eq!(stack[1], original);

    test_input("popd").unwrap();
    assert_eq!(env::current_dir().unwrap(), path1);

    test_input("popd").unwrap();
    assert_eq!(env::current_dir().unwrap(), original);

    let stack = Shed::meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 0);
  }

  #[test]
  fn test_pushd_rotate_plus() {
    let g = TestGuard::new();
    let original = env::current_dir().unwrap();
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();
    let path1 = canon(tmp1.path());
    let path2 = canon(tmp2.path());

    // Build stack: cwd=original, then pushd path1, pushd path2
    // Stack after: cwd=path2, [path1, original]
    test_input(format!("pushd {}", path1.display())).unwrap();
    test_input(format!("pushd {}", path2.display())).unwrap();
    g.read_output();

    // pushd +1 rotates: [path2, path1, original] -> rotate_left(1) -> [path1, original, path2]
    // pop front -> cwd=path1, stack=[original, path2]
    test_input("pushd +1").unwrap();
    assert_eq!(env::current_dir().unwrap(), path1);

    let stack = Shed::meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 2);
    assert_eq!(stack[0], original);
    assert_eq!(stack[1], path2);
  }

  #[test]
  fn test_pushd_no_cd_flag() {
    let _g = TestGuard::new();
    state::Shed::meta_mut(|m| m.dirs_mut().clear());
    let original = env::current_dir().unwrap();
    let tmp = TempDir::new().unwrap();
    let path = canon(tmp.path());

    test_input(format!("pushd -n {}", path.display())).unwrap();

    // -n means don't cd...
    assert_eq!(env::current_dir().unwrap(), original);
    // ...but the *target* dir must be on the stack (regression: it used to push
    // the cwd instead and drop the target).
    let stack: Vec<PathBuf> = state::Shed::meta(|m| m.dirs().iter().cloned().collect());
    assert_eq!(
      stack,
      vec![path],
      "pushd -n must push the target, got: {stack:?}"
    );
  }

  #[test]
  fn pushd_index_cd_failure_leaves_stack_intact() {
    // Regression: the indexed rotation used to mutate the stack before the cd,
    // so a cd failure (target removed from disk) dropped an entry. The rotation
    // is now committed only after a successful cd.
    let _g = TestGuard::new();
    state::Shed::meta_mut(|m| m.dirs_mut().clear());
    let tmp = TempDir::new().unwrap();
    let path = canon(tmp.path());
    test_input(format!("pushd -n {}", path.display())).unwrap();
    let before: Vec<PathBuf> = state::Shed::meta(|m| m.dirs().iter().cloned().collect());

    // Remove the target from disk, then try to rotate-and-cd onto it.
    std::fs::remove_dir_all(tmp.path()).unwrap();
    let _ = test_input("pushd +1");

    let after: Vec<PathBuf> = state::Shed::meta(|m| m.dirs().iter().cloned().collect());
    assert_eq!(before, after, "cd failure must not corrupt the stack");
  }

  #[test]
  fn test_dirs_clear() {
    let _g = TestGuard::new();
    let tmp = TempDir::new().unwrap();

    test_input(format!("pushd {}", tmp.path().display())).unwrap();
    assert_eq!(Shed::meta(|m| m.dirs().len()), 1);

    test_input("dirs -c").unwrap();
    assert_eq!(Shed::meta(|m| m.dirs().len()), 0);
  }

  #[test]
  fn test_dirs_one_per_line() {
    let g = TestGuard::new();
    let original = env::current_dir().unwrap();
    let tmp = TempDir::new().unwrap();
    let path = canon(tmp.path());

    test_input(format!("pushd {}", path.display())).unwrap();
    g.read_output();

    test_input("dirs -p").unwrap();
    let out = g.read_output();
    let lines: Vec<&str> = out.split('\n').filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], super::truncate_home_path(&path.to_string_lossy()));
    assert_eq!(
      lines[1],
      super::truncate_home_path(&original.to_string_lossy())
    );
  }

  #[test]
  fn test_popd_indexed_from_top() {
    let _g = TestGuard::new();
    let original = env::current_dir().unwrap();
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();
    let path1 = canon(tmp1.path());
    let path2 = canon(tmp2.path());

    // Stack: cwd=path2, [path1, original]
    test_input(format!("pushd {}", path1.display())).unwrap();
    test_input(format!("pushd {}", path2.display())).unwrap();

    // popd +1 removes index (1-1)=0 from stored dirs, i.e. path1
    test_input("popd +1").unwrap();
    assert_eq!(env::current_dir().unwrap(), path2); // no cd

    let stack = Shed::meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 1);
    assert_eq!(stack[0], original);
  }

  #[test]
  fn test_pushd_nonexistent_dir() {
    let _g = TestGuard::new();

    test_input("pushd /nonexistent_dir_12345").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Dirs::execute =====================

  fn clear_stack() {
    Shed::meta_mut(|m| m.dirs_mut().clear());
  }

  #[test]
  fn dirs_default_prints_current_dir() {
    let g = TestGuard::new();
    clear_stack();
    test_input("dirs").unwrap();
    let out = g.read_output();
    // The default fmt always includes cwd; we just verify some output.
    assert!(!out.is_empty(), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn dirs_p_one_per_line() {
    let g = TestGuard::new();
    clear_stack();
    let t1 = TempDir::new().unwrap();
    test_input(format!("pushd {}", t1.path().display())).unwrap();
    g.read_output();
    test_input("dirs -p").unwrap();
    let out = g.read_output();
    // -p separates entries with newlines; with 2 dirs we'd have one '\n'.
    assert!(out.contains('\n'), "got: {out:?}");
  }

  #[test]
  fn dirs_v_indexed_listing() {
    let g = TestGuard::new();
    clear_stack();
    let t1 = TempDir::new().unwrap();
    test_input(format!("pushd {}", t1.path().display())).unwrap();
    g.read_output();
    test_input("dirs -v").unwrap();
    let out = g.read_output();
    // -v prefixes entries with their index, so "0\t" appears.
    assert!(out.contains("0\t"), "got: {out:?}");
  }

  #[test]
  fn dirs_c_clears_stack() {
    let _g = TestGuard::new();
    clear_stack();
    let t1 = TempDir::new().unwrap();
    test_input(format!("pushd {}", t1.path().display())).unwrap();
    let len_before = Shed::meta(|m| m.dirs().len());
    assert!(len_before > 0);
    test_input("dirs -c").unwrap();
    let len_after = Shed::meta(|m| m.dirs().len());
    assert_eq!(len_after, 0);
  }

  #[test]
  fn dirs_with_plus_index_picks_from_top() {
    let g = TestGuard::new();
    clear_stack();
    let t1 = TempDir::new().unwrap();
    let t2 = TempDir::new().unwrap();
    let p1 = canon(t1.path()).to_string_lossy().to_string();
    let p2 = canon(t2.path()).to_string_lossy().to_string();
    test_input(format!("pushd {p1}")).unwrap();
    test_input(format!("pushd {p2}")).unwrap();
    g.read_output();

    // The default format must print ONLY the selected entry, not the whole
    // stack: +0 is cwd (t2), +1 is the top of the saved stack (t1).
    test_input("dirs +0").unwrap();
    assert_eq!(g.read_output().trim(), p2);
    test_input("dirs +1").unwrap();
    let out = g.read_output();
    assert_eq!(out.trim(), p1);
    assert!(
      !out.contains(&p2),
      "index selection should not print the whole stack: {out:?}"
    );
  }

  #[test]
  fn dirs_index_out_of_range_errors() {
    let _g = TestGuard::new();
    clear_stack();
    test_input("dirs +99").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn dirs_unknown_arg_errors() {
    let _g = TestGuard::new();
    clear_stack();
    test_input("dirs random_garbage_arg").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn dirs_l_flag_disables_home_truncation() {
    // -l makes the listing show absolute paths rather than truncating
    // $HOME to ~. Without inspecting the contents we just verify the
    // command succeeds.
    let _g = TestGuard::new();
    clear_stack();
    test_input("dirs -l").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== PopDir::execute extra branches =====================

  #[test]
  fn popd_plus_zero_acts_like_plain_popd() {
    let _g = TestGuard::new();
    clear_stack();
    let original = env::current_dir().unwrap();
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().to_path_buf();
    test_input(format!("pushd {}", path.display())).unwrap();
    test_input("popd +0").unwrap();
    // +0 pops top and cds back.
    assert_eq!(env::current_dir().unwrap(), original);
  }

  #[test]
  fn popd_plus_index_out_of_range_errors() {
    let _g = TestGuard::new();
    clear_stack();
    let tmp = TempDir::new().unwrap();
    test_input(format!("pushd {}", tmp.path().display())).unwrap();
    test_input("popd +99").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn popd_minus_index_removes_from_bottom() {
    let _g = TestGuard::new();
    clear_stack();
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();
    let path1 = canon(tmp1.path());
    let path2 = canon(tmp2.path());
    // Stack: cwd=path2, dirs=[path1, original]
    test_input(format!("pushd {}", path1.display())).unwrap();
    test_input(format!("pushd {}", path2.display())).unwrap();
    // -0 is the bottom of the stack: `original`. Removing it should
    // leave dirs=[path1] and cwd untouched (no cd on indexed popd).
    test_input("popd -0").unwrap();
    assert_eq!(env::current_dir().unwrap(), path2);
    let stack = Shed::meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 1);
    assert_eq!(stack[0], path1);
  }

  #[test]
  fn popd_minus_index_out_of_range_errors() {
    let _g = TestGuard::new();
    clear_stack();
    test_input("popd -5").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn popd_n_flag_pops_without_cd() {
    let _g = TestGuard::new();
    clear_stack();
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().to_path_buf();
    test_input(format!("pushd {}", path.display())).unwrap();
    let before_cwd = env::current_dir().unwrap();
    test_input("popd -n").unwrap();
    // -n: pop the saved dir but do NOT cd.
    assert_eq!(env::current_dir().unwrap(), before_cwd);
    let stack = Shed::meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 0);
  }

  #[test]
  fn popd_n_flag_on_empty_stack_is_ok() {
    // With -n we skip the empty-stack ExecFail and just return 0.
    let _g = TestGuard::new();
    clear_stack();
    test_input("popd -n").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }
}
