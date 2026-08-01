use std::path::PathBuf;

use crate::{ShErrKind, Shed, state::vars::VarStr};

use super::{ShResult, sherr, state::util::source_file};

pub(super) struct Source;
impl super::Builtin for Source {
  fn is_special(&self) -> bool {
    true
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut arg_vec = args.argv.into_iter();
    let Some((file, span)) = arg_vec.next() else {
      return Err(sherr!(
        ExecFail @ args.span,
        "source: filename argument required",
      ));
    };
    let path = PathBuf::from(file);

    if !path.exists() {
      return Err(sherr!(
        ExecFail @ span,
        "source: File '{}' not found", path.display(),
      ));
    } else if !path.is_file() {
      return Err(sherr!(
        ExecFail @ span,
        "source: Given path '{}' is not a file", path.display(),
      ));
    }

    let extra: Vec<VarStr> = arg_vec.map(|(arg, _)| arg).collect();
    let saved_argv = (!extra.is_empty()).then(|| {
      Shed::vars_mut(|v| {
        let scope = v.cur_scope_mut();
        let saved = scope.sh_argv().clone();
        let dollar0 = saved.front().cloned().unwrap_or_default();
        scope.sh_argv_mut().clear();
        scope.bpush_arg(dollar0);
        for arg in &extra {
          scope.bpush_arg(arg.clone());
        }
        saved
      })
    });

    let result = source_file(path);

    if let Some(saved) = saved_argv {
      Shed::vars_mut(|v| {
        let scope = v.cur_scope_mut();
        scope.sh_argv_mut().clear();
        for arg in saved {
          scope.bpush_arg(arg);
        }
      });
    }

    if let Err(e) = result
      && let ShErrKind::Raised(_, code) = e.kind()
    {
      Shed::set_status(*code);
      return Err(e.force_promote(span));
    }

    Ok(())
  }
}

#[cfg(test)]
pub mod tests {
  use std::io::Write;

  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, test_input};
  use crate::var;
  use tempfile::{NamedTempFile, TempDir};

  #[test]
  fn source_simple() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"some_var=some_val").unwrap();

    test_input(format!("source {path}")).unwrap();
    let var = var!("some_var");
    assert_eq!(var, "some_val".to_string());
  }

  #[test]
  fn source_multiple_commands() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"x=1\ny=2\nz=3").unwrap();

    test_input(format!("source {path}")).unwrap();
    assert_eq!(var!("x"), "1");
    assert_eq!(var!("y"), "2");
    assert_eq!(var!("z"), "3");
  }

  #[test]
  fn source_defines_function() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"greet() { echo hi; }").unwrap();

    test_input(format!("source {path}")).unwrap();
    let func = Shed::logic(|l| l.get_func("greet"));
    assert!(func.is_some());
  }

  #[test]
  fn source_defines_alias() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"alias ll='ls -la'").unwrap();

    test_input(format!("source {path}")).unwrap();
    let alias = Shed::logic(|l| l.get_alias("ll"));
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
  fn source_passes_positional_params() {
    // POSIX: `. file a b c` sources only `file`; a/b/c become $1/$2/$3.
    let guard = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file
      .write_all(b"echo \"count=$# all=$* one=$1 two=$2\"")
      .unwrap();

    test_input(format!("source {path} x y z")).unwrap();
    let out = guard.read_output();
    assert!(out.contains("count=3"), "got: {out:?}");
    assert!(out.contains("all=x y z"), "got: {out:?}");
    assert!(out.contains("one=x two=y"), "got: {out:?}");
  }

  #[test]
  fn source_restores_positional_params() {
    // The caller's positional parameters are restored after the source.
    let guard = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b":").unwrap(); // no-op body

    test_input(format!(
      "set -- outer1 outer2; source {path} inner; echo \"after=$# $1\""
    ))
    .unwrap();
    let out = guard.read_output();
    assert!(out.contains("after=2 outer1"), "got: {out:?}");
  }

  #[test]
  fn source_no_args_leaves_positionals_unchanged() {
    let guard = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"echo \"inner=$# $1\"").unwrap();

    test_input(format!(
      "set -- keep1 keep2; source {path}; echo \"after=$# $1\""
    ))
    .unwrap();
    let out = guard.read_output();
    assert!(
      out.contains("inner=2 keep1"),
      "sourced script should see caller's params: {out:?}"
    );
    assert!(out.contains("after=2 keep1"), "got: {out:?}");
  }

  // ===================== Dot syntax =====================

  #[test]
  fn source_dot_syntax() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"dot_var=dot_val").unwrap();

    test_input(format!(". {path}")).unwrap();
    assert_eq!(var!("dot_var"), "dot_val");
  }

  // ===================== Error cases =====================

  #[test]
  fn source_nonexistent_file() {
    let _g = TestGuard::new();
    test_input("source /tmp/__no_such_file_xyz__").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn source_directory_fails() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    test_input(format!("source {}", dir.path().display())).ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Status =====================

  #[test]
  fn source_status_zero() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"true").unwrap();

    test_input(format!("source {path}")).unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn source_top_level_return_exits_script_cleanly() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file
      .write_all(b"before=set\nreturn 0\nafter=should_not_run\n")
      .unwrap();

    test_input(format!("source {path}")).unwrap();
    assert_eq!(var!("before"), "set");
    assert_eq!(var!("after"), "");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn source_top_level_return_propagates_status() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file.write_all(b"return 42").unwrap();

    test_input(format!("source {path}")).unwrap();
    assert_eq!(state::Shed::get_status(), 42);
  }

  #[test]
  fn source_return_inside_conditional() {
    let _g = TestGuard::new();
    let mut file = NamedTempFile::new().unwrap();
    let path = file.path().display().to_string();
    file
      .write_all(
        b"GUARD=yes\n\
          if [ -n \"$GUARD\" ]; then return; fi\n\
          after=should_not_run\n",
      )
      .unwrap();

    test_input(format!("source {path}")).unwrap();
    assert_eq!(var!("after"), "");
  }
}
