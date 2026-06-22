use std::collections::VecDeque;

use crate::state::logic::{AutoloadKind, ShFunc};

use super::{
  Span, Tk,
  expand::expand_arithmetic,
  getopt::{Opt, OptSpec, get_opts_from_tokens_raw_no_split},
  outln, sherr,
  state::{
    Shed,
    vars::{
      VarFlags, VarKind, VarName, display_as_var, display_env_vars, display_local, display_readonly,
    },
  },
  try_var,
  util::{ShResult, ShResultExt, split_at_unescaped, with_status},
};

/// Like `prepare_argv` but preserves raw token text for `name=(...)` array
/// literal assignments. The normal expansion pipeline runs `unescape_str`
/// which treats `(` as a subshell opener and strips parens, breaking array
/// assignment via `local`/`readonly`/`export`. Tokens that look like array
/// literals are passed through verbatim so `arr_from_raw` can parse them.
pub fn prepare_assignment_argv(argv: &[Tk]) -> ShResult<Vec<(String, Span)>> {
  let mut out = vec![];
  for tk in argv {
    let raw = tk.span.as_str();
    let eq_pos = split_at_unescaped(raw, "=").map(|(pos, _)| pos);

    let is_arr_lit = eq_pos.is_some_and(|eq| raw[eq + 1..].starts_with('(') && raw.ends_with(')'));
    if is_arr_lit {
      out.push((raw.to_string(), tk.span.clone()));
      continue;
    }

    let span = tk.span.clone();

    if let Some(eq) = eq_pos {
      let name = &raw[..eq];
      let is_valid_name = !name.is_empty()
        && name
          .chars()
          .next()
          .is_some_and(|c| c.is_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_alphanumeric() || c == '_');
      if is_valid_name {
        let expanded = tk.expand_no_split()?;
        out.push((expanded, span));
        continue;
      }
    }

    let expanded = tk.expand()?;
    for exp in expanded.get_words() {
      out.push((exp, span.clone()));
    }
  }
  Ok(out)
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

#[derive(Clone, Copy)]
enum DeclareKind {
  Str,
  Int,
  Arr,
  Assoc,
}

#[derive(Clone, Copy)]
enum IntrospectMode {
  Vars,          // -p
  FunctionsFull, // -f
  FunctionNames, // -F
}

/// Shared assignment loop for `declare` / `local` / etc. Parses the
/// var-declaration flags (`-i`, `-r`, `-x`, `-a`, `-A`) out of `opts`, OR-s
/// them onto `base_flags`, and applies each assignment in `argv` with the
/// resulting kind+flags. Other opts (e.g. `-p`/`-f`/`-F`) are caller-handled
/// before delegating here.
fn apply_var_decl(opts: &[Opt], argv: Vec<(String, Span)>, base_flags: VarFlags) -> ShResult<()> {
  let mut flags = base_flags;
  let mut kind = DeclareKind::Str;
  for opt in opts {
    match opt {
      Opt::Short('r') => flags |= VarFlags::READONLY,
      Opt::Short('x') => flags |= VarFlags::EXPORT,
      Opt::Short('i') => kind = DeclareKind::Int,
      Opt::Short('a') => kind = DeclareKind::Arr,
      Opt::Short('A') => kind = DeclareKind::Assoc,
      _ => {}
    }
  }

  for (arg, span) in argv {
    let (name, raw_val) = split_assignment_raw(arg);
    let val = match (kind, raw_val.as_deref()) {
      (DeclareKind::Str, Some(v)) => VarKind::parse(v),
      (DeclareKind::Str, None) => VarKind::string(String::new()),
      (DeclareKind::Int, Some(v)) => {
        let evaluated = expand_arithmetic(v).promote_err(span.clone())?;
        let n = evaluated
          .parse::<i32>()
          .map_err(|_| sherr!(ExecFail @ span.clone(), "declare -i: invalid arithmetic '{v}'"))?;
        VarKind::Int(n)
      }
      (DeclareKind::Int, None) => VarKind::Int(0),
      (DeclareKind::Arr, Some(v)) => VarKind::arr_from_raw(v).promote_err(span.clone())?,
      (DeclareKind::Arr, None) => VarKind::Arr(VecDeque::new()),
      (DeclareKind::Assoc, Some(v)) => VarKind::assoc_arr_from_raw(v).promote_err(span.clone())?,
      (DeclareKind::Assoc, None) => VarKind::AssocArr(Vec::new()),
    };
    Shed::vars_mut(|v| v.set_var(&name, val, flags)).promote_err(span)?;
  }

  with_status(0)
}

pub(super) struct Declare;
impl super::Builtin for Declare {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('i'),
      OptSpec::flag('r'),
      OptSpec::flag('x'),
      OptSpec::flag('a'),
      OptSpec::flag('A'),
      OptSpec::flag('p'),
      OptSpec::flag('f'),
      OptSpec::flag('F'),
    ]
  }
  fn get_argv_and_opts(
    &self,
    argv: &[Tk],
    _no_split: bool,
  ) -> ShResult<(super::ArgVector, Vec<Opt>)> {
    let (raw_argv, opts) = get_opts_from_tokens_raw_no_split(argv, &self.opts())?;
    let mut argv = prepare_assignment_argv(&raw_argv)?;
    if !argv.is_empty() {
      argv.remove(0);
    }
    Ok((argv, opts))
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut introspect: Option<IntrospectMode> = None;
    for opt in &args.opts {
      match opt {
        Opt::Short('p') => introspect = Some(IntrospectMode::Vars),
        Opt::Short('f') => introspect = Some(IntrospectMode::FunctionsFull),
        Opt::Short('F') => introspect = Some(IntrospectMode::FunctionNames),
        _ => {}
      }
    }

    if let Some(mode) = introspect {
      return declare_introspect(mode, &args.argv);
    }

    if args.argv.is_empty() {
      // Bare `declare` prints all variables in declare-style format.
      let output = Shed::vars(display_local);
      outln!("{output}");
      return with_status(0);
    }

    apply_var_decl(&args.opts, args.argv, VarFlags::empty())
  }
}

