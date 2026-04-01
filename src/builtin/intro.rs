use std::os::unix::fs::PermissionsExt;

use ariadne::{Fmt, Span};

use crate::{
  builtin::BUILTINS,
  libsh::error::{ShResult, next_color},
  parse::{NdRule, Node, execute::prepare_argv, lex::KEYWORDS},
  prelude::*,
  procio::borrow_fd,
  sherr,
  state::{self, ShAlias, ShFunc, read_logic},
};

pub fn type_builtin(node: Node) -> ShResult<()> {
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

  /*
   * we have to check in the same order that the dispatcher checks this
   * 1. function
   * 2. builtin
   * 3. command
   */

  'outer: for (arg, span) in argv {
    let stdout = borrow_fd(STDOUT_FILENO);
    if let Some(func) = read_logic(|v| v.get_func(&arg)) {
      let ShFunc { body: _, source } = func;
      let (line, col) = source.line_and_col();
      let name = source.source().name();
      let msg = format!(
        "{arg} is a function defined at {name}:{}:{}\n",
        line + 1,
        col + 1
      );
      write(stdout, msg.as_bytes())?;
    } else if let Some(alias) = read_logic(|v| v.get_alias(&arg)) {
      let ShAlias { body, source } = alias;
      let (line, col) = source.line_and_col();
      let name = source.source().name();
      let msg = format!(
        "{arg} is an alias for '{body}' defined at {name}:{}:{}\n",
        line + 1,
        col + 1
      );
      write(stdout, msg.as_bytes())?;
    } else if BUILTINS.contains(&arg.as_str()) {
      let msg = format!("{arg} is a shell builtin\n");
      write(stdout, msg.as_bytes())?;
    } else if KEYWORDS.contains(&arg.as_str()) {
      let msg = format!("{arg} is a shell keyword\n");
      write(stdout, msg.as_bytes())?;
    } else {
      let path = env::var("PATH").unwrap_or_default();
      let paths = path.split(':').map(Path::new).collect::<Vec<_>>();

      for path in paths {
        if let Ok(entries) = path.read_dir() {
          for entry in entries.flatten() {
            let Ok(meta) = std::fs::metadata(entry.path()) else {
              continue;
            };
            let is_exec = meta.permissions().mode() & 0o111 != 0;

            if meta.is_file()
              && is_exec
              && let Some(name) = entry.file_name().to_str()
              && name == arg
            {
              let msg = format!("{arg} is {}\n", entry.path().display());
              write(stdout, msg.as_bytes())?;
              continue 'outer;
            }
          }
        }
      }

      state::set_status(1);
      return Err(sherr!(
        NotFound @ span,
        "'{}' is not a command, function, or alias", arg.fg(next_color())
      ));
    }
  }

  state::set_status(0);
  Ok(())
}

#[cfg(test)]
mod tests {
  use crate::state::{self};
  use crate::testutil::{TestGuard, test_input};

  // ===================== Builtins =====================

  #[test]
  fn type_builtin_echo() {
    let guard = TestGuard::new();
    test_input("type echo").unwrap();
    let out = guard.read_output();
    assert!(out.contains("echo"));
    assert!(out.contains("shell builtin"));
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
    assert!(out.contains("/")); // Should show a path
  }

  // ===================== Not found =====================

  #[test]
  fn type_not_found() {
    let _g = TestGuard::new();
    let result = test_input("type __hopefully____not_______a____command__");
    assert!(result.is_err());
    assert_eq!(state::get_status(), 1);
  }

  // ===================== Priority order =====================

  #[test]
  fn type_function_shadows_builtin() {
    let guard = TestGuard::new();
    // Define a function named 'echo' — should shadow the builtin
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
    assert_eq!(state::get_status(), 0);
  }
}
