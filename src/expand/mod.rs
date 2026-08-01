mod alias;
mod arithmetic;
mod brace;
mod escape;
pub(super) mod markers;
mod param;
mod prompt;
pub(crate) mod subshell;
mod util;
mod var;

use std::{convert::Into, rc::Rc};

pub(super) use alias::{expand_alias_with_pos, expand_aliases, expand_keymap};
pub(super) use arithmetic::{expand_arithmetic, expand_arithmetic_wrapped};
pub(super) use escape::{
  escape_glob, escape_str, read_hex, read_octal, read_stty_escape, shell_quote, shell_quote_fmt,
  unescape_heredoc, unescape_prompt, unescape_str,
};
pub(super) use prompt::expand_prompt;
pub(super) use util::{expand_case_pattern, glob_to_regex};
pub(super) use var::{expand_glob, expand_raw, expand_raw_inner};

use crate::{
  state::vars::{VarStr, VarStrSliceExt},
  util::scratch_buf,
};

use super::{
  eval::{
    self,
    lex::{Tk, TkFlags, TkRule},
  },
  keys, match_loop, procio, sherr, shopt, state, status_msg, try_var, util as crate_util,
  util::{QuoteState, ShErr, ShErrKind, ShResult, ShResultExt},
  var,
};

pub(crate) const PARAMETERS: [char; 8] = ['-', '@', '*', '#', '$', '?', '!', '0'];

impl Tk {
  /// Create a new expanded token
  pub fn expand(&self) -> ShResult<Self> {
    if let TkRule::Expanded { .. } = self.class {
      return Ok(self.clone());
    }
    if self.is_literal() {
      let raw = self.span.as_str().into();
      let class = TkRule::Expanded { exp: [raw].into() };
      return Ok(Self {
        class,
        ..self.clone()
      });
    }

    let flags = self.flags;
    let span = self.span.clone();
    let exp = Expander::new(self).expand().promote_err(span.clone())?;
    let class = TkRule::Expanded { exp: exp.into() };
    Ok(Self { class, span, flags })
  }
  pub fn expand_to_words(&self) -> ShResult<Rc<[VarStr]>> {
    if let TkRule::Expanded { exp } = &self.class {
      return Ok(exp.clone());
    }
    if self.is_literal() {
      return Ok([self.span.as_str().into()].into());
    }
    let span = self.span.clone();
    Expander::new(self)
      .expand()
      .map(Into::into)
      .promote_err(span)
  }
  pub fn expand_no_side_effects(&self) -> ShResult<Self> {
    if let TkRule::Expanded { .. } = self.class {
      return Ok(self.clone());
    }
    if self.is_literal() {
      let raw = self.span.as_str().into();
      let class = TkRule::Expanded { exp: [raw].into() };
      return Ok(Self {
        class,
        ..self.clone()
      });
    }

    let flags = self.flags;
    let span = self.span.clone();
    let exp: VarStr = Expander::new(self)
      .expand_no_side_effects()
      .promote_err(span.clone())?;

    let class = TkRule::Expanded { exp: [exp].into() };
    Ok(Self { class, span, flags })
  }
  pub fn expand_no_split(&self) -> ShResult<VarStr> {
    if let TkRule::Expanded { exp } = &self.class {
      return Ok(exp.join_with(" "));
    }
    if self.is_literal() {
      return Ok(self.span.as_str().into());
    }

    let span = self.span.clone();
    let exp = Expander::new(self)
      .no_glob()
      .no_split()
      .expand_no_split()
      .promote_err(span.clone())?;
    Ok(exp)
  }
  /// Like `expand()` but performs no word splitting. The resulting Tk has
  /// `exp` as a single-element vec holding the unsplit expansion. For use
  /// in contexts like assignment RHS where whitespace must be preserved.
  pub fn expand_unsplit(&self) -> ShResult<Self> {
    if let TkRule::Expanded { .. } = self.class {
      return Ok(self.clone());
    }
    if self.is_literal() {
      let raw = self.span.as_str().into();
      let class = TkRule::Expanded { exp: [raw].into() };
      return Ok(Self {
        class,
        ..self.clone()
      });
    }

    let flags = self.flags;
    let span = self.span.clone();
    let exp: VarStr = Expander::new(self)
      .no_split()
      .expand_no_split()
      .promote_err(span.clone())?;

    let class = TkRule::Expanded { exp: [exp].into() };
    Ok(Self { class, span, flags })
  }
  /// Perform word splitting
  pub fn get_words(&self) -> Rc<[VarStr]> {
    match &self.class {
      TkRule::Expanded { exp } => exp.clone(),
      _ => [self.as_str().into()].into(),
    }
  }

