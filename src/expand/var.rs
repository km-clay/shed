use std::iter::Peekable;
use std::str::Chars;

use nix::unistd::{Uid, User};
use smol_str::format_smolstr;

use crate::{
  state::vars::VarStr,
  util::{self, QuoteState},
};

use super::{
  PARAMETERS, ShResult,
  eval::lex::is_hard_sep,
  markers, match_loop,
  param::perform_param_expansion,
  sherr, shopt,
  subshell::{expand_cmd_sub, expand_proc_sub},
  try_var, var,
};

pub fn expand_raw_inner(
  chars: &mut Peekable<Chars<'_>>,
  allow_side_effects: bool,
  mark_split: bool,
) -> ShResult<String> {
  let mut result = String::new();
  let mut qt_state = QuoteState::default();

  match_loop!(chars.next() => ch, {
    markers::TILDE_SUB => {
      let mut username = util::scratch_buf();
      while chars.peek().is_some_and(|ch| *ch != '/') {
        let ch = chars.next().unwrap();
        username.push(ch);
      }

      let (home, expanded) = if username.is_empty() {
        // standard '~' expansion
        (var!("HOME"), true)
      } else if let Ok(result) = User::from_name(&username)
        && let Some(user) = result
      {
        // username expansion like '~user'
        (user.dir.to_string_lossy().into(), true)
      } else if let Ok(id) = username.parse::<u32>()
        && let Ok(result) = User::from_uid(Uid::from_raw(id))
          && let Some(user) = result
      {
        // uid expansion like '~1000'
        // shed only feature btw B)
        (user.dir.to_string_lossy().into(), true)
      } else {
        (format_smolstr!("~{username}").into(), false)
      };

      if expanded {
        result.push(markers::DUB_QUOTE);
        result.push_str(&home.to_str_lossy());
        result.push(markers::DUB_QUOTE);
      } else {
        result.push_str(&home.to_str_lossy());
      }
    }
    markers::PROC_SUB_OUT if allow_side_effects => {
      let mut inner = util::scratch_buf();
      match_loop!(chars.next() => ch, {
        markers::PROC_SUB_OUT => break,
        _ => inner.push(ch),
      });
      let fd_path = expand_proc_sub(&inner, false)?;
      result.push_str(&fd_path);
    }
    markers::PROC_SUB_IN if allow_side_effects => {
      let mut inner = util::scratch_buf();
      match_loop!(chars.next() => ch, {
        markers::PROC_SUB_IN => break,
        _ => inner.push(ch),
      });
      let fd_path = expand_proc_sub(&inner, true)?;
      result.push_str(&fd_path);
    }
    markers::DUB_QUOTE => {
      if !qt_state.in_single() {
        qt_state.toggle_double();
      }
      result.push(ch);
    }
    markers::SNG_QUOTE => {
      if !qt_state.in_double() {
        qt_state.toggle_single();
      }
      result.push(ch);
    }
    markers::VAR_SUB => {
      let mut expanded = expand_var(chars, allow_side_effects)?;

      if mark_split && qt_state.outside() {
        expanded = format_smolstr!("{}{expanded}{}", markers::EXPAND_START, markers::EXPAND_END).into();
      }
      result.push_str(&expanded.to_str_lossy());
    }
    _ => result.push(ch),
  });

  Ok(result)
}

pub fn expand_raw(chars: &mut Peekable<Chars<'_>>) -> ShResult<String> {
  expand_raw_inner(chars, true, false)
}

