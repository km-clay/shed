use std::iter::Peekable;
use std::str::Chars;

use crate::expand::util::is_var_name_ch;
use crate::match_loop;
use crate::readline::markers;
use crate::state::read_vars;

/// Strip ESCAPE markers from a string, leaving the characters they protect intact.
pub(super) fn strip_escape_markers(s: &str) -> String {
  s.replace(markers::ESCAPE, "")
}

const SPECIAL_CHARS: &str = "#$^*()=|{}[]`<>?~;& '\"";

/// Processes strings into intermediate representations that are more readable
/// by the program.
///
/// Clean up a single layer of escape characters, and then replace control
/// characters like '$' with a non-character unicode representation that is
/// unmistakable by the rest of the code
pub fn unescape_str(raw: &str) -> String {
  let mut chars = raw.chars().peekable();
  let mut result = String::new();
  let mut last_was_word_break = false;
  let word_breaks =
    read_vars(|v| v.try_get_var("COMP_WORDBREAKS")).unwrap_or("\"'><=;|&(: ".into());
  let ifs = read_vars(|v| v.try_get_var("IFS")).unwrap_or(" \t\n".into());
  let word_breaks = format!("{word_breaks}{ifs}");
  let mut first_char = true;

  while let Some(ch) = chars.next() {
    match ch {
      '~' if last_was_word_break || first_char => result.push(markers::TILDE_SUB),
      '\\' => {
        if let Some(next_ch) = chars.next() {
          result.push(markers::ESCAPE);
          result.push(next_ch)
        }
      }
      '(' => read_subsh(&mut chars, &mut result),
      '"' => read_dub_quote(&mut chars, &mut result),
      '\'' => read_sng_quote(&mut chars, &mut result),
      '`' => read_backtick(&mut chars, &mut result),
      '<' if chars.peek() == Some(&'(') => read_proc_sub_in(&mut chars, &mut result),
      '>' if chars.peek() == Some(&'(') => read_proc_sub_out(&mut chars, &mut result),
      '$' if chars.peek() == Some(&'\'') => {
        chars.next();
        // read_dollar_quote omits the markers so that it is also compatible with double quoted strings
        // so we push them explicitly here
        result.push(markers::SNG_QUOTE);
        read_dollar_quote(&mut chars, &mut result);
        result.push(markers::SNG_QUOTE);
      }
      '$' => {
        read_varsub(&mut chars, &mut result);
      }
      _ => result.push(ch),
    }
    last_was_word_break = word_breaks.contains(ch);
    first_char = false;
  }

  result
}

fn read_varsub(chars: &mut Peekable<Chars>, result: &mut String) -> bool {
  if chars
    .peek()
    .is_none_or(|ch| *ch != '$' && *ch != '(' && *ch != '{' && !is_var_name_ch(ch))
  {
    chars.next();
    result.push('$');
  } else {
    result.push(markers::VAR_SUB);
    if chars.peek().is_some_and(|ch| *ch == '$') {
      chars.next();
      result.push('$');
      return false;
    }
  }
  true
}

fn read_subsh(chars: &mut Peekable<Chars>, result: &mut String) {
  result.push(markers::SUBSH);
  let mut paren_count = 1;
  match_loop!(chars.next() => subsh_ch, {
    '\\' => {
      result.push(subsh_ch);
      if let Some(next_ch) = chars.next() {
        result.push(next_ch)
      }
    }
    '\'' => {
      result.push(subsh_ch);
      match_loop!(chars.next() => q_ch, {
        '\\' => {
          result.push(q_ch);
          if let Some(next_ch) = chars.next() {
            result.push(next_ch)
          }
        }
        '\'' => {
          result.push(q_ch);
          break;
        }
        _ => result.push(q_ch),
      });
    }
    '$' if chars.peek() == Some(&'\'') => {
      result.push(subsh_ch);
    }
    '$' if chars.peek() != Some(&'(') => {
      read_varsub(chars, result);
    }
    '(' => {
      paren_count += 1;
      result.push(subsh_ch)
    }
    ')' => {
      paren_count -= 1;
      if paren_count == 0 {
        result.push(markers::SUBSH);
        break;
      } else {
        result.push(subsh_ch)
      }
    }
    _ => result.push(subsh_ch),
  });
}

fn read_sng_quote(chars: &mut Peekable<Chars>, result: &mut String) {
  result.push(markers::SNG_QUOTE);
  match_loop!(chars.next() => q_ch, {
    '\\' => match chars.peek() {
      Some(&'\\') | Some(&'\'') => {
        let ch = chars.next().unwrap();
        result.push(ch);
      }
      _ => result.push(q_ch),
    },
    '\'' => {
      result.push(markers::SNG_QUOTE);
      break;
    }
    _ => result.push(q_ch),
  });
}

