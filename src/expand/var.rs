use nix::unistd::{Uid, User};
use smol_str::format_smolstr;

use crate::{
  expand::stream::{Marker, ProcSubKind, Quote, SegCursor, SegStream, Unit},
  state::vars::VarStr,
  util::QuoteState,
};

use super::{
  PARAMETERS, ShResult,
  eval::lex::is_hard_sep,
  match_loop,
  param::perform_param_expansion,
  sherr, shopt,
  subshell::{expand_cmd_sub, expand_proc_sub},
  try_var, var,
};

pub fn expand_raw_inner(
  chars: &mut SegCursor,
  allow_side_effects: bool,
  mark_split: bool,
) -> ShResult<SegStream> {
  let mut result = SegStream::new();
  let mut qt_state = QuoteState::default();

  match_loop!(chars.next() => unit, {
    Unit::Mark(Marker::TildeSub) => {
      let mut username = String::new();
      let mut quoted = false;
      loop {
        match chars.peek() {
          Some(Unit::Byte(b'/')) | None => break,
          Some(Unit::Byte(b)) => {
            username.push(b as char);
            chars.next();
          }
          // A quote or expansion anywhere in the tilde-prefix suppresses tilde
          // expansion (bash): emit `~` + prefix literally and let the marker
          // be processed normally.
          Some(Unit::Mark(_)) => {
            quoted = true;
            break;
          }
        }
      }
      if quoted {
        result.push_byte(b'~');
        result.push_bytes(username.as_bytes());
        continue;
      }

      let (home, expanded): (VarStr, bool) = if username.is_empty() {
        // standard '~' expansion
        (var!("HOME"), true)
      } else if let Ok(Some(user)) = User::from_name(&username) {
        // username expansion like '~user'
        (user.dir.to_string_lossy().as_ref().into(), true)
      } else if let Ok(id) = username.parse::<u32>()
        && let Ok(Some(user)) = User::from_uid(Uid::from_raw(id))
      {
        // uid expansion like '~1000'
        // shed only feature btw B)
        (user.dir.to_string_lossy().as_ref().into(), true)
      } else {
        (format_smolstr!("~{username}").as_str().into(), false)
      };

      if expanded {
        result.push_marker(Marker::Quote(Quote::Double));
        result.push_bytes(home.as_bytes());
        result.push_marker(Marker::Quote(Quote::Double));
      } else {
        result.push_bytes(home.as_bytes());
      }
    }
    Unit::Mark(Marker::ProcSub(kind)) if allow_side_effects => {
      let mut inner: Vec<u8> = Vec::new();
      match_loop!(chars.next() => n, {
        Unit::Mark(Marker::ProcSub(_)) => break,
        Unit::Byte(b) => inner.push(b),
        _ => {}
      });
      // `expand_proc_sub`'s flag means "the substituted path is writable", i.e.
      // an *output* proc sub `>(...)`; `<(...)` (In) redirects the child's
      // stdout into the pipe and is the `false` case.
      let fd_path = expand_proc_sub(
        &String::from_utf8_lossy(&inner),
        matches!(kind, ProcSubKind::Out),
      )?;
      result.push_bytes(fd_path.as_bytes());
    }
    Unit::Mark(Marker::Quote(q)) => {
      match q {
        Quote::Double if !qt_state.in_single() => qt_state.toggle_double(),
        Quote::Single if !qt_state.in_double() => qt_state.toggle_single(),
        _ => {}
      }
      result.push_marker(Marker::Quote(q));
    }
    Unit::Mark(Marker::VarSub) => {
      let expanded = expand_var(chars, allow_side_effects)?;

      if mark_split && qt_state.outside() {
        result.push_marker(Marker::ExpandStart);
        result.append(expanded);
        result.push_marker(Marker::ExpandEnd);
      } else {
        result.append(expanded);
      }
    }
    Unit::Byte(b) => result.push_byte(b),
    Unit::Mark(m) => result.push_marker(m),
  });

  Ok(result)
}

pub fn expand_raw(stream: &mut SegCursor) -> ShResult<SegStream> {
  expand_raw_inner(stream, true, false)
}

