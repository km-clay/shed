use std::str::FromStr;

use glob::Pattern;

use crate::expand::escape::{strip_escape_markers, unescape_str};
use crate::expand::util::glob_to_regex;
use crate::expand::var::expand_raw;
use crate::libsh::error::{ShErr, ShResult};
use crate::match_loop;
use crate::sherr;
use crate::state::{VarFlags, VarKind, VarName, read_shopts, read_vars, write_vars};

#[derive(Debug)]
pub enum ParamExp {
  Len,                               // #var_name
  ToUpperFirst,                      // ^var_name
  ToUpperAll,                        // ^^var_name
  ToLowerFirst,                      // ,var_name
  ToLowerAll,                        // ,,var_name
  DefaultUnsetOrNull(String),        // :-
  DefaultUnset(String),              // -
  SetDefaultUnsetOrNull(String),     // :=
  SetDefaultUnset(String),           // =
  AltSetNotNull(String),             // :+
  AltNotNull(String),                // +
  ErrUnsetOrNull(String),            // :?
  ErrUnset(String),                  // ?
  SliceOpen(usize),                  // :pos
  SliceClosed(usize, usize),         // :pos:len
  RemShortestPrefix(String),         // #pattern
  RemLongestPrefix(String),          // ##pattern
  RemShortestSuffix(String),         // %pattern
  RemLongestSuffix(String),          // %%pattern
  ReplaceFirstMatch(String, String), // /search/replace
  ReplaceAllMatches(String, String), // //search/replace
  ReplacePrefix(String, String),     // #search/replace
  ReplaceSuffix(String, String),     // %search/replace
  VarNamesWithPrefix(String),        // !prefix@ || !prefix*
  ExpandInnerVar(String),            // !var
}

impl FromStr for ParamExp {
  type Err = ShErr;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    use ParamExp::*;

    let parse_err = || Err(sherr!(SyntaxErr, "Invalid parameter expansion",));

    if s == "^^" {
      return Ok(ToUpperAll);
    }
    if s == "^" {
      return Ok(ToUpperFirst);
    }
    if s == ",," {
      return Ok(ToLowerAll);
    }
    if s == "," {
      return Ok(ToLowerFirst);
    }

    // Handle indirect var expansion: ${!var}
    if let Some(var) = s.strip_prefix('!') {
      if var.ends_with('*') || var.ends_with('@') {
        return Ok(VarNamesWithPrefix(var.to_string()));
      }
      return Ok(ExpandInnerVar(var.to_string()));
    }

    // Pattern removals
    if let Some(rest) = s.strip_prefix("##") {
      return Ok(RemLongestPrefix(rest.to_string()));
    } else if let Some(rest) = s.strip_prefix('#') {
      return Ok(RemShortestPrefix(rest.to_string()));
    }
    if let Some(rest) = s.strip_prefix("%%") {
      return Ok(RemLongestSuffix(rest.to_string()));
    } else if let Some(rest) = s.strip_prefix('%') {
      return Ok(RemShortestSuffix(rest.to_string()));
    }

    // Replacements
    if let Some(rest) = s.strip_prefix("//") {
      let mut parts = rest.splitn(2, '/');
      let pattern = parts.next().unwrap_or("");
      let repl = parts.next().unwrap_or("");
      return Ok(ReplaceAllMatches(pattern.to_string(), repl.to_string()));
    }
    if let Some(rest) = s.strip_prefix('/') {
      if let Some(rest) = rest.strip_prefix('%') {
        let mut parts = rest.splitn(2, '/');
        let pattern = parts.next().unwrap_or("");
        let repl = parts.next().unwrap_or("");
        return Ok(ReplaceSuffix(pattern.to_string(), repl.to_string()));
      } else if let Some(rest) = rest.strip_prefix('#') {
        let mut parts = rest.splitn(2, '/');
        let pattern = parts.next().unwrap_or("");
        let repl = parts.next().unwrap_or("");
        return Ok(ReplacePrefix(pattern.to_string(), repl.to_string()));
      } else {
        let mut parts = rest.splitn(2, '/');
        let pattern = parts.next().unwrap_or("");
        let repl = parts.next().unwrap_or("");
        return Ok(ReplaceFirstMatch(pattern.to_string(), repl.to_string()));
      }
    }

