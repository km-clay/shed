pub mod alias;
pub mod arithmetic;
mod brace;
pub mod escape;
pub mod param;
pub mod prompt;
pub mod subshell;
pub mod util;
pub mod var;

pub use alias::{expand_aliases, expand_keymap, parse_key_alias};
pub use arithmetic::expand_arithmetic;
pub use escape::{as_var_val_display, escape_str, unescape_heredoc, unescape_math, unescape_str};
pub use param::{ParamExp, parse_pos_len, perform_param_expansion};
pub use prompt::{PromptTk, expand_prompt, format_cmd_runtime};
pub use subshell::{expand_cmd_sub, expand_proc_sub};
pub use util::{expand_case_pattern, glob_to_regex, is_var_name_ch};
pub use var::{expand_glob, expand_raw, expand_var};

use crate::libsh::error::{ShResult, ShResultExt};
use crate::match_loop;
use crate::parse::lex::{Tk, TkFlags, TkRule};
use crate::prelude::*;
use crate::readline::markers;

pub(crate) const PARAMETERS: [char; 7] = ['@', '*', '#', '$', '?', '!', '0'];

impl Tk {
  /// Create a new expanded token
  pub fn expand(self) -> ShResult<Self> {
    let flags = self.flags;
    let span = self.span.clone();
    let exp = Expander::new(self)?.expand().promote_err(span.clone())?;
    let class = TkRule::Expanded { exp };
    Ok(Self { class, span, flags })
  }
  pub fn expand_no_glob(self) -> ShResult<Self> {
    let flags = self.flags;
    let span = self.span.clone();
    let exp = Expander::new(self)?
      .no_glob()
      .expand()
      .promote_err(span.clone())?;
    let class = TkRule::Expanded { exp };
    Ok(Self { class, span, flags })
  }
  /// Perform word splitting
  pub fn get_words(&self) -> Vec<String> {
    match &self.class {
      TkRule::Expanded { exp } => exp.clone(),
      _ => vec![self.to_string()],
    }
  }

  pub fn get_first_word(&self) -> Option<String> {
    self.get_words().into_iter().next()
  }
}

pub struct Expander {
  flags: TkFlags,
  noglob: bool,
  raw: String,
}

impl Expander {
  pub fn new(raw: Tk) -> ShResult<Self> {
    let tk_raw = raw.span.as_str();
    Self::from_raw(tk_raw, raw.flags)
  }
  pub fn from_raw(raw: &str, flags: TkFlags) -> ShResult<Self> {
    let raw = brace::expand_braces_full(raw)?.join(" ");
    let unescaped = if flags.contains(TkFlags::IS_HEREDOC) {
      unescape_heredoc(&raw)
    } else {
      unescape_str(&raw)
    };
    Ok(Self {
      raw: unescaped,
      noglob: false,
      flags,
    })
  }
  pub fn no_glob(self) -> Self {
    Self {
      noglob: true,
      ..self
    }
  }
  pub fn expand(&mut self) -> ShResult<Vec<String>> {
    let mut chars = self.raw.chars().peekable();
    self.raw = expand_raw(&mut chars)?;

    let has_trailing_slash = self.raw.ends_with('/');
    let has_leading_dot_slash = self.raw.starts_with("./");

    if !self.noglob
      && let Ok(glob_exp) = expand_glob(&self.raw)
      && !glob_exp.is_empty()
    {
      self.raw = glob_exp;
    }

    if has_trailing_slash && !self.raw.ends_with('/') {
      // glob expansion can remove trailing slashes and leading dot-slashes, but we
      // want to preserve them so that things like tab completion don't break
      self.raw.push('/');
    }
    if has_leading_dot_slash && !self.raw.starts_with("./") {
      self.raw.insert_str(0, "./");
    }

    if self.flags.contains(TkFlags::IS_HEREDOC) {
      Ok(vec![self.raw.clone()])
    } else {
      Ok(self.split_words())
    }
  }
  pub fn split_words(&mut self) -> Vec<String> {
    let mut words = vec![];
    let mut chars = self.raw.chars();
    let mut cur_word = String::new();
    let mut was_quoted = false;
    let ifs = env::var("IFS").unwrap_or_else(|_| " \t\n".to_string());

    'outer: while let Some(ch) = chars.next() {
      match ch {
        markers::ESCAPE => {
          if let Some(next_ch) = chars.next() {
            cur_word.push(next_ch);
          }
        }
        markers::DUB_QUOTE | markers::SNG_QUOTE | markers::SUBSH => {
          match_loop!(chars.next() => q_ch, {
            markers::ARG_SEP if ch == markers::DUB_QUOTE => {
              words.push(mem::take(&mut cur_word));
            }
            _ if q_ch == ch => {
              was_quoted = true;
              continue 'outer; // Isn't rust cool
            }
            _ => cur_word.push(q_ch),
          });
        }
        _ if ifs.contains(ch) || ch == markers::ARG_SEP => {
          if cur_word.is_empty() && !was_quoted {
            cur_word.clear();
          } else {
            words.push(mem::take(&mut cur_word));
          }
          was_quoted = false;
        }
        _ => cur_word.push(ch),
      }
    }
    if words.is_empty() && (cur_word.is_empty() && !was_quoted) {
      return words;
    } else {
      words.push(cur_word);
    }

