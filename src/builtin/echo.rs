use crate::{
  builtin::Builtin,
  expand::expand_prompt,
  getopt::{Opt, OptSpec},
  prelude::*,
  state::read_shopts,
  util::{
    error::{ShResult, ShResultExt},
    strops::expand_ansi_c,
    with_status, write_out,
  },
};

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct EchoFlags: u32 {
    const NO_NEWLINE = 0b000001;
    const USE_ESCAPE = 0b000010;
    const USE_PROMPT = 0b000100;
  }
}

pub(super) struct Echo;
impl Builtin for Echo {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('n'),
      OptSpec::flag('E'),
      OptSpec::flag('e'),
      OptSpec::flag('p'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let xpg_echo = read_shopts(|o| o.core.xpg_echo);
    let mut flags = EchoFlags::empty();
    if xpg_echo {
      flags |= EchoFlags::USE_ESCAPE;
    }
    for opt in &args.opts {
      match opt {
        Opt::Short('n') => flags |= EchoFlags::NO_NEWLINE,
        Opt::Short('p') => flags |= EchoFlags::USE_PROMPT,
        Opt::Short('e') => flags |= EchoFlags::USE_ESCAPE,
        Opt::Short('E') => flags &= !EchoFlags::USE_ESCAPE,
        _ => {}
      }
    }
    let use_prompt = flags.contains(EchoFlags::USE_PROMPT);
    let use_escape = flags.contains(EchoFlags::USE_ESCAPE);

    let prepared: ShResult<Vec<String>> = args
      .argv
      .into_iter()
      .map(|(mut st, sp)| -> ShResult<String> {
        if use_prompt {
          st = expand_prompt(&st).promote_err(sp)?;
        }
        if use_escape {
          st = expand_ansi_c(&st);
        }
        Ok(st)
      })
      .collect();

    let mut joined = prepared?.join(" ");
    if !flags.contains(EchoFlags::NO_NEWLINE) && !joined.ends_with('\n') {
      joined.push('\n');
    }

    write_out(joined)?;

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{self, write_shopts};
  use crate::testutil::{TestGuard, test_input};

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

    // -E without xpg_echo is a no-op - escapes already off
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

  #[test]
  fn echo_unknown_packed_short_is_literal() {
    let guard = TestGuard::new();
    test_input("echo -shed").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "-shed\n");
  }

  #[test]
  fn echo_unknown_packed_short_with_other_args() {
    let guard = TestGuard::new();
    test_input("echo x -shed y").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "x -shed y\n");
  }

  #[test]
  fn echo_dollar_zero_expansion() {
    // $0 expands to a value starting with '-' in login shells; make sure
    // that doesn't get re-parsed as options and duplicated.
    let guard = TestGuard::new();
    test_input("X=-shed; echo $X").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "-shed\n");
  }

  #[test]
  fn echo_partial_match_pack_is_literal() {
    // `-nq` contains recognized 'n' but unknown 'q' -> whole word is literal.
    let guard = TestGuard::new();
    test_input("echo -nq hello").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "-nq hello\n");
  }

  #[test]
  fn echo_unknown_long_opt_is_literal() {
    let guard = TestGuard::new();
    test_input("echo --bogus hi").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "--bogus hi\n");
  }

  #[test]
  fn echo_fully_recognized_pack_still_works() {
    // -ne: both recognized, so 'n' suppresses newline and 'e' enables escapes.
    let guard = TestGuard::new();
    test_input("echo -ne 'a\\tb'").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "a\tb");
  }
}
