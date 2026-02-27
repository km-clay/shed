use std::collections::HashSet;

use crate::expand::perform_param_expansion;
use crate::prompt::readline::markers;
use crate::state::{VarFlags, VarKind};

use super::*;

#[test]
fn simple_expansion() {
  let varsub = "$foo";
  write_vars(|v| {
    v.set_var(
      "foo",
      VarKind::Str("this is the value of the variable".into()),
      VarFlags::NONE,
    )
  });

  let mut tokens: Vec<Tk> = LexStream::new(Arc::new(varsub.to_string()), LexFlags::empty())
    .map(|tk| tk.unwrap())
    .filter(|tk| !matches!(tk.class, TkRule::EOI | TkRule::SOI))
    .collect();
  let var_tk = tokens.pop().unwrap();

  let exp_tk = var_tk.expand().unwrap();
  insta::assert_debug_snapshot!(exp_tk.get_words())
}

#[test]
fn unescape_string() {
  let string = "echo $foo \\$bar";
  let unescaped = unescape_str(string);

  insta::assert_snapshot!(unescaped)
}

#[test]
fn expand_alias_simple() {
  write_logic(|l| {
    l.insert_alias("foo", "echo foo");
    let input = String::from("foo");

    let result = expand_aliases(input, HashSet::new(), l);
    assert_eq!(result.as_str(), "echo foo");
    l.clear_aliases();
  });
}

#[test]
fn expand_alias_in_if() {
  write_logic(|l| {
    l.insert_alias("foo", "echo foo");
    let input = String::from("if foo; then echo bar; fi");

    let result = expand_aliases(input, HashSet::new(), l);
    assert_eq!(result.as_str(), "if echo foo; then echo bar; fi");
    l.clear_aliases();
  });
}

#[test]
fn expand_alias_multiline() {
  write_logic(|l| {
    l.insert_alias("foo", "echo foo");
    l.insert_alias("bar", "echo bar");
    let input = String::from(
      "
			foo
			if true; then
				bar
			fi
		",
    );
    let expected = String::from(
      "
			echo foo
			if true; then
				echo bar
			fi
		",
    );

    let result = expand_aliases(input, HashSet::new(), l);
    assert_eq!(result, expected)
  });
}

#[test]
fn expand_multiple_aliases() {
  write_logic(|l| {
    l.insert_alias("foo", "echo foo");
    l.insert_alias("bar", "echo bar");
    l.insert_alias("biz", "echo biz");
    let input = String::from("foo; bar; biz");

    let result = expand_aliases(input, HashSet::new(), l);
    assert_eq!(result.as_str(), "echo foo; echo bar; echo biz");
  });
}

#[test]
fn alias_in_arg_position() {
  write_logic(|l| {
    l.insert_alias("foo", "echo foo");
    let input = String::from("echo foo");

    let result = expand_aliases(input.clone(), HashSet::new(), l);
    assert_eq!(input, result);
    l.clear_aliases();
  });
}

#[test]
fn expand_recursive_alias() {
  write_logic(|l| {
    l.insert_alias("foo", "echo foo");
    l.insert_alias("bar", "foo bar");

    let input = String::from("bar");
    let result = expand_aliases(input, HashSet::new(), l);
    assert_eq!(result.as_str(), "echo foo bar");
  });
}

#[test]
fn test_infinite_recursive_alias() {
  write_logic(|l| {
    l.insert_alias("foo", "foo bar");

    let input = String::from("foo");
    let result = expand_aliases(input, HashSet::new(), l);
    assert_eq!(result.as_str(), "foo bar");
    l.clear_aliases();
  });
}

#[test]
fn param_expansion_defaultunsetornull() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
    v.set_var("set_var", VarKind::Str("value".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("unset:-default").unwrap();
  assert_eq!(result, "default");
}

#[test]
fn param_expansion_defaultunset() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
    v.set_var("set_var", VarKind::Str("value".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("unset-default").unwrap();
  assert_eq!(result, "default");
}

#[test]
fn param_expansion_setdefaultunsetornull() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
    v.set_var("set_var", VarKind::Str("value".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("unset:=assigned").unwrap();
  assert_eq!(result, "assigned");
}

#[test]
fn param_expansion_setdefaultunset() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
    v.set_var("set_var", VarKind::Str("value".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("unset=assigned").unwrap();
  assert_eq!(result, "assigned");
}

#[test]
fn param_expansion_altsetnotnull() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
    v.set_var("set_var", VarKind::Str("value".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("set_var:+alt").unwrap();
  assert_eq!(result, "alt");
}

#[test]
fn param_expansion_altnotnull() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
    v.set_var("set_var", VarKind::Str("value".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("set_var+alt").unwrap();
  assert_eq!(result, "alt");
}

#[test]
fn param_expansion_len() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("#foo").unwrap();
  assert_eq!(result, "3");
}

#[test]
fn param_expansion_substr() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo:1").unwrap();
  assert_eq!(result, "oo");
}

#[test]
fn param_expansion_substrlen() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo:0:2").unwrap();
  assert_eq!(result, "fo");
}

#[test]
fn param_expansion_remshortestprefix() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo#f*").unwrap();
  assert_eq!(result, "oo");
}

#[test]
fn param_expansion_remlongestprefix() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo##f*").unwrap();
  assert_eq!(result, "");
}

#[test]
fn param_expansion_remshortestsuffix() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo%*o").unwrap();
  assert_eq!(result, "fo");
}

