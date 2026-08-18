use std::rc::Rc;

use regex::Regex;

use crate::{ShResult, expand::glob_to_regex, match_loop, sherr, shopt, util};

#[derive(Debug, Clone)]
pub(crate) enum Pattern {
  Any, // bare *, matches anything
  Equal(Rc<str>),
  Contains(Rc<str>),
  StartsWith(Rc<str>),
  EndsWith(Rc<str>),
  DoubleSided(Rc<str>, Rc<str>), // something like a*b
  Glob(Regex),
}

impl Pattern {
  pub fn compile(mut pattern: &str) -> Self {
    if pattern.chars().all(|c| c == '*') {
      return Self::Any;
    }

    // collapse leading and trailing stars
    while util::starts_with_unescaped(pattern, "*") && pattern.starts_with("**") {
      pattern = &pattern[1..];
    }
    while util::ends_with_unescaped(pattern, "*") && pattern.ends_with("**") {
      pattern = &pattern[..pattern.len() - 1];
    }

    // something like *foo*b[aA]r*b?z or something
    // let regex figure it out
    if util::count_unescaped(pattern, "*") > 2 || util::has_any_unescaped(pattern, &["?", "[", "{"])
    {
      return Self::Glob(glob_to_regex(pattern, true));
    }

    let strip_glob_escapes = |s: &str| -> String {
      let mut out = String::with_capacity(s.len());
      let mut chars = s.chars();
      match_loop!(chars.next() => ch, {
        '\\' => {
          if let Some(next) = chars.next() {
            out.push(next);
          }
        }
        _ => out.push(ch),
      });
      out
    };

    let left_star = util::starts_with_unescaped(pattern, "*");
    let right_star = util::ends_with_unescaped(pattern, "*");

    // The literal body sitting between the optional boundary stars.
    let body = &pattern[usize::from(left_star)..pattern.len() - usize::from(right_star)];

    if util::has_unescaped(body, "*") {
      if !left_star && !right_star && util::count_unescaped(body, "*") == 1 {
        let (star, star_len) = util::split_at_unescaped(body, "*").unwrap();
        let lhs = strip_glob_escapes(&body[..star]).into();
        let rhs = strip_glob_escapes(&body[star + star_len..]).into();
        return Self::DoubleSided(lhs, rhs);
      }
      return Self::Glob(glob_to_regex(pattern, true));
    }

    let body: Rc<str> = strip_glob_escapes(body).into();
    match (left_star, right_star) {
      (false, false) => Self::Equal(body),
      (true, false) => Self::EndsWith(body),
      (false, true) => Self::StartsWith(body),
      (true, true) => Self::Contains(body),
    }
  }
  pub fn is_match(&self, text: &str) -> bool {
    match self {
      Pattern::Any => true,
      Pattern::Equal(s) => text == &**s,
      Pattern::Contains(s) => text.contains(&**s),
      Pattern::StartsWith(s) => text.starts_with(&**s),
      Pattern::EndsWith(s) => text.ends_with(&**s),
      Pattern::Glob(g) => g.is_match(text),
      Pattern::DoubleSided(l, r) => {
        // The prefix and suffix must not overlap: `ab*bc` requires at least
        // `len("ab") + len("bc")` chars, so it can't match `abc`.
        let len_match = text.len() >= l.len() + r.len();
        let both_sides_match = text.starts_with(&**l) && text.ends_with(&**r);
        len_match && both_sides_match
      }
    }
  }
}

pub fn restore_glob_prefix(pattern: &str, mut result: String) -> String {
  if pattern.starts_with("./") && !result.starts_with("./") && !result.starts_with('/') {
    result.insert_str(0, "./");
  }
  if pattern.ends_with('/') && !result.ends_with('/') {
    result.push('/');
  }
  result
}

/// Quick structural check: only return true if the string could plausibly be a glob.
/// A lone `[` or `]` (e.g. from `[ ... ]` test command) is not a valid pattern.
pub(super) fn might_be_glob(s: &str) -> bool {
  let mut open_bracket = false;
  let mut close_bracket = false;
  for b in s.bytes() {
    match b {
      b'*' | b'?' => return true,
      b'[' => open_bracket = true,
      b']' => close_bracket = true,
      _ => {}
    }
  }
  open_bracket && close_bracket
}

pub fn expand_glob(raw: &str) -> ShResult<Vec<String>> {
  let mut words = vec![];

  if !might_be_glob(raw) || shopt!(set.noglob) {
    return Ok(vec![raw.to_string()]);
  }
  let escaped = super::escape_glob(raw);

  let final_component = raw.rsplit('/').next().unwrap_or(raw);
  let explicit_leading_dot = final_component.starts_with('.');
  let opts = glob::MatchOptions {
    require_literal_leading_dot: !(shopt!(core.dotglob) || explicit_leading_dot),
    ..Default::default()
  };

  let entries =
    glob::glob_with(&escaped, opts).map_err(|_| sherr!(ParseErr, "Invalid glob pattern"))?;
  for entry in entries {
    let entry = entry.map_err(|_| sherr!(SyntaxErr, "Invalid filename found in glob"))?;
    // Never let a pattern (e.g. `.*`) expand to the `.` or `..` directory
    // entries. `Path::file_name` returns `None` for a path whose final
    // component is `.`/`..`, which is exactly what we want to drop.
    if entry.file_name().is_none() {
      continue;
    }
    let entry_raw = entry
      .to_str()
      .ok_or_else(|| sherr!(SyntaxErr, "Non-UTF8 filename found in glob"))?;
    // The match is a real filename and becomes a final word verbatim; escaping
    // it (previously stripped again downstream) would leak backslashes.
    words.push(entry_raw.to_string());
  }
  Ok(words)
}

pub(crate) fn replace_posix_classes(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut chars = s.chars().peekable();
  let mut in_bracket = false;

  match_loop!(chars.next() => ch, {
    '\\' => {
      out.push(ch);
      if let Some(next_ch) = chars.next() {
        out.push(next_ch);
      }
    }
    '[' if !in_bracket => {
      in_bracket = true;
      out.push(ch);

      // convert '^' to '!'
      // glob crate uses '!' for negation in it's patterns
      if chars.peek() == Some(&'^') {
        chars.next();
        out.push('!');
      }
    }
    ']' if in_bracket => {
      in_bracket = false;
      out.push(ch);
    }
    '[' if in_bracket && chars.peek() == Some(&':') => {
      chars.next();
      let mut name = String::new();
      match_loop!(chars.peek() => &ch => ch, {
        ':' => {
          chars.next();
          break
        }
        _ => {
          name.push(ch);
          chars.next();
        }
      });

      if chars.peek() == Some(&']')
      && let Some(posix_chars) = posix_class_chars(&name) {
        chars.next();
        out.push_str(posix_chars);
      } else {
        out.push('[');
        out.push(':');
        out.push_str(&name);
      }

    }
    _ => out.push(ch),
  });

  out
}

fn posix_class_chars(name: &str) -> Option<&'static str> {
  match name {
    "alnum" => Some("a-zA-Z0-9"),
    "alpha" => Some("a-zA-Z"),
    "blank" => Some(" \t"),
    "cntrl" => Some("\x00-\x1F\x7F"),
    "digit" => Some("0-9"),
    "graph" => Some("!-~"),
    "lower" => Some("a-z"),
    "print" => Some(" -~"),
    "punct" => Some("!-/:-@\\[-`{-~"),
    "space" => Some(" \t\r\n\x0b\x0c"),
    "upper" => Some("A-Z"),
    "xdigit" => Some("A-Fa-f0-9"),
    _ => None,
  }
}
