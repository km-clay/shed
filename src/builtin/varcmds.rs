use crate::{
  expand::as_var_val_display,
  getopt::Opt,
  parse::lex::{Span, Tk},
  prelude::*,
  sherr,
  state::{ScopeStack, VarFlags, VarKind, read_vars, write_vars},
  util::{
    error::{ShResult, ShResultExt},
    strops::split_at_unescaped,
    with_status, write_ln_out,
  },
};

/// Like `prepare_argv` but preserves raw token text for `name=(...)` array
/// literal assignments. The normal expansion pipeline runs `unescape_str`
/// which treats `(` as a subshell opener and strips parens, breaking array
/// assignment via `local`/`readonly`/`export`. Tokens that look like array
/// literals are passed through verbatim so `arr_from_raw` can parse them.
pub fn prepare_assignment_argv(argv: Vec<Tk>) -> ShResult<Vec<(String, Span)>> {
  let mut args = vec![];
  for tk in argv {
    let raw = tk.span.as_str();
    let is_arr_lit = raw.find('=').is_some_and(|eq| {
      raw[eq + 1..].starts_with('(') && raw.ends_with(')')
    });
    if is_arr_lit {
      args.push((raw.to_string(), tk.span.clone()));
    } else {
      let span = tk.span.clone();
      let expanded = tk.expand()?;
      for exp in expanded.get_words() {
        args.push((exp, span.clone()));
      }
    }
  }
  Ok(args)
}

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
  fn get_argv_and_opts(&self, argv: Vec<Tk>) -> ShResult<(Vec<(String, Span)>, Vec<Opt>)> {
    let mut argv = prepare_assignment_argv(argv)?;
    if !argv.is_empty() { argv.remove(0); }
    Ok((argv, vec![]))
  }
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
  fn get_argv_and_opts(&self, argv: Vec<Tk>) -> ShResult<(Vec<(String, Span)>, Vec<Opt>)> {
    let mut argv = prepare_assignment_argv(argv)?;
    if !argv.is_empty() { argv.remove(0); }
    Ok((argv, vec![]))
  }
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
  fn get_argv_and_opts(&self, argv: Vec<Tk>) -> ShResult<(Vec<(String, Span)>, Vec<Opt>)> {
    let mut argv = prepare_assignment_argv(argv)?;
    if !argv.is_empty() { argv.remove(0); }
    Ok((argv, vec![]))
  }
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

  // ===================== array literal assignments =====================

  #[test]
  fn local_array_inline() {
    let _g = TestGuard::new();
    test_input("foo() { local arr=(a b c); echo \"${arr[0]} ${arr[1]} ${arr[2]}\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("a b c"), "got {out:?}");
  }

  #[test]
  fn local_array_with_inner_whitespace() {
    let _g = TestGuard::new();
    test_input("foo() { local arr=( a b c ); echo \"${arr[0]}\"; echo \"${arr[2]}\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("a"));
    assert!(out.contains("c"));
  }

  #[test]
  fn local_array_multiline() {
    let _g = TestGuard::new();
    let func = "foo() { local arr=(\n  one\n  two\n  three\n); echo \"${arr[0]}\"; echo \"${arr[1]}\"; echo \"${arr[2]}\"; }";
    test_input(func).unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("one"));
    assert!(out.contains("two"));
    assert!(out.contains("three"));
  }

  #[test]
  fn local_array_iterable() {
    // for-loop over the array elements should iterate each element.
    let _g = TestGuard::new();
    test_input("foo() { local arr=(x y z); for e in \"${arr[@]}\"; do echo $e; done; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines, vec!["x", "y", "z"]);
  }

  #[test]
  fn readonly_array_inline() {
    let _g = TestGuard::new();
    test_input("readonly arr=(1 2 3)").unwrap();
    test_input("echo \"${arr[1]}\"").unwrap();
    let flags = read_vars(|v| v.get_var_flags("arr"));
    assert!(
      flags.unwrap().contains(VarFlags::READONLY),
      "readonly arr=(...) should set READONLY"
    );
  }

  #[test]
  fn local_non_array_still_expands_dollar() {
    // Array detection should not interfere with normal $var expansion in
    // non-array assignments to declaration builtins.
    let _g = TestGuard::new();
    test_input("foo() { local x=$HOME; echo \"x=$x\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    let home = std::env::var("HOME").unwrap_or_default();
    assert!(out.contains(&format!("x={home}")), "got {out:?}");
  }

  #[test]
  fn local_mixed_array_and_scalar() {
    // Multiple declarations in one call, mixed array and scalar.
    let _g = TestGuard::new();
    test_input("foo() { local x=foo arr=(a b c) y=bar; echo \"$x ${arr[1]} $y\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("foo b bar"), "got {out:?}");
  }

  #[test]
  fn local_array_iteration_unquoted() {
    // for opt in $arr unquoted relies on string-join + word-split. Should
    // still iterate over each element for plain alphanumeric content.
    let _g = TestGuard::new();
    test_input("foo() { local arr=(red green blue); for c in $arr; do echo $c; done; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines, vec!["red", "green", "blue"]);
  }

  // ===================== block-scoped local =====================

  #[test]
  fn local_brace_group_scoped() {
    // local declared in a brace group dies when the brace closes.
    let guard = TestGuard::new();
    test_input("{ local x=inside; echo \"in=$x\"; }; echo \"out=$x\"").unwrap();
    let out = guard.read_output();
    assert!(out.contains("in=inside"), "got {out:?}");
    assert!(out.contains("out="), "x should be unset outside block; got {out:?}");
    // 'out=inside' would mean the local leaked
    assert!(!out.contains("out=inside"), "local leaked out of block: {out:?}");
  }

  #[test]
  fn local_nested_brace_groups() {
    // Inner brace group's local doesn't leak to outer; outer's doesn't leak past outer.
    let guard = TestGuard::new();
    test_input("{ { local x=inner; }; echo \"middle=$x\"; }; echo \"outer=$x\"").unwrap();
    let out = guard.read_output();
    assert!(out.contains("middle="), "got {out:?}");
    assert!(!out.contains("middle=inner"), "inner local leaked to middle: {out:?}");
    assert!(out.contains("outer="), "got {out:?}");
    assert!(!out.contains("outer=inner"), "inner local leaked to outer: {out:?}");
  }

  #[test]
  fn local_shadows_outer_within_block() {
    // local x in inner block shadows outer; outer value restored after block.
    let guard = TestGuard::new();
    test_input("foo() { local x=outer; { local x=inner; echo \"in=$x\"; }; echo \"after=$x\"; }").unwrap();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("in=inner"), "got {out:?}");
    assert!(out.contains("after=outer"), "shadow not restored: {out:?}");
  }

  #[test]
  fn local_in_function_still_scoped() {
    // Function-level local still works (existing behavior preserved).
    let guard = TestGuard::new();
    test_input("foo() { local fnvar=inside; echo \"fn=$fnvar\"; }").unwrap();
    test_input("foo").unwrap();
    test_input("echo \"after=$fnvar\"").unwrap();
    let out = guard.read_output();
    assert!(out.contains("fn=inside"), "got {out:?}");
    assert!(out.contains("after="), "got {out:?}");
    assert!(!out.contains("after=inside"), "function local leaked: {out:?}");
  }

  #[test]
  fn local_brace_group_inside_function() {
    // Brace group inside a function: local scoped to brace, not whole function.
    let guard = TestGuard::new();
    test_input("foo() { { local x=brace; }; echo \"after_brace=$x\"; }").unwrap();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("after_brace="), "got {out:?}");
    assert!(!out.contains("after_brace=brace"), "brace-local leaked into function scope: {out:?}");
  }
}
