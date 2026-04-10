use ariadne::Fmt;

use crate::{
  expand::as_var_val_display, libsh::error::{ShResult, next_color}, parse::{NdRule, Node, execute::prepare_argv}, prelude::*, procio::borrow_fd, sherr, state::{self, read_logic, write_logic}
};

pub fn alias(node: Node) -> ShResult<()> {
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
    let mut alias_output = read_logic(|l| {
      l.aliases()
        .iter()
        .map(|ent| format!("{}={}", ent.0, as_var_val_display(&ent.1.to_string())))
        .collect::<Vec<_>>()
    });
    alias_output.sort(); // Sort them alphabetically
    let mut alias_output = alias_output.join("\n"); // Join them with newlines
    alias_output.push('\n'); // Push a final newline

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, alias_output.as_bytes())?; // Write it
  } else {
    for (arg, span) in argv {
      let Some((name, body)) = arg.split_once('=') else {
        let Some(alias) = read_logic(|l| l.get_alias(&arg)) else {
          return Err(sherr!(
            SyntaxErr @ span,
            "alias: Expected an assignment in alias args",
          ));
        };

        let alias_output = format!("{arg}='{alias}'");

        let stdout = borrow_fd(STDOUT_FILENO);
        write(stdout, alias_output.as_bytes())?; // Write it
        state::set_status(0);
        return Ok(());
      };
      if name == "command" || name == "builtin" {
        return Err(sherr!(
          ExecFail @ span,
          "alias: Cannot assign alias to reserved name '{}'", name.fg(next_color())
        ));
      }
      write_logic(|l| l.insert_alias(name, body, span.clone()));
    }
  }

  state::set_status(0);
  Ok(())
}

/// Remove one or more aliases by name
pub fn unalias(node: Node) -> ShResult<()> {
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
    let mut alias_output = read_logic(|l| {
      l.aliases()
        .iter()
        .map(|ent| format!("{}={}", ent.0, as_var_val_display(&ent.1.to_string())))
        .collect::<Vec<_>>()
    });
    alias_output.sort(); // Sort them alphabetically
    let mut alias_output = alias_output.join("\n"); // Join them with newlines
    alias_output.push('\n'); // Push a final newline

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, alias_output.as_bytes())?; // Write it
  } else {
    for (arg, span) in argv {
      if read_logic(|l| l.get_alias(&arg)).is_none() {
        return Err(sherr!(
          SyntaxErr @ span,
          "unalias: alias '{}' not found", arg.fg(next_color()),
        ));
      };
      write_logic(|l| l.remove_alias(&arg));
    }
  }
  state::set_status(0);
  Ok(())
}

#[cfg(test)]
mod tests {
  use crate::state::{self, read_logic};
  use crate::testutil::{TestGuard, test_input};
  use pretty_assertions::assert_eq;

  #[test]
  fn alias_set_and_expand() {
    let guard = TestGuard::new();
    test_input("alias ll='ls -la'").unwrap();

    let alias = read_logic(|l| l.get_alias("ll"));
    assert!(alias.is_some());
    assert_eq!(alias.unwrap().body, "ls -la");

    test_input("alias ll").unwrap();
    let out = guard.read_output();
    assert!(out.contains("ll"));
    assert!(out.contains("ls -la"));
  }

  #[test]
  fn alias_multiple() {
    let _guard = TestGuard::new();
    test_input("alias a='echo a' b='echo b'").unwrap();

    assert_eq!(read_logic(|l| l.get_alias("a")).unwrap().body, "echo a");
    assert_eq!(read_logic(|l| l.get_alias("b")).unwrap().body, "echo b");
  }

  #[test]
  fn alias_overwrite() {
    let _guard = TestGuard::new();
    test_input("alias x='first'").unwrap();
    test_input("alias x='second'").unwrap();

    assert_eq!(read_logic(|l| l.get_alias("x")).unwrap().body, "second");
  }

  #[test]
  fn alias_list_sorted() {
    let guard = TestGuard::new();
    test_input("alias z='zzz' a='aaa' m='mmm'").unwrap();
    guard.read_output();

    test_input("alias").unwrap();
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().collect();

    assert!(lines.len() >= 3);
    let a_pos = lines.iter().position(|l| l.contains("a=")).unwrap();
    let m_pos = lines.iter().position(|l| l.contains("m=")).unwrap();
    let z_pos = lines.iter().position(|l| l.contains("z=")).unwrap();
    assert!(a_pos < m_pos);
    assert!(m_pos < z_pos);
  }

  #[test]
  fn alias_reserved_name_command() {
    let _guard = TestGuard::new();
    let result = test_input("alias command='something'");
    assert!(result.is_err());
  }

  #[test]
  fn alias_reserved_name_builtin() {
    let _guard = TestGuard::new();
    let result = test_input("alias builtin='something'");
    assert!(result.is_err());
  }

  #[test]
  fn alias_missing_equals() {
    let _guard = TestGuard::new();
    let result = test_input("alias noequals");
    assert!(result.is_err());
  }

  #[test]
  fn alias_expansion_in_command() {
    let guard = TestGuard::new();
    test_input("alias greet='echo hello'").unwrap();
    guard.read_output();

    test_input("greet").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\n");
  }

  #[test]
  fn alias_expansion_with_args() {
    let guard = TestGuard::new();
    test_input("alias e='echo'").unwrap();
    guard.read_output();

    test_input("e foo bar").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "foo bar\n");
  }

  #[test]
  fn unalias_removes() {
    let _guard = TestGuard::new();
    test_input("alias tmp='something'").unwrap();
    assert!(read_logic(|l| l.get_alias("tmp")).is_some());

    test_input("unalias tmp").unwrap();
    assert!(read_logic(|l| l.get_alias("tmp")).is_none());
  }

  #[test]
  fn unalias_nonexistent() {
    let _guard = TestGuard::new();
    let result = test_input("unalias nosuchalias");
    assert!(result.is_err());
  }

  #[test]
  fn unalias_multiple() {
    let _guard = TestGuard::new();
    test_input("alias a='1' b='2' c='3'").unwrap();
    test_input("unalias a c").unwrap();

    assert!(read_logic(|l| l.get_alias("a")).is_none());
    assert!(read_logic(|l| l.get_alias("b")).is_some());
    assert!(read_logic(|l| l.get_alias("c")).is_none());
  }

  #[test]
  fn unalias_no_args_lists() {
    let guard = TestGuard::new();
    test_input("alias x='hello'").unwrap();
    guard.read_output();

    test_input("unalias").unwrap();
    let out = guard.read_output();
    assert!(out.contains("x"));
    assert!(out.contains("hello"));
  }

  #[test]
  fn alias_empty_body() {
    let _guard = TestGuard::new();
    test_input("alias empty=''").unwrap();

    let alias = read_logic(|l| l.get_alias("empty"));
    assert!(alias.is_some());
    assert_eq!(alias.unwrap().body, "");
  }

  #[test]
  fn alias_status_zero() {
    let _guard = TestGuard::new();
    test_input("alias ok='true'").unwrap();
    assert_eq!(state::get_status(), 0);

    test_input("unalias ok").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
