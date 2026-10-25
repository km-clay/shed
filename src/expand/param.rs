use std::rc::Rc;

use crate::{
  expand::{Words, arithmetic::Num, glob::Pattern},
  set_var,
  state::params,
};
use bstr::ByteSlice;
use itertools::Itertools;

use crate::{
  eval::lex::{Span, TkFlags},
  expand::{Expander, arithmetic, stream::SegStream, var},
  sherr, shopt,
  state::{
    Shed,
    scopes::ScopeStack,
    vars::{ArrIndex, ShellParam, VarKind, VarName, VarStr},
  },
  util::error::ShResult,
  var, varstr,
};

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
  SliceOpen(SegStream),                    // :offset
  SliceClosed(SegStream, SegStream),       // :offset:length
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
    None       => (rest, SegStream::new()),
  }
}

pub(crate) fn parse_param_exp(body: &SegStream) -> ShResult<ParamExp> {
  use ParamExp as PE;

  let parse_err = || Err(sherr!(SyntaxErr, "Invalid parameter expansion",));

  // The operator is markerless ASCII at the very front of the body, so match it
  // against the leading bytes; the operand (which may carry markers) is the
  // remainder stream after the operator is peeled off.
  let lead      = body.as_bytes();
  let lead_run  = body.leading_bytes();

  match lead {
    b"^^" => return Ok(PE::ToUpperAll),
    b"^"  => return Ok(PE::ToUpperFirst),
    b",," => return Ok(PE::ToLowerAll),
    b","  => return Ok(PE::ToLowerFirst),
    _     => {}
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
    match rest.as_bytes().first() {
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

  // The `:` operators are handled above, so a
  // remaining leading `:` is a substring slice.
  if lead.first() == Some(&b':') {
    let operand = body.split_off_front(1).1;
    return Ok(match operand.split_once_unescaped(b':') {
      Some((offset, length)) => PE::SliceClosed(offset, length),
      None => PE::SliceOpen(operand),
    });
  }

  parse_err()
}

/// Resolve one substring index (offset or length). The operand may carry
/// variable/command markers and is an arithmetic expression, so it is
/// first `$`-expanded, and then evaluated as
/// arithmetic
fn eval_slice_index(
  span: Option<Span>,
  stream: &SegStream,
  allow_side_effects: bool,
) -> ShResult<Num> {
  let expanded = var::expand_raw_inner(span, &mut stream.cursor(), allow_side_effects, false)?;
  let bytes    = expanded.as_bytes();
  // An empty index (e.g. an unset variable) is 0, as in an empty arith context.
  if bytes.iter().all(u8::is_ascii_whitespace) {
    return Ok(0);
  }
  arithmetic::eval_expanded(bytes)
}

/// Resolve a possibly-negative substring offset against a char count `n`.
/// A negative offset counts from the end; results are clamped to `[0, n]`.
fn resolve_offset(pos: Num, n: Num) -> Num {
  if pos < 0 {
    (n + pos).max(0)
  } else {
    pos.min(n)
  }
}

fn align_slice(start: Num, len: Option<Num>, n: Num) -> ShResult<(usize, usize)> {
  let start = resolve_offset(start, n);
  let end = match len {
    Some(l) if l < 0 => n + l,
    Some(l)          => (start + l).min(n),
    None             => n,
  };

  if end < start {
    return Err(sherr!(ExecFail, "substring expression < 0"));
  }
  Ok((start as usize, (end - start) as usize))
}

fn slice_chars(value: &VarStr, start: Num, len: Option<Num>) -> ShResult<VarStr> {
  let bytes        = value.as_bytes();
  let starts       = bytes.char_indices().map(|(s, _, _)| s).collect_vec();
  let (skip, take) = align_slice(start, len, starts.len() as Num)?;

  let start_byte   = starts.get(skip).copied().unwrap_or(bytes.len());
  let end_byte     = starts.get(skip + take).copied().unwrap_or(bytes.len());

  value
    .try_slice(start_byte..end_byte)
    .ok_or_else(|| sherr!(ExecFail, "substring expression < 0"))
}

fn slice_words(words: Words, start: Num, len: Option<Num>) -> ShResult<Words> {
  let (skip, take) = align_slice(start, len, words.len() as Num)?;

  let slice        = words.into_iter().skip(skip).take(take).collect();

  Ok(slice)
}

fn slice_param(parsed: &VarName, start: Num, len: Option<Num>) -> ShResult<SegStream> {
  if parsed.index().is_none() && matches!(parsed.name(), "@" | "*") {
    let words = Shed::vars(|v| {
      v.sh_argv_scope()
        .sh_argv()
        .iter()
        .cloned()
        .collect::<Words>()
    });
    let sliced = slice_words(words, start, len)?;

    if parsed.name() == "*" {
      Ok(SegStream::from(
        sliced.join_with(&params::get_separator().to_str_lossy()),
      ))
    } else {
      Ok(var::emit_words(parsed, &sliced))
    }
  } else {
    let value = Shed::vars(|v| v.resolve_words(parsed).unwrap_or_default()).join_with("");

    slice_chars(&value, start, len).map(SegStream::from)
  }
}

#[derive(Debug, Clone, Copy)]
enum TrimLen {
  Longest,
  Shortest,
}

#[derive(Debug, Clone, Copy)]
enum Trim {
  Prefix(TrimLen),
  Suffix(TrimLen),
}

fn trim_param(
  parsed: &VarName,
  words: Words,
  pat_src: SegStream,
  trim: Trim,
) -> ShResult<SegStream> {
  let expanded = Expander::from_raw_no_brace_pattern(pat_src, TkFlags::empty())
    .no_glob()
    .expand_for_glob()?;

  let pattern = Shed::meta_mut(|m| m.get_glob(&expanded));

  let trimmed = words.map_content(|word| {
    let range = {
      let hit = match trim {
        Trim::Prefix(TrimLen::Longest)  => pattern.match_longest_prefix(&word),
        Trim::Prefix(TrimLen::Shortest) => pattern.match_shortest_prefix(&word),
        Trim::Suffix(TrimLen::Longest)  => pattern.match_longest_suffix(&word),
        Trim::Suffix(TrimLen::Shortest) => pattern.match_shortest_suffix(&word),
      };

      match (hit, trim) {
        (Some(hit_len), Trim::Prefix(_)) => hit_len..word.len(),
        (Some(hit_len), Trim::Suffix(_)) => 0..word.len() - hit_len,

        (None, _) => 0..word.len(),
      }
    };

    VarStr::from(&word[range])
  });

  Ok(var::emit_words(parsed, &trimmed))
}

#[derive(Debug, Clone, Copy)]
enum ReplaceMode {
  First,
  All,
  Prefix,
  Suffix,
}

#[derive(Debug, Clone)]
struct ReplaceSpec {
  pat    : Rc<Pattern>,
  replace: VarStr,
}

impl ReplaceSpec {
  fn from_parts(
    search: SegStream,
    replace: SegStream,
    mode: ReplaceMode,
  ) -> ShResult<Option<Self>> {
    let expanded_search = Expander::from_raw_pattern(search, TkFlags::empty())
      .no_glob()
      .expand_for_glob()?;

    if expanded_search.is_empty() && matches!(mode, ReplaceMode::First | ReplaceMode::All) {
      return Ok(None);
    }

    let expanded_replace = Expander::from_raw_pattern(replace, TkFlags::empty())
      .no_glob()
      .expand_for_glob()?;

    Ok(Some(Self {
      pat    : Shed::meta_mut(|m| m.get_glob(&expanded_search)),
      replace: expanded_replace,
    }))
  }
}

fn replace_in(haystack: &VarStr, spec: ReplaceSpec, mode: ReplaceMode) -> Option<VarStr> {
  let ReplaceSpec { pat, replace } = spec;

  match mode {
    ReplaceMode::First => {
      let mut out: Vec<u8> = Vec::with_capacity(haystack.len());
      let     (start, end) = pat.find(haystack, 0)?;

      out.extend_from_slice(&haystack[..start]);
      out.extend_from_slice(&replace);
      out.extend_from_slice(&haystack[end..]);

      Some(VarStr::from(out))
    }
    ReplaceMode::All => {
      let mut out  = Vec::new();
      let mut last = 0;
      let mut from = 0;
      let mut hit  = false;

      while let Some((start, end)) = pat.find(haystack, from) {
        hit = true;
        out.extend_from_slice(&haystack[last..start]);
        out.extend_from_slice(&replace);
        last = end;
        from = if end > start { end } else { end + 1 };
      }

      hit.then(|| {
        out.extend_from_slice(&haystack[last..]);
        VarStr::from(out)
      })
    }
    ReplaceMode::Prefix => {
      let     len = pat.match_longest_prefix(haystack)?;
      let mut out = replace;

      out.push_slice(&haystack[len..]);

      Some(out)
    }
    ReplaceMode::Suffix => {
      let     len = pat.match_longest_suffix(haystack)?;
      let     pos = haystack.len() - len;
      let mut out = haystack.clone();

      out.truncate(pos);
      out.push_slice(&replace);

      Some(out)
    }
  }
}

fn replace_param(
  parsed: &VarName,
  words: Words,
  search: SegStream,
  replace: SegStream,
  mode: ReplaceMode,
) -> ShResult<SegStream> {
  let Some(spec) = ReplaceSpec::from_parts(search, replace, mode)? else {
    return Ok(var::emit_words(parsed, &words));
  };

  let replaced = words.map_content(|word| replace_in(&word, spec.clone(), mode).unwrap_or(word));

  Ok(var::emit_words(parsed, &replaced))
}

enum Case {
  Upper,
  Lower,
}

fn change_first_case(parsed: &VarName, words: Words, case: Case) -> SegStream {
  let mapped = words.map_content(|v| {
    if v.is_empty() {
      return v;
    }

    let first = v.try_slice(..1).unwrap();
    let rest  = v.try_slice(1..).unwrap();

    match case {
      Case::Upper => first.to_ascii_uppercase().chain(rest),
      Case::Lower => first.to_ascii_lowercase().chain(rest),
    }
  });

  var::emit_words(parsed, &mapped)
}

fn null_expand(parsed: &VarName) -> SegStream {
  if parsed.is_all_split() {
    var::emit_words(parsed, &Words::default())
  } else {
    SegStream::new()
  }
}

fn handle_len_expansion(raw: &str, allow_side_effects: bool) -> ShResult<SegStream> {
  let var_spec = raw.strip_prefix('#').unwrap();
  if var_spec.is_empty() || var_spec == "*" || var_spec == "@" {
    // this is either asking for the `#` parameter directly, or asking for the length
    // of `$*` or `$@`. All of these refer to the same thing: the number of positional
    // arguments.
    return Ok(Shed::vars(|v| v.get_param(ShellParam::ArgCount)).into());
  }

  if let Ok(param) = var_spec.parse::<ShellParam>() {
    let len = Shed::vars(|v| v.try_get_param(param)).map_or(0, |val| val.chars().count());
    return Ok(varstr!("{len}").into());
  }

  let parsed = VarName::parse(var_spec, allow_side_effects)?;
  if let Some(idx) = parsed.index() {
    match idx {
      ArrIndex::AllSplit | ArrIndex::AllJoined | ArrIndex::ArgCount => {
        let var = Shed::vars(|v| v.get_var_meta(parsed.name()));
        let len = match var.kind() {
          VarKind::Arr(items)      => items.len(),
          VarKind::AssocArr(items) => items.len(),
          _ => 0,
        };

        return Ok(varstr!("{len}").into());
      }
      _ => {
        let val = Shed::vars(|v| v.index_var(parsed.name(), idx))?;
        let len = val.chars().count();
        return Ok(varstr!("{len}").into());
      }
    }
  }

  let var = Shed::vars(|v| v.get_var_meta(var_spec));
  let len = match var.kind() {
    VarKind::Magic(func)     => func().unwrap_or_default().chars().count(),
    VarKind::Str(v)          => v.chars().count(),
    VarKind::Int(i)          => varstr!("{i}").chars().count(),
    VarKind::Arr(items)      => items.len(),
    VarKind::AssocArr(items) => items.len(),
    VarKind::Unset           => 0,
  };

  Ok(varstr!("{len}").into())
}

pub(crate) fn perform_param_expansion(
  span: Option<Span>,
  body: &SegStream,
  allow_side_effects: bool,
) -> ShResult<SegStream> {
  if body.as_bytes().starts_with(b"#") {
    return handle_len_expansion(&body.content().to_str_lossy(), allow_side_effects);
  }

  let (mut parsed, consumed) = VarName::parse_stream(body, allow_side_effects)?;

  if matches!(parsed.index(), Some(ArrIndex::Raw(_))) {
    let tag = Shed::vars(|v| v.try_get_var_kind_tag(parsed.name()));
    if let Some(tag) = tag {
      let resolved = parsed.index().unwrap().clone().resolve_for(tag)?;
      parsed.set_index(resolved);
    }
  }

  let get     = |v: &ScopeStack| v.resolve_words(&parsed).unwrap_or_default();
  let try_get = |v: &ScopeStack| v.resolve_words(&parsed).filter(|w| !w.is_empty());
  let operand = body.split_off_front(consumed).1;

  let Ok(expansion) = parse_param_exp(&operand) else {
    if let Some(fields) = Shed::vars(|v| v.split_values(&parsed)) {
      return Ok(var::emit_words(&parsed, &fields));
    }
    let words = Shed::vars(try_get);
    if words.is_none() && shopt!(set.nounset) {
      return Err(sherr!(NotFound, "Variable '{}' is not set", parsed.name()));
    }

    return Ok(var::emit_words(&parsed, &words.unwrap_or_default()));
  };

  match expansion {
    ParamExp::ToUpperFirst => Ok(change_first_case(&parsed, Shed::vars(get), Case::Upper)),
    ParamExp::ToLowerFirst => Ok(change_first_case(&parsed, Shed::vars(get), Case::Lower)),

    ParamExp::ToUpperAll => {
      let words  = Shed::vars(get);
      let mapped = words.map_content(|c| c.to_ascii_uppercase());

      Ok(var::emit_words(&parsed, &mapped))
    }
    ParamExp::ToLowerAll => {
      let words  = Shed::vars(get);
      let mapped = words.map_content(|c| c.to_ascii_lowercase());

      Ok(var::emit_words(&parsed, &mapped))
    }
    ParamExp::DefaultUnsetOrNull(default) => match Shed::vars(try_get).filter(|v| !v.empty_param())
    {
      Some(val) => Ok(var::emit_words(&parsed, &val)),
      None      => var::expand_raw_inner(span, &mut default.cursor(), allow_side_effects, false),
    },
    ParamExp::DefaultUnset(default) => match Shed::vars(try_get) {
      Some(val) => Ok(var::emit_words(&parsed, &val)),
      None      => var::expand_raw_inner(span, &mut default.cursor(), allow_side_effects, false),
    },
    ParamExp::SetDefaultUnsetOrNull(default) => {
      match Shed::vars(try_get).filter(|v| !v.empty_param()) {
        Some(val) => Ok(var::emit_words(&parsed, &val)),
        None => {
          let expanded =
            var::expand_raw_inner(span, &mut default.cursor(), allow_side_effects, false)?;
          if allow_side_effects {
            let stored = VarStr::from(expanded.as_bytes());
            set_var!(parsed.name(), VarKind::string(stored))?;
          }
          Ok(expanded)
        }
      }
    }
    ParamExp::SetDefaultUnset(default) => match Shed::vars(try_get) {
      Some(val) => Ok(var::emit_words(&parsed, &val)),
      None => {
        let expanded =
          var::expand_raw_inner(span, &mut default.cursor(), allow_side_effects, false)?;
        if allow_side_effects {
          let stored = VarStr::from(expanded.as_bytes());
          set_var!(parsed.name(), VarKind::string(stored))?;
        }
        Ok(expanded)
      }
    },
    ParamExp::AltSetNotNull(alt) => match Shed::vars(try_get).filter(|v| !v.empty_param()) {
      Some(_) => var::expand_raw_inner(span, &mut alt.cursor(), allow_side_effects, false),
      None    => Ok(null_expand(&parsed)),
    },
    ParamExp::AltNotNull(alt) => match Shed::vars(try_get) {
      Some(_) => var::expand_raw_inner(span, &mut alt.cursor(), allow_side_effects, false),
      None    => Ok(null_expand(&parsed)),
    },
    ParamExp::ErrUnsetOrNull(err) => match Shed::vars(try_get).filter(|v| !v.empty_param()) {
      Some(val) => Ok(var::emit_words(&parsed, &val)),
      None => {
        if !allow_side_effects {
          return Ok(SegStream::new());
        }
        let expanded = var::expand_raw_inner(span, &mut err.cursor(), allow_side_effects, false)?;
        Err(sherr!(ExecFail, "{}", String::from_utf8_lossy(expanded.as_bytes())))
      }
    },
    ParamExp::ErrUnset(err) => match Shed::vars(try_get) {
      Some(val) => Ok(var::emit_words(&parsed, &val)),
      None => {
        if !allow_side_effects {
          return Ok(SegStream::new());
        }
        let expanded = var::expand_raw_inner(span, &mut err.cursor(), allow_side_effects, false)?;
        Err(sherr!(ExecFail, "{}", String::from_utf8_lossy(expanded.as_bytes())))
      }
    },
    ParamExp::SliceOpen(offset) => {
      let pos = eval_slice_index(span, &offset, allow_side_effects)?;
      slice_param(&parsed, pos, None)
    }
    ParamExp::SliceClosed(offset, length) => {
      let pos = eval_slice_index(span, &offset, allow_side_effects)?;
      let len = eval_slice_index(span, &length, allow_side_effects)?;
      slice_param(&parsed, pos, Some(len))
    }

    ParamExp::RemShortestPrefix(p) => {
      trim_param(&parsed, Shed::vars(get), p, Trim::Prefix(TrimLen::Shortest))
    }
    ParamExp::RemLongestPrefix(p) => {
      trim_param(&parsed, Shed::vars(get), p, Trim::Prefix(TrimLen::Longest))
    }
    ParamExp::RemShortestSuffix(p) => {
      trim_param(&parsed, Shed::vars(get), p, Trim::Suffix(TrimLen::Shortest))
    }
    ParamExp::RemLongestSuffix(p) => {
      trim_param(&parsed, Shed::vars(get), p, Trim::Suffix(TrimLen::Longest))
    }

    ParamExp::ReplaceFirstMatch(s, r) => {
      replace_param(&parsed, Shed::vars(get), s, r, ReplaceMode::First)
    }
    ParamExp::ReplaceAllMatches(s, r) => {
      replace_param(&parsed, Shed::vars(get), s, r, ReplaceMode::All)
    }
    ParamExp::ReplacePrefix(s, r) => {
      replace_param(&parsed, Shed::vars(get), s, r, ReplaceMode::Prefix)
    }
    ParamExp::ReplaceSuffix(s, r) => {
      replace_param(&parsed, Shed::vars(get), s, r, ReplaceMode::Suffix)
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
        let var_name = inner.find('[').map_or(&inner[..], |pos| &inner[..pos]);

        if inner.contains("[*]") {
          return Shed::vars(|v| v.get_array_keys(var_name)).map(Into::into);
        }

        let name = VarName::parse(&inner, allow_side_effects)?;
        match Shed::vars(|v| v.split_keys(&name)) {
          Some(fields) => Ok(var::emit_words(&parsed, &fields)),
          None         => unreachable!("inner has an index, so it always returns fields"),
        }
      } else {
        let inner_name = VarName::parse(&inner, allow_side_effects)?;
        let value = Shed::vars(|v| v.resolve_words(&inner_name).unwrap_or_default()).as_one_word();
        Ok(var!(&value.to_str_lossy()).into())
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::set_var;
  use crate::state::{Shed, vars::VarFlags, vars::VarKind};
  use crate::tests::testutil::{TestGuard, test_input};

  fn test_param_parse(val: &str) -> ParamExp {
    parse_param_exp(&SegStream::from_bytes(val.as_bytes())).unwrap()
  }

  fn test_param_expansion(val: &str) -> ShResult<VarStr> {
    perform_param_expansion(None, &SegStream::from_bytes(val.as_bytes()), true)
      .map(|s| VarStr::from(s.as_bytes()))
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
    assert!(matches!(exp, ParamExp::SliceOpen(ref s) if s == "2"));
  }

  #[test]
  fn param_exp_substr_len() {
    let exp = test_param_parse(":1:3");
    assert!(matches!(exp, ParamExp::SliceClosed(ref a, ref b) if a == "1" && b == "3"));
  }

  #[test]
  fn param_exp_substr_negative_offset_parses() {
    let exp = test_param_parse(": -2");
    assert!(matches!(exp, ParamExp::SliceOpen(ref s) if s == " -2"));
  }

  #[test]
  fn param_exp_substr_paren_negative_offset_parses() {
    let exp = test_param_parse(":(-2)");
    assert!(matches!(exp, ParamExp::SliceOpen(ref s) if s == "(-2)"));
  }

  #[test]
  fn param_exp_substr_negative_length_parses() {
    let exp = test_param_parse(":1:-1");
    assert!(matches!(exp, ParamExp::SliceClosed(ref a, ref b) if a == "1" && b == "-1"));
  }

  fn set_v_abcdef() {
    set_var!("V", VarKind::Str("abcdef".into())).unwrap();
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
    set_var!("SET", VarKind::Str("value".into())).unwrap();

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
    set_var!("SET", VarKind::Str("value".into())).unwrap();

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
    set_var!("STR", VarKind::Str("hello".into())).unwrap();

    let result = test_param_expansion("#STR").unwrap();
    assert_eq!(result, "5");
  }

  #[test]
  fn param_substr() {
    let _guard = TestGuard::new();
    set_var!("STR", VarKind::Str("hello world".into())).unwrap();

    let result = test_param_expansion("STR:6").unwrap();
    assert_eq!(result, "world");
  }

  #[test]
  fn param_substr_len() {
    let _guard = TestGuard::new();
    set_var!("STR", VarKind::Str("hello world".into())).unwrap();

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
    set_var!("STR", VarKind::Str("hello hello".into())).unwrap();

    let result = test_param_expansion("STR/hello/world").unwrap();
    assert_eq!(result, "world hello");
  }

  #[test]
  fn param_replace_all() {
    let _guard = TestGuard::new();
    set_var!("STR", VarKind::Str("hello hello".into())).unwrap();

    let result = test_param_expansion("STR//hello/world").unwrap();
    assert_eq!(result, "world world");
  }

  #[test]
  fn param_indirect() {
    let _guard = TestGuard::new();
    set_var!("REF", VarKind::Str("TARGET".into())).unwrap();
    set_var!("TARGET", VarKind::Str("value".into())).unwrap();

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
    set_var!("branch", VarKind::Str("## main".into())).unwrap();

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
    set_var!("v", VarKind::Str("abc(x)".into())).unwrap();

    test_input("echo ${v%(x)}").unwrap();
    assert_eq!(guard.read_output(), "abc\n");
  }

  #[test]
  fn param_exp_suffix_removal_bare_parens_double_quoted() {
    let guard = TestGuard::new();
    set_var!("v", VarKind::Str("abc(x)".into())).unwrap();

    test_input("echo \"${v%(x)}\"").unwrap();
    assert_eq!(guard.read_output(), "abc\n");
  }

  #[test]
  fn param_exp_prefix_removal_bare_parens() {
    let guard = TestGuard::new();
    set_var!("v", VarKind::Str("(x)abc".into())).unwrap();

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
    set_var!("u", VarKind::Str("a}b".into())).unwrap();

    test_input("echo ${u/\\}/_}").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  #[test]
  fn param_exp_replace_escaped_brace_double_quoted() {
    let guard = TestGuard::new();
    set_var!("u", VarKind::Str("a}b".into())).unwrap();

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
    set_var!("s", VarKind::Str("a/b".into())).unwrap();

    test_input("echo ${s/\\//_}").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  #[test]
  fn param_exp_replace_escaped_slash_double_quoted() {
    let guard = TestGuard::new();
    set_var!("s", VarKind::Str("a/b".into())).unwrap();

    test_input("echo \"${s/\\//_}\"").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  // A single quote inside a `${...}` protects a structural char (`}`, `/`) as
  // a quoted literal — even within double quotes, where a single quote is
  // otherwise literal. bash re-enables single-quote quoting inside `${...}`.
  #[test]
  fn param_exp_replace_single_quoted_brace_double_quoted() {
    let guard = TestGuard::new();
    set_var!("u", VarKind::Str("a}b".into())).unwrap();

    test_input("echo \"${u/'}'/_}\"").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  #[test]
  fn param_exp_replace_single_quoted_slash_double_quoted() {
    let guard = TestGuard::new();
    set_var!("s", VarKind::Str("a/b".into())).unwrap();

    test_input("echo \"${s/'/'/_}\"").unwrap();
    assert_eq!(guard.read_output(), "a_b\n");
  }

  // A single quote *outside* a `${...}` stays literal inside double quotes.
  #[test]
  fn double_quote_apostrophe_stays_literal() {
    let guard = TestGuard::new();
    set_var!("x", VarKind::Str("hi".into())).unwrap();

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
    set_var!("foo", VarKind::Str("bar".into())).unwrap();

    test_input("echo \"${foo/bar/\"biz\"}\"").unwrap();
    assert_eq!(guard.read_output(), "biz\n");
  }

  // A literal `'` inside that nested `"..."` must survive — the operand is
  // re-expanded, and the second unescape pass has to preserve the existing
  // quote-region markers rather than re-reading the `'` as a quote opener.
  #[test]
  fn param_exp_replace_nested_double_quote_with_apostrophe() {
    let guard = TestGuard::new();
    set_var!("foo", VarKind::Str("bar".into())).unwrap();

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
    set_var!("s", VarKind::Str("first\nsecond".into())).unwrap();

    test_input("printf '[%s]' \"${s%%$'\\n'*}\"").unwrap();
    assert_eq!(guard.read_output(), "[first]");
  }

  // A `$var` inside the nested `"..."` still expands.
  #[test]
  fn param_exp_replace_nested_double_quote_expands_var() {
    let guard = TestGuard::new();
    set_var!("foo", VarKind::Str("bar".into())).unwrap();
    set_var!("x", VarKind::Str("XX".into())).unwrap();

    test_input("echo \"${foo/bar/\"$x\"}\"").unwrap();
    assert_eq!(guard.read_output(), "XX\n");
  }

  #[test]
  fn param_exp_quoted_glob_meta_is_literal() {
    let guard = TestGuard::new();
    set_var!("foo", VarKind::Str("ba*r".into())).unwrap();

    // "*" makes the asterisk literal — strips the literal "*r" suffix.
    test_input("echo ${foo%\"*\"r}").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "ba\n");
  }

  #[test]
  fn param_exp_unquoted_glob_meta_is_wildcard() {
    let guard = TestGuard::new();
    set_var!("foo", VarKind::Str("ba*r".into())).unwrap();

    // unquoted *r is a glob — shortest match is just "r".
    test_input("echo ${foo%*r}").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "ba*\n");
  }

  #[test]
  fn param_exp_backslash_glob_meta_is_literal() {
    let guard = TestGuard::new();
    set_var!("foo", VarKind::Str("ba*r".into())).unwrap();

    test_input("echo ${foo%\\*r}").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "ba\n");
  }

  #[test]
  fn param_exp_single_quoted_glob_meta_is_literal() {
    let guard = TestGuard::new();
    set_var!("foo", VarKind::Str("ba*r".into())).unwrap();

    test_input("echo ${foo%'*'r}").unwrap();
    let out = guard.read_output();
    assert_eq!(out, "ba\n");
  }

  // ===================== Case conversion =====================

  fn set(name: &str, val: &str) {
    set_var!(name, VarKind::Str(val.into())).unwrap();
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
    let _g     = TestGuard::new();
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
    let _g     = TestGuard::new();
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
    let _g     = TestGuard::new();
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
    let _g     = TestGuard::new();
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

  #[test]
  fn slice_array_at_selects_elements() {
    let guard = TestGuard::new();
    test_input(r#"arr=(a b c d); printf '<%s>' "${arr[@]:1:2}""#).unwrap();
    assert_eq!(guard.read_output(), "<b><c>");
  }

  #[test]
  fn slice_array_at_start_only_takes_the_rest() {
    let guard = TestGuard::new();
    test_input(r#"arr=(a b c d); printf '<%s>' "${arr[@]:2}""#).unwrap();
    assert_eq!(guard.read_output(), "<c><d>");
  }

  #[test]
  fn slice_array_star_joins_the_selected_elements() {
    let guard = TestGuard::new();
    test_input(r#"arr=(a b c d); printf '<%s>' "${arr[*]:1:2}""#).unwrap();
    assert_eq!(guard.read_output(), "<b c>");
  }

  #[test]
  fn slice_positional_params_selects_elements() {
    let guard = TestGuard::new();
    test_input(r#"set -- one two three; printf '<%s>' "${@:1:2}""#).unwrap();
    assert_eq!(guard.read_output(), "<one><two>");
  }

  #[test]
  fn slice_on_scalar_with_at_subscript_selects_elements_not_chars() {
    let _g = TestGuard::new();
    set("x", "foo");
    // Deviation: bash treats `[@]` on a scalar as a no-op subscript and
    // substrings to "f". shed treats a scalar as a one-element array.
    assert_eq!(test_param_expansion("x[@]:0:1").unwrap(), "foo");
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
