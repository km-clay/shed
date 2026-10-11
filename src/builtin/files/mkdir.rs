use std::{
  fs::Permissions,
  io::{self, ErrorKind as EK},
  os::unix::fs::PermissionsExt,
  path::Path,
};

use nix::{
  errno::Errno,
  libc,
  sys::stat::{self, Mode},
  unistd,
};

use crate::{
  opt, sherr,
  state::vars::VarStr,
  util::{
    self,
    error::{ShErr, ShResultExt},
    strops,
  },
};

use super::{
  super::{Builtin, BuiltinArgs, ShResult, opt::OptSpec},
  chmod,
};

/// `mkdir(2)`, with `EEXIST` on an existing directory reported as
/// `Ok(false)` when `tolerate_existing` is set. An existing non-directory
/// stays an error so the caller can describe what is in the way.
fn make_dir(path: &Path, mode: stat::mode_t, tolerate_existing: bool) -> Result<bool, Errno> {
  match unistd::mkdir(path, Mode::from_bits_truncate(mode)) {
    Ok(()) => Ok(true),
    Err(Errno::EEXIST) if tolerate_existing && path.is_dir() => Ok(false),
    Err(e) => Err(e),
  }
}

#[expect(clippy::needless_pass_by_value)]
fn get_mode(mode: VarStr) -> ShResult<stat::mode_t> {
  let spec   = mode.to_str_lossy();
  let digits = spec.strip_prefix("0o").unwrap_or(&spec);
  let octal  = u32::from_str_radix(digits, 8).ok().filter(|&n| n <= 0o7777);

  let mode = match octal {
    Some(octal) => octal,
    None if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
      return Err(
        sherr!(ExecFail, "invalid mode: `{spec}`")
          .with_note("octal mode must be in the range 0o0000 to 0o7777".into())
          .with_code(2),
      );
    }
    None => {
      let clauses = strops::parse_mode_clauses(&spec).with_code(2)?;
      strops::apply_mode_clauses(0o777, &clauses)
    }
  };

  Ok(mode as stat::mode_t)
}

pub(super) struct MkDir;
impl Builtin for MkDir {
  fn strict_opts(&self) -> bool {
    true
  }

  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::new_short("parents", b'p'), opt!("mode" | b'm', 1)]
  }
  #[allow(clippy::useless_conversion)]
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    if args.no_arguments() {
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "missing argument for `fs mkdir`").with_code(2),
      );
    }

    let mut status         = 0;
    let     arguments      = args.arguments();
    let     mode_specified = args.has_opt("mode");
    let     mkdir_all      = args.has_opt("parents");
    let mode: stat::mode_t = args
      .opt_value("mode")
      .map(get_mode)
      .transpose()
      .option_promote(args.opt_span("mode"))?
      .unwrap_or(0o777);

    // Parent components are created at the default mode
    // with `u+wx` guaranteed, so each one can be descended into to create the next.
    let umask = stat::umask(Mode::empty());
    stat::umask(umask);
    let default_mode  = 0o777 & !u32::from(umask.bits());
    let ancestor_mode = default_mode | 0o300;
    let umask_hides_u = default_mode & 0o300 != 0o300;

    'args: for (dir, d_span) in arguments {
      let path: &Path = dir.as_ref();

      if mkdir_all {
        // `ancestors()` ends in the empty path for a relative operand, and
        // yields outermost last, so drop `path` itself, drop the empty tail,
        // and walk back outwards.
        let mut parents: Vec<&Path> = path
          .ancestors()
          .skip(1)
          .filter(|p| !p.as_os_str().is_empty())
          .collect();
        parents.reverse();

        for parent in parents {
          let created = match make_dir(parent, 0o777, true) {
            Ok(created) => created,
            Err(e) => {
              let io_err = io::Error::from_raw_os_error(e as i32);
              let err    = handle_mkdir_err(io_err, &VarStr::from(parent)).promote(d_span);

              err.print_error();
              status = 1;
              continue 'args;
            }
          };

          // `mkdir(2)` filters the mode through the umask, which cannot be
          // expressed as a mode argument, so a umask that masks `u+wx` needs
          // the bits restored before the next component can be created in it.
          if created
            && umask_hides_u
            && let Err(e) = std::fs::set_permissions(parent, Permissions::from_mode(ancestor_mode))
          {
            let err = chmod::handle_err(e, &VarStr::from(parent))
              .with_note("the parent directory needs `u+wx` to create anything inside it".into())
              .promote(d_span);

            err.print_error();
            status = 1;
            continue 'args;
          }
        }
      }

      let created = match make_dir(path, mode, mkdir_all) {
        Ok(created) => created,
        Err(e) => {
          let io_err = io::Error::from_raw_os_error(e as i32);
          let err    = handle_mkdir_err(io_err, dir).promote(d_span);

          err.print_error();
          status = 1;
          continue 'args;
        }
      };

      // An operand that was already a directory keeps the mode it had.
      if mode_specified
        && created
        && let Err(e) = std::fs::set_permissions(path, Permissions::from_mode(mode.into()))
      {
        let err = chmod::handle_err(e, dir)
          .with_note("the directory was created; only its mode could not be set".into())
          .promote(d_span);

        err.print_error();
        status = 1;
      }
    }

    util::with_status(status)
  }
}

