use super::{
  ShResult,
  escape::unescape_str,
  match_loop,
  stream::{Marker, Unit},
  var::expand_raw,
};

/// Expand a case pattern: performs variable/command expansion while preserving
/// glob metacharacters that were inside quotes as literals (by backslash-escaping them).
/// Unquoted glob chars (*, ?, [) pass through for `glob_to_regex` to interpret.
pub fn expand_case_pattern(raw: &str) -> ShResult<String> {
  let unescaped = unescape_str(raw);
  let expanded = expand_raw(&mut unescaped.cursor())?;

  let mut result: Vec<u8> = Vec::new();
  let mut in_quote = false;
  let mut cursor = expanded.cursor();

  match_loop!(cursor.next() => unit, {
    Unit::Mark(Marker::Quote(_)) => {
      in_quote = !in_quote;
    }
    Unit::Mark(Marker::Escape) => {
      if let Some(next) = cursor.next_byte() {
        // Backslash-escaped glob meta-chars must remain literal in the resulting
        // pattern, otherwise glob_to_regex would treat them as wildcards.
        if matches!(next, b'*' | b'?' | b'[' | b']' | b'\\') {
          result.push(b'\\');
        }
        result.push(next);
      }
    }
    Unit::Byte(b @ (b'*' | b'?' | b'[' | b']' | b'\\')) if in_quote => {
      result.push(b'\\');
      result.push(b);
    }
    Unit::Byte(b) => result.push(b),
    Unit::Mark(_) => {}
  });

  Ok(String::from_utf8_lossy(&result).into_owned())
}

pub fn is_var_name_ch(ch: char) -> bool {
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
