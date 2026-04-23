use std::{env, path::PathBuf};

use ariadne::Fmt;
use nix::{libc::STDOUT_FILENO, unistd::write};

use crate::{
  getopt::{Opt, OptSpec},
  parse::lex::Span,
  procio::borrow_fd,
  sherr,
  state::{change_dir, read_meta, write_meta},
  util::{
    error::{ShResult, ShResultExt, next_color},
    with_status, write_out,
  },
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
  let no_cd = args.opts.iter().any(|o| matches!(o, Opt::Short('n')));
  let mut index = None;
  let mut dir = None;

  for (arg, _) in &args.argv {
    if is_index_arg(arg) {
      index = Some(parse_stack_idx(arg, args.span(), cmd)?);
    } else if arg.starts_with('-') {
      return Err(sherr!(
        ExecFail @ args.span(),
        "{cmd}: invalid option: '{}'", arg.fg(next_color()),
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
          target.display().fg(next_color()),
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
    vec![OptSpec::flag('n')]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let blame = args.span();
    let parsed = parse_dirstack_args(&args, "pushd")?;

    if let Some(idx) = parsed.index {
      let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
      let new_cwd = write_meta(|m| {
        let dirs = m.dirs_mut();
        dirs.push_front(cwd);
        match idx {
          StackIdx::FromTop(n) => dirs.rotate_left(n),
          StackIdx::FromBottom(n) => dirs.rotate_right(n + 1),
        }
        dirs.pop_front()
      });

      if let Some(dir) = new_cwd
        && !parsed.no_cd
      {
        change_dir(&dir).promote_err(blame)?;
        print_dirs()?;
      }
    } else if let Some(dir) = parsed.dir {
      let old_dir = env::current_dir()?;
      if old_dir != dir {
        write_meta(|m| m.push_dir(old_dir));
      }

      if parsed.no_cd {
        return with_status(0);
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
    vec![OptSpec::flag('n')]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let blame = args.span();
    let parsed = parse_dirstack_args(&args, "popd")?;

    if let Some(idx) = parsed.index {
      match idx {
        StackIdx::FromTop(0) => {
          // +0 is same as plain popd: pop top, cd to it
          let dir = write_meta(|m| m.pop_dir());
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
          write_meta(|m| {
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
          write_meta(|m| -> ShResult<()> {
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
      let dir = write_meta(|m| m.pop_dir());

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
  fn opts(&self) -> Vec<crate::getopt::OptSpec> {
    vec![
      OptSpec::flag('p'),
      OptSpec::flag('v'),
      OptSpec::flag('c'),
      OptSpec::flag('l'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut abbreviate_home = true;
    let mut one_per_line = false;
    let mut one_per_line_indexed = false;
    let mut clear_stack = false;
    let mut target_idx: Option<StackIdx> = None;
    let blame = args.span();
    let argv = args.argv;

    for opt in args.opts {
      match opt {
        Opt::Short('p') => one_per_line = true,
        Opt::Short('v') => one_per_line_indexed = true,
        Opt::Short('c') => clear_stack = true,
        Opt::Short('l') => abbreviate_home = false,
        _ => {}
      }
    }

    for (arg, _) in argv {
      match arg.as_str() {
        _ if is_index_arg(&arg) => {
          target_idx = Some(parse_stack_idx(&arg, blame.clone(), "dirs")?);
        }
        _ if arg.starts_with('-') => {
          return Err(sherr!(
            ExecFail @ blame,
            "dirs: invalid option: '{}'", arg.fg(next_color()),
          ));
        }
        _ => {
          return Err(sherr!(
            ExecFail @ blame,
            "dirs: unexpected argument: '{}'", arg.fg(next_color()),
          ));
        }
      }
    }

    if clear_stack {
      write_meta(|m| m.dirs_mut().clear());
      return Ok(());
    }

    let mut dirs: Vec<String> = read_meta(|m| {
      let current_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
      let stack = [current_dir]
        .into_iter()
        .chain(m.dirs().clone())
        .map(|d| d.to_string_lossy().to_string());

      if abbreviate_home {
        stack.map(truncate_home_path).collect()
      } else {
        stack.collect()
      }
    });

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

    let mut output = String::new();

    if one_per_line {
      output = dirs.join("\n");
    } else if one_per_line_indexed {
      for (i, dir) in dirs.iter_mut().enumerate() {
        *dir = format!("{i}\t{dir}");
      }
      output = dirs.join("\n");
      output.push('\n');
    } else {
      print_dirs()?;
    }

    write_out(output)?;

    with_status(0)
  }
}

pub fn truncate_home_path(path: String) -> String {
  if let Ok(home) = env::var("HOME")
    && path.starts_with(&home)
  {
    let new = path.strip_prefix(&home).unwrap();
    return format!("~{new}");
  }
  path.to_string()
}

enum StackIdx {
  FromTop(usize),
  FromBottom(usize),
}

fn print_dirs() -> ShResult<()> {
  let current_dir = env::current_dir()?;
  let dirs_iter = read_meta(|m| m.dirs().clone().into_iter());
  let all_dirs = [current_dir]
    .into_iter()
    .chain(dirs_iter)
    .map(|d| d.to_string_lossy().to_string())
    .map(truncate_home_path)
    .collect::<Vec<_>>()
    .join(" ");

  let stdout = borrow_fd(STDOUT_FILENO);
  write(stdout, all_dirs.as_bytes())?;
  write(stdout, b"\n")?;

  Ok(())
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
        "{cmd}: invalid argument: '{}'", arg.fg(next_color()),
      ));
    }
  }

  let n = digits.parse::<usize>().map_err(|e| {
    sherr!(
      ExecFail @ blame,
      "{cmd}: invalid index: '{}'", e.fg(next_color()),
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
    state::{self, read_meta},
    testutil::{TestGuard, test_input},
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
    assert_eq!(new_dir, PathBuf::from("/tmp"));

    let dir_stack = read_meta(|m| m.dirs().clone());
    assert_eq!(dir_stack.len(), 1);
    assert_eq!(dir_stack[0], current_dir);

    let out = g.read_output();
    let path = super::truncate_home_path(current_dir.to_string_lossy().to_string());
    assert_eq!(out, format!("/tmp {path}\n"));
  }

  #[test]
  fn test_popd_interactive() {
    let g = TestGuard::new();
    let current_dir = env::current_dir().unwrap();
    let tempdir = TempDir::new().unwrap();
    let tempdir_raw = tempdir.path().to_path_buf().to_string_lossy().to_string();

    test_input(format!("pushd {tempdir_raw}")).unwrap();

    let dir_stack = read_meta(|m| m.dirs().clone());
    assert_eq!(dir_stack.len(), 1);
    assert_eq!(dir_stack[0], current_dir);

    assert_eq!(env::current_dir().unwrap(), tempdir.path());
    g.read_output(); // consume output of pushd

    test_input("popd").unwrap();

    assert_eq!(env::current_dir().unwrap(), current_dir);
    let out = g.read_output();
    let path = super::truncate_home_path(current_dir.to_string_lossy().to_string());
    assert_eq!(out, format!("{path}\n"));
  }

  #[test]
  fn test_popd_empty_stack() {
    let _g = TestGuard::new();

    test_input("popd").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn test_pushd_multiple_then_popd() {
    let g = TestGuard::new();
    let original = env::current_dir().unwrap();
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();
    let path1 = tmp1.path().to_path_buf();
    let path2 = tmp2.path().to_path_buf();

    test_input(format!("pushd {}", path1.display())).unwrap();
    test_input(format!("pushd {}", path2.display())).unwrap();
    g.read_output();

    assert_eq!(env::current_dir().unwrap(), path2);
    let stack = read_meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 2);
    assert_eq!(stack[0], path1);
    assert_eq!(stack[1], original);

    test_input("popd").unwrap();
    assert_eq!(env::current_dir().unwrap(), path1);

    test_input("popd").unwrap();
    assert_eq!(env::current_dir().unwrap(), original);

    let stack = read_meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 0);
  }

  #[test]
  fn test_pushd_rotate_plus() {
    let g = TestGuard::new();
    let original = env::current_dir().unwrap();
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();
    let path1 = tmp1.path().to_path_buf();
    let path2 = tmp2.path().to_path_buf();

    // Build stack: cwd=original, then pushd path1, pushd path2
    // Stack after: cwd=path2, [path1, original]
    test_input(format!("pushd {}", path1.display())).unwrap();
    test_input(format!("pushd {}", path2.display())).unwrap();
    g.read_output();

    // pushd +1 rotates: [path2, path1, original] -> rotate_left(1) -> [path1, original, path2]
    // pop front -> cwd=path1, stack=[original, path2]
    test_input("pushd +1").unwrap();
    assert_eq!(env::current_dir().unwrap(), path1);

    let stack = read_meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 2);
    assert_eq!(stack[0], original);
    assert_eq!(stack[1], path2);
  }

  #[test]
  fn test_pushd_no_cd_flag() {
    let _g = TestGuard::new();
    let original = env::current_dir().unwrap();
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().to_path_buf();

    test_input(format!("pushd -n {}", path.display())).unwrap();

    // -n means don't cd, but the dir should still be on the stack
    assert_eq!(env::current_dir().unwrap(), original);
  }

  #[test]
  fn test_dirs_clear() {
    let _g = TestGuard::new();
    let tmp = TempDir::new().unwrap();

    test_input(format!("pushd {}", tmp.path().display())).unwrap();
    assert_eq!(read_meta(|m| m.dirs().len()), 1);

    test_input("dirs -c").unwrap();
    assert_eq!(read_meta(|m| m.dirs().len()), 0);
  }

  #[test]
  fn test_dirs_one_per_line() {
    let g = TestGuard::new();
    let original = env::current_dir().unwrap();
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().to_path_buf();

    test_input(format!("pushd {}", path.display())).unwrap();
    g.read_output();

    test_input("dirs -p").unwrap();
    let out = g.read_output();
    let lines: Vec<&str> = out.split('\n').filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
      lines[0],
      super::truncate_home_path(path.to_string_lossy().to_string())
    );
    assert_eq!(
      lines[1],
      super::truncate_home_path(original.to_string_lossy().to_string())
    );
  }

  #[test]
  fn test_popd_indexed_from_top() {
    let _g = TestGuard::new();
    let original = env::current_dir().unwrap();
    let tmp1 = TempDir::new().unwrap();
    let tmp2 = TempDir::new().unwrap();
    let path1 = tmp1.path().to_path_buf();
    let path2 = tmp2.path().to_path_buf();

    // Stack: cwd=path2, [path1, original]
    test_input(format!("pushd {}", path1.display())).unwrap();
    test_input(format!("pushd {}", path2.display())).unwrap();

    // popd +1 removes index (1-1)=0 from stored dirs, i.e. path1
    test_input("popd +1").unwrap();
    assert_eq!(env::current_dir().unwrap(), path2); // no cd

    let stack = read_meta(|m| m.dirs().clone());
    assert_eq!(stack.len(), 1);
    assert_eq!(stack[0], original);
  }

  #[test]
  fn test_pushd_nonexistent_dir() {
    let _g = TestGuard::new();

    test_input("pushd /nonexistent_dir_12345").ok();
    assert_ne!(state::get_status(), 0);
  }
}
