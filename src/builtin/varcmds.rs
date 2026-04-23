use crate::{
  expand::as_var_val_display,
  prelude::*,
  sherr,
  state::{ScopeStack, VarFlags, VarKind, read_vars, write_vars},
  util::{
    error::{ShResult, ShResultExt},
    strops::split_at_unescaped,
    with_status, write_ln_out,
  },
};

/// Display key/value pairs as '{key}={value}\n'
///
/// The 'value' is escaped in such a way that the whole line can be reused as a shell assignment
pub fn display_as_vars(vars: impl Iterator<Item = (impl ToString, impl ToString)>) -> String {
  let mut vars = vars
    .map(|(k, v)| display_as_var(k, v))
    .collect::<Vec<String>>();
  vars.sort();
  vars.join("\n")
}

pub fn display_as_var(name: impl ToString, value: impl ToString) -> String {
  format!(
    "{}={}",
    name.to_string(),
    as_var_val_display(&value.to_string())
  )
}

fn display_env_vars() -> String {
  display_as_vars(env::vars())
}

fn display_vars_internal(vars: &ScopeStack, filter: Option<VarFlags>) -> String {
  let vars = vars.flatten_vars().into_iter();

  if let Some(flags) = filter {
    display_as_vars(vars.filter(|(_, v)| v.flags().contains(flags)))
  } else {
    display_as_vars(vars)
  }
}

fn display_readonly(vars: &ScopeStack) -> String {
  display_vars_internal(vars, Some(VarFlags::READONLY))
}

fn display_local(vars: &ScopeStack) -> String {
  display_vars_internal(vars, None)
}

pub fn split_assignment(arg: String) -> (String, Option<VarKind>) {
  let Some((e, l)) = split_at_unescaped(&arg, "=") else {
    return (arg, None);
  };
  let var = arg[..e].trim().to_string();
  let val = arg[e + l..].to_string();
  (var, Some(VarKind::parse(&val)))
}

pub fn split_assignment_raw(arg: String) -> (String, Option<String>) {
  let Some((e, l)) = split_at_unescaped(&arg, "=") else {
    return (arg, None);
  };
  let var = arg[..e].trim().to_string();
  let val = arg[e + l..].to_string();
  (var, Some(val))
}

pub(super) struct Readonly;
impl super::Builtin for Readonly {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      // Display the local variables
      write_ln_out(read_vars(display_readonly))?;

      return with_status(0);
    }

    for (arg, span) in args.argv {
      let (var, val) = split_assignment(arg);
      write_vars(|v| {
        v.set_var(&var, val.unwrap_or_default(), VarFlags::READONLY)
          .promote_err(span)
      })?;
    }

    with_status(0)
  }
}

pub(super) struct Unset;
impl super::Builtin for Unset {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    for (arg, span) in args.argv {
      if !read_vars(|v| v.var_exists(&arg)) {
        return Err(sherr!(
            ExecFail @ span,
            "unset: No such variable '{arg}'",
        ));
      }
      write_vars(|v| v.unset_var(&arg))?;
    }

    with_status(0)
  }
}

pub(super) struct Export;
impl super::Builtin for Export {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      // Display the environment variables
      write_ln_out(display_env_vars())?;
      return with_status(0);
    }

    for (arg, span) in args.argv {
      let (var, val) = split_assignment(arg);
      if let Some(val) = val {
        write_vars(|v| v.set_var(&var, val, VarFlags::EXPORT)).promote_err(span)?;
      } else {
        // Export an existing variable, if any
        write_vars(|v| v.export_var(&var));
      }
    }

    with_status(0)
  }
}