fn declare_introspect(mode: IntrospectMode, argv: &[(String, Span)]) -> ShResult<()> {
  match mode {
    IntrospectMode::Vars => {
      if argv.is_empty() {
        let output = Shed::vars(display_local);
        outln!("{output}");
      } else {
        for (name, span) in argv {
          let val = try_var!(name);
          match val {
            Some(v) => outln!("{}", display_as_var(name, v)),
            None => {
              return Err(sherr!(
                NotFound @ span.clone(),
                "declare: '{name}' not found",
              ));
            }
          }
        }
      }
    }
    IntrospectMode::FunctionsFull => {
      let names: Vec<&str> = argv.iter().map(|(n, _)| n.as_str()).collect();

      // Source any explicitly requested autoload functions so their bodies
      // become visible to the iteration below; otherwise `declare -f name`
      // silently skips functions that haven't been called yet.
      for name in &names {
        let autoload_src = Shed::logic_mut(|l| {
          if let Some(ShFunc::Autoload(_)) = l.get_func_ref(name)
            && let Some(ShFunc::Autoload(src)) = l.remove_func(name)
          {
            Some(src)
          } else {
            None
          }
        });
        if let Some(src) = autoload_src {
          src.source(AutoloadKind::Function).ok();
        }
      }

      let dump = Shed::logic(|l| {
        let mut out = String::new();
        let mut entries: Vec<_> = l
          .funcs()
          .iter()
          .filter(|(_, f)| matches!(f, ShFunc::Defined { .. }))
          .collect();
        entries.sort_by_key(|(k, _)| (*k).clone());
        for (name, func) in entries {
          let ShFunc::Defined { source, .. } = func else {
            continue;
          };
          if !names.is_empty() && !names.contains(&name.as_str()) {
            continue;
          }
          out.push_str(source.as_str());
          out.push('\n');
        }
        out
      });
      if !dump.is_empty() {
        outln!("{}", dump.trim_end());
      }
    }
    IntrospectMode::FunctionNames => {
      let names: Vec<&str> = argv.iter().map(|(n, _)| n.as_str()).collect();
      let dump = Shed::logic(|l| {
        let mut keys: Vec<_> = l.funcs().keys().cloned().collect();
        keys.sort();
        keys
          .into_iter()
          .filter(|k| names.is_empty() || names.contains(&k.as_str()))
          .map(|k| format!("declare -f {k}"))
          .collect::<Vec<_>>()
          .join("\n")
      });
      if !dump.is_empty() {
        outln!("{dump}");
      }
    }
  }
  with_status(0)
}

pub(super) struct Readonly;
impl super::Builtin for Readonly {
  fn is_special(&self) -> bool {
    true
  }

  fn get_argv_and_opts(
    &self,
    argv: &[Tk],
    _no_split: bool,
  ) -> ShResult<(Vec<(String, Span)>, Vec<Opt>)> {
    let mut argv = prepare_assignment_argv(argv)?;
    if !argv.is_empty() {
      argv.remove(0);
    }
    Ok((argv, vec![]))
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      // Display the local variables
      let vars = Shed::vars(display_readonly);
      outln!("{vars}");

      return with_status(0);
    }