fn read_dub_quote(chars: &mut Peekable<Chars>, result: &mut String) {
  result.push(markers::DUB_QUOTE);
  match_loop!(chars.next() => q_ch, {
    '\\' => {
      if let Some(next_ch) = chars.next() {
        match next_ch {
          '"' | '\\' | '`' | '$' | '!' => {
            // discard the backslash
          }
          _ => {
            result.push(q_ch);
          }
        }
        result.push(next_ch);
      }
    }
    '$' if chars.peek() == Some(&'\'') => {
      chars.next();
      read_dollar_quote(chars, result);
    }
    '$' => {
      if read_varsub(chars, result) && chars.peek() == Some(&'(') {
        chars.next();
        read_subsh(chars, result);
      }
    }
    '`' => read_backtick(chars, result),
    '"' => {
      result.push(markers::DUB_QUOTE);
      break;
    }
    _ => result.push(q_ch),
  });
}

fn read_dollar_quote(chars: &mut Peekable<Chars>, result: &mut String) {
  match_loop!(chars.next() => q_ch, {
    '\'' => {
      break;
    }
    '\\' => {
      let Some(esc) = chars.next() else { continue };
      match esc {
        'n' => result.push('\n'),
        't' => result.push('\t'),
        'r' => result.push('\r'),
        '\'' => result.push('\''),
        '\\' => result.push('\\'),
        'a' => result.push('\x07'),
        'b' => result.push('\x08'),
        'e' | 'E' => result.push('\x1b'),
        'v' => result.push('\x0b'),
        'x' => read_hex(chars, result),
        'o' => read_octal(chars, result),
        _ => result.push(esc),
      }
    }
    _ => result.push(q_ch),
  });
}

fn read_octal(chars: &mut Peekable<Chars>, result: &mut String) {
  let mut oct = String::new();
  for _ in 0..3 {
    if let Some(o) = chars.peek() {
      if o.is_digit(8) {
        oct.push(*o);
        chars.next();
      } else {
        break;
      }
    } else {
      break;
    }
  }
  if let Ok(byte) = u8::from_str_radix(&oct, 8) {
    result.push(byte as char);
  } else {
    result.push_str(&format!("\\o{oct}"));
  }
}

fn read_hex(chars: &mut Peekable<Chars>, result: &mut String) {
  let mut hex = String::new();
  if let Some(h1) = chars.next() {
    hex.push(h1);
  } else {
    result.push_str("\\x");
    return;
  }
  if let Some(h2) = chars.next() {
    hex.push(h2);
  } else {
    result.push_str(&format!("\\x{hex}"));
    return;
  }
  if let Ok(byte) = u8::from_str_radix(&hex, 16) {
    result.push(byte as char);
  } else {
    result.push_str(&format!("\\x{hex}"));
  }
}

fn read_proc_sub_in(chars: &mut Peekable<Chars>, result: &mut String) {
  read_proc_sub(chars, result, false);
}

fn read_proc_sub_out(chars: &mut Peekable<Chars>, result: &mut String) {
  read_proc_sub(chars, result, true);
}

fn read_proc_sub(chars: &mut Peekable<Chars>, result: &mut String, input: bool) {
  let marker = if input {
    markers::PROC_SUB_IN
  } else {
    markers::PROC_SUB_OUT
  };
  chars.next();
  let mut paren_count = 1;
  result.push(marker);
  match_loop!(chars.next() => subsh_ch, {
    '\\' => {
      result.push(subsh_ch);
      if let Some(next_ch) = chars.next() {
        result.push(next_ch)
      }
    }
    '$' if chars.peek() == Some(&'\'') => {
      result.push(subsh_ch);
    }
    '(' => {
      result.push(subsh_ch);
      paren_count += 1;
    }
    ')' => {
      paren_count -= 1;
      if paren_count <= 0 {
        result.push(marker);
        break;
      } else {
        result.push(subsh_ch);
      }
    }
    _ => result.push(subsh_ch),
  });
}

