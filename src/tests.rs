use crate::testutil::{TestGuard, test_input};
// General miscellaneous test module for stuff that doesn't quite fit in elsewhere

// ===================== Dollar quoting =====================

#[test]
fn dollar_quote_in_cmd_sub() {
  let guard = TestGuard::new();
  test_input("echo $(echo $'foo\\n\\n\\n\\n')").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "foo\n");
}

#[test]
fn dollar_quote_standalone() {
  let guard = TestGuard::new();
  test_input("echo $'hello\\nworld'").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\nworld\n");
}

#[test]
fn dollar_quote_in_double_quotes() {
  let guard = TestGuard::new();
  test_input("echo \"$'foo\\t'\"").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "foo\t\n");
}

#[test]
fn dollar_quote_escape_sequences() {
  let guard = TestGuard::new();
  test_input("echo $'\\a\\b\\e\\v'").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "\x07\x08\x1b\x0b\n");
}

#[test]
fn dollar_quote_carriage_return() {
  let guard = TestGuard::new();
  test_input("echo $'foo\\rbar'").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "foo\rbar\n");
}

#[test]
fn dollar_quote_escaped_single_quote() {
  let guard = TestGuard::new();
  test_input("echo $'it\\'s'").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "it's\n");
}

#[test]
fn dollar_quote_escaped_backslash() {
  let guard = TestGuard::new();
  test_input("echo $'back\\\\slash'").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "back\\slash\n");
}

#[test]
fn dollar_quote_hex_escape() {
  let guard = TestGuard::new();
  test_input("echo $'\\x41\\x42\\x43'").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "ABC\n");
}

#[test]
fn dollar_quote_octal_escape() {
  let guard = TestGuard::new();
  test_input("echo $'\\o101\\o102\\o103'").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "ABC\n");
}

#[test]
fn dollar_quote_unknown_escape_passthrough() {
  let guard = TestGuard::new();
  test_input("echo $'\\z'").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "z\n");
}

#[test]
fn dollar_quote_concatenated_with_regular_string() {
  let guard = TestGuard::new();
  test_input("echo $'hello\\n'world").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\nworld\n");
}

// ===================== Command substitution =====================

#[test]
fn nested_cmd_sub() {
  let guard = TestGuard::new();
  test_input("echo $(echo $(echo hello))").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\n");
}

#[test]
fn cmd_sub_trailing_newlines_stripped() {
  let guard = TestGuard::new();
  test_input("echo \"$(printf 'hello\\n\\n\\n')\"").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\n");
}

#[test]
fn cmd_sub_with_dollar_quote_inside() {
  let guard = TestGuard::new();
  test_input("echo $(printf $'%s\\n' hello world)").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello world\n");
}

#[test]
fn backtick_cmd_sub() {
  let guard = TestGuard::new();
  test_input("echo `echo hello`").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\n");
}

#[test]
fn cmd_sub_in_double_quotes() {
  let guard = TestGuard::new();
  test_input("echo \"result: $(echo ok)\"").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "result: ok\n");
}

// ===================== Quoting =====================

#[test]
fn single_quote_preserves_literal() {
  let guard = TestGuard::new();
  test_input("echo 'hello $USER \\n'").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello $USER \\n\n");
}

#[test]
fn double_quote_expands_vars() {
  let guard = TestGuard::new();
  test_input("FOO=bar; echo \"hello $FOO\"").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello bar\n");
}

#[test]
fn double_quote_backslash_special_chars() {
  let guard = TestGuard::new();
  test_input("echo \"a\\\"b\"").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "a\"b\n");
}

#[test]
fn double_quote_backslash_preserves_non_special() {
  let guard = TestGuard::new();
  test_input("echo \"a\\zb\"").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "a\\zb\n");
}

#[test]
fn double_quote_dollar_quote_inside() {
  let guard = TestGuard::new();
  test_input("echo \"prefix$'\\t'suffix\"").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "prefix\tsuffix\n");
}

#[test]
fn double_quote_backtick_cmd_sub() {
  let guard = TestGuard::new();
  test_input("echo \"hello `echo world`\"").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello world\n");
}

// ===================== Variable substitution edge cases =====================

#[test]
fn dollar_dollar_expands_to_pid() {
  let guard = TestGuard::new();
  test_input("echo $$").unwrap();
  let out = guard.read_output();
  // Should be a numeric PID
  assert!(
    out.trim().parse::<u32>().is_ok(),
    "expected numeric PID, got: {out}"
  );
}

#[test]
fn bare_dollar_at_end() {
  let guard = TestGuard::new();
  test_input("echo foo$").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "foo$\n");
}

#[test]
fn bare_dollar_before_space() {
  let guard = TestGuard::new();
  test_input("echo $ foo").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "$ foo\n");
}
