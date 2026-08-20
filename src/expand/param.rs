use crate::eval::lex::TkFlags;
use crate::expand::Expander;
use crate::expand::stream::{Marker, SegStream, Unit};
use crate::expand::var::expand_raw_inner;
use crate::state::vars::VarStr;
use crate::state::{
  Shed, scopes::ScopeStack, vars::ArrIndex, vars::ShellParam, vars::VarFlags, vars::VarKind,
  vars::VarName,
};
use crate::util::ShResult;
use crate::{match_loop, util};
use crate::{sherr, shopt, var};

#[derive(Debug)]
pub(crate) enum ParamExp {
  ToUpperFirst,                            // ^var_name
  ToUpperAll,                              // ^^var_name
  ToLowerFirst,                            // ,var_name
  ToLowerAll,                              // ,,var_name
  DefaultUnsetOrNull(SegStream),           // :-
  DefaultUnset(SegStream),                 // -
  SetDefaultUnsetOrNull(SegStream),        // :=
  SetDefaultUnset(SegStream),              // =
  AltSetNotNull(SegStream),                // :+
  AltNotNull(SegStream),                   // +
  ErrUnsetOrNull(SegStream),               // :?
  ErrUnset(SegStream),                     // ?
  SliceOpen(i64),                          // :pos  (pos may be negative: from end)
  SliceClosed(i64, i64),                   // :pos:len  (either may be negative)
  RemShortestPrefix(SegStream),            // #pattern
  RemLongestPrefix(SegStream),             // ##pattern
  RemShortestSuffix(SegStream),            // %pattern
  RemLongestSuffix(SegStream),             // %%pattern
  ReplaceFirstMatch(SegStream, SegStream), // /search/replace
  ReplaceAllMatches(SegStream, SegStream), // //search/replace
  ReplacePrefix(SegStream, SegStream),     // #search/replace
  ReplaceSuffix(SegStream, SegStream),     // %search/replace
  VarNamesWithPrefix(String),              // !prefix@ || !prefix*
  ExpandInnerVar(String),                  // !var
}

/// Parse a parameter expansion
///
/// The `allow_side_effects` thing prevents state-mutating stuff like "set if null" or expanding command subs
/// It's set to false in places like the syntax highlighter where we really dont want to be silently executing
/// unfinished commands.
/// Split a `${var/pat/rep}` operand into its search pattern and replacement at
/// the first unescaped, unquoted `/`. A missing separator means an empty
/// replacement (`${var/pat}` deletes matches).
fn split_search_repl(rest: SegStream) -> (SegStream, SegStream) {
  match rest.split_once_unescaped(b'/') {
    Some(pair) => pair,
    None => (rest, SegStream::new()),
  }
}

pub fn parse_param_exp(body: &SegStream, allow_side_effects: bool) -> ShResult<ParamExp> {
  use ParamExp as PE;

  let parse_err = || Err(sherr!(SyntaxErr, "Invalid parameter expansion",));

  // The operator is markerless ASCII at the very front of the body, so match it
  // against the leading bytes; the operand (which may carry markers) is the
  // remainder stream after the operator is peeled off.
  let lead = body.to_bytes();
  let lead_run = body.leading_bytes();

  match lead.as_slice() {
    b"^^" => return Ok(PE::ToUpperAll),
    b"^" => return Ok(PE::ToUpperFirst),
    b",," => return Ok(PE::ToLowerAll),
    b"," => return Ok(PE::ToLowerFirst),
    _ => {}
  }

  // Handle indirect var expansion: ${!var}
  if lead.first() == Some(&b'!') {
    let var = String::from_utf8_lossy(&lead[1..]).into_owned();
    if var.ends_with(']') && (var.contains("[@]") || var.contains("[*]")) {
      return Ok(PE::ExpandInnerVar(var));
    }
    if var.ends_with('*') || var.ends_with('@') {
      return Ok(PE::VarNamesWithPrefix(var));
    }
    return Ok(PE::ExpandInnerVar(var));
  }

  // Pattern removals
  if lead_run.starts_with(b"##") {
    return Ok(PE::RemLongestPrefix(body.split_off_front(2).1));
  } else if lead_run.starts_with(b"#") {
    return Ok(PE::RemShortestPrefix(body.split_off_front(1).1));
  }
  if lead_run.starts_with(b"%%") {
    return Ok(PE::RemLongestSuffix(body.split_off_front(2).1));
  } else if lead_run.starts_with(b"%") {
    return Ok(PE::RemShortestSuffix(body.split_off_front(1).1));
  }

  // Replacements. The pattern/replacement separator is the first `/` that is
  // not escaped (by an `ESCAPE` marker) or quoted, so a literal `/` in the
  // pattern (e.g. `${v/\//_}`) is honored.
  if lead_run.starts_with(b"//") {
    let (pattern, repl) = split_search_repl(body.split_off_front(2).1);
    return Ok(PE::ReplaceAllMatches(pattern, repl));
  }
  if lead_run.starts_with(b"/") {
    let rest = body.split_off_front(1).1;
    match rest.to_bytes().first() {
      Some(&b'%') => {
        let (pattern, repl) = split_search_repl(rest.split_off_front(1).1);
        return Ok(PE::ReplaceSuffix(pattern, repl));
      }
      Some(&b'#') => {
        let (pattern, repl) = split_search_repl(rest.split_off_front(1).1);
        return Ok(PE::ReplacePrefix(pattern, repl));
      }
      _ => {
        let (pattern, repl) = split_search_repl(rest);
        return Ok(PE::ReplaceFirstMatch(pattern, repl));
      }
    }
  }

  // Fallback / assignment / alt
  if lead_run.starts_with(b":-") {
    return Ok(PE::DefaultUnsetOrNull(body.split_off_front(2).1));
  } else if lead_run.starts_with(b"-") {
    return Ok(PE::DefaultUnset(body.split_off_front(1).1));
  } else if lead_run.starts_with(b":+") {
    return Ok(PE::AltSetNotNull(body.split_off_front(2).1));
  } else if lead_run.starts_with(b"+") {
    return Ok(PE::AltNotNull(body.split_off_front(1).1));
  } else if lead_run.starts_with(b":=") {
    return Ok(PE::SetDefaultUnsetOrNull(body.split_off_front(2).1));
  } else if lead_run.starts_with(b"=") {
    return Ok(PE::SetDefaultUnset(body.split_off_front(1).1));
  } else if lead_run.starts_with(b":?") {
    return Ok(PE::ErrUnsetOrNull(body.split_off_front(2).1));
  } else if lead_run.starts_with(b"?") {
    return Ok(PE::ErrUnset(body.split_off_front(1).1));
  }

  // Substring. The offset/length are numeric; a lossy str view suffices (a
  // variable offset like `${v:$x}` is not resolved through this path).
  if let Some((pos, len)) = parse_pos_len(&String::from_utf8_lossy(&lead), allow_side_effects) {
    return Ok(match len {
      Some(l) => PE::SliceClosed(pos, l),
      None => PE::SliceOpen(pos),
    });
  }

  parse_err()
}

/// Expand and parse one signed substring component (offset or length).
///
/// Handles bash's disambiguating forms for a negative offset: a leading space
/// (`${v: -2}`) and a single layer of surrounding parens (`${v:(-2)}`).
fn parse_signed_component(s: &str, allow_side_effects: bool) -> Option<i64> {
  let input = SegStream::from_bytes(s.as_bytes());
  let expanded = expand_raw_inner(&mut input.cursor(), allow_side_effects, false)
    .map_or_else(|_| s.as_bytes().to_vec(), SegStream::into_bytes);
  let expanded = String::from_utf8_lossy(&expanded);
  let trimmed = expanded.trim();
  let trimmed = trimmed
    .strip_prefix('(')
    .and_then(|t| t.strip_suffix(')'))
    .map_or(trimmed, str::trim);
  trimmed.parse::<i64>().ok()
}