    // Fallback / assignment / alt
    if let Some(rest) = s.strip_prefix(":-") {
      return Ok(DefaultUnsetOrNull(rest.to_string()));
    } else if let Some(rest) = s.strip_prefix('-') {
      return Ok(DefaultUnset(rest.to_string()));
    } else if let Some(rest) = s.strip_prefix(":+") {
      return Ok(AltSetNotNull(rest.to_string()));
    } else if let Some(rest) = s.strip_prefix('+') {
      return Ok(AltNotNull(rest.to_string()));
    } else if let Some(rest) = s.strip_prefix(":=") {
      return Ok(SetDefaultUnsetOrNull(rest.to_string()));
    } else if let Some(rest) = s.strip_prefix('=') {
      return Ok(SetDefaultUnset(rest.to_string()));
    } else if let Some(rest) = s.strip_prefix(":?") {
      return Ok(ErrUnsetOrNull(rest.to_string()));
    } else if let Some(rest) = s.strip_prefix('?') {
      return Ok(ErrUnset(rest.to_string()));
    }

    // Substring
    if let Some((pos, len)) = parse_pos_len(s) {
      return Ok(match len {
        Some(l) => SliceClosed(pos, l),
        None => SliceOpen(pos),
      });
    }

    parse_err()
  }
}

pub fn parse_pos_len(s: &str) -> Option<(usize, Option<usize>)> {
  let raw = s.strip_prefix(':')?;
  if let Some((start, len)) = raw.split_once(':') {
    let start = expand_raw(&mut start.chars().peekable()).unwrap_or_else(|_| start.to_string());
    let len = expand_raw(&mut len.chars().peekable()).unwrap_or_else(|_| len.to_string());
    Some((start.parse::<usize>().ok()?, len.parse::<usize>().ok()))
  } else {
    let raw = expand_raw(&mut raw.chars().peekable()).unwrap_or_else(|_| raw.to_string());
    Some((raw.parse::<usize>().ok()?, None))
  }
}

