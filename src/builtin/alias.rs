use ariadne::Fmt;

use crate::{
  builtin::varcmds::{display_as_var, display_as_vars, split_assignment_raw},
  outln, sherr,
  state::{read_logic, write_logic},
  util::{
    error::{ShResult, next_color},
    with_status,
  },
};

pub(super) struct Alias;
impl super::Builtin for Alias {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      let output = read_logic(|l| display_as_vars(l.aliases().iter()));
      outln!("{output}")?;

      return with_status(0);
    }

    for (arg, span) in args.argv {
      let (name, value) = split_assignment_raw(arg);
      if name == "command" || name == "builtin" {
        return Err(sherr!(
          ExecFail @ span,
          "Cannot assign alias to reserved name '{}'", name.fg(next_color())
        ));
      }

      if let Some(value) = value {
        write_logic(|l| l.insert_alias(&name, &value, span.clone()));
      } else if let Some(alias) = read_logic(|l| l.get_alias(&name)) {
        outln!("{}", display_as_var(name, alias.body))?;
      } else {
        return Err(sherr!(
          SyntaxErr @ span,
          "Unknown alias '{name}'",
        ));
      }
    }

    with_status(0)
  }
}

pub(super) struct Unalias;
impl super::Builtin for Unalias {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      let output = read_logic(|l| display_as_vars(l.aliases().iter()));
      outln!("{output}")?;

      return with_status(0);
    }

    for (arg, span) in args.argv {
      if read_logic(|l| l.get_alias(&arg)).is_none() {
        return Err(sherr!(
          SyntaxErr @ span,
          "unalias: alias '{}' not found", arg.fg(next_color()),
        ));
      };
      write_logic(|l| l.remove_alias(&arg));
    }

    with_status(0)
  }
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
    test_input("alias command='something'").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn alias_reserved_name_builtin() {
    let _guard = TestGuard::new();
    test_input("alias builtin='something'").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn alias_missing_equals() {
    let _guard = TestGuard::new();
    test_input("alias noequals").ok();
    assert_ne!(state::get_status(), 0);
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
    test_input("unalias nosuchalias").ok();
    assert_ne!(state::get_status(), 0);
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