  pub fn get_first_word(&self) -> Option<VarStr> {
    self.get_words().iter().next().cloned()
  }
}

pub struct Expander {
  flags: TkFlags,
  noglob: bool,
  nosplit: bool,
  allow_side_effects: bool,
  raw: String,
}

impl Expander {
  pub fn new(raw: &Tk) -> Self {
    let tk_raw = raw.span.as_str();
    Self::from_raw(tk_raw, raw.flags)
  }
  pub fn from_raw(raw: &str, flags: TkFlags) -> Self {
    Self::from_raw_inner(raw, flags, true, false)
  }
  #[allow(dead_code)]
  pub fn from_raw_no_brace(raw: &str, flags: TkFlags) -> Self {
    Self::from_raw_inner(raw, flags, false, false)
  }
  /// Like `from_raw` but the operand is a parameter-expansion pattern or
  /// replacement (`${var#pat}`, `${var%pat}`, `${var/pat/rep}`): a bare `(` is
  /// literal, not a subshell.
  pub fn from_raw_pattern(raw: &str, flags: TkFlags) -> Self {
    Self::from_raw_inner(raw, flags, true, true)
  }
  /// Brace-free variant of `from_raw_pattern`.
  pub fn from_raw_no_brace_pattern(raw: &str, flags: TkFlags) -> Self {
    Self::from_raw_inner(raw, flags, false, true)
  }
  fn from_raw_inner(raw: &str, flags: TkFlags, expand_braces: bool, for_pattern: bool) -> Self {
    let raw = if expand_braces && raw.contains('{') {
      brace::expand_braces_full(raw).join(" ")
    } else {
      raw.to_string()
    };
    let unescaped = if flags.contains(TkFlags::IS_HEREDOC) {
      unescape_heredoc(&raw)
    } else if for_pattern {
      escape::unescape_pattern(&raw)
    } else {
      unescape_str(&raw)
    };
    Self {
      raw: unescaped,
      noglob: false,
      nosplit: false,
      allow_side_effects: true,
      flags,
    }
  }
  pub fn no_glob(self) -> Self {
    Self {
      noglob: true,
      ..self
    }
  }
  pub fn no_split(self) -> Self {
    Self {
      nosplit: true,
      ..self
    }
  }
  pub fn expand(&mut self) -> ShResult<Vec<VarStr>> {
    let mark_split = !self.flags.contains(TkFlags::IS_HEREDOC) && !self.nosplit;
    let res: VarStr = self.expand_inner(mark_split)?.into();
    let words = if mark_split {
      self.split_words()
    } else {
      vec![res]
    };

    if self.noglob {
      return Ok(
        words
          .into_iter()
          .map(|mut s| {
            escape::strip_escape_markers(&mut s);
            s
          })
          .collect(),
      );
    }

    let nullglob = shopt!(core.nullglob);
    let mut glob_words: Vec<VarStr> = Vec::with_capacity(words.len());

    for mut word in words {
      if !var::might_be_glob(&word) {
        escape::strip_escape_markers(&mut word);
        glob_words.push(word);
        continue;
      }

      let expansions = expand_glob(&word).unwrap_or_else(|_| vec![word.to_string()]);

      if expansions.is_empty() {
        if !nullglob {
          escape::strip_escape_markers(&mut word);
          glob_words.push(word);
        }
        continue;
      }

      for exp in expansions {
        let mut exp = var::restore_glob_prefix(&word, exp).into();
        escape::strip_escape_markers(&mut exp);
        glob_words.push(exp);
      }
    }

    Ok(glob_words)
  }
  pub fn expand_no_side_effects(&mut self) -> ShResult<VarStr> {
    self.allow_side_effects = false;
    let raw = self.expand_inner(false)?;
    Ok(markers::strip_markers(&raw).into())
  }
  pub fn expand_no_split(&mut self) -> ShResult<VarStr> {
    let raw = self.expand_inner(false)?;
    Ok(markers::strip_markers(&raw).into())
  }
  pub fn expand_keep_quotes(&mut self) -> ShResult<VarStr> {
    let mut raw = self.expand_inner(false)?;
    raw = raw.replace(markers::DUB_QUOTE, "\"");
    raw = raw.replace(markers::SNG_QUOTE, "'");
    Ok(markers::strip_markers(&raw).into())
  }
  pub fn expand_for_glob(&mut self) -> ShResult<VarStr> {
    let raw = self.expand_inner(false)?;
    Ok(escape::markers_to_glob_escapes(&raw).into())
  }
  pub fn expand_inner(&mut self, mark_split: bool) -> ShResult<String> {
    let mut chars = self.raw.chars().peekable();
    self.raw = expand_raw_inner(&mut chars, self.allow_side_effects, mark_split)?;

    Ok(self.raw.clone())
  }
  /// Perform POSIX word splitting.
  ///
  /// Resolves escapes and the special `$@`/`$*` cases, and performs IFS field
  /// splitting — but only inside `EXPAND_START`/`EXPAND_END` runs, i.e. on text
  /// that came from an unquoted expansion. Literal characters are never split.
  pub fn split_words(&mut self) -> Vec<VarStr> {
    let mut words = vec![];
    let mut chars = self.raw.chars();
    let mut cur_word = scratch_buf();
    let mut was_quoted = false;
    let ifs = state::util::get_separators();
    // Delimiter-run tracking: whitespace and non-whitespace IFS chars combine
    // into one run that delimits a single field. A second non-WS IFS in the
    // same run emits an additional empty field (per POSIX step 5).
    let mut in_delim_run = false;
    let mut delim_has_non_ws = false;

    let mut expansion_depth = 0;

    'outer: while let Some(ch) = chars.next() {
      match ch {
        markers::EXPAND_START => expansion_depth += 1,
        markers::EXPAND_END => {
          if expansion_depth > 0 {
            expansion_depth -= 1;
          }
        }
        markers::ESCAPE => {
          in_delim_run = false;
          delim_has_non_ws = false;
          if let Some(next_ch) = chars.next() {
            // Preserve the ESCAPE marker so glob expansion (running after
            // split_words) treats backslash-escaped meta chars as literal.
            // expand() will strip remaining ESCAPE markers after globbing.
            cur_word.push(markers::ESCAPE);
            cur_word.push(next_ch);
          }
        }
        markers::DUB_QUOTE | markers::SNG_QUOTE | markers::SUBSH => {
          in_delim_run = false;
          delim_has_non_ws = false;
          match_loop!(chars.next() => q_ch, {
            markers::ARG_SEP if ch == markers::DUB_QUOTE => {
              words.push(std::mem::take(&mut cur_word).into());
            }
            _ if q_ch == ch => {
              was_quoted = true;
              continue 'outer; // Isn't rust cool
            }
            _ => {
              // Quote-region content: glob meta chars inside quotes must
              // remain literal at glob time. Prepend ESCAPE so escape_glob
              // converts them to glob-literal form.
              if matches!(q_ch, '*' | '?' | '[' | ']') {
                cur_word.push(markers::ESCAPE);
              }
              cur_word.push(q_ch);
            }
          });
        }
        _ if (expansion_depth > 0 && ifs.contains(ch)) || ch == markers::ARG_SEP => {
          let is_ws = matches!(ch, ' ' | '\t' | '\n') || ch == markers::ARG_SEP;
          if !in_delim_run {
            // Just exited a field (or saw leading IFS). Decide whether to emit.
            if is_ws {
              if !cur_word.is_empty() || was_quoted {
                words.push(std::mem::take(&mut cur_word).into());
                was_quoted = false;
              }
            } else {
              // Non-WS IFS always emits (preserves leading/middle empty fields).
              words.push(std::mem::take(&mut cur_word).into());
              was_quoted = false;
              delim_has_non_ws = true;
            }
            in_delim_run = true;
          } else if !is_ws {
            // Already in a delimiter run and we hit another non-WS IFS char.
            if delim_has_non_ws {
              // Second non-WS in this run -> emit an empty field.
              words.push(VarStr::new());
            } else {
              // First non-WS adjacent to WS in the run -> just absorb into the run.
              delim_has_non_ws = true;
            }
          }
          // else: WS within an existing delim run -> absorb
        }
        _ => {
          in_delim_run = false;
          delim_has_non_ws = false;
          cur_word.push(ch);
        }
      }
    }
    if words.is_empty() && (cur_word.is_empty() && !was_quoted) {
      return words;
    } else if !cur_word.is_empty() || was_quoted {
      words.push(cur_word.into());
    }