#[test]
fn param_expansion_remlongestsuffix() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo%%*o").unwrap();
  assert_eq!(result, "");
}

#[test]
fn param_expansion_replacefirstmatch() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo/foo/X").unwrap();
  assert_eq!(result, "X");
}

#[test]
fn param_expansion_replaceallmatches() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo//o/X").unwrap();
  assert_eq!(result, "fXX");
}

#[test]
fn param_expansion_replaceprefix() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo/#f/X").unwrap();
  assert_eq!(result, "Xoo");
}

#[test]
fn param_expansion_replacesuffix() {
  write_vars(|v| {
    v.set_var("foo", VarKind::Str("foo".into()), VarFlags::NONE);
  });
  let result = perform_param_expansion("foo/%o/X").unwrap();
  assert_eq!(result, "foX");
}

// ============================================================================
// Double-Quote Escape Tests (POSIX)
// ============================================================================

#[test]
fn dquote_escape_dollar() {
  // "\$foo" should strip backslash, produce literal $foo (no expansion)
  let result = unescape_str(r#""\$foo""#);
  assert!(
    !result.contains(markers::VAR_SUB),
    "Escaped $ should not become VAR_SUB"
  );
  assert!(result.contains('$'), "Literal $ should be preserved");
  assert!(!result.contains('\\'), "Backslash should be stripped");
}

#[test]
fn dquote_escape_backslash() {
  // "\\" in double quotes should produce a single backslash
  let result = unescape_str(r#""\\""#);
  let inner: String = result
    .chars()
    .filter(|&c| c != markers::DUB_QUOTE)
    .collect();
  assert_eq!(
    inner, "\\",
    "Double backslash should produce single backslash"
  );
}

#[test]
fn dquote_escape_quote() {
  // "\"" should produce a literal double quote
  let result = unescape_str(r#""\"""#);
  let inner: String = result
    .chars()
    .filter(|&c| c != markers::DUB_QUOTE)
    .collect();
  assert!(
    inner.contains('"'),
    "Escaped quote should produce literal quote"
  );
}

#[test]
fn dquote_escape_backtick() {
  // "\`" should strip backslash, produce literal backtick
  let result = unescape_str(r#""\`""#);
  let inner: String = result
    .chars()
    .filter(|&c| c != markers::DUB_QUOTE)
    .collect();
  assert_eq!(
    inner, "`",
    "Escaped backtick should produce literal backtick"
  );
}

#[test]
fn dquote_escape_nonspecial_preserves_backslash() {
  // "\a" inside double quotes should preserve the backslash (a is not special)
  let result = unescape_str(r#""\a""#);
  let inner: String = result
    .chars()
    .filter(|&c| c != markers::DUB_QUOTE)
    .collect();
  assert_eq!(
    inner, "\\a",
    "Backslash before non-special char should be preserved"
  );
}

#[test]
fn dquote_unescaped_dollar_expands() {
  // "$foo" inside double quotes should produce VAR_SUB (expansion marker)
  let result = unescape_str(r#""$foo""#);
  assert!(
    result.contains(markers::VAR_SUB),
    "Unescaped $ should become VAR_SUB"
  );
}

#[test]
fn dquote_mixed_escapes() {
  // "hello \$world \\end" should have literal $, single backslash
  let result = unescape_str(r#""hello \$world \\end""#);
  assert!(
    !result.contains(markers::VAR_SUB),
    "Escaped $ should not expand"
  );
  assert!(result.contains('$'), "Literal $ should be in output");
  // Should have exactly one backslash (from \\)
  let inner: String = result
    .chars()
    .filter(|&c| c != markers::DUB_QUOTE)
    .collect();
  let backslash_count = inner.chars().filter(|&c| c == '\\').count();
  assert_eq!(backslash_count, 1, "\\\\  should produce one backslash");
}
