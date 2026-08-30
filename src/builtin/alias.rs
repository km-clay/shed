//! Contains builtins that pertain to shell aliases.
//!
//! `alias` - create, list, and display shell aliases
//! `unalias` - remove shell aliases
//! `excmd` - create, list, and display ex command aliases

use bstr::ByteSlice;

use crate::opt;

use super::{
  ShResult, Shed,
  opt::OptSpec,
  sherr,
  state::vars::{display_as_var, display_as_vars},
  varcmds::split_assignment_raw,
  with_status,
};
use crate::procio::outln_bytes;

/// Create a shell alias
///
/// Aliases are stored in the shell state and can be used to create shortcuts for commands.
/// The `alias` command can be used to create, list, and display aliases.
pub(super) struct Alias;
impl super::Builtin for Alias {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      let output = Shed::logic(|l| display_as_vars(l.aliases().iter()));
      outln_bytes(&output);

      return with_status(0);
    }

    for (arg, span) in args.arguments() {
      let (name, value) = split_assignment_raw(arg);

      // reject these two
      if name == b"command" || name == b"builtin" {
        return Err(sherr!(
          ExecFail @ span.clone(),
          "Cannot assign alias to reserved name '{}'",
          name.to_str_lossy()
        ));
      }

      if let Some(value) = value {
        Shed::logic_mut(|l| l.insert_alias(&name.to_str_lossy(), &value.into(), span.clone()));
      } else if let Some(alias) = Shed::logic(|l| l.get_alias(&name.to_str_lossy())) {
        outln_bytes(&display_as_var(name, alias.body()));
      } else {
        return Err(sherr!(
          SyntaxErr @ span.clone(),
          "Unknown alias '{}'",
          name.to_str_lossy()
        ));
      }
    }

    with_status(0)
  }
}

/// Unset a shell alias
pub(super) struct Unalias;
impl super::Builtin for Unalias {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      let output = Shed::logic(|l| display_as_vars(l.aliases().iter()));
      outln_bytes(&output);

      return with_status(0);
    }

    for (arg, span) in args.arguments() {
      if Shed::logic(|l| l.get_alias(&arg.to_str_lossy())).is_none() {
        return Err(sherr!(
          SyntaxErr @ span.clone(),
          "unalias: alias '{arg}' not found",
        ));
      }
      Shed::logic_mut(|l| l.remove_alias(&arg.to_str_lossy()));
    }

    with_status(0)
  }
}

/// Create an ex mode alias.
///
/// Ex mode aliases are stored in the shell state and can be used to create shortcuts for ex commands.
pub(super) struct ExCmd;
impl super::Builtin for ExCmd {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("remove" | b'r')]
  }
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      let output = Shed::logic(|l| display_as_vars(l.ex_aliases().iter()));
      outln_bytes(&output);

      return with_status(0);
    }

    let mut remove = false;

    for opt in args.options() {
      if opt.key() == "remove" {
        remove = true;
      }
    }

    for (arg, span) in args.arguments() {
      let (name, value) = split_assignment_raw(arg);
      let name = &name.to_str_lossy();

      if !remove && let Some(value) = value {
        Shed::logic_mut(|l| l.insert_ex_alias(name, &value.into(), span.clone()));
      } else {
        match Shed::logic(|l| l.get_ex_alias(name)) {
          Some(_) if remove => Shed::logic_mut(|l| l.remove_ex_alias(name)),
          Some(alias) => outln_bytes(&display_as_var(name.as_bytes(), alias.body())),
          None => return Err(sherr!(SyntaxErr @ span.clone(),"Unknown ex command alias '{name}'")),
        }
      }
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::{
    state::{self, Shed},
    tests::testutil::{TestGuard, test_input},
  };
  use pretty_assertions::assert_eq;

  #[test]
  fn alias_set_and_expand() {
    let guard = TestGuard::new();
    test_input("alias ll='ls -la'").unwrap();

    let alias = Shed::logic(|l| l.get_alias("ll"));
    assert!(alias.is_some());
    assert_eq!(alias.unwrap().body(), "ls -la");

    test_input("alias ll").unwrap();
    let out = guard.read_output();
    assert!(out.contains("ll"));
    assert!(out.contains("ls -la"));
  }

  #[test]
  fn alias_multiple() {
    let _guard = TestGuard::new();
    test_input("alias a='echo a' b='echo b'").unwrap();

    assert_eq!(Shed::logic(|l| l.get_alias("a")).unwrap().body(), "echo a");
    assert_eq!(Shed::logic(|l| l.get_alias("b")).unwrap().body(), "echo b");
  }

  #[test]
  fn alias_overwrite() {
    let _guard = TestGuard::new();
    test_input("alias x='first'").unwrap();
    test_input("alias x='second'").unwrap();

    assert_eq!(Shed::logic(|l| l.get_alias("x")).unwrap().body(), "second");
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
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn alias_reserved_name_builtin() {
    let _guard = TestGuard::new();
    test_input("alias builtin='something'").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn alias_missing_equals() {
    let _guard = TestGuard::new();
    test_input("alias noequals").ok();
    assert_ne!(state::Shed::get_status(), 0);
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
    assert!(Shed::logic(|l| l.get_alias("tmp")).is_some());

    test_input("unalias tmp").unwrap();
    assert!(Shed::logic(|l| l.get_alias("tmp")).is_none());
  }

  #[test]
  fn unalias_nonexistent() {
    let _guard = TestGuard::new();
    test_input("unalias nosuchalias").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn unalias_multiple() {
    let _guard = TestGuard::new();
    test_input("alias a='1' b='2' c='3'").unwrap();
    test_input("unalias a c").unwrap();

    assert!(Shed::logic(|l| l.get_alias("a")).is_none());
    assert!(Shed::logic(|l| l.get_alias("b")).is_some());
    assert!(Shed::logic(|l| l.get_alias("c")).is_none());
  }

  #[test]
  fn unalias_no_args_lists() {
    let guard = TestGuard::new();
    test_input("alias x='hello'").unwrap();
    guard.read_output();

    test_input("unalias").unwrap();
    let out = guard.read_output();
    assert!(out.contains('x'));
    assert!(out.contains("hello"));
  }

  #[test]
  fn alias_empty_body() {
    let _guard = TestGuard::new();
    test_input("alias empty=''").unwrap();

    let alias = Shed::logic(|l| l.get_alias("empty"));
    assert!(alias.is_some());
    assert_eq!(alias.unwrap().body(), "");
  }

  #[test]
  fn alias_status_zero() {
    let _guard = TestGuard::new();
    test_input("alias ok='true'").unwrap();
    assert_eq!(state::Shed::get_status(), 0);

    test_input("unalias ok").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }
}
