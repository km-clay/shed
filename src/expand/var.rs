use std::iter::Peekable;
use std::str::Chars;

use nix::unistd::{Uid, User};

use crate::expand::PARAMETERS;
use crate::expand::escape::escape_str;
use crate::expand::param::perform_param_expansion;
use crate::expand::subshell::{expand_cmd_sub, expand_proc_sub};
use crate::libsh::error::ShResult;
use crate::match_loop;
use crate::parse::lex::is_hard_sep;
use crate::prelude::*;
use crate::readline::markers;
use crate::sherr;
use crate::state::{ArrIndex, read_vars};

pub fn expand_raw(chars: &mut Peekable<Chars<'_>>) -> ShResult<String> {
  let mut result = String::new();

  match_loop!(chars.next() => ch, {
    markers::TILDE_SUB => {
      let mut username = String::new();
      while chars.peek().is_some_and(|ch| *ch != '/') {
        let ch = chars.next().unwrap();
        username.push(ch);
      }

      let home = if username.is_empty() {
        // standard '~' expansion
        env::var("HOME").unwrap_or_default()
      } else if let Ok(result) = User::from_name(&username)
        && let Some(user) = result
      {
        // username expansion like '~user'
        user.dir.to_string_lossy().to_string()
      } else if let Ok(id) = username.parse::<u32>()
        && let Ok(result) = User::from_uid(Uid::from_raw(id))
          && let Some(user) = result
      {
        // uid expansion like '~1000'
        // shed only feature btw B)
        user.dir.to_string_lossy().to_string()
      } else {
        // no match, use literal
        format!("~{username}")
      };

      result.push_str(&home);
    }
    markers::PROC_SUB_OUT => {
      let mut inner = String::new();
      match_loop!(chars.next() => ch, {
        markers::PROC_SUB_OUT => break,
        _ => inner.push(ch),
      });
      let fd_path = expand_proc_sub(&inner, false)?;
      result.push_str(&fd_path);
    }
    markers::PROC_SUB_IN => {
      let mut inner = String::new();
      match_loop!(chars.next() => ch, {
        markers::PROC_SUB_IN => break,
        _ => inner.push(ch),
      });
      let fd_path = expand_proc_sub(&inner, true)?;
      result.push_str(&fd_path);
    }
    markers::VAR_SUB => {
      let expanded = expand_var(chars)?;
      result.push_str(&expanded);
    }
    _ => result.push(ch),
  });

  Ok(result)
}

pub fn expand_var(chars: &mut Peekable<Chars<'_>>) -> ShResult<String> {
  let mut var_name = String::new();
  let mut brace_depth: i32 = 0;
  let mut inner_brace_depth: i32 = 0;
  let mut bracket_depth: i32 = 0;
  let mut idx_brace_depth: i32 = 0;
  let mut idx_raw = String::new();
  let mut idx = None;
  let mut split_start = None;
  let mut split_len = None;
  let mut split_raw = String::new();
  let mut in_operator = false;
  match_loop!(chars.peek() => &ch => ch, {
    markers::SUBSH if var_name.is_empty() => {
      chars.next(); // now safe to consume
      let mut subsh_body = String::new();
      let mut found_end = false;
      match_loop!(chars.next() => c, {
        markers::SUBSH => {
          found_end = true;
          break;
        }
        _ => subsh_body.push(c),
      });
      if !found_end {
        // if there isnt a closing SUBSH, we are probably in some tab completion context
        // and we got passed some unfinished input. Just treat it as literal text
        return Ok(format!("$({subsh_body}"));
      }
      let expanded = expand_cmd_sub(&subsh_body)?;
      return Ok(expanded);
    }
    '{' if var_name.is_empty() && brace_depth == 0 => {
      chars.next(); // consume the brace
      brace_depth += 1;
    }
    '}' if brace_depth > 0 && bracket_depth == 0 && inner_brace_depth == 0 => {
      chars.next(); // consume the brace
      let val = if let Some(idx) = idx {
        match idx {
          ArrIndex::AllSplit => {
            let arg_sep = markers::ARG_SEP.to_string();
            let elems = read_vars(|v| v.get_arr_elems(&var_name))?;
            let start = split_start.unwrap_or(0);
            let end = start + split_len.unwrap_or(elems.len().saturating_sub(start));
            elems[start..end.min(elems.len())].join(&arg_sep)
          }
          ArrIndex::ArgCount => read_vars(|v| v.get_arr_elems(&var_name))
            .map(|elems| elems.len().to_string())
            .unwrap_or_else(|_| "0".to_string()),
          ArrIndex::AllJoined => {
            let ifs = read_vars(|v| v.try_get_var("IFS"))
              .unwrap_or_else(|| " \t\n".to_string())
              .chars()
              .next()
              .unwrap_or(' ')
              .to_string();

            let elems = read_vars(|v| v.get_arr_elems(&var_name))?;
            let start = split_start.unwrap_or(0);
            let end = start + split_len.unwrap_or(elems.len().saturating_sub(start));
            elems[start..end.min(elems.len())].join(&ifs)
          }
          _ => read_vars(|v| v.index_var(&var_name, idx))?,
        }
      } else {
        perform_param_expansion(&var_name)?
      };
      return Ok(val);
    }
    '[' if brace_depth > 0 && bracket_depth == 0 && inner_brace_depth == 0 && !in_operator => {
      chars.next(); // consume the bracket
      bracket_depth += 1;
    }
    ']' if bracket_depth > 0 && idx_brace_depth == 0 => {
      bracket_depth -= 1;
      chars.next(); // consume the bracket
      if bracket_depth == 0 {
        let expanded_idx = expand_raw(&mut idx_raw.chars().peekable())?;
        idx = Some(expanded_idx.parse::<ArrIndex>().map_err(|_| {
          sherr!(
            ParseErr,
            "Array index must be a number, got '{expanded_idx}'",
          )
        })?);
      }
    }
    ':' if matches!(idx, Some(ArrIndex::AllSplit | ArrIndex::AllJoined)) => {
      chars.next();
      match_loop!(chars.peek() => ch, {
        ':' => {
          chars.next();
          let expanded = expand_raw(&mut split_raw.chars().peekable())?;
          let Ok(split_idx) = expanded.parse::<usize>() else {
            return Err(sherr!(
                ParseErr,
                "Split index must be a number, got '{expanded}'",
            ));
          };
          if split_start.is_none() {
            split_start = Some(split_idx);
          } else if split_len.is_none() {
            split_len = Some(split_idx);
          } else {
            return Err(sherr!(ParseErr, "Too many ':' in split index",));
          }
          split_raw.clear();
        }
        '}' => {
          let expanded = expand_raw(&mut split_raw.chars().peekable())?;
          let Ok(split_idx) = expanded.parse::<usize>() else {
            return Err(sherr!(
                ParseErr,
                "Split index must be a number, got '{expanded}'",
            ));
          };

          if split_start.is_none() {
            split_start = Some(split_idx);
          } else if split_len.is_none() {
            split_len = Some(split_idx);
          } else {
            return Err(sherr!(ParseErr, "Too many ':' in split index",));
          }
          break;
        }
        _ => {
          split_raw.push(*ch);
          chars.next();
        }
      });
    }
    ch if bracket_depth > 0 => {
      chars.next(); // safe to consume
      if ch == '{' {
        idx_brace_depth += 1;
      }
      if ch == '}' {
        idx_brace_depth -= 1;
      }
      idx_raw.push(ch);
    }
    ch if brace_depth > 0 => {
      chars.next(); // safe to consume
      if ch == '{' {
        inner_brace_depth += 1;
      }
      if ch == '}' {
        inner_brace_depth -= 1;
      }
      if !in_operator && matches!(ch, '#' | '%' | ':' | '/' | '-' | '+' | '=' | '?' | '!') {
        in_operator = true;
      }
      var_name.push(ch);
    }
    ch if var_name.is_empty() && PARAMETERS.contains(&ch) => {
      chars.next();
      let parameter = format!("{ch}");
      let val = read_vars(|v| v.get_var(&parameter));

      if (ch == '@' || ch == '*') && val.is_empty() {
        return Ok(markers::NULL_EXPAND.to_string());
      }

      return Ok(val);
    }
    ch if is_hard_sep(ch) || !(ch.is_alphanumeric() || ch == '_' || ch == '-') => {
      let val = read_vars(|v| v.get_var(&var_name));
      return Ok(val);
    }
    _ => {
      chars.next();
      var_name.push(ch);
    }
  });
  if !var_name.is_empty() {
    let var_val = read_vars(|v| v.get_var(&var_name));
    Ok(var_val)
  } else {
    Ok(String::new())
  }
}

