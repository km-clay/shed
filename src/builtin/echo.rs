use crate::{
  expand::expand_prompt,
  getopt::{Opt, OptSpec, get_opts_from_tokens},
  libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt},
  parse::{NdRule, Node},
  prelude::*,
  procio::borrow_fd,
  state::{self, read_shopts},
};

pub const ECHO_OPTS: [OptSpec; 4] = [
  OptSpec {
    opt: Opt::Short('n'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('E'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('e'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('p'),
    takes_arg: false,
  },
];

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct EchoFlags: u32 {
    const NO_NEWLINE = 0b000001;
    const NO_ESCAPE  = 0b000010;
    const USE_ESCAPE = 0b000100;
    const USE_PROMPT = 0b001000;
  }
}

pub fn echo(node: Node) -> ShResult<()> {
  let blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };
  assert!(!argv.is_empty());
  let (mut argv, opts) = get_opts_from_tokens(argv, &ECHO_OPTS)?;
  let flags = get_echo_flags(opts).blame(blame)?;
  if !argv.is_empty() {
    argv.remove(0);
  }

  let output_channel = borrow_fd(STDOUT_FILENO);
  let xpg_echo = read_shopts(|o| o.core.xpg_echo); // If true, echo expands escape sequences by default, and -E opts out

  let use_escape =
    (xpg_echo && !flags.contains(EchoFlags::NO_ESCAPE)) || flags.contains(EchoFlags::USE_ESCAPE);

  let mut echo_output = prepare_echo_args(
    argv
      .into_iter()
      .map(|a| a.0) // Extract the String from the tuple of (String,Span)
      .collect::<Vec<_>>(),
    use_escape,
    flags.contains(EchoFlags::USE_PROMPT),
  )?
  .join(" ");

  if !flags.contains(EchoFlags::NO_NEWLINE) && !echo_output.ends_with('\n') {
    echo_output.push('\n')
  }

  write(output_channel, echo_output.as_bytes())?;

  state::set_status(0);
  Ok(())
}

pub fn prepare_echo_args(
  argv: Vec<String>,
  use_escape: bool,
  use_prompt: bool,
) -> ShResult<Vec<String>> {
  if !use_escape {
    if use_prompt {
      let expanded: ShResult<Vec<String>> = argv
        .into_iter()
        .map(|s| expand_prompt(s.as_str()))
        .collect();
      return expanded;
    }
    return Ok(argv);
  }

  let mut prepared_args = Vec::with_capacity(argv.len());

  for arg in argv {
    let mut prepared_arg = String::new();
    if use_prompt {
      prepared_arg = expand_prompt(&prepared_arg)?;
    }

    let mut chars = arg.chars().peekable();

    while let Some(c) = chars.next() {
      if c == '\\' {
        if let Some(&next_char) = chars.peek() {
          match next_char {
            'n' => {
              prepared_arg.push('\n');
              chars.next();
            }
            't' => {
              prepared_arg.push('\t');
              chars.next();
            }
            'r' => {
              prepared_arg.push('\r');
              chars.next();
            }
            'a' => {
              prepared_arg.push('\x07');
              chars.next();
            }
            'b' => {
              prepared_arg.push('\x08');
              chars.next();
            }
            'e' | 'E' => {
              prepared_arg.push('\x1b');
              chars.next();
            }
            'x' => {
              chars.next(); // consume 'x'
              let mut hex_digits = String::new();
              for _ in 0..2 {
                if let Some(&hex_char) = chars.peek() {
                  if hex_char.is_ascii_hexdigit() {
                    hex_digits.push(hex_char);
                    chars.next();
                  } else {
                    break;
                  }
                } else {
                  break;
                }
              }
              if let Ok(value) = u8::from_str_radix(&hex_digits, 16) {
                prepared_arg.push(value as char);
              } else {
                prepared_arg.push('\\');
                prepared_arg.push('x');
                prepared_arg.push_str(&hex_digits);
              }
            }
            '0' => {
              chars.next(); // consume '0'
              let mut octal_digits = String::new();
              for _ in 0..3 {
                if let Some(&octal_char) = chars.peek() {
                  if ('0'..='7').contains(&octal_char) {
                    octal_digits.push(octal_char);
                    chars.next();
                  } else {
                    break;
                  }
                } else {
                  break;
                }
              }
              if let Ok(value) = u8::from_str_radix(&octal_digits, 8) {
                prepared_arg.push(value as char);
              } else {
                prepared_arg.push('\\');
                prepared_arg.push('0');
                prepared_arg.push_str(&octal_digits);
              }
            }
            '\\' => {
              prepared_arg.push('\\');
              chars.next();
            }
            _ => prepared_arg.push(c),
          }
        } else {
          prepared_arg.push(c);
        }
      } else {
        prepared_arg.push(c);
      }
    }

    prepared_args.push(prepared_arg);
  }

  Ok(prepared_args)
}

pub fn get_echo_flags(opts: Vec<Opt>) -> ShResult<EchoFlags> {
  let mut flags = EchoFlags::empty();

  for opt in opts {
    match opt {
      Opt::Short('n') => flags |= EchoFlags::NO_NEWLINE,
      Opt::Short('e') => flags |= EchoFlags::USE_ESCAPE,
      Opt::Short('p') => flags |= EchoFlags::USE_PROMPT,
      Opt::Short('E') => flags |= EchoFlags::NO_ESCAPE,
      _ => {
        return Err(ShErr::simple(
          ShErrKind::ExecFail,
          format!("echo: Unexpected flag '{opt}'"),
        ));
      }
    }
  }

  Ok(flags)
}

#[cfg(test)]
mod tests {
  use super::prepare_echo_args;
  use crate::state::{self, write_shopts};
  use crate::testutil::{TestGuard, test_input};

  // ===================== Pure: prepare_echo_args =====================

  #[test]
  fn prepare_no_escape() {
    let result = prepare_echo_args(vec!["hello\\nworld".into()], false, false).unwrap();
    assert_eq!(result, vec!["hello\\nworld"]);
  }

  #[test]
  fn prepare_escape_newline() {
    let result = prepare_echo_args(vec!["hello\\nworld".into()], true, false).unwrap();
    assert_eq!(result, vec!["hello\nworld"]);
  }

  #[test]
  fn prepare_escape_tab() {
    let result = prepare_echo_args(vec!["a\\tb".into()], true, false).unwrap();
    assert_eq!(result, vec!["a\tb"]);
  }

  #[test]
  fn prepare_escape_carriage_return() {
    let result = prepare_echo_args(vec!["a\\rb".into()], true, false).unwrap();
    assert_eq!(result, vec!["a\rb"]);
  }

  #[test]
  fn prepare_escape_bell() {
    let result = prepare_echo_args(vec!["a\\ab".into()], true, false).unwrap();
    assert_eq!(result, vec!["a\x07b"]);
  }

  #[test]
  fn prepare_escape_backspace() {
    let result = prepare_echo_args(vec!["a\\bb".into()], true, false).unwrap();
    assert_eq!(result, vec!["a\x08b"]);
  }

  #[test]
  fn prepare_escape_escape_char() {
    let result = prepare_echo_args(vec!["a\\eb".into()], true, false).unwrap();
    assert_eq!(result, vec!["a\x1bb"]);
  }

  #[test]
  fn prepare_escape_upper_e() {
    let result = prepare_echo_args(vec!["a\\Eb".into()], true, false).unwrap();
    assert_eq!(result, vec!["a\x1bb"]);
  }

  #[test]
  fn prepare_escape_backslash() {
    let result = prepare_echo_args(vec!["a\\\\b".into()], true, false).unwrap();
    assert_eq!(result, vec!["a\\b"]);
  }

  #[test]
  fn prepare_escape_hex() {
    let result = prepare_echo_args(vec!["\\x41".into()], true, false).unwrap();
    assert_eq!(result, vec!["A"]);
  }

  #[test]
  fn prepare_escape_hex_lowercase() {
    let result = prepare_echo_args(vec!["\\x61".into()], true, false).unwrap();
    assert_eq!(result, vec!["a"]);
  }

  #[test]
  fn prepare_escape_octal() {
    let result = prepare_echo_args(vec!["\\0101".into()], true, false).unwrap();
    assert_eq!(result, vec!["A"]); // octal 101 = 65 = 'A'
  }

  #[test]
  fn prepare_escape_multiple() {
    let result = prepare_echo_args(vec!["a\\nb\\tc".into()], true, false).unwrap();
    assert_eq!(result, vec!["a\nb\tc"]);
  }

  #[test]
  fn prepare_multiple_args() {
    let result = prepare_echo_args(vec!["hello".into(), "world".into()], false, false).unwrap();
    assert_eq!(result, vec!["hello", "world"]);
  }

  #[test]
  fn prepare_trailing_backslash() {
    let result = prepare_echo_args(vec!["hello\\".into()], true, false).unwrap();
    assert_eq!(result, vec!["hello\\"]);
  }

  #[test]
  fn prepare_unknown_escape_literal() {
    // Unknown escape like \z should keep the backslash
    let result = prepare_echo_args(vec!["\\z".into()], true, false).unwrap();
    assert_eq!(result, vec!["\\z"]);
  }

  // ===================== Integration: basic echo =====================

  #[test]
  fn echo_simple() {
    let guard = TestGuard::new();
    test_input("echo hello").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\n");
  }

  #[test]
  fn echo_multiple_args() {
    let guard = TestGuard::new();
    test_input("echo hello world").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello world\n");
  }

  #[test]
  fn echo_no_args() {
    let guard = TestGuard::new();
    test_input("echo").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "\n");
  }

  #[test]
  fn echo_status_zero() {
    let _g = TestGuard::new();
    test_input("echo hello").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  // ===================== Integration: -n flag =====================

  #[test]
  fn echo_no_newline() {
    let guard = TestGuard::new();
    test_input("echo -n hello").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello");
  }

  #[test]
  fn echo_no_newline_no_args() {
    let guard = TestGuard::new();
    test_input("echo -n").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "");
  }

  // ===================== Integration: -e flag =====================

  #[test]
  fn echo_escape_newline() {
    let guard = TestGuard::new();
    test_input("echo -e 'hello\\nworld'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\nworld\n");
  }

  #[test]
  fn echo_escape_tab() {
    let guard = TestGuard::new();
    test_input("echo -e 'a\\tb'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "a\tb\n");
  }

  #[test]
  fn echo_no_escape_by_default() {
    let guard = TestGuard::new();
    test_input("echo 'hello\\nworld'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\\nworld\n");
  }

  // ===================== Integration: -E flag + xpg_echo =====================

  #[test]
  fn echo_xpg_echo_expands_by_default() {
    let guard = TestGuard::new();
    write_shopts(|o| o.core.xpg_echo = true);

    test_input("echo 'hello\\nworld'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\nworld\n");
  }

  #[test]
  fn echo_xpg_echo_suppressed_by_big_e() {
    let guard = TestGuard::new();
    write_shopts(|o| o.core.xpg_echo = true);

    test_input("echo -E 'hello\\nworld'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\\nworld\n");
  }

  #[test]
  fn echo_small_e_overrides_without_xpg() {
    let guard = TestGuard::new();
    write_shopts(|o| o.core.xpg_echo = false);

    test_input("echo -e 'a\\tb'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "a\tb\n");
  }

  #[test]
  fn echo_big_e_noop_without_xpg() {
    let guard = TestGuard::new();
    write_shopts(|o| o.core.xpg_echo = false);

    // -E without xpg_echo is a no-op — escapes already off
    test_input("echo -E 'hello\\nworld'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "hello\\nworld\n");
  }

  // ===================== Integration: combined flags =====================

  #[test]
  fn echo_n_and_e() {
    let guard = TestGuard::new();
    test_input("echo -n -e 'a\\nb'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "a\nb");
  }

  #[test]
  fn echo_xpg_n_suppresses_newline() {
    let guard = TestGuard::new();
    write_shopts(|o| o.core.xpg_echo = true);

    test_input("echo -n 'hello\\nworld'").unwrap();
    let out = guard.read_output();
    // xpg_echo expands \n, -n suppresses trailing newline
    assert_eq!(out, "hello\nworld");
  }
}