pub fn parse_pos_len(s: &str, allow_side_effects: bool) -> Option<(i64, Option<i64>)> {
  let raw = s.strip_prefix(':')?;
  if let Some((start, len)) = raw.split_once(':') {
    Some((
      parse_signed_component(start, allow_side_effects)?,
      parse_signed_component(len, allow_side_effects),
    ))
  } else {
    Some((parse_signed_component(raw, allow_side_effects)?, None))
  }
}

/// Resolve a possibly-negative substring offset against a char count `n`.
/// A negative offset counts from the end; results are clamped to `[0, n]`.
fn resolve_offset(pos: i64, n: i64) -> i64 {
  if pos < 0 {
    (n + pos).max(0)
  } else {
    pos.min(n)
  }
}

/// Expand an array subscript (`[...]`) in a `${...}` body *before* the body is
/// flattened for name/operator parsing, so `${arr[$i]}` resolves `$i`.
fn expand_body_subscripts(body: &SegStream, allow_side_effects: bool) -> ShResult<SegStream> {
  let mut out = SegStream::new();
  let mut cursor = body.cursor();
  let mut in_name = true;
  let mut seen_any = false;
  while let Some(unit) = cursor.next() {
    match unit {
      // A leading `#` is the length operator; the name follows it.
      Unit::Byte(b'#') if !seen_any => out.push_byte(b'#'),
      Unit::Byte(b'[') if in_name => {
        in_name = false;
        out.push_byte(b'[');
        let mut inner = SegStream::new();
        let mut depth = 1;
        while let Some(u) = cursor.next() {
          match u {
            Unit::Byte(b'[') => {
              depth += 1;
              inner.push(u);
            }
            Unit::Byte(b']') => {
              depth -= 1;
              if depth == 0 {
                break;
              }
              inner.push(u);
            }
            _ => inner.push(u),
          }
        }
        out.append(expand_raw_inner(
          &mut inner.cursor(),
          allow_side_effects,
          false,
        )?);
        out.push_byte(b']');
      }
      Unit::Byte(b) if b.is_ascii_alphanumeric() || b == b'_' => out.push_byte(b),
      _ => {
        in_name = false;
        out.push(unit);
      }
    }
    seen_any = true;
  }
  Ok(out)
}

