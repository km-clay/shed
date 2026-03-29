use std::iter::Peekable;
use std::str::Chars;

use scopeguard::defer;

use crate::expand::util::is_var_name_ch;
use crate::parse::lex::is_hard_sep;
use crate::prelude::*;
use crate::readline::markers;

/// Strip ESCAPE markers from a string, leaving the characters they protect intact.
pub(super) fn strip_escape_markers(s: &str) -> String {
  s.replace(markers::ESCAPE, "")
}

/// Processes strings into intermediate representations that are more readable
/// by the program
///
/// Clean up a single layer of escape characters, and then replace control
/// characters like '$' with a non-character unicode representation that is
/// unmistakable by the rest of the code
pub fn unescape_str(raw: &str) -> String {
  let mut chars = raw.chars().peekable();
  let mut result = String::new();
  let mut first_char = true;

  while let Some(ch) = chars.next() {
    match ch {
      '~' if first_char => result.push(markers::TILDE_SUB),
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
      '$' => { read_varsub(&mut chars, &mut result); },
      _ => result.push(ch),
    }
    first_char = false;
  }

  result
}

fn read_varsub(chars: &mut Peekable<Chars>, result: &mut String) -> bool {
	if chars.peek().is_none_or(|ch| *ch != '$' && *ch != '(' && *ch != '{' && !is_var_name_ch(ch)) {
		chars.next();
		result.push('$');
	} else {
		result.push(markers::VAR_SUB);
		if chars.peek().is_some_and(|ch| *ch == '$') {
			chars.next();
			result.push('$');
			return false
		}
	}
	true
}

fn read_subsh(chars: &mut Peekable<Chars>, result: &mut String) {
	result.push(markers::SUBSH);
	let mut paren_count = 1;
	while let Some(subsh_ch) = chars.next() {
		match subsh_ch {
			'\\' => {
				result.push(subsh_ch);
				if let Some(next_ch) = chars.next() {
					result.push(next_ch)
				}
			}
			'$' if chars.peek() == Some(&'\'') => {
				result.push(subsh_ch);
			}
			'$' if chars.peek() != Some(&'(') => { read_varsub(chars, result); },
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
		}
	}
}

fn read_sng_quote(chars: &mut Peekable<Chars>, result: &mut String) {
	result.push(markers::SNG_QUOTE);
	while let Some(q_ch) = chars.next() {
		match q_ch {
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
		}
	}
}

fn read_dub_quote(chars: &mut Peekable<Chars>, result: &mut String) {
	result.push(markers::DUB_QUOTE);
	while let Some(q_ch) = chars.next() {
		match q_ch {
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
				if read_varsub(chars, result)
				&& chars.peek() == Some(&'(') {
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
		}
	}
}

fn read_dollar_quote(chars: &mut Peekable<Chars>, result: &mut String) {
	while let Some(q_ch) = chars.next() {
		match q_ch {
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
		}
	}
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
	while let Some(subsh_ch) = chars.next() {
		match subsh_ch {
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
		}
	}
}

fn read_backtick(chars: &mut Peekable<Chars>, result: &mut String) {
	result.push(markers::VAR_SUB);
	result.push(markers::SUBSH);
	while let Some(bt_ch) = chars.next() {
		match bt_ch {
			'\\' => {
				result.push(bt_ch);
				if let Some(next_ch) = chars.next() {
					result.push(next_ch);
				}
			}
			'$' if chars.peek() == Some(&'\'') => {
				result.push(bt_ch);
			}
			'`' => {
				result.push(markers::SUBSH);
				break;
			}
			_ => result.push(bt_ch),
		}
	}
}

/// Like unescape_str but for heredoc bodies. Only processes:
/// - $var / ${var} / $(cmd) substitution markers
/// - Backslash escapes (only before $, `, \, and newline)
///
/// Everything else (quotes, tildes, globs, process subs, etc.) is literal.
pub fn unescape_heredoc(raw: &str) -> String {
  let mut chars = raw.chars().peekable();
  let mut result = String::new();

  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        match chars.peek() {
          Some('$') | Some('`') | Some('\\') | Some('\n') => {
            let next_ch = chars.next().unwrap();
            if next_ch == '\n' {
              // line continuation — discard both backslash and newline
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
    }
  }

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

  while let Some(ch) = chars.next() {
    match ch {
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
          while let Some(subsh_ch) = chars.next() {
            match subsh_ch {
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
            }
          }
        }
      }
      _ => result.push(ch),
    }
  }
  result
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
}
