use crate::expand::expand_raw;
use crate::libsh::error::ShResult;
use crate::parse::lex::QuoteState;

/// Check if a string contains valid brace expansion patterns.
/// Returns true if there's a valid {a,b} or {1..5} pattern at the outermost
/// level.
fn has_braces(s: &str) -> bool {
  let s = expand_raw(&mut s.chars().peekable()).unwrap_or(s.to_string());
  let mut chars = s.chars().peekable();
  let mut depth = 0;
  let mut found_open = false;
  let mut has_comma = false;
  let mut has_range = false;
  let mut qt_state = QuoteState::default();

  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        chars.next();
      } // skip escaped char
      '\'' => qt_state.toggle_single(),
      '"' => qt_state.toggle_double(),
      '{' if qt_state.outside() => {
        if depth == 0 {
          found_open = true;
          has_comma = false;
          has_range = false;
        }
        depth += 1;
      }
      '}' if qt_state.outside() && depth > 0 => {
        depth -= 1;
        if depth == 0 && found_open && (has_comma || has_range) {
          return true;
        }
      }
      ',' if qt_state.outside() && depth == 1 => {
        has_comma = true;
      }
      '.' if qt_state.outside() && depth == 1 => {
        if chars.peek() == Some(&'.') {
          chars.next();
          has_range = true;
        }
      }
      _ => {}
    }
  }
  false
}

/// Expand braces in a string, zsh-style: one level per call, loop until  done.
/// Returns a Vec of expanded strings.
pub(super) fn expand_braces_full(input: &str) -> ShResult<Vec<String>> {
  let mut results = vec![input.to_string()];

  // Keep expanding until no results contain braces
  loop {
    let mut any_expanded = false;
    let mut new_results = Vec::new();

    for word in results {
      if has_braces(&word) {
        any_expanded = true;
        let expanded = expand_one_brace(&word)?;
        new_results.extend(expanded);
      } else {
        new_results.push(word);
      }
    }

    results = new_results;
    if !any_expanded {
      break;
    }
  }

  Ok(results)
}

/// Expand the first (outermost) brace expression in a word.
/// "pre{a,b}post" -> ["preapost", "prebpost"]
/// "pre{1..3}post" -> ["pre1post", "pre2post", "pre3post"]
fn expand_one_brace(word: &str) -> ShResult<Vec<String>> {
  let (prefix, inner, suffix) = match get_brace_parts(word) {
    Some(parts) => parts,
    None => return Ok(vec![word.to_string()]), // No valid braces
  };

  // Split the inner content on top-level commas, or expand as range
  let parts = split_brace_inner(&inner);

  // If we got back a single part with no expansion, treat as literal
  if parts.len() == 1 && parts[0] == inner {
    // Check if it's a range
    if let Some(range_parts) = try_expand_range(&inner) {
      return Ok(
        range_parts
          .into_iter()
          .map(|p| format!("{}{}{}", prefix, p, suffix))
          .collect(),
      );
    }
    // Not a valid brace expression, return as-is with literal braces
    return Ok(vec![format!("{}{{{}}}{}", prefix, inner, suffix)]);
  }

  Ok(
    parts
      .into_iter()
      .map(|p| format!("{}{}{}", prefix, p, suffix))
      .collect(),
  )
}

/// Extract prefix, inner, and suffix from a brace expression.
/// "pre{a,b}post" -> Some(("pre", "a,b", "post"))
fn get_brace_parts(word: &str) -> Option<(String, String, String)> {
  let mut chars = word.chars().peekable();
  let mut prefix = String::new();
  let mut qt_state = QuoteState::default();

  // Find the opening brace
  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        prefix.push(ch);
        if let Some(next) = chars.next() {
          prefix.push(next);
        }
      }
      '\'' => {
        qt_state.toggle_single();
        prefix.push(ch);
      }
      '"' => {
        qt_state.toggle_double();
        prefix.push(ch);
      }
      '{' if qt_state.outside() => {
        break;
      }
      _ => prefix.push(ch),
    }
  }

  // Find matching closing brace
  let mut depth = 1;
  let mut inner = String::new();
  qt_state = QuoteState::default();

  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        inner.push(ch);
        if let Some(next) = chars.next() {
          inner.push(next);
        }
      }
      '\'' => {
        qt_state.toggle_single();
        inner.push(ch);
      }
      '"' => {
        qt_state.toggle_double();
        inner.push(ch);
      }
      '{' if qt_state.outside() => {
        depth += 1;
        inner.push(ch);
      }
      '}' if qt_state.outside() => {
        depth -= 1;
        if depth == 0 {
          break;
        }
        inner.push(ch);
      }
      _ => inner.push(ch),
    }
  }

  if depth != 0 {
    return None; // Unbalanced braces
  }

  // Collect suffix
  let suffix: String = chars.collect();

  Some((prefix, inner, suffix))
}

