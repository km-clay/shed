pub mod posix;
pub mod testutil;

use testutil::{TestGuard, test_input};
// General miscellaneous test module for stuff that doesn't quite fit in elsewhere
// Stuff written in here is usually "I found a random bug and wrote a test case that asserts its non-existence"

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

// ===================== Byte transparency =====================
//
// The expansion pipeline is byte-native: arbitrary non-UTF-8 bytes must pass
// through printf, command substitution, and variables without being laundered
// into the U+FFFD replacement character, matching how bash preserves raw bytes.

#[test]
fn printf_octal_escape_emits_raw_bytes() {
  let guard = TestGuard::new();
  test_input("printf '\\377\\376'").unwrap();
  assert_eq!(guard.read_output_bytes(), b"\xff\xfe");
}

#[test]
fn printf_backslash_b_hex_emits_raw_byte() {
  let guard = TestGuard::new();
  test_input("printf '%b' 'x\\xffy'").unwrap();
  assert_eq!(guard.read_output_bytes(), b"x\xffy");
}

#[test]
fn printf_backslash_b_octal_emits_raw_byte() {
  let guard = TestGuard::new();
  test_input("printf '%b' '\\0377'").unwrap();
  assert_eq!(guard.read_output_bytes(), b"\xff");
}

#[test]
fn printf_non_utf8_in_format_string() {
  let guard = TestGuard::new();
  test_input("printf '\\377%s\\376' hi").unwrap();
  assert_eq!(guard.read_output_bytes(), b"\xffhi\xfe");
}

#[test]
fn cmd_sub_captures_raw_bytes() {
  let guard = TestGuard::new();
  test_input("x=$(printf '\\377'); printf '%s' \"$x\"").unwrap();
  assert_eq!(guard.read_output_bytes(), b"\xff");
}

#[test]
fn cmd_sub_raw_bytes_survive_concatenation() {
  let guard = TestGuard::new();
  test_input("x=$(printf 'a\\377b'); printf 'pre%spost' \"$x\"").unwrap();
  assert_eq!(guard.read_output_bytes(), b"prea\xffbpost");
}

#[test]
fn printf_percent_c_of_raw_byte() {
  let guard = TestGuard::new();
  test_input("x=$(printf '\\377'); printf '%c' \"$x\"").unwrap();
  assert_eq!(guard.read_output_bytes(), b"\xff");
}

// `declare -p`/`export -p`/`set` must round-trip a variable holding non-UTF-8
// bytes as a reusable `$'...'` ANSI-C assignment, not launder it into U+FFFD.

#[test]
fn declare_p_emits_ansi_c_for_non_utf8_value() {
  let guard = TestGuard::new();
  test_input("x=$(printf 'a\\377b'); declare -p x").unwrap();
  assert_eq!(guard.read_output_bytes(), b"x=$'a\\xffb'\n");
}

#[test]
fn export_p_emits_ansi_c_for_non_utf8_value() {
  let guard = TestGuard::new();
  test_input("x=$(printf 'a\\377b'); export x; export -p").unwrap();
  let out = guard.read_output_bytes();
  let needle = b"export x=$'a\\xffb'";
  assert!(
    out.windows(needle.len()).any(|w| w == needle),
    "missing byte-native export line in: {out:?}"
  );
}

#[test]
fn set_dump_emits_ansi_c_for_non_utf8_value() {
  let guard = TestGuard::new();
  test_input("x=$(printf 'a\\377b'); set").unwrap();
  let out = guard.read_output_bytes();
  let needle = b"x=$'a\\xffb'";
  assert!(
    out.windows(needle.len()).any(|w| w == needle),
    "missing byte-native set line in: {out:?}"
  );
}

#[test]
fn quote_emits_ansi_c_for_non_utf8_arg() {
  let guard = TestGuard::new();
  test_input("x=$(printf 'a\\377b'); quote \"$x\"").unwrap();
  assert_eq!(guard.read_output_bytes(), b"$'a\\xffb'\n");
}

#[test]
fn arrops_pop_emits_raw_bytes() {
  let guard = TestGuard::new();
  test_input("push arr \"$(printf 'x\\377y')\"; pop arr").unwrap();
  assert_eq!(guard.read_output_bytes(), b"x\xffy\n");
}

#[test]
fn param_prefix_removal_preserves_non_utf8() {
  // Regression (ultrareview bug_005): `${x#pat}` sliced a lossy view, so a raw
  // byte became U+FFFD. It must trim on the byte value.
  let guard = TestGuard::new();
  test_input("x=$(printf 'a\\377b'); printf '%s' \"${x#a}\"").unwrap();
  assert_eq!(guard.read_output_bytes(), b"\xffb");
}

#[test]
fn param_suffix_removal_preserves_non_utf8() {
  let guard = TestGuard::new();
  test_input("x=$(printf 'a\\377b'); printf '%s' \"${x%b}\"").unwrap();
  assert_eq!(guard.read_output_bytes(), b"a\xff");
}

#[test]
fn param_replace_preserves_non_utf8() {
  let guard = TestGuard::new();
  test_input("x=$(printf 'a\\377b'); printf '%s' \"${x/a/Z}\"").unwrap();
  assert_eq!(guard.read_output_bytes(), b"Z\xffb");
}

#[test]
fn test_command_non_utf8_operand_is_nonempty() {
  // Regression (ultrareview bug_003): a non-UTF-8 operand made the argv parser
  // treat it as end-of-input, truncating the leaf and mis-evaluating.
  let guard = TestGuard::new();
  test_input("x=$(printf 'a\\377b'); [ \"$x\" ]; printf '%d' \"$?\"").unwrap();
  assert_eq!(guard.read_output_bytes(), b"0");
}

#[test]
fn unquote_dollar_quote_preserves_non_utf8() {
  // Regression: `unquote` used to `from_utf8_lossy` the `$'...'` expansion,
  // mangling raw bytes. It must now emit them verbatim.
  let guard = TestGuard::new();
  test_input(r#"unquote "\$'a\377b'""#).unwrap();
  assert_eq!(guard.read_output_bytes(), b"a\xffb\n");
}

#[test]
fn cd_and_pwd_preserve_non_utf8_dir() {
  use std::os::unix::ffi::OsStrExt;
  let guard = TestGuard::new();
  let tmp = tempfile::TempDir::new().unwrap();
  let subdir = tmp.path().join(std::ffi::OsStr::from_bytes(b"ba\xffd"));
  std::fs::create_dir(&subdir).unwrap();

  // The tempdir prefix is UTF-8; the non-UTF-8 leaf arrives at runtime via
  // command substitution, so the source stays valid UTF-8.
  let base = tmp.path().display().to_string();
  test_input(format!("cd '{base}'; cd \"$(printf 'ba\\377d')\"; pwd -P")).unwrap();

  let out = guard.read_output_bytes();
  assert!(
    out.ends_with(b"ba\xffd\n"),
    "pwd -P did not preserve non-UTF-8 dir bytes: {out:?}"
  );
}