pub fn expand_var(chars: &mut Peekable<Chars<'_>>, allow_side_effects: bool) -> ShResult<VarStr> {
  let mut var_name = util::scratch_buf();
  let mut brace_depth: i32 = 0;
  let mut inner_brace_depth: i32 = 0;
  let mut prev_was_dollar = false;
  let mut in_subsh = false;
  match_loop!(chars.peek() => &ch => ch, {
    markers::SUBSH if var_name.is_empty() => {
      chars.next(); // now safe to consume
      let mut subsh_body = util::scratch_buf();
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
        return Ok(format_smolstr!("$({subsh_body}").into());
      }
      if allow_side_effects {
        let expanded = expand_cmd_sub(&subsh_body)?;
        return Ok(expanded);
      }
      return Ok(subsh_body.into());
    }
    '{' if var_name.is_empty() && brace_depth == 0 => {
      chars.next(); // consume the brace
      brace_depth += 1;
      prev_was_dollar = false;
    }
    '}' if brace_depth > 0 && inner_brace_depth == 0 && !in_subsh => {
      chars.next(); // consume the brace
      let val = perform_param_expansion(&var_name, allow_side_effects)?;
      return Ok(val);
    }
    markers::ESCAPE if brace_depth > 0 => {
      chars.next();
      var_name.push(markers::ESCAPE);
      if let Some(next_ch) = chars.next() {
        var_name.push(next_ch);
      }
      prev_was_dollar = false;
    }
    markers::DUB_QUOTE | markers::SNG_QUOTE if brace_depth > 0 => {
      let opener = ch;
      chars.next();
      var_name.push(opener);
      while let Some(&next_ch) = chars.peek() {
        chars.next();
        var_name.push(next_ch);
        if next_ch == opener {
          break;
        }
        if next_ch == markers::ESCAPE
        && let Some(esc_ch) = chars.next() {
          var_name.push(esc_ch);
        }
      }
      prev_was_dollar = false;
    }
    ch if brace_depth > 0 => {
      chars.next(); // safe to consume
      if ch == markers::SUBSH {
        in_subsh = !in_subsh;
      } else if !in_subsh {
        if ch == '{' && prev_was_dollar {
          inner_brace_depth += 1;
        } else if ch == '}' && inner_brace_depth > 0 {
          inner_brace_depth -= 1;
        }
      }
      prev_was_dollar = !in_subsh && ch == markers::VAR_SUB;
      var_name.push(ch);
    }
    ch if var_name.is_empty() && (PARAMETERS.contains(&ch) || ch.is_ascii_digit()) => {
      chars.next();
      let mut buf = [0u8; 4];
      let parameter = ch.encode_utf8(&mut buf);
      let val = var!(parameter);

      if ch == '@' && val.is_empty() {
        let mut buf = [0u8; 4];
        return Ok(VarStr::from(markers::NULL_EXPAND.encode_utf8(&mut buf)));
      }

      return Ok(val);
    }
    ch if is_hard_sep(ch) || !(ch.is_alphanumeric() || ch == '_') => {
      let val = try_var!(&var_name);
      if val.is_none() && shopt!(set.nounset) {
        return Err(sherr!(NotFound, "Variable '{var_name}' is not set"));
      }
      return Ok(val.unwrap_or_default());
    }
    _ => {
      chars.next();
      var_name.push(ch);
    }
  });
  if var_name.is_empty() {
    Ok(VarStr::default())
  } else {
    let val = try_var!(&var_name);
    if val.is_none() && shopt!(set.nounset) {
      return Err(sherr!(NotFound, "Variable '{var_name}' is not set"));
    }
    Ok(val.unwrap_or_default())
  }
}

#[cfg(test)]
mod tests {
  use super::super::escape_glob;
  use super::var;
  use crate::expand::escape::unescape_str;
  use crate::expand::{expand_glob, expand_raw, markers};
  use crate::state::vars::VarStr;
  use crate::state::{Shed, vars::VarFlags, vars::VarKind};
  use crate::tests::testutil::TestGuard;

  // ===================== Variable Expansion (TestGuard) =====================