    let null_exp = markers::NULL_EXPAND.to_string();
    words.retain(|w| w != &null_exp);
    for w in &mut words {
      if w.contains(markers::NULL_EXPAND) {
        *w = w.replace(markers::NULL_EXPAND, "").into();
      }
    }
    words
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::collections::VecDeque;

  use crate::state::{
    Shed,
    vars::{ArrIndex, VarFlags, VarKind, VarStr},
  };
  use crate::tests::testutil::{TestGuard, test_input};

  // ===================== Word Splitting (TestGuard) =====================

  #[test]
  fn word_split_default_ifs() {
    let _guard = TestGuard::new();

    let raw = format!(
      "{}hello world\tfoo{}",
      markers::EXPAND_START,
      markers::EXPAND_END
    );
    let mut exp = Expander {
      allow_side_effects: true,
      raw,
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["hello", "world", "foo"]);
  }

  #[test]
  fn word_split_custom_ifs() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::empty())).unwrap();

    let raw = format!("{}a:b:c{}", markers::EXPAND_START, markers::EXPAND_END);
    let mut exp = Expander {
      allow_side_effects: true,
      raw,
      noglob: false,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.split_words();
    assert_eq!(words, vec!["a", "b", "c"]);
  }

  #[test]
  fn word_split_empty_ifs() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(VarStr::default()), VarFlags::empty()))
      .unwrap();

    // Even as expansion output, an empty IFS suppresses all field splitting.
    let raw = format!(
      "{}hello world{}",
      markers::EXPAND_START,
      markers::EXPAND_END
    );
    let mut exp = Expander {
      allow_side_effects: true,
      raw,
      noglob: false,
      nosplit: false,
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
      allow_side_effects: true,
      raw,
      noglob: false,
      nosplit: false,
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
      allow_side_effects: true,
      raw,
      noglob: true,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.expand().unwrap();
    assert_eq!(words, vec!["hello world"]);
  }

  #[test]
  fn word_split_escaped_tab() {
    let _guard = TestGuard::new();

    let raw = format!("hello{}world", unescape_str("\\\t"));
    let mut exp = Expander {
      allow_side_effects: true,
      raw,
      noglob: true,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    let words = exp.expand().unwrap();
    assert_eq!(words, vec!["hello\tworld"]);
  }

  #[test]
  fn word_split_escaped_custom_ifs() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::empty())).unwrap();

    let raw = format!("a{}b:c", unescape_str("\\:"));
    let mut exp = Expander {
      allow_side_effects: true,
      raw,
      noglob: true,
      nosplit: false,
      flags: TkFlags::empty(),
    };
    // A literal word with no expansion is never field-split, so neither the
    // escaped `\:` nor the bare `:` splits — both are literal colons and the
    // word stays whole (matches bash: `IFS=:; echo a\:b:c` -> `a:b:c`).
    let words = exp.expand().unwrap();
    assert_eq!(words, vec!["a:b:c"]);
  }

  // ===================== Array Indexing (TestGuard) =====================

  #[test]
  fn array_index_first() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("arr", VarKind::arr(["a", "b", "c"]), VarFlags::empty())).unwrap();

    let val = Shed::vars(|v| v.index_var("arr", &ArrIndex::Literal(0))).unwrap();
    assert_eq!(val, "a");
  }

  #[test]
  fn array_index_second() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("arr", VarKind::arr(["x", "y", "z"]), VarFlags::empty())).unwrap();

    let val = Shed::vars(|v| v.index_var("arr", &ArrIndex::Literal(1))).unwrap();
    assert_eq!(val, "y");
  }

  #[test]
  fn array_all_elems() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("arr", VarKind::arr(["a", "b", "c"]), VarFlags::empty())).unwrap();

    let elems = Shed::vars(|v| v.try_get_arr_elems("arr")).unwrap();
    assert_eq!(elems, vec!["a", "b", "c"]);
  }

  #[test]
  fn array_elem_count() {
    let _guard = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("arr", VarKind::arr(["a", "b", "c"]), VarFlags::empty())).unwrap();

    let elems = Shed::vars(|v| v.try_get_arr_elems("arr")).unwrap();
    assert_eq!(elems.len(), 3);
  }

  // ===================== Direct Input Tests (TestGuard) =====================

  #[test]
  fn index_simple() {
    let guard = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "arr",
        VarKind::Arr(VecDeque::from(["foo".into(), "bar".into(), "biz".into()])),
        VarFlags::empty(),
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
    Shed::vars_mut(|v| {
      v.set_var(
        "arr",
        VarKind::Arr(VecDeque::from(["foo".into(), "bar".into(), "biz".into()])),
        VarFlags::empty(),
      )
    })
    .unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "i",
        VarKind::Arr(VecDeque::from(["0".into(), "1".into(), "2".into()])),
        VarFlags::empty(),
      )
    })
    .unwrap();

    test_input("echo $echo ${var:-${arr[$(($(echo ${i[@]:1:1}) + 1))]}}").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "biz\n");
  }
}
