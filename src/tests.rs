use crate::testutil::{TestGuard, test_input};
// General miscellaneous test module for stuff that doesn't quite fit in elsewhere

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