fn handle_mkdir_err(io_err: io::Error, dir: &VarStr) -> ShErr {
  match io_err.kind() {
    EK::AlreadyExists => {
      let path: &Path = dir.as_ref();
      if path.exists() && !path.is_dir() {
        sherr!(ExecFail, "cannot make directory `{dir}`: exists but is not a directory")
      } else {
        sherr!(ExecFail, "cannot make directory `{dir}`: name already exists")
      }
    }
    EK::PermissionDenied => sherr!(ExecFail, "cannot make directory `{dir}`: permission denied"),
    EK::InvalidFilename  => sherr!(ExecFail, "cannot make directory `{dir}`: name too long"),
    EK::ReadOnlyFilesystem => {
      sherr!(ExecFail, "cannot make directory `{dir}`: read-only filesystem")
    }
    EK::StorageFull => sherr!(ExecFail, "cannot make directory `{dir}`: no space left on device"),
    EK::NotADirectory => {
      sherr!(ExecFail, "cannot make directory `{dir}`: path component is not a directory")
    }
    EK::NotFound => {
      sherr!(ExecFail, "cannot make directory `{dir}`: parent directory does not exist")
        .with_note("the `-p` flag makes `fs mkdir` create parents as well: `fs mkdir -p`".into())
    }

    _ => {
      if io_err.raw_os_error() == Some(libc::ELOOP) {
        sherr!(ExecFail, "cannot make directory `{dir}`: too many levels of symbolic links")
      } else {
        sherr!(ExecFail, "cannot make directory `{dir}`: {io_err}")
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use std::fs;
  use std::os::unix::fs::PermissionsExt;

  use nix::sys::stat::{self, Mode};
  use tempfile::TempDir;

  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  fn mode_of(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
  }

  /// `mkdir(2)` filters through the umask, so the expected default is not a
  /// constant.
  #[expect(clippy::useless_conversion)]
  fn default_mode() -> u32 {
    let umask = stat::umask(Mode::empty());
    stat::umask(umask);
    0o777 & !u32::from(umask.bits())
  }

  #[test]
  fn fs_mkdir_parents_creates_the_whole_chain() {
    let _g   = TestGuard::new();
    let dir  = TempDir::new().unwrap();
    let leaf = dir.path().join("a/b/c");
    test_input(format!("fs mkdir -p {}", leaf.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert!(leaf.is_dir());
    assert!(dir.path().join("a/b").is_dir());
  }

  #[test]
  fn fs_mkdir_parents_is_idempotent() {
    let _g   = TestGuard::new();
    let dir  = TempDir::new().unwrap();
    let leaf = dir.path().join("a/b/c");
    test_input(format!("fs mkdir -p {}", leaf.display())).unwrap();
    test_input(format!("fs mkdir -p {}", leaf.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn fs_mkdir_parents_mode_applies_to_the_leaf_only() {
    let _g   = TestGuard::new();
    let dir  = TempDir::new().unwrap();
    let leaf = dir.path().join("a/b/c");
    test_input(format!("fs mkdir -p -m 700 {}", leaf.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(mode_of(&leaf), 0o700);
    assert_eq!(mode_of(&dir.path().join("a")), default_mode());
    assert_eq!(mode_of(&dir.path().join("a/b")), default_mode());
  }

  #[test]
  fn fs_mkdir_parents_leaves_an_existing_mode_alone() {
    let _g   = TestGuard::new();
    let dir  = TempDir::new().unwrap();
    let leaf = dir.path().join("existing");
    fs::create_dir(&leaf).unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o755)).unwrap();
    test_input(format!("fs mkdir -p -m 700 {}", leaf.display())).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(mode_of(&leaf), 0o755);
  }

  #[test]
  fn fs_mkdir_without_parents_still_rejects_an_existing_dir() {
    let _g  = TestGuard::new();
    let dir = TempDir::new().unwrap();
    test_input(format!("fs mkdir {}", dir.path().display())).ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn fs_mkdir_parents_rejects_a_non_directory_in_the_way() {
    let _g      = TestGuard::new();
    let dir     = TempDir::new().unwrap();
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, b"").unwrap();
    test_input(format!("fs mkdir -p {}", blocker.join("deep").display())).ok();
    assert_ne!(state::Shed::get_status(), 0);
    assert!(!blocker.join("deep").exists());
  }
}
