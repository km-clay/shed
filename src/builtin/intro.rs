use ariadne::Span;

use super::{
  eval::lex::KEYWORDS,
  opt::OptSpec,
  outln, sherr,
  state::{
    self, Shed,
    logic::{AutoloadSrc, ShFunc},
    meta::UtilKind,
    vars::VarKind,
  },
  util::{ShResult, with_status},
};

pub(super) struct Type;
impl super::Builtin for Type {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("short", b's'),
      OptSpec::new_short("terse", b't'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut status = 0;
    let short = args.options().any(|o| o.key() == "short");
    let terse = args.options().any(|o| o.key() == "terse");

    for (arg, span) in args.arguments() {
      if terse {
        let word = if let Some(util) = state::util::which_util(&arg.to_str_lossy()) {
          match util.kind() {
            UtilKind::Alias => Some("alias"),
            UtilKind::Function => Some("function"),
            UtilKind::Builtin => Some("builtin"),
            UtilKind::Command(_) | UtilKind::File(_) => Some("file"),
          }
        } else if KEYWORDS.contains(&arg.as_bytes()) {
          Some("keyword")
        } else {
          None
        };
        match word {
          Some(w) => outln!("{w}"),
          None => status = 1,
        }
        continue;
      }

      if let Some(util) = state::util::which_util(&arg.to_str_lossy()) {
        match util.kind() {
          UtilKind::Alias => {
            let alias = Shed::logic(|v| v.get_alias(&arg.to_str_lossy())).unwrap();
            let (line, col) = alias.source().line_and_col();
            let name = alias.source().source().name();
            if short {
              outln!("alias");
            } else {
              outln!(
                "{arg} is an alias for '{alias_body}' defined at {name}:{ln}:{co}",
                ln = line + 1,
                co = col + 1,
                alias_body = alias.body(),
              );
            }
          }
          UtilKind::Function => {
            let func = Shed::logic(|v| v.get_func(&arg.to_str_lossy())).unwrap();
            match func {
              ShFunc::Autoload(src) => {
                let (origin, location) = match &src {
                  AutoloadSrc::Path(p) => ("external", p.display().to_string()),
                  AutoloadSrc::Embedded(s) => ("internal", s.clone()),
                };
                if short {
                  outln!("{arg} ({origin}) -> {location}");
                } else {
                  outln!(
                    "{arg} is an {origin} autoloading shell function, pointing at '{location}'"
                  );
                }
              }
              ShFunc::Defined { source, .. } => {
                let (line, col) = source.line_and_col();
                let name = source.source().name();
                if short {
                  outln!("function");
                } else {
                  outln!(
                    "{arg} is a function defined at {name}:{ln}:{co}",
                    ln = line + 1,
                    co = col + 1,
                    name = name,
                  );
                }
              }
            }
          }
          UtilKind::Builtin => {
            if short {
              outln!("builtin");
            } else {
              outln!("{arg} is a shell builtin");
            }
          }
          UtilKind::Command(path_buf) | UtilKind::File(path_buf) => {
            if short {
              outln!("external");
            } else {
              outln!("{arg} is {}", path_buf.display());
            }
          }
        }
      } else if KEYWORDS.contains(&arg.as_bytes()) {
        if short {
          outln!("keyword");
        } else {
          outln!("{arg} is a shell keyword");
        }
      } else if let Some(var) = Shed::vars(|v| v.try_get_var_meta(&arg.to_str_lossy())) {
        if short {
          match var.kind() {
            VarKind::Str(_) => outln!("string"),
            VarKind::Int(_) => outln!("integer"),
            VarKind::Arr(_) => outln!("array"),
            VarKind::AssocArr(_) => outln!("assoc_array"),
            VarKind::Magic(_) => outln!("magic"),
            VarKind::Unset => outln!("unset"),
          }
        } else {
          match var.kind() {
            VarKind::Str(_) => outln!("{arg} is a string variable"),
            VarKind::Int(_) => outln!("{arg} is an integer variable"),
            VarKind::Arr(_) => outln!("{arg} is an array variable"),
            VarKind::AssocArr(_) => outln!("{arg} is an associative array"),
            VarKind::Magic(_) => outln!("{arg} is a magic variable"),
            VarKind::Unset => outln!("{arg} is a declared, unset variable"),
          }
        }
      } else {
        sherr!(
          NotFound @ span.clone(),
          "'{arg}' is not a command, function, or alias",
        )
        .print_error();

        status = 1;
      }
    }

    with_status(status)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{self};
  use crate::tests::testutil::{TestGuard, test_input};

  // ===================== Builtins =====================

  #[test]
  fn type_t_classifies() {
    let g = TestGuard::new();
    test_input("type -t echo").unwrap();
    assert_eq!(g.read_output().trim(), "builtin");

    test_input("tt_fn() { :; }").unwrap();
    test_input("type -t tt_fn").unwrap();
    assert_eq!(g.read_output().trim(), "function");

    test_input("type -t if").unwrap();
    assert_eq!(g.read_output().trim(), "keyword");
  }

  #[test]
  fn type_t_variable_and_unknown_are_unknown() {
    // `type -t` does not classify variables and errors on unknown names.
    let _g = TestGuard::new();
    test_input("tt_var=5; type -t tt_var").ok();
    assert_ne!(state::Shed::get_status(), 0);
    test_input("type -t no_such_name_zzz").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn type_builtin_echo() {
    let guard = TestGuard::new();
    test_input("type echo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("echo"));
    assert!(
      out.contains("shell builtin"),
      "Expected 'shell builtin' in output, got: {out}"
    );
  }

  #[test]
  fn type_builtin_cd() {
    let guard = TestGuard::new();
    test_input("type cd").unwrap();
    let out = guard.read_output();
    assert!(out.contains("cd"));
    assert!(out.contains("shell builtin"));
  }

  // ===================== Keywords =====================

  #[test]
  fn type_keyword_if() {
    let guard = TestGuard::new();
    test_input("type if").unwrap();
    let out = guard.read_output();
    assert!(out.contains("if"));
    assert!(out.contains("shell keyword"));
  }

  #[test]
  fn type_keyword_for() {
    let guard = TestGuard::new();
    test_input("type for").unwrap();
    let out = guard.read_output();
    assert!(out.contains("for"));
    assert!(out.contains("shell keyword"));
  }

  // ===================== Functions =====================

  #[test]
  fn type_function() {
    let guard = TestGuard::new();
    test_input("myfn() { echo hi; }").unwrap();
    guard.read_output();

    test_input("type myfn").unwrap();
    let out = guard.read_output();
    assert!(out.contains("myfn"));
    assert!(out.contains("function"));
  }

  // ===================== Aliases =====================

  #[test]
  fn type_alias() {
    let guard = TestGuard::new();
    test_input("alias ll='ls -la'").unwrap();
    guard.read_output();

    test_input("type ll").unwrap();
    let out = guard.read_output();
    assert!(out.contains("ll"));
    assert!(out.contains("alias"));
    assert!(out.contains("ls -la"));
  }

  // ===================== External commands =====================

  #[test]
  fn type_external_command() {
    let guard = TestGuard::new();
    // /bin/cat or /usr/bin/cat should exist on any Unix system
    test_input("type cat").unwrap();
    let out = guard.read_output();
    assert!(out.contains("cat"));
    assert!(out.contains("is"));
    assert!(out.contains('/')); // Should show a path
  }

  // ===================== Not found =====================

  #[test]
  fn type_not_found() {
    let _g = TestGuard::new();
    let result = test_input("type __hopefully____not_______a____command__");
    assert!(result.is_ok());
    assert_eq!(state::Shed::get_status(), 1);
  }

  // ===================== Priority order =====================

  #[test]
  fn type_function_shadows_builtin() {
    let guard = TestGuard::new();
    // Define a function named 'echo' - should shadow the builtin
    test_input("echo() { true; }").unwrap();
    guard.read_output();

    test_input("type echo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("function"));
  }

  #[test]
  fn type_alias_shadows_external() {
    let guard = TestGuard::new();
    test_input("alias cat='echo meow'").unwrap();
    guard.read_output();

    test_input("type cat").unwrap();
    let out = guard.read_output();
    // alias check comes before external PATH scan
    assert!(out.contains("alias"));
  }

  // ===================== Status =====================

  #[test]
  fn type_status_zero_on_found() {
    let _g = TestGuard::new();
    test_input("type echo").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }
}