fn read_backtick(chars: &mut Peekable<Chars>, result: &mut String) {
  result.push(markers::VAR_SUB);
  result.push(markers::SUBSH);
  match_loop!(chars.next() => bt_ch, {
    '\\' => {
      result.push(bt_ch);
      if let Some(next_ch) = chars.next() {
        result.push(next_ch);
      }
    }
    // fun fact: this one match arm allows us to parse backtick statements nested in regular command subs inside of other backtick statements.
    // Not even zsh's parser handles this case
    '$' if chars.peek() == Some(&'(') => {
      chars.next();
      result.push_str("$(");
      let mut paren_count = 1;
      match_loop!(chars.next() => subsh_ch, {
        '\\' => {
          result.push(subsh_ch);
          if let Some(next_ch) = chars.next() {
            result.push(next_ch)
          }
        }
        '(' => {
          paren_count += 1;
          result.push(subsh_ch);
        }
        ')' => {
          paren_count -= 1;
          result.push(subsh_ch);
          if paren_count == 0 {
            break;
          }
        }
        _ => result.push(subsh_ch),
      });
    }
    '`' => {
      result.push(markers::SUBSH);
      log::debug!("Finished reading backtick: {result}");
      break;
    }
    _ => result.push(bt_ch),
  });
}

/// Like unescape_str but for heredoc bodies. Only processes:
/// - $var / ${var} / $(cmd) substitution markers
/// - Backslash escapes (only before $, `, \, and newline)
///
/// Everything else (quotes, tildes, globs, process subs, etc.) is literal.
pub fn unescape_heredoc(raw: &str) -> String {
  let mut chars = raw.chars().peekable();
  let mut result = String::new();

  match_loop!(chars.next() => ch, {
    '\\' => {
      match chars.peek() {
        Some('$') | Some('`') | Some('\\') | Some('\n') => {
          let next_ch = chars.next().unwrap();
          if next_ch == '\n' {
            // line continuation - discard both backslash and newline
            continue;
          }
          result.push(markers::ESCAPE);
          result.push(next_ch);
        }
        _ => {
          // backslash is literal
          result.push('\\');
        }
      }
    }
    '$' if chars.peek() == Some(&'(') => {
      result.push(markers::VAR_SUB);
      chars.next(); // consume '('
      read_subsh(&mut chars, &mut result);
    }
    '$' => {
      read_varsub(&mut chars, &mut result);
    }
    '`' => {
      read_backtick(&mut chars, &mut result);
    }
    _ => result.push(ch),
  });

  result
}

/// Opposite of unescape_str - escapes a string to be executed as literal text
/// Used for completion results, and glob filename matches.
pub fn escape_str(raw: &str, use_marker: bool) -> String {
  let mut result = String::new();
  let mut chars = raw.chars();
  let mut is_first = true;
  let esc_ch = if use_marker { markers::ESCAPE } else { '\\' };

  while let Some(ch) = chars.next() {
    match ch {
      '\'' | '"' | '\\' | '|' | '&' | ';' | '(' | ')' | '<' | '>' | '$' | '*' | '!' | '`' | '{'
      | '?' | '[' | '#' | ' ' | '\t' | '\n' => {
        if ch == '$' && is_first {
          // TODO: Find a less hacky way to prevent completed variables from being escaped
          result.push('$');
          is_first = false;
          continue;
        }
        result.push(esc_ch);
        result.push(ch);
      }
      '~' if result.is_empty() => {
        result.push(esc_ch);
        result.push(ch);
      }
      _ => {
        result.push(ch);
      }
    }
    is_first = false;
  }

  result
}

pub fn unescape_math(raw: &str) -> String {
  let mut chars = raw.chars().peekable();
  let mut result = String::new();

  match_loop!(chars.next() => ch, {
    '\\' => {
      if let Some(next_ch) = chars.next() {
        result.push(next_ch)
      }
    }
    '$' => {
      result.push(markers::VAR_SUB);
      if chars.peek() == Some(&'(') {
        result.push(markers::SUBSH);
        chars.next();
        let mut paren_count = 1;
        match_loop!(chars.next() => subsh_ch, {
          '\\' => {
            result.push(subsh_ch);
            if let Some(next_ch) = chars.next() {
              result.push(next_ch)
            }
          }
          '$' if chars.peek() != Some(&'(') => result.push(markers::VAR_SUB),
          '(' => {
            paren_count += 1;
            result.push(subsh_ch)
          }
          ')' => {
            paren_count -= 1;
            if paren_count == 0 {
              result.push(markers::SUBSH);
              break;
            } else {
              result.push(subsh_ch)
            }
          }
          _ => result.push(subsh_ch),
        });
      }
    }
    _ => result.push(ch),
  });
  result
}