pub fn expand_var(stream: &mut SegCursor, allow_side_effects: bool) -> ShResult<SegStream> {
  let mut var_name = SegStream::new();
  let mut brace_depth: i32 = 0;
  let mut inner_brace_depth: i32 = 0;
  let mut prev_was_dollar = false;
  let mut in_subsh = false;

  match_loop!(stream.peek() => unit, {
    Unit::Mark(Marker::Subshell) if var_name.is_empty() => {
      stream.bump();
      let mut subsh_body: Vec<u8> = Vec::new();
      let mut found_end = false;
      match_loop!(stream.next() => n_unit, {
        Unit::Mark(Marker::Subshell) => {
          found_end = true;
          break;
        }
        Unit::Byte(b) => subsh_body.push(b),
        _ => {}
      });

      if !found_end {
        // if there isnt a closing SUBSH, we are probably in some tab completion context
        // and we got passed some unfinished input. Just treat it as literal text
        let mut out = SegStream::from_bytes(b"$(");
        out.push_bytes(&subsh_body);
        return Ok(out);
      }
      if allow_side_effects {
        let expanded = expand_cmd_sub(&String::from_utf8_lossy(&subsh_body))?;
        return Ok(SegStream::from_bytes(expanded.as_bytes()));
      }
      return Ok(SegStream::from_bytes(&subsh_body));
    }
    Unit::Byte(b'{') if var_name.is_empty() && brace_depth == 0 => {
      stream.bump();
      brace_depth += 1;
      prev_was_dollar = false;
    }
    Unit::Byte(b'}') if brace_depth > 0 && inner_brace_depth == 0 && !in_subsh => {
      stream.bump();
      return perform_param_expansion(&var_name, allow_side_effects);
    }
    Unit::Mark(Marker::Escape) if brace_depth > 0 => {
      stream.bump();
      var_name.push_marker(Marker::Escape);
      if let Some(next_unit) = stream.next() {
        var_name.push(next_unit);
      }
      prev_was_dollar = false;
    }
    Unit::Mark(Marker::Quote(q)) if brace_depth > 0 => {
      stream.bump();
      var_name.push_marker(Marker::Quote(q));
      while let Some(next_unit) = stream.next() {
        var_name.push(next_unit);
        if next_unit == Unit::Mark(Marker::Quote(q)) {
          break;
        }
        if next_unit == Unit::Mark(Marker::Escape)
          && let Some(escaped) = stream.next()
        {
          var_name.push(escaped);
        }
      }
      prev_was_dollar = false;
    }
    _ if brace_depth > 0 => {
      stream.bump();
      match unit {
        Unit::Mark(Marker::Subshell) => in_subsh = !in_subsh,
        Unit::Byte(b'{') if !in_subsh && prev_was_dollar => inner_brace_depth += 1,
        Unit::Byte(b'}') if !in_subsh && inner_brace_depth > 0 => inner_brace_depth -= 1,
        _ => {}
      }
      prev_was_dollar = !in_subsh && unit == Unit::Mark(Marker::VarSub);
      var_name.push(unit);
    }
    Unit::Byte(b) if var_name.is_empty() && (PARAMETERS.contains(&(b as char)) || b.is_ascii_digit()) => {
      stream.bump();
      let mut buf = [0u8; 4];
      let parameter = (b as char).encode_utf8(&mut buf);
      let val = var!(parameter);

      if b == b'@' && val.is_empty() {
        let mut out = SegStream::new();
        out.push_marker(Marker::NullExpand);
        return Ok(out);
      }

      return Ok(val.into());
    }
    Unit::Byte(b) if is_hard_sep(b) || !(b.is_ascii_alphanumeric() || b == b'_') => {
      return lookup_var(&var_name);
    }
    Unit::Mark(_) => {
      return lookup_var(&var_name);
    }
    Unit::Byte(b) => {
      stream.bump();
      var_name.push_byte(b);
    }
  });
  if var_name.is_empty() {
    Ok(SegStream::new())
  } else {
    lookup_var(&var_name)
  }
}

/// Look up a bare `$name` and return its value, honoring `set -u` (nounset).
fn lookup_var(var_name: &SegStream) -> ShResult<SegStream> {
  let name_bytes = var_name.to_bytes();
  let name = String::from_utf8_lossy(&name_bytes);
  let val = try_var!(name.as_ref());
  if val.is_none() && shopt!(set.nounset) {
    return Err(sherr!(NotFound, "Variable '{name}' is not set"));
  }
  Ok(val.unwrap_or_default().into())
}

#[cfg(test)]
mod tests {
  use bstr::ByteSlice;
  use itertools::Itertools;

  use super::var;
  use crate::expand::escape::unescape_str;
  use crate::expand::glob::expand_glob;

