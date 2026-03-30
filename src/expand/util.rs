use regex::Regex;

use crate::expand::escape::unescape_str;
use crate::expand::var::expand_raw;
use crate::libsh::error::ShResult;
use crate::readline::markers;

/// Expand a case pattern: performs variable/command expansion while preserving
/// glob metacharacters that were inside quotes as literals (by backslash-escaping them).
/// Unquoted glob chars (*, ?, [) pass through for glob_to_regex to interpret.
pub fn expand_case_pattern(raw: &str) -> ShResult<String> {
  let unescaped = unescape_str(raw);
  let expanded = expand_raw(&mut unescaped.chars().peekable())?;

  let mut result = String::new();
  let mut in_quote = false;
  let mut chars = expanded.chars();

  while let Some(ch) = chars.next() {
    match ch {
      markers::DUB_QUOTE | markers::SNG_QUOTE => {
        in_quote = !in_quote;
      }
      markers::ESCAPE => {
        if let Some(next_ch) = chars.next() {
          result.push(next_ch);
        }
      }
      '*' | '?' | '[' | ']' if in_quote => {
        result.push('\\');
        result.push(ch);
      }
      _ => result.push(ch),
    }
  }
  Ok(result)
}

pub fn is_var_name_ch(ch: &char) -> bool {
  matches!(ch,
    '@' |
    '*' |
    '#' |
    '?' |
    '!' |
    '-' |
    '_' |
    '{' |
    'A'..='Z' |
    'a'..='z' |
    '0'..='9'
  )
}

pub fn glob_to_regex(glob: &str, anchored: bool) -> Regex {
  // fnmatch_regex always produces ^...$, so get the pattern string and strip if unanchored
  let pattern = fnmatch_regex::glob_to_regex_pattern(glob).unwrap_or_else(|_| regex::escape(glob));
  let pattern = if anchored {
    pattern
  } else {
    pattern
      .strip_prefix('^')
      .unwrap_or(&pattern)
      .strip_suffix('$')
      .unwrap_or(&pattern)
      .to_string()
  };
  Regex::new(&pattern).unwrap()
}

#[cfg(test)]
mod tests {
  use super::*;

  // ===================== glob_to_regex =====================

  #[test]
  fn glob_star_matches_anything() {
    let re = glob_to_regex("*", false);
    assert!(re.is_match("anything"));
    assert!(re.is_match(""));
  }

  #[test]
  fn glob_question_matches_single() {
    let re = glob_to_regex("?", true);
    assert!(re.is_match("a"));
    assert!(!re.is_match("ab"));
    assert!(!re.is_match(""));
  }

  #[test]
  fn glob_star_dot_ext() {
    let re = glob_to_regex("*.txt", true);
    assert!(re.is_match("hello.txt"));
    assert!(re.is_match(".txt"));
    assert!(!re.is_match("hello.rs"));
  }

  #[test]
  fn glob_char_class() {
    let re = glob_to_regex("[abc]", true);
    assert!(re.is_match("a"));
    assert!(re.is_match("b"));
    assert!(!re.is_match("d"));
  }

  #[test]
  fn glob_dot_escaped() {
    let re = glob_to_regex("foo.bar", true);
    assert!(re.is_match("foo.bar"));
    assert!(!re.is_match("fooXbar"));
  }

  #[test]
  fn glob_special_chars_escaped() {
    let re = glob_to_regex("a+b(c)", true);
    assert!(re.is_match("a+b(c)"));
    assert!(!re.is_match("ab"));
  }

  #[test]
  fn glob_anchored_vs_unanchored() {
    let anchored = glob_to_regex("hello", true);
    assert!(anchored.is_match("hello"));
    assert!(!anchored.is_match("say hello"));

    let unanchored = glob_to_regex("hello", false);
    assert!(unanchored.is_match("hello"));
    assert!(unanchored.is_match("say hello world"));
  }
}