/// Escapes a string for displaying as a var value
pub fn as_var_val_display(s: &str) -> String {
  let has_control = s.chars().any(|c| c.is_ascii_control());
  let has_special = s.chars().any(|c| SPECIAL_CHARS.contains(c));

  if has_control {
    // $'...' ANSI-C quoting: backslashes and all special chars must be escaped
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
      match ch {
        '\\' => result.push_str("\\\\"),
        '\'' => result.push_str("\\'"),
        '\n' => result.push_str("\\n"),
        '\r' => result.push_str("\\r"),
        '\t' => result.push_str("\\t"),
        '\x07' => result.push_str("\\a"),
        '\x08' => result.push_str("\\b"),
        '\x0B' => result.push_str("\\v"),
        '\x0C' => result.push_str("\\f"),
        c if c.is_ascii_control() => result.push_str(&format!("\\x{:02x}", c as u8)),
        c => result.push(c),
      }
    }
    format!("$'{result}'")
  } else if has_special {
    // Single quotes: only ' needs escaping, and \ only needs escaping when
    // it precedes ' (to avoid \' being misread as an escaped quote)
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
      match ch {
        '\\' if chars.peek() == Some(&'\'') => result.push_str("\\\\"),
        '\'' => result.push_str("\\'"),
        c => result.push(c),
      }
    }
    format!("'{result}'")
  } else {
    s.to_string()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // ===================== unescape_str =====================

  #[test]
  fn unescape_backslash() {
    let result = unescape_str("hello\\nworld");
    let expected = format!("hello{}nworld", markers::ESCAPE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_tilde_at_start() {
    let result = unescape_str("~/foo");
    assert!(result.starts_with(markers::TILDE_SUB));
    assert!(result.ends_with("/foo"));
  }

  #[test]
  fn unescape_tilde_not_at_start() {
    let result = unescape_str("a~b");
    assert!(!result.contains(markers::TILDE_SUB));
    assert!(result.contains('~'));
  }

  #[test]
  fn unescape_dollar_becomes_var_sub() {
    let result = unescape_str("$foo");
    assert!(result.starts_with(markers::VAR_SUB));
    assert!(result.ends_with("foo"));
  }

  #[test]
  fn unescape_single_quotes() {
    let result = unescape_str("'hello'");
    let expected = format!("{}hello{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_double_quotes() {
    let result = unescape_str("\"hello\"");
    let expected = format!("{}hello{}", markers::DUB_QUOTE, markers::DUB_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_newline() {
    let result = unescape_str("$'\\n'");
    let expected = format!("{}\n{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_tab() {
    let result = unescape_str("$'\\t'");
    let expected = format!("{}\t{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_escape() {
    let result = unescape_str("$'\\e'");
    let expected = format!("{}\x1b{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_hex() {
    let result = unescape_str("$'\\x41'");
    let expected = format!("{}A{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_backslash() {
    let result = unescape_str("$'\\\\'");
    let expected = format!("{}\\{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  // ===================== as_var_val_display =====================

  #[test]
  fn display_simple_value_unquoted() {
    assert_eq!(as_var_val_display("hello"), "hello");
  }

  #[test]
  fn display_value_with_spaces_single_quoted() {
    assert_eq!(as_var_val_display("hello world"), "'hello world'");
  }

  #[test]
  fn display_backslash_no_escaping_in_single_quote_context() {
    // backslash not before ' — should not be doubled
    assert_eq!(as_var_val_display("\\@prompt "), "'\\@prompt '");
  }

  #[test]
  fn display_backslash_before_quote_escaped() {
    // backslash before ' must be escaped to avoid \' being read as escaped quote
    assert_eq!(as_var_val_display("bar\\' biz"), "'bar\\\\\\' biz'");
  }

  #[test]
  fn display_single_quote_escaped() {
    assert_eq!(as_var_val_display("it's"), "'it\\'s'");
  }

  #[test]
  fn display_control_char_uses_ansi_c_quoting() {
    assert_eq!(as_var_val_display("foo\nbar"), "$'foo\\nbar'");
  }

  #[test]
  fn display_backslash_escaped_in_ansi_c_context() {
    assert_eq!(as_var_val_display("foo\\\nbar"), "$'foo\\\\\\nbar'");
  }

  #[test]
  fn display_tab_uses_ansi_c_quoting() {
    assert_eq!(as_var_val_display("foo\tbar"), "$'foo\\tbar'");
  }

  #[test]
  fn display_special_chars_single_quoted() {
    assert_eq!(as_var_val_display("$VAR"), "'$VAR'");
    assert_eq!(as_var_val_display("foo|bar"), "'foo|bar'");
    assert_eq!(as_var_val_display("foo&bar"), "'foo&bar'");
  }

  #[test]
  fn display_empty_string() {
    assert_eq!(as_var_val_display(""), "");
  }
}