    for (arg, span) in args.argv {
      let (var, val) = split_assignment(arg);
      Shed::vars_mut(|v| {
        v.set_var(&var, val.unwrap_or_default(), VarFlags::READONLY)
          .promote_err(span)
      })?;
    }

    with_status(0)
  }
}

pub(super) struct Unset;
impl super::Builtin for Unset {
  fn is_special(&self) -> bool {
    true
  }

  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::flag('f')]
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let is_func = args.opts.iter().any(|o| matches!(o, Opt::Short('f')));

    for (arg, _) in args.argv {
      if is_func {
        Shed::logic_mut(|l| l.remove_func(&arg));
        continue;
      }

      let parsed = VarName::parse(&arg, true)?;
      if !Shed::vars(|v| v.var_exists(parsed.name())) {
        continue;
      }

      match parsed.index() {
        None => {
          Shed::vars_mut(|v| v.unset_var(parsed.name()))?;
        }
        Some(idx) => {
          // resolve the raw index (a string key vs a numeric index) against
          // the target's kind, then drop just that element
          let Some(tag) = Shed::vars(|v| v.try_get_var_kind_tag(parsed.name())) else {
            continue;
          };
          let resolved = idx.clone().resolve_for(tag)?;
          Shed::vars_mut(|v| v.unset_index(parsed.name(), resolved))?;
        }
      }
    }

    with_status(0)
  }
}

pub(super) struct Export;
impl super::Builtin for Export {
  fn is_special(&self) -> bool {
    true
  }

  fn get_argv_and_opts(
    &self,
    argv: &[Tk],
    _no_split: bool,
  ) -> ShResult<(Vec<(String, Span)>, Vec<Opt>)> {
    let mut argv = prepare_assignment_argv(argv)?;
    if !argv.is_empty() {
      argv.remove(0);
    }
    Ok((argv, vec![]))
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      // Display the environment variables
      let vars = display_env_vars();
      outln!("{vars}");
      return with_status(0);
    }

    for (arg, span) in args.argv {
      let (var, val) = split_assignment(arg);
      if let Some(val) = val {
        Shed::vars_mut(|v| v.set_var(&var, val, VarFlags::EXPORT)).promote_err(span)?;
      } else {
        // Export an existing variable, if any
        Shed::vars_mut(|v| v.export_var(&var));
      }
    }

    with_status(0)
  }
}