  #[test]
  fn var_expansion_basic() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("MYVAR", VarKind::Str("hello".into()), VarFlags::empty()))
      .unwrap();

    let raw = unescape_str("$MYVAR");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn var_expansion_braced() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("FOO", VarKind::Str("bar".into()), VarFlags::empty())).unwrap();

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
    Shed::vars_mut(|v| v.set_var("A", VarKind::Str("hello".into()), VarFlags::empty())).unwrap();
    Shed::vars_mut(|v| v.set_var("B", VarKind::Str("world".into()), VarFlags::empty())).unwrap();

    let raw = unescape_str("${A}_${B}");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(result, "hello_world");
  }

  // ===================== Tilde Expansion (TestGuard) =====================

  #[test]
  fn tilde_expansion_home() {
    let _guard = TestGuard::new();
    let home = var!("HOME");

    let raw = unescape_str("~/foo");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(
      result,
      format!("{}{}{}/foo", markers::DUB_QUOTE, home, markers::DUB_QUOTE)
    );
  }

  #[test]
  fn tilde_expansion_bare() {
    let _guard = TestGuard::new();
    let home = var!("HOME");

    let raw = unescape_str("~");
    let result = expand_raw(&mut raw.chars().peekable()).unwrap();
    assert_eq!(
      result,
      format!("{}{}{}", markers::DUB_QUOTE, home, markers::DUB_QUOTE)
    );
  }

  // ===================== escape_glob =====================

  #[test]
  fn escape_glob_passthrough_when_no_escapes() {
    // No `\` chars → output equals input.
    assert_eq!(escape_glob("foo*bar", false), "foo*bar");
    assert_eq!(escape_glob("plain", false), "plain");
  }

  #[test]
  fn escape_glob_wraps_escaped_star() {
    // `\*` → `[*]` (glob-literal star)
    assert_eq!(escape_glob("foo\\*", false), "foo[*]");
  }

  #[test]
  fn escape_glob_wraps_escaped_question_mark() {
    assert_eq!(escape_glob("foo\\?", false), "foo[?]");
  }

  #[test]
  fn escape_glob_wraps_escaped_bracket() {
    assert_eq!(escape_glob("foo\\[bar", false), "foo[[]bar");
  }

  #[test]
  fn escape_glob_strips_non_meta_escapes() {
    // `\ ` (escaped space) becomes literal space — not a glob meta, so
    // bracket-wrap is unnecessary.
    assert_eq!(escape_glob("my\\ file", false), "my file");
  }

  #[test]
  fn escape_glob_drops_trailing_escape() {
    // Lone trailing `\` with nothing to escape — silently dropped.
    assert_eq!(escape_glob("foo\\", false), "foo");
  }

  #[test]
  fn escape_glob_with_marker_form() {
    // use_markers=true reads the ESCAPE marker char, not literal `\`.
    use crate::expand::markers;
    let input = format!("foo{}*", markers::ESCAPE);
    assert_eq!(escape_glob(&input, true), "foo[*]");
  }

  // ===================== expand_glob with escapes =====================

  #[test]
  fn expand_glob_matches_escaped_space() {
    use crate::expand::markers::strip_markers;
    // The original bug: `my\ *` should match a file named `my file.txt`.
    let _g = TestGuard::new();
    let tmp = std::env::temp_dir().join("shed_test_glob_escape");
    std::fs::create_dir_all(&tmp).ok();
    let target = tmp.join("my file.txt");
    std::fs::write(&target, "").unwrap();

    let saved_dir = std::env::current_dir().ok();
    std::env::set_current_dir(&tmp).unwrap();

    // After unescape_str, `my\ *` becomes `my{ESCAPE} *`.
    let unescaped = unescape_str("my\\ *");
    let result = expand_glob(&unescaped);

    if let Some(prev) = saved_dir {
      let _ = std::env::set_current_dir(prev);
    }
    std::fs::remove_dir_all(&tmp).ok();

    let result = result.expect("expand_glob should succeed").join(" ");
    // Glob expansion should match `my file.txt`. Result is escape-marker-
    // wrapped post-glob; check via strip_markers.
    let stripped = strip_markers(&result);
    assert!(
      stripped.contains("my file.txt"),
      "expected match for 'my\\ *'; got {stripped:?}"
    );
  }

  #[test]
  fn expand_glob_leading_dot_matches_bash_rule() {
    use crate::expand::markers::strip_markers;
    let _g = TestGuard::new();
    let tmp = std::env::temp_dir().join("shed_test_glob_dotfiles");
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(&tmp).unwrap();
    for f in [".foo", ".bar", "visible.txt"] {
      std::fs::write(tmp.join(f), "").unwrap();
    }

    let saved = std::env::current_dir().ok();
    std::env::set_current_dir(&tmp).unwrap();

    let glob = |p: &str| -> Vec<String> {
      expand_glob(p)
        .unwrap()
        .iter()
        .map(|s| strip_markers(s))
        .collect()
    };
    // Capture everything before restoring cwd so an assert failure can't leak it.
    let dot_f = glob(".f*");
    let star = glob("*");
    let dot_star = glob(".*");

    if let Some(prev) = saved {
      let _ = std::env::set_current_dir(prev);
    }
    std::fs::remove_dir_all(&tmp).ok();

    // An explicit leading dot matches dotfiles.
    assert_eq!(dot_f, vec![".foo".to_string()], "`.f*` should match `.foo`");
    // A bare wildcard must NOT sweep up dotfiles.
    assert!(star.contains(&"visible.txt".to_string()), "got {star:?}");
    assert!(
      !star.iter().any(|s| s.starts_with('.')),
      "`*` must not match dotfiles: {star:?}"
    );
    // `.*` matches real dotfiles but never `.` or `..`.
    assert!(
      dot_star.contains(&".foo".to_string()) && dot_star.contains(&".bar".to_string()),
      "`.*` should match dotfiles: {dot_star:?}"
    );
    assert!(
      !dot_star
        .iter()
        .any(|s| { s == "." || s == ".." || s.ends_with("/.") || s.ends_with("/..") }),
      "`.*` must not include `.` or `..`: {dot_star:?}"
    );
  }

  // ===================== Tk::expand glob tests (full pipeline) =====================

  /// Helper: drive the full expansion pipeline (`unescape_str` → `expand_raw` →
  /// `split_words` → `expand_glob` → strip ESCAPE) on a raw shell word.
  fn expand_words_in(dir: &std::path::Path, raw: &str) -> Vec<VarStr> {
    use crate::eval::lex::TkFlags;
    use crate::expand::Expander;

    let saved = std::env::current_dir().ok();
    std::env::set_current_dir(dir).unwrap();
    let result = Expander::from_raw(raw, TkFlags::empty()).expand().unwrap();
    if let Some(prev) = saved {
      let _ = std::env::set_current_dir(prev);
    }
    result
  }

  /// Build a tempdir populated with the given filenames.
  fn make_fixture(name: &str, files: &[&str]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in files {
      std::fs::File::create(dir.join(f)).unwrap();
    }
    dir
  }

  #[test]
  fn glob_quoted_prefix_unquoted_meta_matches() {
    // `"path/"*` should glob — only `*` is unquoted, the prefix is literal.
    // This is the cd-completion case.
    let _g = TestGuard::new();
    let dir = make_fixture("shed_glob_qprefix", &["alpha", "beta", "gamma"]);
    let pattern = format!(r#""{}/"*"#, dir.display());
    let words = expand_words_in(&dir, &pattern);
    let _ = std::fs::remove_dir_all(&dir);

    let mut got: Vec<String> = words
      .iter()
      .filter_map(|w| {
        std::path::Path::new(w)
          .file_name()
          .map(|n| n.to_string_lossy().into_owned())
      })
      .collect();
    got.sort();
    assert_eq!(got, vec!["alpha", "beta", "gamma"]);
  }

  #[test]
  fn glob_fully_quoted_is_literal() {
    // `"*"` should be a literal `*` — no expansion.
    let _g = TestGuard::new();
    let dir = make_fixture("shed_glob_full_quote", &["a", "b"]);
    let words = expand_words_in(&dir, r#""*""#);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(words, vec!["*"]);
  }

  #[test]
  fn glob_squote_is_literal() {
    // `'*'` should be a literal `*` — no expansion.
    let _g = TestGuard::new();
    let dir = make_fixture("shed_glob_squote", &["a", "b"]);
    let words = expand_words_in(&dir, "'*'");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(words, vec!["*"]);
  }

  #[test]
  fn glob_backslash_escaped_is_literal() {
    // `\*` should be a literal `*`.
    let _g = TestGuard::new();
    let dir = make_fixture("shed_glob_bs_escape", &["a", "b"]);
    let words = expand_words_in(&dir, r"\*");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(words, vec!["*"]);
  }

  #[test]
  fn glob_unquoted_expands() {
    // Baseline: unquoted `*` globs as expected.
    let _g = TestGuard::new();
    let dir = make_fixture("shed_glob_unquoted", &["a.txt", "b.txt", "c.log"]);
    let words = expand_words_in(&dir, "*.txt");
    let _ = std::fs::remove_dir_all(&dir);

    let mut got = words;
    got.sort();
    assert_eq!(got, vec!["a.txt", "b.txt"]);
  }

  #[test]
  fn glob_quoted_prefix_with_subdir_unquoted_meta() {
    // `"a/"*.txt` — prefix quoted, suffix has unquoted glob meta.
    let _g = TestGuard::new();
    let outer = make_fixture("shed_glob_subdir", &[]);
    let sub = outer.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::File::create(sub.join("a.txt")).unwrap();
    std::fs::File::create(sub.join("b.txt")).unwrap();

    let pattern = format!(r#""{}/sub/"*.txt"#, outer.display());
    let words = expand_words_in(&outer, &pattern);
    let _ = std::fs::remove_dir_all(&outer);

    let mut got: Vec<String> = words
      .iter()
      .filter_map(|w| {
        std::path::Path::new(w)
          .file_name()
          .map(|n| n.to_string_lossy().into_owned())
      })
      .collect();
    got.sort();
    assert_eq!(got, vec!["a.txt", "b.txt"]);
  }
}