  // expand_raw is SegStream-native; render it back to a marker-char string for
  // these assertions (markers as sentinel chars). Plain-text results compare
  // unchanged; the tilde tests check the quote-marker wrapping.
  fn expand_raw(cur: &mut crate::expand::stream::SegCursor) -> super::ShResult<String> {
    super::expand_raw(cur).map(|seg| {
      use crate::expand::stream::StreamSeg;
      let mut out = String::new();
      for s in seg.stream() {
        match s {
          StreamSeg::Bytes(b) => out.push_str(&String::from_utf8_lossy(b)),
          StreamSeg::Mark(m) => out.push(marker_char(*m)),
        }
      }
      out
    })
  }
  fn marker_char(m: crate::expand::stream::Marker) -> char {
    use crate::expand::stream::{Marker, ProcSubKind, Quote};
    match m {
      Marker::Quote(Quote::Double) => '\u{fdd0}',
      Marker::Quote(Quote::Single) => '\u{fdd1}',
      Marker::TildeSub => '\u{fdd2}',
      Marker::ProcSub(ProcSubKind::In) => '\u{fdd3}',
      Marker::ProcSub(ProcSubKind::Out) => '\u{fdd4}',
      Marker::NullExpand => '\u{fdd5}',
      Marker::ArgSep => '\u{fdd6}',
      Marker::Subshell => '\u{fdd7}',
      Marker::VarSub => '\u{fdd8}',
      Marker::Escape => '\u{fdd9}',
      Marker::ExpandStart => '\u{fde1}',
      Marker::ExpandEnd => '\u{fde2}',
    }
  }
  #[allow(dead_code)]
  mod markers {
    pub const DUB_QUOTE: char = '\u{fdd0}';
  }
  use crate::state::vars::VarStr;
  use crate::state::{Shed, vars::VarFlags, vars::VarKind};
  use crate::tests::testutil::TestGuard;

  // ===================== Variable Expansion (TestGuard) =====================

  #[test]
  fn var_expansion_basic() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("MYVAR", VarKind::Str("hello".into()), VarFlags::empty()))
      .unwrap();

    let raw = unescape_str(b"$MYVAR");
    let result = expand_raw(&mut raw.cursor()).unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn var_expansion_braced() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("FOO", VarKind::Str("bar".into()), VarFlags::empty())).unwrap();

    let raw = unescape_str(b"${FOO}");
    let result = expand_raw(&mut raw.cursor()).unwrap();
    assert_eq!(result, "bar");
  }

  #[test]
  fn var_expansion_unset_empty() {
    let _guard = TestGuard::new();

    let raw = unescape_str(b"$NONEXISTENT");
    let result = expand_raw(&mut raw.cursor()).unwrap();
    assert_eq!(result, "");
  }

  #[test]
  fn var_expansion_concatenated() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("A", VarKind::Str("hello".into()), VarFlags::empty())).unwrap();
    Shed::vars_mut(|v| v.set_var("B", VarKind::Str("world".into()), VarFlags::empty())).unwrap();

    let raw = unescape_str(b"${A}_${B}");
    let result = expand_raw(&mut raw.cursor()).unwrap();
    assert_eq!(result, "hello_world");
  }

  // ===================== Tilde Expansion (TestGuard) =====================

  #[test]
  fn tilde_expansion_home() {
    let _guard = TestGuard::new();
    let home = var!("HOME");

    let raw = unescape_str(b"~/foo");
    let result = expand_raw(&mut raw.cursor()).unwrap();
    assert_eq!(
      result,
      format!("{}{}{}/foo", markers::DUB_QUOTE, home, markers::DUB_QUOTE)
    );
  }

  #[test]
  fn tilde_expansion_bare() {
    let _guard = TestGuard::new();
    let home = var!("HOME");

    let raw = unescape_str(b"~");
    let result = expand_raw(&mut raw.cursor()).unwrap();
    assert_eq!(
      result,
      format!("{}{}{}", markers::DUB_QUOTE, home, markers::DUB_QUOTE)
    );
  }

  #[test]
  fn escape_glob_with_marker_form() {
    // The ESCAPE-marker → glob-literal conversion lives in
    // `markers_to_glob_escapes` now (escape_glob is plain-backslash only).
    use crate::expand::stream::{Marker, SegStream};
    let mut seg = SegStream::new();
    seg.push_bytes(b"foo");
    seg.push_marker(Marker::Escape);
    seg.push_bytes(b"*");
    assert_eq!(
      crate::expand::escape::markers_to_glob_escapes(&seg),
      b"foo\\*".to_vec()
    );
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

    // After unescape_str, `my\ *` becomes `my{ESCAPE} *`; convert to a glob
    // pattern the way `expand()` does before matching.
    let unescaped = unescape_str(b"my\\ *");
    let pattern = crate::expand::escape::markers_to_glob_escapes(&unescaped);
    let result = expand_glob(&pattern, false)
      .into_iter()
      .map(|word| word.to_str_lossy().to_string())
      .join(" ");

    if let Some(prev) = saved_dir {
      let _ = std::env::set_current_dir(prev);
    }
    std::fs::remove_dir_all(&tmp).ok();

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
      expand_glob(p.as_bytes(), false)
        .into_iter()
        .map(|word| word.to_str_lossy().to_string())
        .map(|s| strip_markers(&s))
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
    let result = Expander::from_raw(raw.as_bytes(), TkFlags::empty())
      .expand()
      .unwrap();
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