pub fn perform_param_expansion(body: &SegStream, allow_side_effects: bool) -> ShResult<SegStream> {
  // Resolve the array subscript first (`${arr[$i]}`), then parse the name /
  // operator against a lossy string view; the operand (the suffix after the
  // operator) keeps its markers and is sliced back off `body` once we know the
  // split point.
  let body = expand_body_subscripts(body, allow_side_effects)?;
  let body_bytes = body.to_bytes();
  let raw = String::from_utf8_lossy(&body_bytes);
  let mut chars = raw.chars();
  let mut var_name = util::scratch_buf();
  let mut rest = util::scratch_buf();
  if raw.starts_with('#') {
    let var_spec = raw.strip_prefix('#').unwrap();
    if var_spec.is_empty() || var_spec == "*" || var_spec == "@" {
      // this is either asking for the `#` parameter directly, or asking for the length
      // of `$*` or `$@`. All of these refer to the same thing: the number of positional
      // arguments.
      return Ok(Shed::vars(|v| v.get_param(ShellParam::ArgCount)).into());
    }

    if let Ok(param) = var_spec.parse::<ShellParam>() {
      let len = Shed::vars(|v| v.try_get_param(param)).map_or(0, |val| val.len());
      return Ok(len.to_string().into());
    }
    let parsed = VarName::parse(var_spec, allow_side_effects)?;
    if let Some(idx) = parsed.index() {
      match idx {
        ArrIndex::AllSplit | ArrIndex::AllJoined | ArrIndex::ArgCount => {
          let var = Shed::vars(|v| v.get_var_meta(parsed.name()));
          return Ok(
            match var.kind() {
              VarKind::Arr(items) => items.len(),
              VarKind::AssocArr(items) => items.len(),
              _ => 0,
            }
            .to_string()
            .into(),
          );
        }
        _ => {
          let val = Shed::vars(|v| v.index_var(parsed.name(), idx))?;
          return Ok(val.len().to_string().into());
        }
      }
    }
    let var = Shed::vars(|v| v.get_var_meta(var_spec));
    return Ok(
      match var.kind() {
        VarKind::Magic(func) => func().unwrap_or_default().len(),
        VarKind::Str(_) | VarKind::Int(_) => var.to_string().len(),
        VarKind::Arr(items) => items.len(),
        VarKind::AssocArr(items) => items.len(),
        VarKind::Unset => 0,
      }
      .to_string()
      .into(),
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
      let mut idx_content = util::scratch_buf();
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

    // it's a shell parameter, don't get it confused with the operators below
    ch if var_name.is_empty() && matches!(ch, '?' | '!' | '#' | '-') => {

      let next_is_name_char = chars
        .clone()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_');
      if next_is_name_char {
        rest.push(ch);
        rest.push_str(&chars.collect::<String>());
      } else {
        var_name.push(ch);
        rest.push_str(&chars.collect::<String>());
      }
      break;
    }
    '!' | '#' | '%' | ':' | '-' | '+' | '^' | ',' | '=' | '/' | '?' => {
      rest.push(ch);
      rest.push_str(&chars.collect::<String>());
      break;
    }
    _ => var_name.push(ch),
  });

  let mut parsed = VarName::parse(&var_name, allow_side_effects)?;

  if matches!(parsed.index(), Some(ArrIndex::Raw(_))) {
    let tag = Shed::vars(|v| v.try_get_var_kind_tag(parsed.name()));
    if let Some(tag) = tag {
      let resolved = parsed.index().unwrap().clone().resolve_for(tag)?;
      parsed.set_index(resolved);
    }
  }
  let get = |v: &ScopeStack| v.resolve_var(&parsed).unwrap_or_default();
  let try_get = |v: &ScopeStack| v.resolve_var(&parsed);

  let operand = body.split_off_front(var_name.len()).1;
  let _ = &rest;
  if let Ok(expansion) = parse_param_exp(&operand, allow_side_effects) {
    match expansion {
      ParamExp::ToUpperAll => {
        let value = Shed::vars(get);
        let value = value.to_str_lossy();
        let new = value.to_uppercase();
        Ok(new.into())
      }
      ParamExp::ToUpperFirst => {
        let value = Shed::vars(get);
        let value = value.to_str_lossy();
        let mut chars = value.chars();
        let first = chars
          .next()
          .map(|c| c.to_uppercase().to_string())
          .unwrap_or_default();

        let new = first + chars.as_str();
        Ok(new.into())
      }
      ParamExp::ToLowerAll => {
        let value = Shed::vars(get);
        let value = value.to_str_lossy();
        let new = value.to_lowercase();
        Ok(new.into())
      }
      ParamExp::ToLowerFirst => {
        let value = Shed::vars(get);
        let value = value.to_str_lossy();
        let mut chars = value.chars();
        let first = chars
          .next()
          .map(|c| c.to_lowercase().to_string())
          .unwrap_or_default();
        let new = first + chars.as_str();
        Ok(new.into())
      }
      ParamExp::DefaultUnsetOrNull(default) => {
        match Shed::vars(try_get).filter(|v| !v.is_empty()) {
          Some(val) => Ok(val.into()),
          None => expand_raw_inner(&mut default.cursor(), allow_side_effects, false),
        }
      }
      ParamExp::DefaultUnset(default) => match Shed::vars(try_get) {
        Some(val) => Ok(val.into()),
        None => expand_raw_inner(&mut default.cursor(), allow_side_effects, false),
      },
      ParamExp::SetDefaultUnsetOrNull(default) => {
        if let Some(val) = Shed::vars(try_get).filter(|v| !v.is_empty()) {
          Ok(val.into())
        } else {
          let expanded = expand_raw_inner(&mut default.cursor(), allow_side_effects, false)?;
          if allow_side_effects {
            let stored = VarStr::from(expanded.to_bytes());
            Shed::vars_mut(|v| {
              v.set_var(parsed.name(), VarKind::string(stored), VarFlags::empty())
            })?;
          }
          Ok(expanded)
        }
      }
      ParamExp::SetDefaultUnset(default) => {
        if let Some(val) = Shed::vars(try_get) {
          Ok(val.into())
        } else {
          let expanded = expand_raw_inner(&mut default.cursor(), allow_side_effects, false)?;
          if allow_side_effects {
            let stored = VarStr::from(expanded.to_bytes());
            Shed::vars_mut(|v| {
              v.set_var(parsed.name(), VarKind::string(stored), VarFlags::empty())
            })?;
          }
          Ok(expanded)
        }
      }
      ParamExp::AltSetNotNull(alt) => match Shed::vars(try_get).filter(|v| !v.is_empty()) {
        Some(_) => expand_raw_inner(&mut alt.cursor(), allow_side_effects, false),
        None => Ok(SegStream::new()),
      },
      ParamExp::AltNotNull(alt) => match Shed::vars(try_get) {
        Some(_) => expand_raw_inner(&mut alt.cursor(), allow_side_effects, false),
        None => Ok(SegStream::new()),
      },
      ParamExp::ErrUnsetOrNull(err) => {
        if let Some(val) = Shed::vars(try_get).filter(|v| !v.is_empty()) {
          Ok(val.into())
        } else {
          if !allow_side_effects {
            return Ok(SegStream::new());
          }
          let expanded = expand_raw_inner(&mut err.cursor(), allow_side_effects, false)?;
          Err(sherr!(
            ExecFail,
            "{}",
            String::from_utf8_lossy(&expanded.into_bytes())
          ))
        }
      }
      ParamExp::ErrUnset(err) => {
        if let Some(val) = Shed::vars(try_get) {
          Ok(val.into())
        } else {
          if !allow_side_effects {
            return Ok(SegStream::new());
          }
          let expanded = expand_raw_inner(&mut err.cursor(), allow_side_effects, false)?;
          Err(sherr!(
            ExecFail,
            "{}",
            String::from_utf8_lossy(&expanded.into_bytes())
          ))
        }
      }
      ParamExp::SliceOpen(pos) => {
        let value = Shed::vars(get);
        let value = value.to_str_lossy();
        let chars: Vec<char> = value.chars().collect();
        let n = chars.len() as i64;
        let start = resolve_offset(pos, n) as usize;
        let substr: String = chars[start..].iter().collect();
        Ok(substr.into())
      }
      ParamExp::SliceClosed(pos, len) => {
        let value = Shed::vars(get);
        let value = value.to_str_lossy();
        let chars: Vec<char> = value.chars().collect();
        let n = chars.len() as i64;
        let start = resolve_offset(pos, n);
        // A negative length is an offset from the end of the string; a positive
        // one counts forward from `start`. bash errors if the end lands before
        // the start ("substring expression < 0").
        let end = if len < 0 {
          n + len
        } else {
          (start + len).min(n)
        };
        if end < start {
          return Err(sherr!(ExecFail, "substring expression < 0"));
        }
        let substr: String = chars[start as usize..end as usize].iter().collect();
        Ok(substr.into())
      }
      ParamExp::RemShortestPrefix(prefix) => {
        let value = Shed::vars(get);
        let value = value.as_bytes();
        let expanded = Expander::from_raw_no_brace_pattern(prefix, TkFlags::empty())
          .no_glob()
          .expand_for_glob()?;

        let pattern = Shed::meta_mut(|m| m.get_glob(&expanded.to_str_lossy()));
        if let Some(len) = pattern.match_shortest_prefix(value) {
          return Ok(VarStr::from(&value[len..]).into());
        }

        Ok(VarStr::from(value).into())
      }
      ParamExp::RemLongestPrefix(prefix) => {
        let value = Shed::vars(get);
        let value = value.as_bytes();
        let expanded = Expander::from_raw_no_brace_pattern(prefix, TkFlags::empty())
          .no_glob()
          .expand_for_glob()?;

        let pattern = Shed::meta_mut(|m| m.get_glob(&expanded.to_str_lossy()));
        if let Some(len) = pattern.match_longest_prefix(value) {
          return Ok(VarStr::from(&value[len..]).into());
        }

        Ok(VarStr::from(value).into()) // no match
      }
      ParamExp::RemShortestSuffix(suffix) => {
        let value = Shed::vars(get);
        let value = value.as_bytes();
        let expanded = Expander::from_raw_no_brace_pattern(suffix, TkFlags::empty())
          .no_glob()
          .expand_for_glob()?;

        let pattern = Shed::meta_mut(|m| m.get_glob(&expanded.to_str_lossy()));
        if let Some(len) = pattern.match_shortest_suffix(value) {
          let pos = value.len() - len;
          return Ok(VarStr::from(&value[..pos]).into());
        }

        Ok(VarStr::from(value).into())
      }
      ParamExp::RemLongestSuffix(suffix) => {
        let value = Shed::vars(get);
        let value = value.as_bytes();
        let expanded_suffix = Expander::from_raw_no_brace_pattern(suffix, TkFlags::empty())
          .no_glob()
          .expand_for_glob()?;

        let pattern = Shed::meta_mut(|m| m.get_glob(&expanded_suffix.to_str_lossy()));
        if let Some(len) = pattern.match_longest_suffix(value) {
          let pos = value.len() - len;
          return Ok(VarStr::from(&value[..pos]).into());
        }

        Ok(VarStr::from(value).into())
      }
      ParamExp::ReplaceFirstMatch(search, replace) => {
        let value = Shed::vars(get);
        let value = value.as_bytes();
        let expanded_search = Expander::from_raw_pattern(search, TkFlags::empty())
          .no_glob()
          .expand_for_glob()?;

        if expanded_search.is_empty() {
          return Ok(VarStr::from(value).into());
        }

        let expanded_replace = Expander::from_raw_pattern(replace, TkFlags::empty())
          .no_glob()
          .expand_no_split()?;
        let glob = Shed::meta_mut(|m| m.get_glob(&expanded_search.to_str_lossy())); // unanchored

        if let Some((start, end)) = glob.find(value, 0) {
          let mut result = Vec::with_capacity(value.len());
          result.extend_from_slice(&value[..start]);
          result.extend_from_slice(expanded_replace.as_bytes());
          result.extend_from_slice(&value[end..]);
          Ok(VarStr::from(result).into())
        } else {
          Ok(VarStr::from(value).into())
        }
      }
      ParamExp::ReplaceAllMatches(search, replace) => {
        let value = Shed::vars(get);
        let value = value.as_bytes();
        let expanded_search = Expander::from_raw_pattern(search, TkFlags::empty())
          .no_glob()
          .expand_for_glob()?;

        if expanded_search.is_empty() {
          return Ok(VarStr::from(value).into());
        }

        let expanded_replace = Expander::from_raw_pattern(replace, TkFlags::empty())
          .no_glob()
          .expand_no_split()?;
        let glob = Shed::meta_mut(|m| m.get_glob(&expanded_search.to_str_lossy()));
        let mut result: Vec<u8> = Vec::new();
        let mut last_match_end = 0;
        let mut from = 0;

        while let Some((start, end)) = glob.find(value, from) {
          result.extend_from_slice(&value[last_match_end..start]);
          result.extend_from_slice(expanded_replace.as_bytes());
          last_match_end = end;
          // non-overlapping; step past a zero-width match so we don't spin
          from = if end > start { end } else { end + 1 };
        }

        // Append the rest of the string
        result.extend_from_slice(&value[last_match_end..]);
        Ok(VarStr::from(result).into())
      }
      ParamExp::ReplacePrefix(search, replace) => {
        let value = Shed::vars(get);
        let value = value.as_bytes();
        let expanded_search = Expander::from_raw_pattern(search, TkFlags::empty())
          .no_glob()
          .expand_for_glob()?;
        let expanded_replace = Expander::from_raw_pattern(replace, TkFlags::empty())
          .no_glob()
          .expand_no_split()?;

        let pattern = Shed::meta_mut(|m| m.get_glob(&expanded_search.to_str_lossy()));
        if let Some(len) = pattern.match_longest_prefix(value) {
          let mut result = expanded_replace.as_bytes().to_vec();
          result.extend_from_slice(&value[len..]);
          return Ok(VarStr::from(result).into());
        }

        Ok(VarStr::from(value).into())
      }
      ParamExp::ReplaceSuffix(search, replace) => {
        let value = Shed::vars(get);
        let value = value.as_bytes();
        let expanded_search = Expander::from_raw_pattern(search, TkFlags::empty())
          .no_glob()
          .expand_for_glob()?;
        let expanded_replace = Expander::from_raw_pattern(replace, TkFlags::empty())
          .no_glob()
          .expand_no_split()?;

        let pattern = Shed::meta_mut(|m| m.get_glob(&expanded_search.to_str_lossy()));
        if let Some(len) = pattern.match_longest_suffix(value) {
          let pos = value.len() - len;
          let mut result = Vec::with_capacity(pos + expanded_replace.as_bytes().len());
          result.extend_from_slice(&value[..pos]);
          result.extend_from_slice(expanded_replace.as_bytes());
          return Ok(VarStr::from(result).into());
        }

        Ok(VarStr::from(value).into())
      }
      ParamExp::VarNamesWithPrefix(prefix) => {
        let flat = Shed::vars(ScopeStack::flatten_vars);
        let match_vars: Vec<_> = flat
          .keys()
          .filter(|var| var.starts_with(&prefix))
          .cloned()
          .collect();
        Ok(match_vars.join(" ").into())
      }
      ParamExp::ExpandInnerVar(inner) => {
        if inner.contains("[@]") || inner.contains("[*]") {
          let var_name = if let Some(pos) = inner.find('[') {
            &inner[..pos]
          } else {
            &inner
          };
          let joined = inner.contains("[*]");
          Shed::vars(|v| v.get_array_keys(var_name, joined)).map(Into::into)
        } else {
          let inner_name = VarName::parse(&inner, allow_side_effects)?;
          let value = Shed::vars(|v| v.resolve_var(&inner_name).unwrap_or_default());
          Ok(var!(&value.to_str_lossy()).into())
        }
      }
    }
  } else {
    let var = Shed::vars(try_get);
    // "${@}" must expand to zero fields
    if var_name.as_str() == "@" && var.as_deref().unwrap_or_default().is_empty() {
      let mut out = SegStream::new();
      out.push_marker(Marker::NullExpand);
      return Ok(out);
    }
    if var.is_none() && shopt!(set.nounset) {
      return Err(sherr!(NotFound, "Variable '{}' is not set", parsed.name()));
    }
    Ok(var.unwrap_or_default().into())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::state::{Shed, vars::VarFlags, vars::VarKind};
  use crate::tests::testutil::{TestGuard, test_input};

  fn test_param_parse(val: &str) -> ParamExp {
    parse_param_exp(&SegStream::from_bytes(val.as_bytes()), true).unwrap()
  }

  fn test_param_expansion(val: &str) -> ShResult<VarStr> {
    perform_param_expansion(&SegStream::from_bytes(val.as_bytes()), true)
      .map(|s| VarStr::from(s.into_bytes()))
  }

  // ===================== ParamExp parsing =====================

  #[test]
  fn param_exp_default_unset_or_null() {
    let exp = test_param_parse(":-default");
    assert!(matches!(exp, ParamExp::DefaultUnsetOrNull(ref d) if d == "default"));
  }

  #[test]
  fn param_exp_default_unset() {
    let exp = test_param_parse("-fallback");
    assert!(matches!(exp, ParamExp::DefaultUnset(ref d) if d == "fallback"));
  }

  #[test]
  fn param_exp_set_default_unset_or_null() {
    let exp = test_param_parse(":=val");
    assert!(matches!(exp, ParamExp::SetDefaultUnsetOrNull(ref v) if v == "val"));
  }

  #[test]
  fn param_exp_set_default_unset() {
    let exp = test_param_parse("=val");
    assert!(matches!(exp, ParamExp::SetDefaultUnset(ref v) if v == "val"));
  }

  #[test]
  fn param_exp_alt_set_not_null() {
    let exp = test_param_parse(":+alt");
    assert!(matches!(exp, ParamExp::AltSetNotNull(ref a) if a == "alt"));
  }

  #[test]
  fn param_exp_alt_not_null() {
    let exp = test_param_parse("+alt");
    assert!(matches!(exp, ParamExp::AltNotNull(ref a) if a == "alt"));
  }

  #[test]
  fn param_exp_err_unset_or_null() {
    let exp = test_param_parse(":?errmsg");
    assert!(matches!(exp, ParamExp::ErrUnsetOrNull(ref e) if e == "errmsg"));
  }

  #[test]
  fn param_exp_err_unset() {
    let exp = test_param_parse("?errmsg");
    assert!(matches!(exp, ParamExp::ErrUnset(ref e) if e == "errmsg"));
  }

  #[test]
  fn param_exp_len() {
    let exp = test_param_parse("##pattern");
    assert!(matches!(exp, ParamExp::RemLongestPrefix(ref p) if p == "pattern"));
  }

  #[test]
  fn param_exp_rem_shortest_prefix() {
    let exp = test_param_parse("#pat");
    assert!(matches!(exp, ParamExp::RemShortestPrefix(ref p) if p == "pat"));
  }

  #[test]
  fn param_exp_rem_longest_prefix() {
    let exp = test_param_parse("##pat");
    assert!(matches!(exp, ParamExp::RemLongestPrefix(ref p) if p == "pat"));
  }

  #[test]
  fn param_exp_rem_shortest_suffix() {
    let exp = test_param_parse("%pat");
    assert!(matches!(exp, ParamExp::RemShortestSuffix(ref p) if p == "pat"));
  }

  #[test]
  fn param_exp_rem_longest_suffix() {
    let exp = test_param_parse("%%pat");
    assert!(matches!(exp, ParamExp::RemLongestSuffix(ref p) if p == "pat"));
  }

  #[test]
  fn param_exp_replace_first() {
    let exp = test_param_parse("/old/new");
    assert!(matches!(exp, ParamExp::ReplaceFirstMatch(ref s, ref r) if s == "old" && r == "new"));
  }

  #[test]
  fn param_exp_replace_all() {
    let exp = test_param_parse("//old/new");
    assert!(matches!(exp, ParamExp::ReplaceAllMatches(ref s, ref r) if s == "old" && r == "new"));
  }

  #[test]
  fn param_exp_replace_prefix() {
    let exp = test_param_parse("/#old/new");
    assert!(matches!(exp, ParamExp::ReplacePrefix(ref s, ref r) if s == "old" && r == "new"));
  }

  #[test]
  fn param_exp_replace_suffix() {
    let exp = test_param_parse("/%old/new");
    assert!(matches!(exp, ParamExp::ReplaceSuffix(ref s, ref r) if s == "old" && r == "new"));
  }

  #[test]
  fn param_exp_indirect() {
    let exp = test_param_parse("!var");
    assert!(matches!(exp, ParamExp::ExpandInnerVar(ref v) if v == "var"));
  }

  #[test]
  fn param_exp_var_names_prefix() {
    let exp = test_param_parse("!prefix*");
    assert!(matches!(exp, ParamExp::VarNamesWithPrefix(ref p) if p == "prefix*"));
  }

  #[test]
  fn param_exp_substr() {
    let exp = test_param_parse(":2");
    assert!(matches!(exp, ParamExp::SliceOpen(2)));
  }

  #[test]
  fn param_exp_substr_len() {
    let exp = test_param_parse(":1:3");
    assert!(matches!(exp, ParamExp::SliceClosed(1, 3)));
  }

  #[test]
  fn param_exp_substr_negative_offset_parses() {
    let exp = test_param_parse(": -2");
    assert!(matches!(exp, ParamExp::SliceOpen(-2)));
  }

  #[test]
  fn param_exp_substr_paren_negative_offset_parses() {
    let exp = test_param_parse(":(-2)");
    assert!(matches!(exp, ParamExp::SliceOpen(-2)));
  }

  #[test]
  fn param_exp_substr_negative_length_parses() {
    let exp = test_param_parse(":1:-1");
    assert!(matches!(exp, ParamExp::SliceClosed(1, -1)));
  }

  fn set_v_abcdef() {
    Shed::vars_mut(|v| v.set_var("V", VarKind::Str("abcdef".into()), VarFlags::empty())).unwrap();
  }

  #[test]
  fn substr_negative_offset_counts_from_end() {
    let _guard = TestGuard::new();
    set_v_abcdef();
    assert_eq!(test_param_expansion("V: -2").unwrap(), "ef");
  }

  #[test]
  fn substr_negative_offset_with_length() {
    let _guard = TestGuard::new();
    set_v_abcdef();
    assert_eq!(test_param_expansion("V: -3:2").unwrap(), "de");
  }

  #[test]
  fn substr_negative_length_is_end_offset() {
    let _guard = TestGuard::new();
    set_v_abcdef();
    assert_eq!(test_param_expansion("V:1:-1").unwrap(), "bcde");
  }

  #[test]
  fn substr_paren_negative_offset() {
    let _guard = TestGuard::new();
    set_v_abcdef();
    assert_eq!(test_param_expansion("V:(-2)").unwrap(), "ef");
  }

  #[test]
  fn substr_positive_forms_still_work() {
    let _guard = TestGuard::new();
    set_v_abcdef();
    assert_eq!(test_param_expansion("V:2").unwrap(), "cdef");
    assert_eq!(test_param_expansion("V:1:3").unwrap(), "bcd");
  }

  #[test]
  fn substr_end_before_start_errors() {
    let _guard = TestGuard::new();
    set_v_abcdef();
    // ${V:4:-5}: end = 6 - 5 = 1 < start 4 -> error
    assert!(test_param_expansion("V:4:-5").is_err());
  }

  // ===================== Parameter Expansion (TestGuard) =====================

  #[test]
  fn param_default_unset_or_null_unset() {
    let _guard = TestGuard::new();
    let result = test_param_expansion("UNSET:-fallback").unwrap();
    assert_eq!(result, "fallback");
  }

  #[test]
  fn param_default_unset_or_null_null() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "EMPTY",
        VarKind::string(VarStr::default()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let result = test_param_expansion("EMPTY:-fallback").unwrap();
    assert_eq!(result, "fallback");
  }

  #[test]
  fn param_default_unset_or_null_set() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("SET", VarKind::Str("value".into()), VarFlags::empty())).unwrap();

    let result = test_param_expansion("SET:-fallback").unwrap();
    assert_eq!(result, "value");
  }

  #[test]
  fn param_default_unset_only() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "EMPTY",
        VarKind::string(VarStr::default()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    // ${EMPTY-fallback} - EMPTY is set (even if null), so returns null
    let result = test_param_expansion("EMPTY-fallback").unwrap();
    assert_eq!(result, "");
  }

  #[test]
  fn param_alt_set_not_null() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("SET", VarKind::Str("value".into()), VarFlags::empty())).unwrap();

    let result = test_param_expansion("SET:+alt").unwrap();
    assert_eq!(result, "alt");
  }

  #[test]
  fn param_alt_unset() {
    let _guard = TestGuard::new();

    let result = test_param_expansion("UNSET:+alt").unwrap();
    assert_eq!(result, "");
  }

  #[test]
  fn param_err_unset() {
    let _guard = TestGuard::new();

    let result = test_param_expansion("UNSET:?variable not set");
    assert!(result.is_err());
  }

  #[test]
  fn param_assoc_missing_key_is_unset_not_empty() {
    // The set-tests (+ / -) must distinguish a missing assoc key from a
    // present-but-empty one, which means resolving the element to "unset"
    // when the key is absent rather than the default empty string.
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "S",
        VarKind::AssocArr(vec![
          ("present".into(), "v".into()),
          ("empty".into(), "".into()),
        ]),
        VarFlags::empty(),
      )
    })
    .unwrap();

    // `+`: a key that exists is "set" even when its value is empty.
    assert_eq!(test_param_expansion("S[present]+x").unwrap(), "x");
    assert_eq!(test_param_expansion("S[empty]+x").unwrap(), "x");
    assert_eq!(test_param_expansion("S[missing]+x").unwrap(), "");

    // `-`: a missing key falls back to the default; an empty value does not.
    assert_eq!(test_param_expansion("S[missing]-d").unwrap(), "d");
    assert_eq!(test_param_expansion("S[empty]-d").unwrap(), "");
  }

  #[test]
  fn param_length() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("STR", VarKind::Str("hello".into()), VarFlags::empty())).unwrap();

    let result = test_param_expansion("#STR").unwrap();
    assert_eq!(result, "5");
  }

  #[test]
  fn param_substr() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("STR", VarKind::Str("hello world".into()), VarFlags::empty()))
      .unwrap();

    let result = test_param_expansion("STR:6").unwrap();
    assert_eq!(result, "world");
  }

  #[test]
  fn param_substr_len() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("STR", VarKind::Str("hello world".into()), VarFlags::empty()))
      .unwrap();

    let result = test_param_expansion("STR:0:5").unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn param_remove_shortest_prefix() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "PATH",
        VarKind::Str("/usr/local/bin".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let result = test_param_expansion("PATH#*/").unwrap();
    assert_eq!(result, "usr/local/bin");
  }

  #[test]
  fn param_remove_longest_prefix() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "PATH",
        VarKind::Str("/usr/local/bin".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let result = test_param_expansion("PATH##*/").unwrap();
    assert_eq!(result, "bin");
  }

  #[test]
  fn param_remove_shortest_suffix() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "FILE",
        VarKind::Str("file.tar.gz".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let result = test_param_expansion("FILE%.*").unwrap();
    assert_eq!(result, "file.tar");
  }

  #[test]
  fn param_remove_longest_suffix() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "FILE",
        VarKind::Str("file.tar.gz".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    let result = test_param_expansion("FILE%%.*").unwrap();
    assert_eq!(result, "file");
  }

  #[test]
  fn param_replace_first() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("STR", VarKind::Str("hello hello".into()), VarFlags::empty()))
      .unwrap();

    let result = test_param_expansion("STR/hello/world").unwrap();
    assert_eq!(result, "world hello");
  }

  #[test]
  fn param_replace_all() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("STR", VarKind::Str("hello hello".into()), VarFlags::empty()))
      .unwrap();

    let result = test_param_expansion("STR//hello/world").unwrap();
    assert_eq!(result, "world world");
  }

  #[test]
  fn param_indirect() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("REF", VarKind::Str("TARGET".into()), VarFlags::empty())).unwrap();
    Shed::vars_mut(|v| v.set_var("TARGET", VarKind::Str("value".into()), VarFlags::empty()))
      .unwrap();

    let result = test_param_expansion("!REF").unwrap();
    assert_eq!(result, "value");
  }

  #[test]
  fn param_set_default_assigns() {
    let _guard = TestGuard::new();

    let result = test_param_expansion("NEWVAR:=assigned").unwrap();
    assert_eq!(result, "assigned");

    // Verify it was actually set
    let val = var!("NEWVAR");
    assert_eq!(val, "assigned");
  }

  // ===================== Parameter Expansion with Escapes (TestGuard) =====================

  #[test]
  fn param_exp_prefix_removal_escaped() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("branch", VarKind::Str("## main".into()), VarFlags::empty()))
      .unwrap();

    test_input("echo \"${branch#\\#\\# }\"").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "main\n");
  }

  #[test]
  fn param_exp_suffix_removal_escaped() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "val",
        VarKind::Str("hello world!!".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();

    test_input("echo \"${val%\\!\\!}\"").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "hello world\n");
  }

  // A bare `(` in a strip pattern is a literal pattern char, not a subshell.
  // Regression: the token-level unescape consumed `(` as a subshell marker,
  // so `${v%(x)}` never matched (and in some cases ran the parenthesized
  // text as a command). Must work both unquoted and inside double quotes.
  #[test]
  fn param_exp_suffix_removal_bare_parens() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("v", VarKind::Str("abc(x)".into()), VarFlags::empty())).unwrap();

    test_input("echo ${v%(x)}").unwrap();
    assert_eq!(guard.read_output(), "abc\n");
  }

  #[test]
  fn param_exp_suffix_removal_bare_parens_double_quoted() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("v", VarKind::Str("abc(x)".into()), VarFlags::empty())).unwrap();

    test_input("echo \"${v%(x)}\"").unwrap();
    assert_eq!(guard.read_output(), "abc\n");
  }

  #[test]
  fn param_exp_prefix_removal_bare_parens() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("v", VarKind::Str("(x)abc".into()), VarFlags::empty())).unwrap();

    test_input("echo ${v#(x)}").unwrap();
    assert_eq!(guard.read_output(), "abc\n");
  }

  // A backslash-escaped `}` inside a `${...}` replace pattern is a literal
  // `}`, not the closing brace — both unquoted and inside double quotes. The
  // double-quoted case regressed because `read_dub_quote` neutralized escapes
  // by de-marking, which doesn't protect the char-driven `${...}` closer scan;
  // it now emits an ESCAPE marker for `}` while inside a `${...}`.
  #[test]
  fn param_exp_replace_escaped_brace_unquoted() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("u", VarKind::Str("a}b".into()), VarFlags::empty())).unwrap();

    test_input("echo ${u/\\}/_}").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  #[test]
  fn param_exp_replace_escaped_brace_double_quoted() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("u", VarKind::Str("a}b".into()), VarFlags::empty())).unwrap();

    test_input("echo \"${u/\\}/_}\"").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  // A `\}` *outside* any `${...}`, inside double quotes, keeps its backslash
  // (bash leaves `"\}"` literal) — the escape protection must stay scoped to
  // parameter expansions.
  #[test]
  fn double_quote_standalone_escaped_brace_keeps_backslash() {
    let guard = TestGuard::new();
    test_input("echo \"a\\}b\"").unwrap();
    assert_eq!(guard.read_output(), "a\\}b\n");
  }

  // A backslash-escaped `/` in a `${v/pat/rep}` pattern is a literal `/`, not
  // the pattern/replacement separator — both unquoted and double-quoted. Uses
  // the marker-aware `split_at_unescaped_markers` split so the escaped `/`
  // reaches the glob matcher instead of splitting the operand there.
  #[test]
  fn param_exp_replace_escaped_slash_unquoted() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("s", VarKind::Str("a/b".into()), VarFlags::empty())).unwrap();

    test_input("echo ${s/\\//_}").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  #[test]
  fn param_exp_replace_escaped_slash_double_quoted() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("s", VarKind::Str("a/b".into()), VarFlags::empty())).unwrap();

    test_input("echo \"${s/\\//_}\"").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  // A single quote inside a `${...}` protects a structural char (`}`, `/`) as
  // a quoted literal — even within double quotes, where a single quote is
  // otherwise literal. bash re-enables single-quote quoting inside `${...}`.
  #[test]
  fn param_exp_replace_single_quoted_brace_double_quoted() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("u", VarKind::Str("a}b".into()), VarFlags::empty())).unwrap();

    test_input("echo \"${u/'}'/_}\"").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  #[test]
  fn param_exp_replace_single_quoted_slash_double_quoted() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("s", VarKind::Str("a/b".into()), VarFlags::empty())).unwrap();

    test_input("echo \"${s/'/'/_}\"").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  // A single quote *outside* a `${...}` stays literal inside double quotes.
  #[test]
  fn double_quote_apostrophe_stays_literal() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("hi".into()), VarFlags::empty())).unwrap();

    test_input("echo \"${x}'s\"").unwrap();
    assert_eq!(guard.read_output(), "hi's\n");
  }

  // A `"..."` nested inside a `${...}` that is itself inside double quotes
  // opens a nested quoted region rather than closing the outer one, so the
  // replacement is taken as the (quote-stripped) inner text. Previously the
  // nested `"` terminated the expansion early, swallowing the rest.
  #[test]
  fn param_exp_replace_nested_double_quote() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("foo", VarKind::Str("bar".into()), VarFlags::empty())).unwrap();

    test_input("echo \"${foo/bar/\"biz\"}\"").unwrap();
    assert_eq!(guard.read_output(), "biz\n");
  }

  // A literal `'` inside that nested `"..."` must survive — the operand is
  // re-expanded, and the second unescape pass has to preserve the existing
  // quote-region markers rather than re-reading the `'` as a quote opener.
  #[test]
  fn param_exp_replace_nested_double_quote_with_apostrophe() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("foo", VarKind::Str("bar".into()), VarFlags::empty())).unwrap();

    test_input("echo \"${foo/bar/\"'biz\"}\"").unwrap();
    assert_eq!(guard.read_output(), "'biz\n");
  }

  // `$'...'` ANSI-C quoting is active inside a `${...}` even within the outer
  // double quotes (matching bash). Regression: the facet-C `'` handling made
  // the closing quote of `$'\n'` re-open a single-quote region and swallow the
  // `}` closer, so `"${s%%$'\n'*}"` expanded to empty.
  #[test]
  fn param_exp_ansi_c_quote_in_double_quoted_operand() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("s", VarKind::Str("first\nsecond".into()), VarFlags::empty()))
      .unwrap();

    test_input("printf '[%s]' \"${s%%$'\\n'*}\"").unwrap();
    assert_eq!(guard.read_output(), "[first]");
  }

  // A `$var` inside the nested `"..."` still expands.
  #[test]
  fn param_exp_replace_nested_double_quote_expands_var() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("foo", VarKind::Str("bar".into()), VarFlags::empty())).unwrap();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("XX".into()), VarFlags::empty())).unwrap();

    test_input("echo \"${foo/bar/\"$x\"}\"").unwrap();
    assert_eq!(guard.read_output(), "XX\n");
  }

  #[test]
  fn param_exp_quoted_glob_meta_is_literal() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("foo", VarKind::Str("ba*r".into()), VarFlags::empty())).unwrap();

    // "*" makes the asterisk literal — strips the literal "*r" suffix.
    test_input("echo ${foo%\"*\"r}").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "ba\n");
  }

  #[test]
  fn param_exp_unquoted_glob_meta_is_wildcard() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("foo", VarKind::Str("ba*r".into()), VarFlags::empty())).unwrap();

    // unquoted *r is a glob — shortest match is just "r".
    test_input("echo ${foo%*r}").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "ba*\n");
  }

  #[test]
  fn param_exp_backslash_glob_meta_is_literal() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("foo", VarKind::Str("ba*r".into()), VarFlags::empty())).unwrap();

    test_input("echo ${foo%\\*r}").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "ba\n");
  }

  #[test]
  fn param_exp_single_quoted_glob_meta_is_literal() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("foo", VarKind::Str("ba*r".into()), VarFlags::empty())).unwrap();

    test_input("echo ${foo%'*'r}").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "ba\n");
  }

  // ===================== Case conversion =====================

  fn set(name: &str, val: &str) {
    Shed::vars_mut(|v| v.set_var(name, VarKind::Str(val.into()), VarFlags::empty())).unwrap();
  }

  #[test]
  fn param_to_upper_all() {
    let _g = TestGuard::new();
    set("x", "hello world");
    assert_eq!(test_param_expansion("x^^").unwrap(), "HELLO WORLD");
  }

  #[test]
  fn param_to_upper_first() {
    let _g = TestGuard::new();
    set("x", "hello world");
    assert_eq!(test_param_expansion("x^").unwrap(), "Hello world");
  }

  #[test]
  fn param_to_upper_first_on_empty() {
    let _g = TestGuard::new();
    set("x", "");
    assert_eq!(test_param_expansion("x^").unwrap(), "");
  }

  #[test]
  fn param_to_lower_all() {
    let _g = TestGuard::new();
    set("x", "HELLO WORLD");
    assert_eq!(test_param_expansion("x,,").unwrap(), "hello world");
  }

  #[test]
  fn param_to_lower_first() {
    let _g = TestGuard::new();
    set("x", "HELLO WORLD");
    assert_eq!(test_param_expansion("x,").unwrap(), "hELLO WORLD");
  }

  // ===================== SetDefault (with colon) =====================

  #[test]
  fn param_set_default_unset_or_null_when_unset() {
    let _g = TestGuard::new();
    let result = test_param_expansion("NEWVAR:=defaultval").unwrap();
    assert_eq!(result, "defaultval");
    // Side effect: variable should now be set.
    assert_eq!(var!("NEWVAR"), "defaultval");
  }

  #[test]
  fn param_set_default_unset_or_null_when_null() {
    let _g = TestGuard::new();
    set("EMPTY", "");
    let result = test_param_expansion("EMPTY:=fallback").unwrap();
    assert_eq!(result, "fallback");
    assert_eq!(var!("EMPTY"), "fallback");
  }

  #[test]
  fn param_set_default_unset_or_null_when_set_no_op() {
    let _g = TestGuard::new();
    set("x", "original");
    let result = test_param_expansion("x:=replacement").unwrap();
    assert_eq!(result, "original");
    assert_eq!(var!("x"), "original");
  }

  // ===================== AltSetNotNull edge: var unset returns empty =====================

  #[test]
  fn param_alt_set_not_null_unset_returns_empty() {
    let _g = TestGuard::new();
    let result = test_param_expansion("UNSET:+alt").unwrap();
    assert_eq!(result, "");
  }

  #[test]
  fn param_alt_set_not_null_null_returns_empty() {
    let _g = TestGuard::new();
    set("EMPTY", "");
    let result = test_param_expansion("EMPTY:+alt").unwrap();
    assert_eq!(result, "");
  }

  // ===================== ErrUnsetOrNull =====================

  #[test]
  fn param_err_unset_or_null_when_unset() {
    let _g = TestGuard::new();
    let result = test_param_expansion("UNSET:?missing!");
    assert!(result.is_err());
  }

  #[test]
  fn param_err_unset_or_null_when_null() {
    let _g = TestGuard::new();
    set("EMPTY", "");
    let result = test_param_expansion("EMPTY:?cannot be empty");
    assert!(result.is_err());
  }

  #[test]
  fn param_err_unset_or_null_when_set_passes_through() {
    let _g = TestGuard::new();
    set("x", "value");
    let result = test_param_expansion("x:?should not fire").unwrap();
    assert_eq!(result, "value");
  }

  #[test]
  fn param_err_unset_when_unset() {
    let _g = TestGuard::new();
    let result = test_param_expansion("UNSET?missing");
    assert!(result.is_err());
  }

  // ===================== Slice out-of-bounds =====================

  #[test]
  fn param_substr_offset_beyond_length() {
    let _g = TestGuard::new();
    set("x", "hi");
    let result = test_param_expansion("x:99").unwrap();
    // Skipping past the end yields an empty string, matching bash
    // behavior. Previously a buggy fallback returned the whole value.
    assert_eq!(result, "");
  }

  #[test]
  fn param_substr_len_beyond_end() {
    let _g = TestGuard::new();
    set("x", "ab");
    let result = test_param_expansion("x:0:99").unwrap();
    assert_eq!(result, "ab");
  }

  // ===================== ReplacePrefix / ReplaceSuffix (execution) =====================

  #[test]
  fn param_replace_prefix_matches() {
    let _g = TestGuard::new();
    set("x", "hello world");
    let result = test_param_expansion("x/#hello/HI").unwrap();
    assert_eq!(result, "HI world");
  }

  #[test]
  fn param_replace_prefix_no_match() {
    let _g = TestGuard::new();
    set("x", "world hello");
    let result = test_param_expansion("x/#hello/HI").unwrap();
    assert_eq!(result, "world hello");
  }

  #[test]
  fn param_replace_suffix_matches() {
    let _g = TestGuard::new();
    set("x", "hello world");
    let result = test_param_expansion("x/%world/EARTH").unwrap();
    assert_eq!(result, "hello EARTH");
  }

  #[test]
  fn param_replace_suffix_no_match() {
    let _g = TestGuard::new();
    set("x", "world hello");
    let result = test_param_expansion("x/%world/EARTH").unwrap();
    assert_eq!(result, "world hello");
  }

  // ===================== VarNamesWithPrefix =====================

  #[test]
  fn param_var_names_with_prefix_returns_empty_for_glob_form() {
    // Pinning current behavior: the parser keeps the trailing `*` as
    // part of the prefix string, so `starts_with("PREFIX_*")` only
    // matches names that literally contain `*` (i.e., nothing real).
    // If/when the glob-prefix logic is fixed to strip the `*`, this
    // test should switch to checking that PREFIX_one and PREFIX_two
    // are returned.
    let _g = TestGuard::new();
    set("PREFIX_one", "1");
    set("PREFIX_two", "2");
    let result = test_param_expansion("!PREFIX_*").unwrap();
    assert_eq!(result, "");
  }

  // ===================== nounset error path =====================

  #[test]
  fn param_nounset_unset_var_errors() {
    let _g = TestGuard::new();
    Shed::shopts_mut(|o| o.set.nounset = true);
    // Bare expansion of an unset var with `set -u` should error.
    let result = test_param_expansion("DEFINITELY_NOT_SET_zzz");
    assert!(result.is_err());
  }

  // ===================== Length with array index branches =====================

  #[test]
  fn param_length_of_array_size_via_at() {
    let _g = TestGuard::new();
    test_input("arr=(a b c d)").unwrap();
    // `${#arr[@]}` returns the element count.
    let result = test_param_expansion("#arr[@]").unwrap();
    assert_eq!(result, "4");
  }

  #[test]
  fn param_length_of_array_element() {
    let _g = TestGuard::new();
    test_input("arr=(hello world!)").unwrap();
    // `${#arr[0]}` returns the length of the first element.
    let result = test_param_expansion("#arr[0]").unwrap();
    assert_eq!(result, "5");
  }

  // ===================== Exit status (issue #115) =====================
  // Parameter expansion never sets `$?`. Every other POSIX shell returns 0 for
  // an assignment-only command regardless of whether a pattern matched, and
  // POSIX §2.9.1 mandates it. These assert the expansion leaves `$?` untouched.

  #[test]
  fn param_strip_no_match_leaves_status_untouched() {
    let _g = TestGuard::new();
    Shed::set_status(0);
    set("x", "abc");
    test_param_expansion("x%z").unwrap(); // no match
    assert_eq!(Shed::get_status(), 0);
  }

  #[test]
  fn param_strip_match_leaves_status_untouched() {
    let _g = TestGuard::new();
    Shed::set_status(42);
    set("x", "abc");
    test_param_expansion("x%c").unwrap(); // matches
    assert_eq!(Shed::get_status(), 42, "expansion must not touch $?");
  }

  #[test]
  fn param_uppercase_leaves_status_untouched() {
    let _g = TestGuard::new();
    Shed::set_status(7);
    set("x", "hello");
    test_param_expansion("x^^").unwrap();
    assert_eq!(Shed::get_status(), 7);
  }

  // ===================== UTF-8 boundary regression =====================
  // Pattern removal must iterate over char boundaries, not byte indices,
  // or it panics on strings containing multi-byte characters.

  #[test]
  fn rem_shortest_prefix_handles_multibyte() {
    let _g = TestGuard::new();
    set("x", "discount — does it apply?");
    let result = test_param_expansion("x#discount — ").unwrap();
    assert_eq!(result, "does it apply?");
  }

  #[test]
  fn rem_longest_prefix_handles_multibyte() {
    let _g = TestGuard::new();
    set("x", "café au lait");
    let result = test_param_expansion("x##*é ").unwrap();
    assert_eq!(result, "au lait");
  }

  #[test]
  fn rem_shortest_suffix_handles_multibyte() {
    let _g = TestGuard::new();
    set("x", "café au lait");
    let result = test_param_expansion("x% au lait").unwrap();
    assert_eq!(result, "café");
  }

  #[test]
  fn rem_longest_suffix_handles_multibyte() {
    let _g = TestGuard::new();
    set("x", "Müller, Hans");
    let result = test_param_expansion("x%%, *").unwrap();
    assert_eq!(result, "Müller");
  }

  // Substring slicing must be char-based, not byte-based. With byte-based
  // slicing, indices that landed inside a multi-byte character returned
  // None from str::get and the fallback handed back the entire string,
  // catastrophically polluting any caller iterating char-by-char.

  #[test]
  fn slice_open_handles_multibyte_char() {
    let _g = TestGuard::new();
    set("x", "Müller");
    // Skip the first char, take the rest.
    let result = test_param_expansion("x:1").unwrap();
    assert_eq!(result, "üller");
  }

  #[test]
  fn slice_closed_picks_one_char_at_each_position() {
    let _g = TestGuard::new();
    set("x", "Müller");
    // Walking char-by-char through a string with multi-byte chars
    // must always return exactly one char, never the whole string.
    for (i, expected) in ["M", "ü", "l", "l", "e", "r"].iter().enumerate() {
      let result = test_param_expansion(&format!("x:{i}:1")).unwrap();
      assert_eq!(result.to_str_lossy(), *expected, "at index {i}");
    }
  }

  #[test]
  fn slice_closed_inside_japanese() {
    let _g = TestGuard::new();
    set("x", "田中さん");
    // Each char is 3 bytes in UTF-8.
    assert_eq!(test_param_expansion("x:0:1").unwrap(), "田");
    assert_eq!(test_param_expansion("x:1:1").unwrap(), "中");
    assert_eq!(test_param_expansion("x:2:2").unwrap(), "さん");
  }

  // ─── pattern operands must not treat `(` as a subshell ────────────
  // A bare `(` in a strip/replace pattern was being consumed by the
  // subshell recognizer (ExpandFlags::SUBSHELL) before reaching the glob
  // matcher, so `${v%(x)}` never matched. Only `$(...)` should run.
  #[test]
  fn rem_shortest_suffix_strips_literal_parens() {
    let _g = TestGuard::new();
    set("x", "abc(x)");
    let result = test_param_expansion("x%(x)").unwrap();
    assert_eq!(result, "abc");
  }

  #[test]
  fn rem_shortest_prefix_strips_literal_parens() {
    let _g = TestGuard::new();
    set("x", "(x)abc");
    let result = test_param_expansion("x#(x)").unwrap();
    assert_eq!(result, "abc");
  }

  #[test]
  fn rem_longest_suffix_strips_literal_parens_glob() {
    let _g = TestGuard::new();
    set("x", "abc(x)(x)");
    let result = test_param_expansion("x%%(x)").unwrap();
    assert_eq!(result, "abc(x)");
  }

  #[test]
  fn rem_longest_prefix_strips_literal_parens_glob() {
    let _g = TestGuard::new();
    set("x", "(x)(x)abc");
    let result = test_param_expansion("x##(x)").unwrap();
    assert_eq!(result, "(x)abc");
  }

  #[test]
  fn rem_suffix_glob_with_open_paren_matches() {
    let _g = TestGuard::new();
    set("x", "abc(x)");
    let result = test_param_expansion("x%(*)").unwrap();
    assert_eq!(result, "abc");
  }

  #[test]
  fn replace_first_match_with_parens_in_search() {
    let _g = TestGuard::new();
    set("x", "abc(x)");
    let result = test_param_expansion("x/abc(x)/X").unwrap();
    assert_eq!(result, "X");
  }

  #[test]
  fn replace_suffix_with_parens_in_search() {
    let _g = TestGuard::new();
    set("x", "abc(x)");
    // Anchored suffix replace uses `${v/%pat/rep}`.
    let result = test_param_expansion("x/%abc(x)/X").unwrap();
    assert_eq!(result, "X");
  }

  #[test]
  fn replace_first_match_with_parens_in_replacement() {
    let _g = TestGuard::new();
    set("x", "abc");
    // A bare `(` in the replacement is literal, not a subshell.
    let result = test_param_expansion("x/abc/(x)").unwrap();
    assert_eq!(result, "(x)");
  }
}
