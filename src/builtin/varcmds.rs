use crate::{
  libsh::error::ShResult, parse::{NdRule, Node, execute::prepare_argv, lex::split_tk_at}, prelude::*, procio::borrow_fd, sherr, expand::as_var_val_display, state::{self, VarFlags, VarKind, read_vars, write_vars}
};

pub fn readonly(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  // Remove "readonly" from argv
  let argv = if !argv.is_empty() {
    &argv[1..]
  } else {
    &argv[..]
  };

  if argv.is_empty() {
    // Display the local variables
    let vars_output = read_vars(|v| {
      let mut vars = v
        .flatten_vars()
        .into_iter()
        .filter(|(_, v)| v.flags().contains(VarFlags::READONLY))
        .map(|(k, v)| format!("{}={}", k, as_var_val_display(&v.to_string())))
        .collect::<Vec<String>>();
      vars.sort();
      let mut vars_joined = vars.join("\n");
      vars_joined.push('\n');
      vars_joined
    });

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, vars_output.as_bytes())?; // Write it
  } else {
    for tk in argv {
      if let Some((var_tk, val_tk)) = split_tk_at(tk, "=") {
        let var = var_tk.expand()?.get_words().join(" ");
        let val = if val_tk.as_str().starts_with('(') && val_tk.as_str().ends_with(')') {
          VarKind::arr_from_tk(val_tk.clone())?
        } else {
          VarKind::Str(val_tk.expand()?.get_words().join(" "))
        };
        write_vars(|v| v.set_var(&var, val, VarFlags::READONLY))?;
      } else {
        let arg = tk.clone().expand()?.get_words().join(" ");
        write_vars(|v| v.set_var(&arg, VarKind::Str(String::new()), VarFlags::READONLY))?;
      }
    }
  }

  state::set_status(0);
  Ok(())
}

pub fn unset(node: Node) -> ShResult<()> {
  let blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }

  if argv.is_empty() {
    return Err(sherr!(
      SyntaxErr @ blame,
      "unset: Expected at least one argument",
    ));
  }

  for (arg, span) in argv {
    if !read_vars(|v| v.var_exists(&arg)) {
      return Err(sherr!(
        ExecFail @ span,
        "unset: No such variable '{arg}'",
      ));
    }
    write_vars(|v| v.unset_var(&arg))?;
  }

  state::set_status(0);
  Ok(())
}

pub fn export(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  // Remove "export" from argv
  let argv = if !argv.is_empty() {
    &argv[1..]
  } else {
    &argv[..]
  };

  if argv.is_empty() {
    // Display the environment variables
    let mut env_output = env::vars()
      .map(|var| format!("{}={}", var.0, as_var_val_display(&var.1.to_string()))) // Get all of them, zip them into one string
      .collect::<Vec<_>>();
    env_output.sort(); // Sort them alphabetically
    let mut env_output = env_output.join("\n"); // Join them with newlines
    env_output.push('\n'); // Push a final newline

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, env_output.as_bytes())?; // Write it
  } else {
    for tk in argv {
      if let Some((var_tk, val_tk)) = split_tk_at(tk, "=") {
        let var = var_tk.expand()?.get_words().join(" ");
        let val = if val_tk.as_str().starts_with('(') && val_tk.as_str().ends_with(')') {
          VarKind::arr_from_tk(val_tk.clone())?
        } else {
          VarKind::Str(val_tk.expand()?.get_words().join(" "))
        };
        write_vars(|v| v.set_var(&var, val, VarFlags::EXPORT))?;
      } else {
        let arg = tk.clone().expand()?.get_words().join(" ");
        write_vars(|v| v.export_var(&arg)); // Export an existing variable, if any
      }
    }
  }
  state::set_status(0);
  Ok(())
}

pub fn local(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  // Remove "local" from argv
  let argv = if !argv.is_empty() {
    &argv[1..]
  } else {
    &argv[..]
  };

  if argv.is_empty() {
    // Display the local variables
    let vars_output = read_vars(|v| {
      let mut vars = v
        .flatten_vars()
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, as_var_val_display(&v.to_string())))
        .collect::<Vec<String>>();
      vars.sort();
      let mut vars_joined = vars.join("\n");
      vars_joined.push('\n');
      vars_joined
    });

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, vars_output.as_bytes())?; // Write it
  } else {
    for tk in argv {
      if let Some((var_tk, val_tk)) = split_tk_at(tk, "=") {
        let var = var_tk.expand()?.get_words().join(" ");
        let val = if val_tk.as_str().starts_with('(') && val_tk.as_str().ends_with(')') {
          VarKind::arr_from_tk(val_tk.clone())?
        } else {
          VarKind::Str(val_tk.expand()?.get_words().join(" "))
        };
        write_vars(|v| v.set_var(&var, val, VarFlags::LOCAL))?;
      } else {
        let arg = tk.clone().expand()?.get_words().join(" ");
        write_vars(|v| v.set_var(&arg, VarKind::Str(String::new()), VarFlags::LOCAL))?;
      }
    }
  }
  state::set_status(0);
  Ok(())
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
    let result = test_input("myvar=world");
    assert!(result.is_err());
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
    let result = test_input("unset __no_such_var__");
    assert!(result.is_err());
  }

  #[test]
  fn unset_no_args_fails() {
    let _g = TestGuard::new();
    let result = test_input("unset");
    assert!(result.is_err());
  }

  #[test]
  fn unset_readonly_fails() {
    let _g = TestGuard::new();
    test_input("readonly myvar=protected").unwrap();
    let result = test_input("unset myvar");
    assert!(result.is_err());
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