pub fn perform_param_expansion(raw: &str) -> ShResult<String> {
  let mut chars = raw.chars();
  let mut var_name = String::new();
  let mut rest = String::new();
  if raw.starts_with('#') {
    let var = read_vars(|v| v.get_var_meta(raw.strip_prefix('#').unwrap()));
    return Ok(
      match var.kind() {
        VarKind::Str(_) | VarKind::Int(_) => var.to_string().len(),
        VarKind::Arr(items) => items.len(),
        VarKind::AssocArr(items) => items.len(),
      }
      .to_string(),
    );
  }

  // Scan for the variable name (may include [index]) and the operator
  let mut is_glob_index = false;
  let mut seen_bracket = false;
  match_loop!(chars.next() => ch, {
    _ if ch == '[' => {
      // Include brackets as part of the var name
      let is_first_bracket = !seen_bracket;
      seen_bracket = true;
      var_name.push(ch);
      let mut idx_content = String::new();
      let mut bracket_depth = 1;
      match_loop!(chars.next() => bc, {
        '[' => { bracket_depth += 1; var_name.push(bc); idx_content.push(bc); }
        ']' => {
          bracket_depth -= 1;
          var_name.push(bc);
          if bracket_depth == 0 {
            if is_first_bracket {
              is_glob_index = idx_content == "@" || idx_content == "*";
            }
            break;
          }
          idx_content.push(bc);
        }
        _ => { var_name.push(bc); idx_content.push(bc); }
      });
    }
    _ if is_glob_index && (ch == ':' || ch.is_ascii_digit()) => {
      // For [@] and [*], include :start:len as part of the var name
      // so VarName::parse handles it as an array slice
      var_name.push(ch);
    }
    '!' | '#' | '%' | ':' | '-' | '+' | '^' | ',' | '=' | '/' | '?' => {
      rest.push(ch);
      rest.push_str(&chars.collect::<String>());
      break;
    }
    _ => var_name.push(ch),
  });

  // Parse and expand the variable name (including any array index) before
  // entering read_vars, to avoid re-entrant borrows from index expansion
  let parsed = VarName::parse(&var_name)?;
  let get = |v: &crate::state::scopes::ScopeStack| v.resolve_var(&parsed).unwrap_or_default();
  let try_get = |v: &crate::state::scopes::ScopeStack| v.resolve_var(&parsed);

  if let Ok(expansion) = rest.parse::<ParamExp>() {
    match expansion {
      ParamExp::Len => unreachable!(),
      ParamExp::ToUpperAll => {
        let value = read_vars(get);
        Ok(value.to_uppercase())
      }
      ParamExp::ToUpperFirst => {
        let value = read_vars(get);
        let mut chars = value.chars();
        let first = chars
          .next()
          .map(|c| c.to_uppercase().to_string())
          .unwrap_or_default();
        Ok(first + chars.as_str())
      }
      ParamExp::ToLowerAll => {
        let value = read_vars(get);
        Ok(value.to_lowercase())
      }
      ParamExp::ToLowerFirst => {
        let value = read_vars(get);
        let mut chars = value.chars();
        let first = chars
          .next()
          .map(|c| c.to_lowercase().to_string())
          .unwrap_or_default();
        Ok(first + chars.as_str())
      }
      ParamExp::DefaultUnsetOrNull(default) => {
        match read_vars(try_get).filter(|v| !v.is_empty()) {
          Some(val) => Ok(val),
          None => expand_raw(&mut default.chars().peekable()),
        }
      }
      ParamExp::DefaultUnset(default) => match read_vars(try_get) {
        Some(val) => Ok(val),
        None => expand_raw(&mut default.chars().peekable()),
      },
      ParamExp::SetDefaultUnsetOrNull(default) => {
        match read_vars(try_get).filter(|v| !v.is_empty()) {
          Some(val) => Ok(val),
          None => {
            let expanded = expand_raw(&mut default.chars().peekable())?;
            write_vars(|v| v.set_var(parsed.name(), VarKind::Str(expanded.clone()), VarFlags::NONE))?;
            Ok(expanded)
          }
        }
      }
      ParamExp::SetDefaultUnset(default) => match read_vars(try_get) {
        Some(val) => Ok(val),
        None => {
          let expanded = expand_raw(&mut default.chars().peekable())?;
          write_vars(|v| v.set_var(parsed.name(), VarKind::Str(expanded.clone()), VarFlags::NONE))?;
          Ok(expanded)
        }
      },
      ParamExp::AltSetNotNull(alt) => {
        match read_vars(try_get).filter(|v| !v.is_empty()) {
          Some(_) => expand_raw(&mut alt.chars().peekable()),
          None => Ok("".into()),
        }
      }
      ParamExp::AltNotNull(alt) => match read_vars(try_get) {
        Some(_) => expand_raw(&mut alt.chars().peekable()),
        None => Ok("".into()),
      },
      ParamExp::ErrUnsetOrNull(err) => {
        match read_vars(try_get).filter(|v| !v.is_empty()) {
          Some(val) => Ok(val),
          None => {
            let expanded = expand_raw(&mut err.chars().peekable())?;
            Err(sherr!(ExecFail, "{expanded}"))
          }
        }
      }
      ParamExp::ErrUnset(err) => match read_vars(try_get) {
        Some(val) => Ok(val),
        None => {
          let expanded = expand_raw(&mut err.chars().peekable())?;
          Err(sherr!(ExecFail, "{expanded}"))
        }
      },
      ParamExp::SliceOpen(pos) => {
        let value = read_vars(get);
        if let Some(substr) = value.get(pos..) {
          Ok(substr.to_string())
        } else {
          Ok(value)
        }
      }
      ParamExp::SliceClosed(pos, len) => {
        let value = read_vars(get);
        let end = pos.saturating_add(len);
        if let Some(substr) = value.get(pos..end) {
          Ok(substr.to_string())
        } else {
          Ok(value)
        }
      }
      ParamExp::RemShortestPrefix(prefix) => {
        let value = read_vars(get);
        let unescaped = unescape_str(&prefix);
        let expanded =
          strip_escape_markers(&expand_raw(&mut unescaped.chars().peekable()).unwrap_or(prefix));
        let pattern = Pattern::new(&expanded).unwrap();
        for i in 0..=value.len() {
          let sliced = &value[..i];
          if pattern.matches(sliced) {
            return Ok(value[i..].to_string());
          }
        }
        Ok(value)
      }
      ParamExp::RemLongestPrefix(prefix) => {
        let value = read_vars(get);
        let unescaped = unescape_str(&prefix);
        let expanded =
          strip_escape_markers(&expand_raw(&mut unescaped.chars().peekable()).unwrap_or(prefix));
        let pattern = Pattern::new(&expanded).unwrap();
        for i in (0..=value.len()).rev() {
          let sliced = &value[..i];
          if pattern.matches(sliced) {
            return Ok(value[i..].to_string());
          }
        }
        Ok(value) // no match
      }
      ParamExp::RemShortestSuffix(suffix) => {
        let value = read_vars(get);
        let unescaped = unescape_str(&suffix);
        let expanded =
          strip_escape_markers(&expand_raw(&mut unescaped.chars().peekable()).unwrap_or(suffix));
        let pattern = Pattern::new(&expanded).unwrap();
        for i in (0..=value.len()).rev() {
          let sliced = &value[i..];
          if pattern.matches(sliced) {
            return Ok(value[..i].to_string());
          }
        }
        Ok(value)
      }
      ParamExp::RemLongestSuffix(suffix) => {
        let value = read_vars(get);
        let unescaped = unescape_str(&suffix);
        let expanded_suffix = strip_escape_markers(
          &expand_raw(&mut unescaped.chars().peekable()).unwrap_or(suffix.clone()),
        );
        let pattern = Pattern::new(&expanded_suffix).unwrap();
        for i in 0..=value.len() {
          let sliced = &value[i..];
          if pattern.matches(sliced) {
            return Ok(value[..i].to_string());
          }
        }
        Ok(value)
      }
      ParamExp::ReplaceFirstMatch(search, replace) => {
        let value = read_vars(get);
        let search = unescape_str(&search);
        let replace = unescape_str(&replace);
        let expanded_search =
          strip_escape_markers(&expand_raw(&mut search.chars().peekable()).unwrap_or(search));
        let expanded_replace =
          strip_escape_markers(&expand_raw(&mut replace.chars().peekable()).unwrap_or(replace));
        let regex = glob_to_regex(&expanded_search, false); // unanchored pattern

        if let Some(mat) = regex.find(&value) {
          let before = &value[..mat.start()];
          let after = &value[mat.end()..];
          let result = format!("{}{}{}", before, expanded_replace, after);
          Ok(result)
        } else {
          Ok(value)
        }
      }
      ParamExp::ReplaceAllMatches(search, replace) => {
        let value = read_vars(get);
        let search = unescape_str(&search);
        let replace = unescape_str(&replace);
        let expanded_search =
          strip_escape_markers(&expand_raw(&mut search.chars().peekable()).unwrap_or(search));
        let expanded_replace =
          strip_escape_markers(&expand_raw(&mut replace.chars().peekable()).unwrap_or(replace));
        let regex = glob_to_regex(&expanded_search, false);
        let mut result = String::new();
        let mut last_match_end = 0;

        for mat in regex.find_iter(&value) {
          result.push_str(&value[last_match_end..mat.start()]);
          result.push_str(&expanded_replace);
          last_match_end = mat.end();
        }

        // Append the rest of the string
        result.push_str(&value[last_match_end..]);
        Ok(result)
      }
      ParamExp::ReplacePrefix(search, replace) => {
        let value = read_vars(get);
        let search = unescape_str(&search);
        let replace = unescape_str(&replace);
        let expanded_search =
          strip_escape_markers(&expand_raw(&mut search.chars().peekable()).unwrap_or(search));
        let expanded_replace =
          strip_escape_markers(&expand_raw(&mut replace.chars().peekable()).unwrap_or(replace));
        let pattern = Pattern::new(&expanded_search).unwrap();
        for i in (0..=value.len()).rev() {
          let sliced = &value[..i];
          if pattern.matches(sliced) {
            return Ok(format!("{}{}", expanded_replace, &value[i..]));
          }
        }
        Ok(value)
      }
      ParamExp::ReplaceSuffix(search, replace) => {
        let value = read_vars(get);
        let search = unescape_str(&search);
        let replace = unescape_str(&replace);
        let expanded_search =
          strip_escape_markers(&expand_raw(&mut search.chars().peekable()).unwrap_or(search));
        let expanded_replace =
          strip_escape_markers(&expand_raw(&mut replace.chars().peekable()).unwrap_or(replace));
        let pattern = Pattern::new(&expanded_search).unwrap();
        for i in (0..=value.len()).rev() {
          let sliced = &value[i..];
          if pattern.matches(sliced) {
            return Ok(format!("{}{}", &value[..i], expanded_replace));
          }
        }
        Ok(value)
      }
      ParamExp::VarNamesWithPrefix(prefix) => {
        let flat = read_vars(|v| v.flatten_vars());
        let match_vars: Vec<_> = flat
          .keys()
          .filter(|var| var.starts_with(&prefix))
          .cloned()
          .collect();
        Ok(match_vars.join(" "))
      }
      ParamExp::ExpandInnerVar(inner) => {
        let inner_name = VarName::parse(&inner)?;
        let value = read_vars(|v| v.resolve_var(&inner_name).unwrap_or_default());
        Ok(read_vars(|v| v.get_var(&value)))
      }
    }
  } else {
    let var = read_vars(try_get);
    if var.is_none() && read_shopts(|o| o.set.nounset) {
      return Err(sherr!(NotFound, "Variable '{}' is not set", parsed.name()));
    }
    Ok(var.unwrap_or_default())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::state::{VarFlags, VarKind, read_vars, write_vars};
  use crate::testutil::{TestGuard, test_input};

  // ===================== ParamExp parsing =====================

  #[test]
  fn param_exp_default_unset_or_null() {
    let exp: ParamExp = ":-default".parse().unwrap();
    assert!(matches!(exp, ParamExp::DefaultUnsetOrNull(ref d) if d == "default"));
  }

  #[test]
  fn param_exp_default_unset() {
    let exp: ParamExp = "-fallback".parse().unwrap();
    assert!(matches!(exp, ParamExp::DefaultUnset(ref d) if d == "fallback"));
  }

  #[test]
  fn param_exp_set_default_unset_or_null() {
    let exp: ParamExp = ":=val".parse().unwrap();
    assert!(matches!(exp, ParamExp::SetDefaultUnsetOrNull(ref v) if v == "val"));
  }

  #[test]
  fn param_exp_set_default_unset() {
    let exp: ParamExp = "=val".parse().unwrap();
    assert!(matches!(exp, ParamExp::SetDefaultUnset(ref v) if v == "val"));
  }

  #[test]
  fn param_exp_alt_set_not_null() {
    let exp: ParamExp = ":+alt".parse().unwrap();
    assert!(matches!(exp, ParamExp::AltSetNotNull(ref a) if a == "alt"));
  }

  #[test]
  fn param_exp_alt_not_null() {
    let exp: ParamExp = "+alt".parse().unwrap();
    assert!(matches!(exp, ParamExp::AltNotNull(ref a) if a == "alt"));
  }

  #[test]
  fn param_exp_err_unset_or_null() {
    let exp: ParamExp = ":?errmsg".parse().unwrap();
    assert!(matches!(exp, ParamExp::ErrUnsetOrNull(ref e) if e == "errmsg"));
  }

  #[test]
  fn param_exp_err_unset() {
    let exp: ParamExp = "?errmsg".parse().unwrap();
    assert!(matches!(exp, ParamExp::ErrUnset(ref e) if e == "errmsg"));
  }

  #[test]
  fn param_exp_len() {
    let exp: ParamExp = "##pattern".parse().unwrap();
    assert!(matches!(exp, ParamExp::RemLongestPrefix(ref p) if p == "pattern"));
  }

  #[test]
  fn param_exp_rem_shortest_prefix() {
    let exp: ParamExp = "#pat".parse().unwrap();
    assert!(matches!(exp, ParamExp::RemShortestPrefix(ref p) if p == "pat"));
  }

  #[test]
  fn param_exp_rem_longest_prefix() {
    let exp: ParamExp = "##pat".parse().unwrap();
    assert!(matches!(exp, ParamExp::RemLongestPrefix(ref p) if p == "pat"));
  }

  #[test]
  fn param_exp_rem_shortest_suffix() {
    let exp: ParamExp = "%pat".parse().unwrap();
    assert!(matches!(exp, ParamExp::RemShortestSuffix(ref p) if p == "pat"));
  }

  #[test]
  fn param_exp_rem_longest_suffix() {
    let exp: ParamExp = "%%pat".parse().unwrap();
    assert!(matches!(exp, ParamExp::RemLongestSuffix(ref p) if p == "pat"));
  }

  #[test]
  fn param_exp_replace_first() {
    let exp: ParamExp = "/old/new".parse().unwrap();
    assert!(matches!(exp, ParamExp::ReplaceFirstMatch(ref s, ref r) if s == "old" && r == "new"));
  }

  #[test]
  fn param_exp_replace_all() {
    let exp: ParamExp = "//old/new".parse().unwrap();
    assert!(matches!(exp, ParamExp::ReplaceAllMatches(ref s, ref r) if s == "old" && r == "new"));
  }

  #[test]
  fn param_exp_replace_prefix() {
    let exp: ParamExp = "/#old/new".parse().unwrap();
    assert!(matches!(exp, ParamExp::ReplacePrefix(ref s, ref r) if s == "old" && r == "new"));
  }

  #[test]
  fn param_exp_replace_suffix() {
    let exp: ParamExp = "/%old/new".parse().unwrap();
    assert!(matches!(exp, ParamExp::ReplaceSuffix(ref s, ref r) if s == "old" && r == "new"));
  }

  #[test]
  fn param_exp_indirect() {
    let exp: ParamExp = "!var".parse().unwrap();
    assert!(matches!(exp, ParamExp::ExpandInnerVar(ref v) if v == "var"));
  }

  #[test]
  fn param_exp_var_names_prefix() {
    let exp: ParamExp = "!prefix*".parse().unwrap();
    assert!(matches!(exp, ParamExp::VarNamesWithPrefix(ref p) if p == "prefix*"));
  }

  #[test]
  fn param_exp_substr() {
    let exp: ParamExp = ":2".parse().unwrap();
    assert!(matches!(exp, ParamExp::SliceOpen(2)));
  }

  #[test]
  fn param_exp_substr_len() {
    let exp: ParamExp = ":1:3".parse().unwrap();
    assert!(matches!(exp, ParamExp::SliceClosed(1, 3)));
  }

  // ===================== Parameter Expansion (TestGuard) =====================

  #[test]
  fn param_default_unset_or_null_unset() {
    let _guard = TestGuard::new();
    let result = perform_param_expansion("UNSET:-fallback").unwrap();
    assert_eq!(result, "fallback");
  }

  #[test]
  fn param_default_unset_or_null_null() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("EMPTY", VarKind::Str("".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("EMPTY:-fallback").unwrap();
    assert_eq!(result, "fallback");
  }

  #[test]
  fn param_default_unset_or_null_set() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("SET", VarKind::Str("value".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("SET:-fallback").unwrap();
    assert_eq!(result, "value");
  }

  #[test]
  fn param_default_unset_only() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("EMPTY", VarKind::Str("".into()), VarFlags::NONE)).unwrap();

    // ${EMPTY-fallback} - EMPTY is set (even if null), so returns null
    let result = perform_param_expansion("EMPTY-fallback").unwrap();
    assert_eq!(result, "");
  }

  #[test]
  fn param_alt_set_not_null() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("SET", VarKind::Str("value".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("SET:+alt").unwrap();
    assert_eq!(result, "alt");
  }

  #[test]
  fn param_alt_unset() {
    let _guard = TestGuard::new();

    let result = perform_param_expansion("UNSET:+alt").unwrap();
    assert_eq!(result, "");
  }

  #[test]
  fn param_err_unset() {
    let _guard = TestGuard::new();

    let result = perform_param_expansion("UNSET:?variable not set");
    assert!(result.is_err());
  }

  #[test]
  fn param_length() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("STR", VarKind::Str("hello".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("#STR").unwrap();
    assert_eq!(result, "5");
  }

  #[test]
  fn param_substr() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("STR", VarKind::Str("hello world".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("STR:6").unwrap();
    assert_eq!(result, "world");
  }

  #[test]
  fn param_substr_len() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("STR", VarKind::Str("hello world".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("STR:0:5").unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn param_remove_shortest_prefix() {
    let _guard = TestGuard::new();
    write_vars(|v| {
      v.set_var(
        "PATH",
        VarKind::Str("/usr/local/bin".into()),
        VarFlags::NONE,
      )
    })
    .unwrap();

    let result = perform_param_expansion("PATH#*/").unwrap();
    assert_eq!(result, "usr/local/bin");
  }

  #[test]
  fn param_remove_longest_prefix() {
    let _guard = TestGuard::new();
    write_vars(|v| {
      v.set_var(
        "PATH",
        VarKind::Str("/usr/local/bin".into()),
        VarFlags::NONE,
      )
    })
    .unwrap();

    let result = perform_param_expansion("PATH##*/").unwrap();
    assert_eq!(result, "bin");
  }

  #[test]
  fn param_remove_shortest_suffix() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("FILE", VarKind::Str("file.tar.gz".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("FILE%.*").unwrap();
    assert_eq!(result, "file.tar");
  }

  #[test]
  fn param_remove_longest_suffix() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("FILE", VarKind::Str("file.tar.gz".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("FILE%%.*").unwrap();
    assert_eq!(result, "file");
  }

  #[test]
  fn param_replace_first() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("STR", VarKind::Str("hello hello".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("STR/hello/world").unwrap();
    assert_eq!(result, "world hello");
  }

  #[test]
  fn param_replace_all() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("STR", VarKind::Str("hello hello".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("STR//hello/world").unwrap();
    assert_eq!(result, "world world");
  }

  #[test]
  fn param_indirect() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("REF", VarKind::Str("TARGET".into()), VarFlags::NONE)).unwrap();
    write_vars(|v| v.set_var("TARGET", VarKind::Str("value".into()), VarFlags::NONE)).unwrap();

    let result = perform_param_expansion("!REF").unwrap();
    assert_eq!(result, "value");
  }

  #[test]
  fn param_set_default_assigns() {
    let _guard = TestGuard::new();

    let result = perform_param_expansion("NEWVAR:=assigned").unwrap();
    assert_eq!(result, "assigned");

    // Verify it was actually set
    let val = read_vars(|v| v.get_var("NEWVAR"));
    assert_eq!(val, "assigned");
  }

  // ===================== Parameter Expansion with Escapes (TestGuard) =====================

  #[test]
  fn param_exp_prefix_removal_escaped() {
    let guard = TestGuard::new();
    write_vars(|v| v.set_var("branch", VarKind::Str("## main".into()), VarFlags::NONE)).unwrap();

    test_input("echo \"${branch#\\#\\# }\"").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "main\n");
  }

  #[test]
  fn param_exp_suffix_removal_escaped() {
    let guard = TestGuard::new();
    write_vars(|v| v.set_var("val", VarKind::Str("hello world!!".into()), VarFlags::NONE)).unwrap();

    test_input("echo \"${val%\\!\\!}\"").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "hello world\n");
  }
}