pub fn expand_glob(raw: &str) -> ShResult<String> {
  let mut words = vec![];

	if !raw.contains(['*', '?', '[']) {
		return Ok(raw.to_string());
	}

  let opts = glob::MatchOptions {
    require_literal_leading_dot: !crate::state::read_shopts(|s| s.core.dotglob),
    ..Default::default()
  };
  for entry in glob::glob_with(raw, opts).map_err(|_| sherr!(SyntaxErr, "Invalid glob pattern"))? {
    let entry = entry.map_err(|_| sherr!(SyntaxErr, "Invalid filename found in glob"))?;
    let entry_raw = entry
      .to_str()
      .ok_or_else(|| sherr!(SyntaxErr, "Non-UTF8 filename found in glob"))?;
    let escaped = escape_str(entry_raw, true);

    words.push(escaped)
  }
  Ok(words.join(" "))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::expand::escape::unescape_str;
  use crate::state::{VarFlags, VarKind, write_vars};
  use crate::testutil::TestGuard;

  // ===================== Variable Expansion (TestGuard) =====================

  #[test]
  fn var_expansion_basic() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("MYVAR", VarKind::Str("hello".into()), VarFlags::NONE)).unwrap();

    let raw = unescape_str("$MYVAR");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn var_expansion_braced() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("FOO", VarKind::Str("bar".into()), VarFlags::NONE)).unwrap();

    let raw = unescape_str("${FOO}");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(result, "bar");
  }

  #[test]
  fn var_expansion_unset_empty() {
    let _guard = TestGuard::new();

    let raw = unescape_str("$NONEXISTENT");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(result, "");
  }

  #[test]
  fn var_expansion_concatenated() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("A", VarKind::Str("hello".into()), VarFlags::NONE)).unwrap();
    write_vars(|v| v.set_var("B", VarKind::Str("world".into()), VarFlags::NONE)).unwrap();

    let raw = unescape_str("${A}_${B}");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(result, "hello_world");
  }

  // ===================== Tilde Expansion (TestGuard) =====================

  #[test]
  fn tilde_expansion_home() {
    let _guard = TestGuard::new();
    let home = std::env::var("HOME").unwrap();

    let raw = unescape_str("~/foo");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(result, format!("{}/foo", home));
  }

  #[test]
  fn tilde_expansion_bare() {
    let _guard = TestGuard::new();
    let home = std::env::var("HOME").unwrap();

    let raw = unescape_str("~");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(result, home);
  }
}