    words.retain(|w| w != &markers::NULL_EXPAND.to_string());
    words
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::VecDeque;

  use crate::state::{ArrIndex, VarFlags, VarKind, read_vars, write_vars};
  use crate::testutil::{TestGuard, test_input};

  // ===================== Word Splitting (TestGuard) =====================

  #[test]
  fn word_split_default_ifs() {
    let _guard = TestGuard::new();

    let mut exp = Expander {
      raw: "hello world\tfoo".to_string(),
      noglob: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["hello", "world", "foo"]);
  }

  #[test]
  fn word_split_custom_ifs() {
    let _guard = TestGuard::new();
    unsafe {
      std::env::set_var("IFS", ":");
    }

    let mut exp = Expander {
      raw: "a:b:c".to_string(),
      noglob: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["a", "b", "c"]);
  }

  #[test]
  fn word_split_empty_ifs() {
    let _guard = TestGuard::new();
    unsafe {
      std::env::set_var("IFS", "");
    }

    let mut exp = Expander {
      raw: "hello world".to_string(),
      noglob: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["hello world"]);
  }

  #[test]
  fn word_split_quoted_no_split() {
    let _guard = TestGuard::new();

    let raw = format!("{}hello world{}", markers::DUB_QUOTE, markers::DUB_QUOTE);
    let mut exp = Expander {
      raw,
      noglob: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["hello world"]);
  }

  // ===================== Escaped Word Splitting =====================

  #[test]
  fn word_split_escaped_space() {
    let _guard = TestGuard::new();

    let raw = format!("hello{}world", unescape_str("\\ "));
    let mut exp = Expander {
      raw,
      noglob: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["hello world"]);
  }

  #[test]
  fn word_split_escaped_tab() {
    let _guard = TestGuard::new();

    let raw = format!("hello{}world", unescape_str("\\\t"));
    let mut exp = Expander {
      raw,
      noglob: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["hello\tworld"]);
  }

  #[test]
  fn word_split_escaped_custom_ifs() {
    let _guard = TestGuard::new();
    unsafe {
      std::env::set_var("IFS", ":");
    }

    let raw = format!("a{}b:c", unescape_str("\\:"));
    let mut exp = Expander {
      raw,
      noglob: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["a:b", "c"]);
  }

  // ===================== Array Indexing (TestGuard) =====================

  #[test]
  fn array_index_first() {
    let _guard = TestGuard::new();
    write_vars(|v| {
      v.set_var(
        "arr",
        VarKind::arr_from_vec(vec!["a".into(), "b".into(), "c".into()]),
        VarFlags::NONE,
      )
    })
    .unwrap();

    let val = read_vars(|v| v.index_var("arr", ArrIndex::Literal(0))).unwrap();
    assert_eq!(val, "a");
  }

  #[test]
  fn array_index_second() {
    let _guard = TestGuard::new();
    write_vars(|v| {
      v.set_var(
        "arr",
        VarKind::arr_from_vec(vec!["x".into(), "y".into(), "z".into()]),
        VarFlags::NONE,
      )
    })
    .unwrap();

    let val = read_vars(|v| v.index_var("arr", ArrIndex::Literal(1))).unwrap();
    assert_eq!(val, "y");
  }

  #[test]
  fn array_all_elems() {
    let _guard = TestGuard::new();
    write_vars(|v| {
      v.set_var(
        "arr",
        VarKind::arr_from_vec(vec!["a".into(), "b".into(), "c".into()]),
        VarFlags::NONE,
      )
    })
    .unwrap();

    let elems = read_vars(|v| v.get_arr_elems("arr")).unwrap();
    assert_eq!(elems, vec!["a", "b", "c"]);
  }

  #[test]
  fn array_elem_count() {
    let _guard = TestGuard::new();
    write_vars(|v| {
      v.set_var(
        "arr",
        VarKind::arr_from_vec(vec!["a".into(), "b".into(), "c".into()]),
        VarFlags::NONE,
      )
    })
    .unwrap();

    let elems = read_vars(|v| v.get_arr_elems("arr")).unwrap();
    assert_eq!(elems.len(), 3);
  }

  // ===================== Direct Input Tests (TestGuard) =====================

  #[test]
  fn index_simple() {
    let guard = TestGuard::new();
    write_vars(|v| {
      v.set_var(
        "arr",
        VarKind::Arr(VecDeque::from(["foo".into(), "bar".into(), "biz".into()])),
        VarFlags::NONE,
      )
    })
    .unwrap();

    test_input("echo $arr").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "foo bar biz\n");
  }

  #[test]
  fn index_cursed() {
    let guard = TestGuard::new();
    write_vars(|v| {
      v.set_var(
        "arr",
        VarKind::Arr(VecDeque::from(["foo".into(), "bar".into(), "biz".into()])),
        VarFlags::NONE,
      )
    })
    .unwrap();
    write_vars(|v| {
      v.set_var(
        "i",
        VarKind::Arr(VecDeque::from(["0".into(), "1".into(), "2".into()])),
        VarFlags::NONE,
      )
    })
    .unwrap();

    test_input("echo $echo ${var:-${arr[$(($(echo ${i[@]:1:1}) + 1))]}}").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "biz\n");
  }
}