/// Split brace inner content on top-level commas.
/// "a,b,c" -> ["a", "b", "c"]
/// "a,{b,c},d" -> ["a", "{b,c}", "d"]
fn split_brace_inner(inner: &str) -> Vec<String> {
  let mut parts = Vec::new();
  let mut current = String::new();
  let mut chars = inner.chars().peekable();
  let mut depth = 0;
  let mut qt_state = QuoteState::default();

  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        current.push(ch);
        if let Some(next) = chars.next() {
          current.push(next);
        }
      }
      '\'' => {
        qt_state.toggle_single();
        current.push(ch);
      }
      '"' => {
        qt_state.toggle_double();
        current.push(ch);
      }
      '{' if qt_state.outside() => {
        depth += 1;
        current.push(ch);
      }
      '}' if qt_state.outside() => {
        depth -= 1;
        current.push(ch);
      }
      ',' if qt_state.outside() && depth == 0 => {
        parts.push(std::mem::take(&mut current));
      }
      _ => current.push(ch),
    }
  }

  parts.push(current);
  parts
}

/// Try to expand a range like "1..5" or "a..z" or "1..10..2"
fn try_expand_range(inner: &str) -> Option<Vec<String>> {
  // Look for ".." pattern
  let parts: Vec<&str> = inner.split("..").collect();

  match parts.len() {
    2 => {
      let start = parts[0];
      let end = parts[1];
      expand_range(start, end, 1)
    }
    3 => {
      let start = parts[0];
      let end = parts[1];
      let step: i32 = parts[2].parse().ok()?;
      if step == 0 {
        return None;
      }
      expand_range(start, end, step.unsigned_abs() as usize)
    }
    _ => None,
  }
}

fn expand_range(start: &str, end: &str, step: usize) -> Option<Vec<String>> {
  // Try character range first
  if is_alpha_range_bound(start) && is_alpha_range_bound(end) {
    let start_char = start.chars().next()? as u8;
    let end_char = end.chars().next()? as u8;
    let reverse = end_char < start_char;

    let (lo, hi) = if reverse {
      (end_char, start_char)
    } else {
      (start_char, end_char)
    };

    let chars: Vec<String> = (lo..=hi)
      .step_by(step)
      .map(|c| (c as char).to_string())
      .collect();

    return Some(if reverse {
      chars.into_iter().rev().collect()
    } else {
      chars
    });
  }

  // Try numeric range
  if is_numeric_range_bound(start) && is_numeric_range_bound(end) {
    let start_num: i32 = start.parse().ok()?;
    let end_num: i32 = end.parse().ok()?;
    let reverse = end_num < start_num;

    // Handle zero-padding
    let pad_width = start.len().max(end.len());
    let needs_padding = start.starts_with('0') || end.starts_with('0');

    let (lo, hi) = if reverse {
      (end_num, start_num)
    } else {
      (start_num, end_num)
    };

    let nums: Vec<String> = (lo..=hi)
      .step_by(step)
      .map(|n| {
        if needs_padding {
          format!("{:0>width$}", n, width = pad_width)
        } else {
          n.to_string()
        }
      })
      .collect();

    return Some(if reverse {
      nums.into_iter().rev().collect()
    } else {
      nums
    });
  }

  None
}

fn is_alpha_range_bound(word: &str) -> bool {
  word.len() == 1 && word.chars().all(|c| c.is_ascii_alphabetic())
}