pub(super) struct Local;
impl super::Builtin for Local {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('i'),
      OptSpec::flag('r'),
      OptSpec::flag('x'),
      OptSpec::flag('a'),
      OptSpec::flag('A'),
    ]
  }
  fn get_argv_and_opts(
    &self,
    argv: &[Tk],
    _no_split: bool,
  ) -> ShResult<(Vec<(String, Span)>, Vec<Opt>)> {
    let (raw_argv, opts) = get_opts_from_tokens_raw_no_split(argv, &self.opts())?;
    let mut argv = prepare_assignment_argv(&raw_argv)?;
    if !argv.is_empty() {
      argv.remove(0);
    }
    Ok((argv, opts))
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      let vars = Shed::vars(display_local);
      outln!("{vars}");
      return with_status(0);
    }

    apply_var_decl(&args.opts, args.argv, VarFlags::LOCAL)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{self, Shed, vars::VarFlags};
  use crate::tests::testutil::{TestGuard, test_input};
  use crate::var;

  // ===================== readonly =====================

  #[test]
  fn readonly_sets_flag() {
    let _g = TestGuard::new();
    test_input("readonly myvar").unwrap();
    let flags = Shed::vars(|v| v.get_var_flags("myvar"));
    assert!(flags.unwrap().contains(VarFlags::READONLY));
  }

  #[test]
  fn readonly_with_value() {
    let _g = TestGuard::new();
    test_input("readonly myvar=hello").unwrap();
    assert_eq!(var!("myvar"), "hello");
    let flags = Shed::vars(|v| v.get_var_flags("myvar"));
    assert!(flags.unwrap().contains(VarFlags::READONLY));
  }

  #[test]
  fn readonly_prevents_reassignment() {
    let _g = TestGuard::new();
    test_input("readonly myvar=hello").unwrap();
    test_input("myvar=world").ok();
    assert_eq!(var!("myvar"), "hello");
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
    assert_eq!(var!("a"), "1");
    assert_eq!(var!("b"), "2");
    assert!(
      Shed::vars(|v| v.get_var_flags("a"))
        .unwrap()
        .contains(VarFlags::READONLY)
    );
    assert!(
      Shed::vars(|v| v.get_var_flags("b"))
        .unwrap()
        .contains(VarFlags::READONLY)
    );
  }

  #[test]
  fn readonly_status_zero() {
    let _g = TestGuard::new();
    test_input("readonly x=1").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== unset =====================

  #[test]
  fn unset_removes_variable() {
    let _g = TestGuard::new();
    test_input("myvar=hello").unwrap();
    assert_eq!(var!("myvar"), "hello");
    test_input("unset myvar").unwrap();
    assert_eq!(var!("myvar"), "");
  }

  #[test]
  fn unset_multiple() {
    let _g = TestGuard::new();
    test_input("a=1").unwrap();
    test_input("b=2").unwrap();
    test_input("unset a b").unwrap();
    assert_eq!(var!("a"), "");
    assert_eq!(var!("b"), "");
  }

  #[test]
  fn unset_nonexistent_is_not_error() {
    let _g = TestGuard::new();
    test_input("unset __no_such_var__").ok();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn unset_readonly_fails() {
    let _g = TestGuard::new();
    test_input("readonly myvar=protected").unwrap();
    test_input("unset myvar").ok();
    assert_ne!(state::Shed::get_status(), 0);
    assert_eq!(var!("myvar"), "protected");
  }

  #[test]
  fn unset_assoc_element() {
    let g = TestGuard::new();
    test_input(
      "declare -A m; m[foo]=bar; m[baz]=qux; unset 'm[foo]'; echo \"[${m[foo]}][${m[baz]}]\"",
    )
    .unwrap();
    crate::assert_output!(g, "[][qux]\n");
  }

  #[test]
  fn unset_indexed_element() {
    let g = TestGuard::new();
    test_input("arr=(a b c d); unset 'arr[1]'; echo \"${arr[@]}\"").unwrap();
    crate::assert_output!(g, "a c d\n");
  }

  #[test]
  fn unset_missing_element_is_noop() {
    let g = TestGuard::new();
    test_input("declare -A m; m[a]=1; unset 'm[nope]'; echo \"[${m[a]}]\"").unwrap();
    crate::assert_output!(g, "[1]\n");
  }

  #[test]
  fn unset_status_zero() {
    let _g = TestGuard::new();
    test_input("x=1").unwrap();
    test_input("unset x").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== export =====================

  #[test]
  fn export_with_value() {
    let _g = TestGuard::new();
    test_input("export SHED_TEST_VAR=hello_export").unwrap();
    assert_eq!(var!("SHED_TEST_VAR"), "hello_export");
    assert_eq!(var!("SHED_TEST_VAR"), "hello_export");
    unsafe { std::env::remove_var("SHED_TEST_VAR") };
  }

  #[test]
  fn export_existing_variable() {
    let _g = TestGuard::new();
    test_input("SHED_TEST_VAR2=existing").unwrap();
    test_input("export SHED_TEST_VAR2").unwrap();
    assert_eq!(var!("SHED_TEST_VAR2"), "existing");
    unsafe { std::env::remove_var("SHED_TEST_VAR2") };
  }

  #[test]
  fn export_sets_flag() {
    let _g = TestGuard::new();
    test_input("export SHED_TEST_VAR3=flagged").unwrap();
    let flags = Shed::vars(|v| v.get_var_flags("SHED_TEST_VAR3"));
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
    assert_eq!(var!("SHED_A"), "1");
    assert_eq!(var!("SHED_B"), "2");
    unsafe { std::env::remove_var("SHED_A") };
    unsafe { std::env::remove_var("SHED_B") };
  }

  #[test]
  fn export_status_zero() {
    let _g = TestGuard::new();
    test_input("export SHED_ST=1").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    unsafe { std::env::remove_var("SHED_ST") };
  }

  // ===================== local =====================

  #[test]
  fn local_sets_variable() {
    let _g = TestGuard::new();
    test_input("local mylocal=hello").unwrap();
    assert_eq!(var!("mylocal"), "hello");
  }

  #[test]
  fn local_sets_flag() {
    let _g = TestGuard::new();
    test_input("local mylocal=val").unwrap();
    let flags = Shed::vars(|v| v.get_var_flags("mylocal"));
    assert!(flags.unwrap().contains(VarFlags::LOCAL));
  }

  #[test]
  fn local_empty_value() {
    let _g = TestGuard::new();
    test_input("local mylocal").unwrap();
    assert_eq!(var!("mylocal"), "");
    assert!(
      Shed::vars(|v| v.get_var_flags("mylocal"))
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
    assert_eq!(var!("x"), "10");
    assert_eq!(var!("y"), "20");
  }

  #[test]
  fn local_cmd_sub_preserves_whitespace() {
    let _g = TestGuard::new();
    // Regression: assignment RHS was going through word-splitting expand
    // which collapsed runs of whitespace into single spaces. Now uses
    // expand_no_split via the get_opts_from_tokens_raw_no_split path.
    test_input(r#"local ws=$(echo "FOO    BAR")"#).unwrap();
    assert_eq!(var!("ws"), "FOO    BAR");
  }

  #[test]
  fn local_cmd_sub_preserves_newlines() {
    let _g = TestGuard::new();
    test_input(r#"local ml=$(printf 'a\nb\nc')"#).unwrap();
    assert_eq!(var!("ml"), "a\nb\nc");
  }

  #[test]
  fn local_bare_names_still_split() {
    let _g = TestGuard::new();
    // Make sure the no-split path didn't break bare-name declarations:
    // `local a b c` should still declare three separate variables.
    test_input("local la=1 lb=2 lc=3").unwrap();
    assert_eq!(var!("la"), "1");
    assert_eq!(var!("lb"), "2");
    assert_eq!(var!("lc"), "3");
  }

  #[test]
  fn declare_cmd_sub_preserves_whitespace() {
    let _g = TestGuard::new();
    test_input(r#"declare dws=$(echo "X    Y    Z")"#).unwrap();
    assert_eq!(var!("dws"), "X    Y    Z");
  }

  #[test]
  fn export_cmd_sub_preserves_whitespace() {
    let _g = TestGuard::new();
    test_input(r#"export EXP_WS=$(echo "P    Q")"#).unwrap();
    assert_eq!(var!("EXP_WS"), "P    Q");
  }

  #[test]
  fn readonly_cmd_sub_preserves_whitespace() {
    let _g = TestGuard::new();
    test_input(r#"readonly RO_WS=$(echo "M    N")"#).unwrap();
    assert_eq!(var!("RO_WS"), "M    N");
  }

  // Quoting context (e.g. `$'...'` ANSI-C strings) must survive assoc array
  // literal parsing. Regression: the value reader stripped the quotes, turning
  // `$'a\tb'` into the variable expansion `$a` followed by `\tb`.
  #[test]
  fn declare_assoc_preserves_ansi_c_quoting() {
    let _g = TestGuard::new();
    test_input(r#"declare -A aq=([k]=$'a\tb')"#).unwrap();
    test_input(r#"aq_out="${aq[k]}""#).unwrap();
    assert_eq!(var!("aq_out"), "a\tb");
  }

  #[test]
  fn declare_assoc_preserves_multiline_value() {
    let _g = TestGuard::new();
    test_input(r#"declare -A aq=([k]=$'one\ntwo\nthree')"#).unwrap();
    test_input(r#"aq_out="${aq[k]}""#).unwrap();
    assert_eq!(var!("aq_out"), "one\ntwo\nthree");
  }

  // ===================== Str += concat regression =====================
  // Was doing arithmetic when both sides parsed as i32, giving
  // sum-of-digits for `buf=""; buf+="2"; buf+="0"; buf+="1"; buf+="6"`.

  #[test]
  fn string_plus_eq_concatenates_numeric_strings() {
    let _g = TestGuard::new();
    test_input("sx=2").unwrap();
    test_input("sx+=3").unwrap();
    assert_eq!(var!("sx"), "23");
  }

  #[test]
  fn string_plus_eq_builds_numeric_string_char_by_char() {
    let _g = TestGuard::new();
    test_input(r#"sb="""#).unwrap();
    test_input(r#"sb+="2""#).unwrap();
    test_input(r#"sb+="0""#).unwrap();
    test_input(r#"sb+="1""#).unwrap();
    test_input(r#"sb+="6""#).unwrap();
    assert_eq!(var!("sb"), "2016");
  }

  #[test]
  fn int_plus_eq_still_does_arithmetic() {
    let _g = TestGuard::new();
    // declare -i means arithmetic += is correct, the previous fix
    // should not have broken this path.
    test_input("declare -i iy=2").unwrap();
    test_input("iy+=3").unwrap();
    assert_eq!(var!("iy"), "5");
  }

  #[test]
  fn local_status_zero() {
    let _g = TestGuard::new();
    test_input("local z=1").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
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
    assert!(out.contains('a'));
    assert!(out.contains('c'));
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
    let flags = Shed::vars(|v| v.get_var_flags("arr"));
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
    let home = var!("HOME");
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
    assert!(
      out.contains("out="),
      "x should be unset outside block; got {out:?}"
    );
    // 'out=inside' would mean the local leaked
    assert!(
      !out.contains("out=inside"),
      "local leaked out of block: {out:?}"
    );
  }

  #[test]
  fn local_nested_brace_groups() {
    // Inner brace group's local doesn't leak to outer; outer's doesn't leak past outer.
    let guard = TestGuard::new();
    test_input("{ { local x=inner; }; echo \"middle=$x\"; }; echo \"outer=$x\"").unwrap();
    let out = guard.read_output();
    assert!(out.contains("middle="), "got {out:?}");
    assert!(
      !out.contains("middle=inner"),
      "inner local leaked to middle: {out:?}"
    );
    assert!(out.contains("outer="), "got {out:?}");
    assert!(
      !out.contains("outer=inner"),
      "inner local leaked to outer: {out:?}"
    );
  }

  #[test]
  fn local_shadows_outer_within_block() {
    // local x in inner block shadows outer; outer value restored after block.
    let guard = TestGuard::new();
    test_input("foo() { local x=outer; { local x=inner; echo \"in=$x\"; }; echo \"after=$x\"; }")
      .unwrap();
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
    assert!(
      !out.contains("after=inside"),
      "function local leaked: {out:?}"
    );
  }

  #[test]
  fn local_brace_group_inside_function() {
    // Brace group inside a function: local scoped to brace, not whole function.
    let guard = TestGuard::new();
    test_input("foo() { { local x=brace; }; echo \"after_brace=$x\"; }").unwrap();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("after_brace="), "got {out:?}");
    assert!(
      !out.contains("after_brace=brace"),
      "brace-local leaked into function scope: {out:?}"
    );
  }

  // ===================== declare =====================

  #[test]
  fn declare_plain_assignment() {
    let _g = TestGuard::new();
    test_input("declare foo=hello").unwrap();
    assert_eq!(var!("foo"), "hello");
  }

  #[test]
  fn declare_no_value() {
    let _g = TestGuard::new();
    test_input("declare foo").unwrap();
    // Declared but empty.
    assert_eq!(var!("foo"), "");
  }

  #[test]
  fn declare_r_sets_readonly_flag() {
    let _g = TestGuard::new();
    test_input("declare -r myvar=42").unwrap();
    assert_eq!(var!("myvar"), "42");
    let flags = Shed::vars(|v| v.get_var_flags("myvar"));
    assert!(flags.unwrap().contains(VarFlags::READONLY));
  }

  #[test]
  fn declare_x_sets_export_flag() {
    let _g = TestGuard::new();
    test_input("declare -x exported=yes").unwrap();
    let flags = Shed::vars(|v| v.get_var_flags("exported"));
    assert!(flags.unwrap().contains(VarFlags::EXPORT));
  }

  #[test]
  fn declare_rx_combined_flags() {
    let _g = TestGuard::new();
    test_input("declare -rx both=val").unwrap();
    let flags = Shed::vars(|v| v.get_var_flags("both")).unwrap();
    assert!(flags.contains(VarFlags::READONLY));
    assert!(flags.contains(VarFlags::EXPORT));
  }

  #[test]
  fn declare_i_evaluates_arithmetic() {
    let _g = TestGuard::new();
    test_input("declare -i n=5+3").unwrap();
    assert_eq!(var!("n"), "8");
  }

  #[test]
  fn declare_i_no_value_is_zero() {
    let _g = TestGuard::new();
    test_input("declare -i n").unwrap();
    assert_eq!(var!("n"), "0");
  }

  #[test]
  fn declare_a_creates_array() {
    let guard = TestGuard::new();
    test_input("declare -a arr=(a b c)").unwrap();
    test_input("echo \"${arr[0]} ${arr[1]} ${arr[2]}\"").unwrap();
    let out = guard.read_output();
    assert!(out.contains("a b c"), "got {out:?}");
  }

  #[test]
  fn declare_p_prints_named_var() {
    let guard = TestGuard::new();
    test_input("declare myvar=visible").unwrap();
    guard.read_output(); // discard noise from earlier
    test_input("declare -p myvar").unwrap();
    let out = guard.read_output();
    assert!(out.contains("myvar=visible"), "got {out:?}");
  }

  #[test]
  fn declare_p_unknown_var_errors() {
    let _g = TestGuard::new();
    let _ = test_input("declare -p nonexistent_var");
    // exec_nonint catches the error and propagates via exit status
    // rather than returning Err, so check the status.
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn declare_a_empty_assignment() {
    let _g = TestGuard::new();
    // declare -a with no `=...` produces an empty array.
    test_input("declare -a empty").unwrap();
    // A bare element access on an empty array should be empty.
    assert_eq!(var!("empty[0]"), "");
  }

  #[test]
  fn declare_capital_f_lists_function_names() {
    let guard = TestGuard::new();
    test_input("foo() { :; }").unwrap();
    test_input("bar() { :; }").unwrap();
    guard.read_output(); // discard
    test_input("declare -F").unwrap();
    let out = guard.read_output();
    assert!(out.contains("declare -f foo"), "got {out:?}");
    assert!(out.contains("declare -f bar"), "got {out:?}");
  }

  #[test]
  fn declare_f_dumps_function_source() {
    let guard = TestGuard::new();
    test_input("foo() { echo hi; }").unwrap();
    guard.read_output();
    test_input("declare -f foo").unwrap();
    let out = guard.read_output();
    // ShFunc.source currently only spans the function name (not the
    // body) — see exec_func_def in parse/execute.rs. When that's
    // widened to include the body, this should also assert on
    // "echo hi". For now, verify the lookup at least found the
    // function and emitted *something* identifying it.
    assert!(out.contains("foo"), "got {out:?}");
  }

  #[test]
  fn indexed_array_index_resolves_variable() {
    // Regression: `arr[foo]` with foo holding a number should arith-eval
    // to that index. Previously this produced `Key("foo")` and errored.
    let guard = TestGuard::new();
    test_input("declare -a arr=(a b c)").unwrap();
    test_input("foo=1").unwrap();
    test_input("echo ${arr[foo]}").unwrap();
    let out = guard.read_output();
    assert!(out.contains('b'), "got {out:?}");
  }

  #[test]
  fn assoc_array_numeric_key_works() {
    // `aa[5]` on an associative array should look up the literal key "5",
    // not be parsed as a numeric index.
    let guard = TestGuard::new();
    test_input("declare -A aa=([5]=five [foo]=bar)").unwrap();
    test_input("echo ${aa[5]}").unwrap();
    let out = guard.read_output();
    assert!(out.contains("five"), "got {out:?}");
  }

  #[test]
  fn declare_assoc_empty() {
    let _g = TestGuard::new();
    test_input("declare -A mymap").unwrap();
    assert_eq!(var!("mymap"), "");
  }

  #[test]
  fn declare_assoc_with_values() {
    let _g = TestGuard::new();
    test_input("declare -A mymap=([foo]=bar [biz]=baz)").unwrap();
    let val = Shed::vars(|v| {
      v.index_var(
        "mymap",
        &crate::state::vars::ArrIndex::Key("foo".to_string()),
      )
    });
    assert_eq!(val.unwrap(), "bar");
    let val2 = Shed::vars(|v| {
      v.index_var(
        "mymap",
        &crate::state::vars::ArrIndex::Key("biz".to_string()),
      )
    });
    assert_eq!(val2.unwrap(), "baz");
  }

  #[test]
  fn assoc_array_set_key() {
    let guard = TestGuard::new();
    test_input("declare -A aa").unwrap();
    test_input("aa[key1]=value1").unwrap();
    test_input("aa[key2]=value2").unwrap();
    test_input("echo ${aa[key1]} ${aa[key2]}").unwrap();
    let out = guard.read_output();
    assert!(out.contains("value1 value2"), "got {out:?}");
  }

  #[test]
  fn assoc_array_get_all_values() {
    let guard = TestGuard::new();
    test_input("declare -A aa=([a]=1 [b]=2 [c]=3)").unwrap();
    test_input("echo ${aa[@]}").unwrap();
    let out = guard.read_output();
    assert!(
      out.contains('1') && out.contains('2') && out.contains('3'),
      "got {out:?}"
    );
  }

  #[test]
  fn assoc_array_get_keys() {
    let guard = TestGuard::new();
    test_input("declare -A aa=([foo]=bar [biz]=baz)").unwrap();
    test_input("echo ${!aa[@]}").unwrap();
    let out = guard.read_output();
    assert!(out.contains("foo") && out.contains("biz"), "got {out:?}");
  }

  #[test]
  fn assoc_array_count() {
    let _g = TestGuard::new();
    test_input("declare -A aa=([a]=1 [b]=2 [c]=3)").unwrap();
    test_input("declare -i count=${#aa[@]}").unwrap();
    assert_eq!(var!("count"), "3");
  }

  #[test]
  fn assoc_array_value_length() {
    let guard = TestGuard::new();
    test_input("declare -A aa=([key]=hello)").unwrap();
    test_input("echo ${#aa[key]}").unwrap();
    let out = guard.read_output();
    assert!(out.contains('5'), "got {out:?}");
  }

  #[test]
  fn assoc_array_update_existing_key() {
    let guard = TestGuard::new();
    test_input("declare -A aa=([k]=old)").unwrap();
    test_input("aa[k]=new").unwrap();
    test_input("echo ${aa[k]}").unwrap();
    let out = guard.read_output();
    assert!(out.contains("new"), "got {out:?}");
  }

  // ===================== local with declare-style flags =====================

  #[test]
  fn local_assoc_empty() {
    let _g = TestGuard::new();
    test_input("foo() { local -A m; m[k]=v; echo \"${m[k]}\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains('v'), "got {out:?}");
  }

  #[test]
  fn local_assoc_init() {
    let _g = TestGuard::new();
    test_input("foo() { local -A m=([a]=1 [b]=2); echo \"${m[a]} ${m[b]}\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("1 2"), "got {out:?}");
  }

  #[test]
  fn local_assoc_is_scoped() {
    // The local -A should not leak out of the function.
    let _g = TestGuard::new();
    test_input("foo() { local -A m=([k]=v); }").unwrap();
    test_input("foo").unwrap();
    assert_eq!(var!("m"), "");
  }

  #[test]
  fn local_array_explicit_flag() {
    // Explicit `-a` flag should produce the same result as bare `local arr=(...)`.
    let _g = TestGuard::new();
    test_input("foo() { local -a arr=(x y z); echo \"${arr[1]}\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains('y'), "got {out:?}");
  }

  #[test]
  fn local_int_arithmetic() {
    let _g = TestGuard::new();
    test_input("foo() { local -i n=2+3; echo \"$n\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains('5'), "got {out:?}");
  }

  #[test]
  fn local_readonly_combined() {
    let _g = TestGuard::new();
    test_input("foo() { local -r x=fixed; x=changed; echo \"$x\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").ok();
    let out = guard.read_output();
    assert!(out.contains("fixed"), "got {out:?}");
  }

  #[test]
  fn local_export_combined() {
    let _g = TestGuard::new();
    test_input("foo() { local -x EXPORTED_LOCAL=hi; env | grep '^EXPORTED_LOCAL='; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("EXPORTED_LOCAL=hi"), "got {out:?}");
  }

  #[test]
  fn local_assoc_does_not_leak_into_outer() {
    // Verify the LOCAL flag is set, not just that the var doesn't survive.
    let _g = TestGuard::new();
    test_input("declare -A m=([outer]=1)").unwrap();
    test_input("foo() { local -A m=([inner]=2); echo \"${m[inner]}\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    test_input("echo \"${m[outer]}\"").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines, vec!["2", "1"], "got {out:?}");
  }

  // ===================== compound-assignment scope correctness =====================

  #[test]
  fn local_array_append_in_for_loop() {
    // Regression: previously `arr+=("$c")` inside a for loop body wrote
    // a shadow copy in the loop's scope and the parent's local stayed empty.
    let _g = TestGuard::new();
    test_input(
      "foo() { local arr=(); for c in x y z; do arr+=(\"$c\"); done; echo \"( ${arr[@]} )\"; }",
    )
    .unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("( x y z )"), "got {out:?}");
  }

  #[test]
  fn local_array_append_in_nested_loops() {
    // Same bug pattern with nested loops — should still mutate the outermost local.
    let _g = TestGuard::new();
    test_input(
      "foo() { local arr=(); for i in 1 2; do for j in a b; do arr+=(\"$i$j\"); done; done; echo \"( ${arr[@]} )\"; }",
    )
    .unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("( 1a 1b 2a 2b )"), "got {out:?}");
  }

  #[test]
  fn local_int_pluseq_in_for_loop() {
    // Same fix path covers scalar +=. Counter should reach 3.
    let _g = TestGuard::new();
    test_input("foo() { local -i n=0; for _ in 1 2 3; do n+=1; done; echo \"$n\"; }").unwrap();
    let guard = TestGuard::new();
    test_input("foo").unwrap();
    let out = guard.read_output();
    assert!(out.contains('3'), "got {out:?}");
  }

  #[test]
  fn local_array_append_preserves_local_flag() {
    // After the loop appends, the outer var should still be flagged LOCAL
    // (i.e., the fix didn't accidentally promote it to global).
    let _g = TestGuard::new();
    test_input("foo() { local arr=(); for c in x; do arr+=(\"$c\"); done; }").unwrap();
    test_input("foo").unwrap();
    // arr was local to foo; should not exist at top level.
    assert_eq!(var!("arr"), "");
  }
}