pub(super) struct Local;
impl super::Builtin for Local {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      write_ln_out(read_vars(display_local))?;
      return with_status(0);
    }

    for (arg, span) in args.argv {
      let (var, val) = split_assignment(arg);
      write_vars(|v| {
        v.set_var(&var, val.unwrap_or_default(), VarFlags::LOCAL)
          .promote_err(span)
      })?;
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{self, VarFlags, read_vars};
  use crate::testutil::{TestGuard, test_input};

  // ===================== readonly =====================

  #[test]
  fn readonly_sets_flag() {
    let _g = TestGuard::new();
    test_input("readonly myvar").unwrap();
    let flags = read_vars(|v| v.get_var_flags("myvar"));
    assert!(flags.unwrap().contains(VarFlags::READONLY));
  }

  #[test]
  fn readonly_with_value() {
    let _g = TestGuard::new();
    test_input("readonly myvar=hello").unwrap();
    assert_eq!(read_vars(|v| v.get_var("myvar")), "hello");
    let flags = read_vars(|v| v.get_var_flags("myvar"));
    assert!(flags.unwrap().contains(VarFlags::READONLY));
  }

  #[test]
  fn readonly_prevents_reassignment() {
    let _g = TestGuard::new();
    test_input("readonly myvar=hello").unwrap();
    test_input("myvar=world").ok();
    assert_ne!(state::get_status(), 0);
    assert_eq!(read_vars(|v| v.get_var("myvar")), "hello");
  }

  #[test]
  fn readonly_display() {
    let guard = TestGuard::new();
    test_input("readonly rdo_test_var=abc").unwrap();
    test_input("readonly").unwrap();
    let out = guard.read_output();
    assert!(out.contains("rdo_test_var=abc"));
  }

  #[test]
  fn readonly_multiple() {
    let _g = TestGuard::new();
    test_input("readonly a=1 b=2").unwrap();
    assert_eq!(read_vars(|v| v.get_var("a")), "1");
    assert_eq!(read_vars(|v| v.get_var("b")), "2");
    assert!(
      read_vars(|v| v.get_var_flags("a"))
        .unwrap()
        .contains(VarFlags::READONLY)
    );
    assert!(
      read_vars(|v| v.get_var_flags("b"))
        .unwrap()
        .contains(VarFlags::READONLY)
    );
  }

  #[test]
  fn readonly_status_zero() {
    let _g = TestGuard::new();
    test_input("readonly x=1").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  // ===================== unset =====================

  #[test]
  fn unset_removes_variable() {
    let _g = TestGuard::new();
    test_input("myvar=hello").unwrap();
    assert_eq!(read_vars(|v| v.get_var("myvar")), "hello");
    test_input("unset myvar").unwrap();
    assert_eq!(read_vars(|v| v.get_var("myvar")), "");
  }

  #[test]
  fn unset_multiple() {
    let _g = TestGuard::new();
    test_input("a=1").unwrap();
    test_input("b=2").unwrap();
    test_input("unset a b").unwrap();
    assert_eq!(read_vars(|v| v.get_var("a")), "");
    assert_eq!(read_vars(|v| v.get_var("b")), "");
  }

  #[test]
  fn unset_nonexistent_fails() {
    let _g = TestGuard::new();
    test_input("unset __no_such_var__").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn unset_readonly_fails() {
    let _g = TestGuard::new();
    test_input("readonly myvar=protected").unwrap();
    test_input("unset myvar").ok();
    assert_ne!(state::get_status(), 0);
    assert_eq!(read_vars(|v| v.get_var("myvar")), "protected");
  }

  #[test]
  fn unset_status_zero() {
    let _g = TestGuard::new();
    test_input("x=1").unwrap();
    test_input("unset x").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  // ===================== export =====================

  #[test]
  fn export_with_value() {
    let _g = TestGuard::new();
    test_input("export SHED_TEST_VAR=hello_export").unwrap();
    assert_eq!(read_vars(|v| v.get_var("SHED_TEST_VAR")), "hello_export");
    assert_eq!(std::env::var("SHED_TEST_VAR").unwrap(), "hello_export");
    unsafe { std::env::remove_var("SHED_TEST_VAR") };
  }

  #[test]
  fn export_existing_variable() {
    let _g = TestGuard::new();
    test_input("SHED_TEST_VAR2=existing").unwrap();
    test_input("export SHED_TEST_VAR2").unwrap();
    assert_eq!(std::env::var("SHED_TEST_VAR2").unwrap(), "existing");
    unsafe { std::env::remove_var("SHED_TEST_VAR2") };
  }

  #[test]
  fn export_sets_flag() {
    let _g = TestGuard::new();
    test_input("export SHED_TEST_VAR3=flagged").unwrap();
    let flags = read_vars(|v| v.get_var_flags("SHED_TEST_VAR3"));
    assert!(flags.unwrap().contains(VarFlags::EXPORT));
    unsafe { std::env::remove_var("SHED_TEST_VAR3") };
  }

  #[test]
  fn export_display() {
    let guard = TestGuard::new();
    test_input("export").unwrap();
    let out = guard.read_output();
    assert!(out.contains("PATH=") || out.contains("HOME="));
  }

  #[test]
  fn export_multiple() {
    let _g = TestGuard::new();
    test_input("export SHED_A=1 SHED_B=2").unwrap();
    assert_eq!(std::env::var("SHED_A").unwrap(), "1");
    assert_eq!(std::env::var("SHED_B").unwrap(), "2");
    unsafe { std::env::remove_var("SHED_A") };
    unsafe { std::env::remove_var("SHED_B") };
  }

  #[test]
  fn export_status_zero() {
    let _g = TestGuard::new();
    test_input("export SHED_ST=1").unwrap();
    assert_eq!(state::get_status(), 0);
    unsafe { std::env::remove_var("SHED_ST") };
  }

  // ===================== local =====================

  #[test]
  fn local_sets_variable() {
    let _g = TestGuard::new();
    test_input("local mylocal=hello").unwrap();
    assert_eq!(read_vars(|v| v.get_var("mylocal")), "hello");
  }

  #[test]
  fn local_sets_flag() {
    let _g = TestGuard::new();
    test_input("local mylocal=val").unwrap();
    let flags = read_vars(|v| v.get_var_flags("mylocal"));
    assert!(flags.unwrap().contains(VarFlags::LOCAL));
  }

  #[test]
  fn local_empty_value() {
    let _g = TestGuard::new();
    test_input("local mylocal").unwrap();
    assert_eq!(read_vars(|v| v.get_var("mylocal")), "");
    assert!(
      read_vars(|v| v.get_var_flags("mylocal"))
        .unwrap()
        .contains(VarFlags::LOCAL)
    );
  }

  #[test]
  fn local_display() {
    let guard = TestGuard::new();
    test_input("lv_test=display_val").unwrap();
    test_input("local").unwrap();
    let out = guard.read_output();
    assert!(out.contains("lv_test=display_val"));
  }

  #[test]
  fn local_multiple() {
    let _g = TestGuard::new();
    test_input("local x=10 y=20").unwrap();
    assert_eq!(read_vars(|v| v.get_var("x")), "10");
    assert_eq!(read_vars(|v| v.get_var("y")), "20");
  }

  #[test]
  fn local_status_zero() {
    let _g = TestGuard::new();
    test_input("local z=1").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