fn is_numeric_range_bound(word: &str) -> bool {
  !word.is_empty() && word.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
  use super::*;

  // ===================== has_braces =====================

  #[test]
  fn has_braces_simple_comma() {
    assert!(has_braces("{a,b,c}"));
  }

  #[test]
  fn has_braces_range() {
    assert!(has_braces("{1..5}"));
  }

  #[test]
  fn has_braces_no_braces() {
    assert!(!has_braces("hello"));
  }

  #[test]
  fn has_braces_single_item() {
    assert!(!has_braces("{hello}"));
  }

  #[test]
  fn has_braces_with_prefix_suffix() {
    assert!(has_braces("pre{a,b}post"));
  }

  #[test]
  fn has_braces_nested() {
    assert!(has_braces("{a,{b,c}}"));
  }

  #[test]
  fn has_braces_quoted_single() {
    assert!(!has_braces("'{a,b}'"));
  }

  #[test]
  fn has_braces_quoted_double() {
    assert!(!has_braces("\"{a,b}\""));
  }

  #[test]
  fn has_braces_escaped() {
    assert!(!has_braces("\\{a,b\\}"));
  }

  // ===================== split_brace_inner =====================

  #[test]
  fn split_inner_simple() {
    assert_eq!(split_brace_inner("a,b,c"), vec!["a", "b", "c"]);
  }

  #[test]
  fn split_inner_nested_braces() {
    assert_eq!(split_brace_inner("a,{b,c},d"), vec!["a", "{b,c}", "d"]);
  }

  #[test]
  fn split_inner_no_comma() {
    assert_eq!(split_brace_inner("abc"), vec!["abc"]);
  }

  #[test]
  fn split_inner_empty_parts() {
    assert_eq!(split_brace_inner(",a,"), vec!["", "a", ""]);
  }

  // ===================== try_expand_range / expand_range =====================

  #[test]
  fn range_numeric() {
    assert_eq!(
      try_expand_range("1..5").unwrap(),
      vec!["1", "2", "3", "4", "5"]
    );
  }

  #[test]
  fn range_alpha() {
    assert_eq!(
      try_expand_range("a..e").unwrap(),
      vec!["a", "b", "c", "d", "e"]
    );
  }

  #[test]
  fn range_with_step() {
    assert_eq!(
      try_expand_range("1..10..2").unwrap(),
      vec!["1", "3", "5", "7", "9"]
    );
  }

  #[test]
  fn range_reverse_numeric() {
    assert_eq!(
      try_expand_range("5..1").unwrap(),
      vec!["5", "4", "3", "2", "1"]
    );
  }

  #[test]
  fn range_reverse_alpha() {
    assert_eq!(
      try_expand_range("e..a").unwrap(),
      vec!["e", "d", "c", "b", "a"]
    );
  }

  #[test]
  fn range_zero_padded() {
    assert_eq!(
      try_expand_range("01..05").unwrap(),
      vec!["01", "02", "03", "04", "05"]
    );
  }

  #[test]
  fn range_invalid() {
    assert!(try_expand_range("abc").is_none());
  }

  #[test]
  fn range_zero_step() {
    assert!(try_expand_range("1..5..0").is_none());
  }

  #[test]
  fn range_single_char() {
    assert_eq!(expand_range("a", "a", 1).unwrap(), vec!["a"]);
  }

  // ===================== expand_braces_full =====================

  #[test]
  fn braces_simple_list() {
    assert_eq!(expand_braces_full("{a,b,c}").unwrap(), vec!["a", "b", "c"]);
  }

  #[test]
  fn braces_with_prefix_suffix() {
    assert_eq!(
      expand_braces_full("pre{a,b}post").unwrap(),
      vec!["preapost", "prebpost"]
    );
  }

  #[test]
  fn braces_nested() {
    assert_eq!(
      expand_braces_full("{a,{b,c}}").unwrap(),
      vec!["a", "b", "c"]
    );
  }

  #[test]
  fn braces_numeric_range() {
    assert_eq!(
      expand_braces_full("{1..5}").unwrap(),
      vec!["1", "2", "3", "4", "5"]
    );
  }

  #[test]
  fn braces_range_with_step() {
    assert_eq!(
      expand_braces_full("{1..10..2}").unwrap(),
      vec!["1", "3", "5", "7", "9"]
    );
  }

  #[test]
  fn braces_alpha_range() {
    assert_eq!(
      expand_braces_full("{a..f}").unwrap(),
      vec!["a", "b", "c", "d", "e", "f"]
    );
  }

  #[test]
  fn braces_reverse_range() {
    assert_eq!(
      expand_braces_full("{5..1}").unwrap(),
      vec!["5", "4", "3", "2", "1"]
    );
  }

  #[test]
  fn braces_reverse_alpha() {
    assert_eq!(
      expand_braces_full("{z..v}").unwrap(),
      vec!["z", "y", "x", "w", "v"]
    );
  }

  #[test]
  fn braces_zero_padded() {
    assert_eq!(
      expand_braces_full("{01..05}").unwrap(),
      vec!["01", "02", "03", "04", "05"]
    );
  }

  #[test]
  fn braces_no_expansion() {
    assert_eq!(expand_braces_full("hello").unwrap(), vec!["hello"]);
  }

  #[test]
  fn braces_multiple_groups() {
    assert_eq!(
      expand_braces_full("{a,b}{1,2}").unwrap(),
      vec!["a1", "a2", "b1", "b2"]
    );
  }

  #[test]
  fn braces_empty_element() {
    let result = expand_braces_full("pre{,a}post").unwrap();
    assert_eq!(result, vec!["prepost", "preapost"]);
  }

  #[test]
  fn braces_cursed() {
    let result = expand_braces_full("foo{a,{1,2,3,{1..4},5},c}{5..1}bar").unwrap();
    assert_eq!(
      result,
      vec![
        "fooa5bar", "fooa4bar", "fooa3bar", "fooa2bar", "fooa1bar", "foo15bar", "foo14bar",
        "foo13bar", "foo12bar", "foo11bar", "foo25bar", "foo24bar", "foo23bar", "foo22bar",
        "foo21bar", "foo35bar", "foo34bar", "foo33bar", "foo32bar", "foo31bar", "foo15bar",
        "foo14bar", "foo13bar", "foo12bar", "foo11bar", "foo25bar", "foo24bar", "foo23bar",
        "foo22bar", "foo21bar", "foo35bar", "foo34bar", "foo33bar", "foo32bar", "foo31bar",
        "foo45bar", "foo44bar", "foo43bar", "foo42bar", "foo41bar", "foo55bar", "foo54bar",
        "foo53bar", "foo52bar", "foo51bar", "fooc5bar", "fooc4bar", "fooc3bar", "fooc2bar",
        "fooc1bar",
      ]
    )
  }
}
