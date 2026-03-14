use std::collections::{HashSet, VecDeque};
use std::iter::Peekable;
use std::str::{Chars, FromStr};

use ariadne::Fmt;
use glob::Pattern;
use regex::Regex;

use crate::libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt, next_color};
use crate::parse::execute::exec_input;
use crate::parse::lex::{LexFlags, LexStream, QuoteState, Tk, TkFlags, TkRule, is_hard_sep};
use crate::parse::{Redir, RedirType};
use crate::prelude::*;
use crate::procio::{IoBuf, IoFrame, IoMode, IoStack};
use crate::readline::keys::{KeyCode, KeyEvent, ModKeys};
use crate::readline::markers;
use crate::state::{
  self, ArrIndex, LogTab, VarFlags, VarKind, read_jobs, read_logic, read_shopts, read_vars,
  write_jobs, write_meta, write_vars,
};

const PARAMETERS: [char; 7] = ['@', '*', '#', '$', '?', '!', '0'];

impl Tk {
  /// Create a new expanded token
  pub fn expand(self) -> ShResult<Self> {
    let flags = self.flags;
    let span = self.span.clone();
    let exp = Expander::new(self)?.expand().promote_err(span.clone())?;
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
}

pub struct Expander {
	flags: TkFlags,
  raw: String,
}

impl Expander {
  pub fn new(raw: Tk) -> ShResult<Self> {
    let tk_raw = raw.span.as_str();
    Self::from_raw(tk_raw, raw.flags)
  }
  pub fn from_raw(raw: &str, flags: TkFlags) -> ShResult<Self> {
    let raw = expand_braces_full(raw)?.join(" ");
    let unescaped = unescape_str(&raw);
    Ok(Self { raw: unescaped, flags })
  }
  pub fn expand(&mut self) -> ShResult<Vec<String>> {
    let mut chars = self.raw.chars().peekable();
    self.raw = expand_raw(&mut chars)?;

    let has_trailing_slash = self.raw.ends_with('/');
    let has_leading_dot_slash = self.raw.starts_with("./");

    if let Ok(glob_exp) = expand_glob(&self.raw)
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
          while let Some(q_ch) = chars.next() {
            match q_ch {
              markers::ARG_SEP if ch == markers::DUB_QUOTE => {
                words.push(mem::take(&mut cur_word));
              }
              _ if q_ch == ch => {
                was_quoted = true;
                continue 'outer; // Isn't rust cool
              }
              _ => cur_word.push(q_ch),
            }
          }
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

/// Check if a string contains valid brace expansion patterns.
/// Returns true if there's a valid {a,b} or {1..5} pattern at the outermost
/// level.
fn has_braces(s: &str) -> bool {
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
fn expand_braces_full(input: &str) -> ShResult<Vec<String>> {
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

pub fn expand_raw(chars: &mut Peekable<Chars<'_>>) -> ShResult<String> {
  let mut result = String::new();

  while let Some(ch) = chars.next() {
    match ch {
      markers::TILDE_SUB => {
        let home = env::var("HOME").unwrap_or_default();
        result.push_str(&home);
      }
      markers::PROC_SUB_OUT => {
        let mut inner = String::new();
        while let Some(ch) = chars.next() {
          match ch {
            markers::PROC_SUB_OUT => break,
            _ => inner.push(ch),
          }
        }
        let fd_path = expand_proc_sub(&inner, false)?;
        result.push_str(&fd_path);
      }
      markers::PROC_SUB_IN => {
        let mut inner = String::new();
        while let Some(ch) = chars.next() {
          match ch {
            markers::PROC_SUB_IN => break,
            _ => inner.push(ch),
          }
        }
        let fd_path = expand_proc_sub(&inner, true)?;
        result.push_str(&fd_path);
      }
      markers::VAR_SUB => {
        let expanded = expand_var(chars)?;
        result.push_str(&expanded);
      }
      _ => result.push(ch),
    }
  }
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
  let mut in_operator = false;
  while let Some(&ch) = chars.peek() {
    match ch {
      markers::SUBSH if var_name.is_empty() => {
        chars.next(); // now safe to consume
        let mut subsh_body = String::new();
        let mut found_end = false;
        while let Some(c) = chars.next() {
          if c == markers::SUBSH {
            found_end = true;
            break;
          }
          subsh_body.push(c);
        }
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
              read_vars(|v| v.get_arr_elems(&var_name))?.join(&arg_sep)
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

              read_vars(|v| v.get_arr_elems(&var_name))?.join(&ifs)
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
            ShErr::simple(
              ShErrKind::ParseErr,
              format!("Array index must be a number, got '{expanded_idx}'"),
            )
          })?);
        }
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
    }
  }
  if !var_name.is_empty() {
    let var_val = read_vars(|v| v.get_var(&var_name));
    Ok(var_val)
  } else {
    Ok(String::new())
  }
}

pub fn expand_glob(raw: &str) -> ShResult<String> {
  let mut words = vec![];

  let opts = glob::MatchOptions {
    require_literal_leading_dot: !crate::state::read_shopts(|s| s.core.dotglob),
    ..Default::default()
  };
  for entry in glob::glob_with(raw, opts)
    .map_err(|_| ShErr::simple(ShErrKind::SyntaxErr, "Invalid glob pattern"))?
  {
    let entry =
      entry.map_err(|_| ShErr::simple(ShErrKind::SyntaxErr, "Invalid filename found in glob"))?;
    let entry_raw = entry
      .to_str()
      .ok_or_else(|| ShErr::simple(ShErrKind::SyntaxErr, "Non-UTF8 filename found in glob"))?;
    let escaped = escape_str(entry_raw, true);

    words.push(escaped)
  }
  Ok(words.join(" "))
}

enum ArithTk {
  Num(f64),
  Op(ArithOp),
  LParen,
  RParen,
  Var(String),
}

impl ArithTk {
  pub fn tokenize(raw: &str) -> ShResult<Option<Vec<Self>>> {
    let mut tokens = Vec::new();
    let mut chars = raw.chars().peekable();

    while let Some(&ch) = chars.peek() {
      match ch {
        ' ' | '\t' => {
          chars.next();
        }
        '0'..='9' | '.' => {
          let mut num = String::new();
          while let Some(&digit) = chars.peek() {
            if digit.is_ascii_digit() || digit == '.' {
              num.push(digit);
              chars.next();
            } else {
              break;
            }
          }
          let Ok(num) = num.parse::<f64>() else {
            panic!()
          };
          tokens.push(Self::Num(num));
        }
        '+' | '-' | '*' | '/' | '%' => {
          let mut buf = String::new();
          buf.push(ch);
          tokens.push(Self::Op(buf.parse::<ArithOp>().unwrap()));
          chars.next();
        }
        '(' => {
          tokens.push(Self::LParen);
          chars.next();
        }
        ')' => {
          tokens.push(Self::RParen);
          chars.next();
        }
        _ if ch.is_alphabetic() || ch == '_' => {
          chars.next();
          let mut var_name = ch.to_string();
          while let Some(ch) = chars.peek() {
            match ch {
              _ if ch.is_alphabetic() || *ch == '_' => {
                var_name.push(*ch);
                chars.next();
              }
              _ => break,
            }
          }

          tokens.push(Self::Var(var_name));
        }
        _ => {
          return Ok(None);
        }
      }
    }

    Ok(Some(tokens))
  }

  fn to_rpn(tokens: Vec<ArithTk>) -> ShResult<Vec<ArithTk>> {
    let mut output = Vec::new();
    let mut ops = Vec::new();

    fn precedence(op: &ArithOp) -> usize {
      match op {
        ArithOp::Add | ArithOp::Sub => 1,
        ArithOp::Mul | ArithOp::Div | ArithOp::Mod => 2,
      }
    }

    for token in tokens {
      match token {
        ArithTk::Num(_) => output.push(token),
        ArithTk::Op(op1) => {
          while let Some(ArithTk::Op(op2)) = ops.last() {
            if precedence(op2) >= precedence(&op1) {
              output.push(ops.pop().unwrap());
            } else {
              break;
            }
          }
          ops.push(ArithTk::Op(op1));
        }
        ArithTk::LParen => ops.push(ArithTk::LParen),
        ArithTk::RParen => {
          while let Some(top) = ops.pop() {
            if let ArithTk::LParen = top {
              break;
            } else {
              output.push(top);
            }
          }
        }
        ArithTk::Var(var) => {
          let Some(val) = read_vars(|v| v.try_get_var(&var)) else {
            return Err(ShErr::simple(
              ShErrKind::NotFound,
              format!(
                "Undefined variable in arithmetic expression: '{}'",
                var.fg(next_color())
              ),
            ));
          };
          let Ok(num) = val.parse::<f64>() else {
            return Err(ShErr::simple(
              ShErrKind::ParseErr,
              format!(
                "Variable '{}' does not contain a number",
                var.fg(next_color())
              ),
            ));
          };

          output.push(ArithTk::Num(num));
        }
      }
    }

    while let Some(op) = ops.pop() {
      output.push(op);
    }

    Ok(output)
  }
  pub fn eval_rpn(tokens: Vec<ArithTk>) -> ShResult<f64> {
    let mut stack = Vec::new();

    for token in tokens {
      match token {
        ArithTk::Num(n) => stack.push(n),
        ArithTk::Op(op) => {
          let rhs = stack.pop().ok_or(ShErr::simple(
            ShErrKind::ParseErr,
            "Missing right-hand operand",
          ))?;
          let lhs = stack.pop().ok_or(ShErr::simple(
            ShErrKind::ParseErr,
            "Missing left-hand operand",
          ))?;
          let result = match op {
            ArithOp::Add => lhs + rhs,
            ArithOp::Sub => lhs - rhs,
            ArithOp::Mul => lhs * rhs,
            ArithOp::Div => lhs / rhs,
            ArithOp::Mod => lhs % rhs,
          };
          stack.push(result);
        }
        _ => {
          return Err(ShErr::simple(
            ShErrKind::ParseErr,
            "Unexpected token during evaluation",
          ));
        }
      }
    }

    if stack.len() != 1 {
      return Err(ShErr::simple(
        ShErrKind::ParseErr,
        "Invalid arithmetic expression",
      ));
    }

    Ok(stack[0])
  }
}

enum ArithOp {
  Add,
  Sub,
  Mul,
  Div,
  Mod,
}

impl FromStr for ArithOp {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    assert!(s.len() == 1);
    match s.chars().next().unwrap() {
      '+' => Ok(Self::Add),
      '-' => Ok(Self::Sub),
      '*' => Ok(Self::Mul),
      '/' => Ok(Self::Div),
      '%' => Ok(Self::Mod),
      _ => Err(ShErr::simple(
        ShErrKind::ParseErr,
        "Invalid arithmetic operator",
      )),
    }
  }
}

pub fn expand_arithmetic(raw: &str) -> ShResult<Option<String>> {
  let body = raw.strip_prefix('(').unwrap().strip_suffix(')').unwrap(); // Unwraps are safe here, we already checked for the parens
  let unescaped = unescape_math(body);
  let expanded = expand_raw(&mut unescaped.chars().peekable())?;
  let Some(tokens) = ArithTk::tokenize(&expanded)? else {
    return Ok(None);
  };
  let rpn = ArithTk::to_rpn(tokens)?;
  let result = ArithTk::eval_rpn(rpn)?;
  Ok(Some(result.to_string()))
}

pub fn expand_proc_sub(raw: &str, is_input: bool) -> ShResult<String> {
  // FIXME: Still a lot of issues here
  // Seems like debugging will be a massive effort
  let (rpipe, wpipe) = IoMode::get_pipes();
  let rpipe_raw = rpipe.src_fd();
  let wpipe_raw = wpipe.src_fd();

  let (proc_fd, register_fd, redir_type, path) = match is_input {
    false => (
      wpipe,
      rpipe,
      RedirType::Output,
      format!("/proc/self/fd/{}", rpipe_raw),
    ),
    true => (
      rpipe,
      wpipe,
      RedirType::Input,
      format!("/proc/self/fd/{}", wpipe_raw),
    ),
  };

  match unsafe { fork()? } {
    ForkResult::Child => {
      // Close the parent's pipe end so the grandchild doesn't inherit it.
      // Without this, >(cmd) hangs because the command holds its own
      // pipe's write end open and never sees EOF.
      drop(register_fd);

      let redir = Redir::new(proc_fd, redir_type);
      let io_frame = IoFrame::from_redir(redir);
      let mut io_stack = IoStack::new();
      io_stack.push_frame(io_frame);

      if let Err(e) = exec_input(
        raw.to_string(),
        Some(io_stack),
        false,
        Some("process_sub".into()),
      ) {
        e.print_error();
        exit(1);
      }
      exit(0);
    }
    ForkResult::Parent { child } => {
      write_jobs(|j| j.register_fd(child, register_fd));
      // Do not wait; process may run in background
      Ok(path)
    }
  }
}

/// Get the command output of a given command input as a String
pub fn expand_cmd_sub(raw: &str) -> ShResult<String> {
  if raw.starts_with('(')
    && raw.ends_with(')')
    && let Some(output) = expand_arithmetic(raw)?
  {
    return Ok(output); // It's actually an arithmetic sub
  }
  let (rpipe, wpipe) = IoMode::get_pipes();
  let cmd_sub_redir = Redir::new(wpipe, RedirType::Output);
  let cmd_sub_io_frame = IoFrame::from_redir(cmd_sub_redir);
  let mut io_stack = IoStack::new();
  let mut io_buf = IoBuf::new(rpipe);

  match unsafe { fork()? } {
    ForkResult::Child => {
      io_stack.push_frame(cmd_sub_io_frame);
      if let Err(e) = exec_input(
        raw.to_string(),
        Some(io_stack),
        false,
        Some("command_sub".into()),
      ) {
        e.print_error();
        unsafe { libc::_exit(1) };
      }
      let status = state::get_status();
      unsafe { libc::_exit(status) };
    }
    ForkResult::Parent { child } => {
      std::mem::drop(cmd_sub_io_frame); // Closes the write pipe

      // Read output first (before waiting) to avoid deadlock if child fills pipe
      // buffer
      loop {
        match io_buf.fill_buffer() {
          Ok(()) => break,
          Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
          Err(e) => return Err(e.into()),
        }
      }

      // Wait for child with EINTR retry
      let status = loop {
        match waitpid(child, Some(WtFlag::WSTOPPED)) {
          Ok(status) => break status,
          Err(Errno::EINTR) => continue,
          Err(e) => return Err(e.into()),
        }
      };

      match status {
        WtStat::Exited(_, code) => {
          state::set_status(code);
          Ok(io_buf.as_str()?.trim_end().to_string())
        }
        _ => Err(ShErr::simple(ShErrKind::InternalErr, "Command sub failed")),
      }
    }
  }
}

/// Strip ESCAPE markers from a string, leaving the characters they protect intact.
fn strip_escape_markers(s: &str) -> String {
  s.replace(markers::ESCAPE, "")
}

/// Processes strings into intermediate representations that are more readable
/// by the program
///
/// Clean up a single layer of escape characters, and then replace control
/// characters like '$' with a non-character unicode representation that is
/// unmistakable by the rest of the code
pub fn unescape_str(raw: &str) -> String {
  let mut chars = raw.chars().peekable();
  let mut result = String::new();
  let mut first_char = true;

  while let Some(ch) = chars.next() {
    match ch {
      '~' if first_char => result.push(markers::TILDE_SUB),
      '\\' => {
        if let Some(next_ch) = chars.next() {
          result.push(markers::ESCAPE);
          result.push(next_ch)
        }
      }
      '(' => {
        result.push(markers::SUBSH);
        let mut paren_count = 1;
        while let Some(subsh_ch) = chars.next() {
          match subsh_ch {
            '\\' => {
              result.push(subsh_ch);
              if let Some(next_ch) = chars.next() {
                result.push(next_ch)
              }
            }
            '$' if chars.peek() != Some(&'(') => result.push(markers::VAR_SUB),
            '(' => {
              paren_count += 1;
              result.push(subsh_ch)
            }
            ')' => {
              paren_count -= 1;
              if paren_count == 0 {
                result.push(markers::SUBSH);
                break;
              } else {
                result.push(subsh_ch)
              }
            }
            _ => result.push(subsh_ch),
          }
        }
      }
      '"' => {
        result.push(markers::DUB_QUOTE);
        while let Some(q_ch) = chars.next() {
          match q_ch {
            '\\' => {
              if let Some(next_ch) = chars.next() {
                match next_ch {
                  '"' | '\\' | '`' | '$' | '!' => {
                    // discard the backslash
                  }
                  _ => {
                    result.push(q_ch);
                  }
                }
                result.push(next_ch);
              }
            }
            '$' if chars.peek() != Some(&'\'') => {
              result.push(markers::VAR_SUB);
              if chars.peek() == Some(&'(') {
                chars.next();
                let mut paren_count = 1;
                result.push(markers::SUBSH);
                while let Some(subsh_ch) = chars.next() {
                  match subsh_ch {
                    '\\' => {
                      result.push(subsh_ch);
                      if let Some(next_ch) = chars.next() {
                        result.push(next_ch)
                      }
                    }
                    '(' => {
                      result.push(subsh_ch);
                      paren_count += 1;
                    }
                    ')' => {
                      paren_count -= 1;
                      if paren_count <= 0 {
                        result.push(markers::SUBSH);
                        break;
                      } else {
                        result.push(subsh_ch);
                      }
                    }
                    _ => result.push(subsh_ch),
                  }
                }
              }
            }
            '$' => {
              chars.next();
              while let Some(q_ch) = chars.next() {
                match q_ch {
                  '\'' => {
                    break;
                  }
                  '\\' => {
                    if let Some(esc) = chars.next() {
                      match esc {
                        'n' => result.push('\n'),
                        't' => result.push('\t'),
                        'r' => result.push('\r'),
                        '\'' => result.push('\''),
                        '\\' => result.push('\\'),
                        'a' => result.push('\x07'),
                        'b' => result.push('\x08'),
                        'e' | 'E' => result.push('\x1b'),
                        'v' => result.push('\x0b'),
                        'x' => {
                          let mut hex = String::new();
                          if let Some(h1) = chars.next() {
                            hex.push(h1);
                          } else {
                            result.push_str("\\x");
                            continue;
                          }
                          if let Some(h2) = chars.next() {
                            hex.push(h2);
                          } else {
                            result.push_str(&format!("\\x{hex}"));
                            continue;
                          }
                          if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                            result.push(byte as char);
                          } else {
                            result.push_str(&format!("\\x{hex}"));
                            continue;
                          }
                        }
                        'o' => {
                          let mut oct = String::new();
                          for _ in 0..3 {
                            if let Some(o) = chars.peek() {
                              if o.is_digit(8) {
                                oct.push(*o);
                                chars.next();
                              } else {
                                break;
                              }
                            } else {
                              break;
                            }
                          }
                          if let Ok(byte) = u8::from_str_radix(&oct, 8) {
                            result.push(byte as char);
                          } else {
                            result.push_str(&format!("\\o{oct}"));
                            continue;
                          }
                        }
                        _ => result.push(esc),
                      }
                    }
                  }
                  _ => result.push(q_ch),
                }
              }
            }
            '"' => {
              result.push(markers::DUB_QUOTE);
              break;
            }
            _ => result.push(q_ch),
          }
        }
      }
      '\'' => {
        result.push(markers::SNG_QUOTE);
        while let Some(q_ch) = chars.next() {
          match q_ch {
            '\\' => {
              if chars.peek() == Some(&'\'') {
                result.push('\'');
                chars.next();
              } else {
                result.push('\\');
              }
            }
            '\'' => {
              result.push(markers::SNG_QUOTE);
              break;
            }
            _ => result.push(q_ch),
          }
        }
      }
      '<' if chars.peek() == Some(&'(') => {
        chars.next();
        let mut paren_count = 1;
        result.push(markers::PROC_SUB_OUT);
        while let Some(subsh_ch) = chars.next() {
          match subsh_ch {
            '\\' => {
              result.push(subsh_ch);
              if let Some(next_ch) = chars.next() {
                result.push(next_ch)
              }
            }
            '(' => {
              result.push(subsh_ch);
              paren_count += 1;
            }
            ')' => {
              paren_count -= 1;
              if paren_count <= 0 {
                result.push(markers::PROC_SUB_OUT);
                break;
              } else {
                result.push(subsh_ch);
              }
            }
            _ => result.push(subsh_ch),
          }
        }
      }
      '>' if chars.peek() == Some(&'(') => {
        chars.next();
        let mut paren_count = 1;
        result.push(markers::PROC_SUB_IN);
        while let Some(subsh_ch) = chars.next() {
          match subsh_ch {
            '\\' => {
              result.push(subsh_ch);
              if let Some(next_ch) = chars.next() {
                result.push(next_ch)
              }
            }
            '(' => {
              result.push(subsh_ch);
              paren_count += 1;
            }
            ')' => {
              paren_count -= 1;
              if paren_count <= 0 {
                result.push(markers::PROC_SUB_IN);
                break;
              } else {
                result.push(subsh_ch);
              }
            }
            _ => result.push(subsh_ch),
          }
        }
      }
      '$' if chars.peek() == Some(&'\'') => {
        chars.next();
        result.push(markers::SNG_QUOTE);
        while let Some(q_ch) = chars.next() {
          match q_ch {
            '\'' => {
              result.push(markers::SNG_QUOTE);
              break;
            }
            '\\' => {
              if let Some(esc) = chars.next() {
                match esc {
                  'n' => result.push('\n'),
                  't' => result.push('\t'),
                  'r' => result.push('\r'),
                  '\'' => result.push('\''),
                  '\\' => result.push('\\'),
                  'a' => result.push('\x07'),
                  'b' => result.push('\x08'),
                  'e' | 'E' => result.push('\x1b'),
                  'v' => result.push('\x0b'),
                  'x' => {
                    let mut hex = String::new();
                    if let Some(h1) = chars.next() {
                      hex.push(h1);
                    } else {
                      result.push_str("\\x");
                      continue;
                    }
                    if let Some(h2) = chars.next() {
                      hex.push(h2);
                    } else {
                      result.push_str(&format!("\\x{hex}"));
                      continue;
                    }
                    if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                      result.push(byte as char);
                    } else {
                      result.push_str(&format!("\\x{hex}"));
                      continue;
                    }
                  }
                  'o' => {
                    let mut oct = String::new();
                    for _ in 0..3 {
                      if let Some(o) = chars.peek() {
                        if o.is_digit(8) {
                          oct.push(*o);
                          chars.next();
                        } else {
                          break;
                        }
                      } else {
                        break;
                      }
                    }
                    if let Ok(byte) = u8::from_str_radix(&oct, 8) {
                      result.push(byte as char);
                    } else {
                      result.push_str(&format!("\\o{oct}"));
                      continue;
                    }
                  }
                  _ => result.push(esc),
                }
              }
            }
            _ => result.push(q_ch),
          }
        }
      }
      '$' => {
        result.push(markers::VAR_SUB);
        if chars.peek() == Some(&'$') {
          chars.next();
          result.push('$');
        }
      }
      _ => result.push(ch),
    }
    first_char = false;
  }

  result
}

/// Opposite of unescape_str - escapes a string to be executed as literal text
/// Used for completion results, and glob filename matches.
pub fn escape_str(raw: &str, use_marker: bool) -> String {
  let mut result = String::new();
  let mut chars = raw.chars();

  while let Some(ch) = chars.next() {
    match ch {
      '\'' | '"' | '\\' | '|' | '&' | ';' | '(' | ')' | '<' | '>' | '$' | '*' | '!' | '`' | '{'
      | '?' | '[' | '#' | ' ' | '\t' | '\n' => {
        if use_marker {
          result.push(markers::ESCAPE);
        } else {
          result.push('\\');
        }
        result.push(ch);
        continue;
      }
      '~' if result.is_empty() => {
        if use_marker {
          result.push(markers::ESCAPE);
        } else {
          result.push('\\');
        }
        result.push(ch);
        continue;
      }
      _ => {
        result.push(ch);
        continue;
      }
    }
  }

  result
}

pub fn unescape_math(raw: &str) -> String {
  let mut chars = raw.chars().peekable();
  let mut result = String::new();

  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        if let Some(next_ch) = chars.next() {
          result.push(next_ch)
        }
      }
      '$' => {
        result.push(markers::VAR_SUB);
        if chars.peek() == Some(&'(') {
          result.push(markers::SUBSH);
          chars.next();
          let mut paren_count = 1;
          while let Some(subsh_ch) = chars.next() {
            match subsh_ch {
              '\\' => {
                result.push(subsh_ch);
                if let Some(next_ch) = chars.next() {
                  result.push(next_ch)
                }
              }
              '$' if chars.peek() != Some(&'(') => result.push(markers::VAR_SUB),
              '(' => {
                paren_count += 1;
                result.push(subsh_ch)
              }
              ')' => {
                paren_count -= 1;
                if paren_count == 0 {
                  result.push(markers::SUBSH);
                  break;
                } else {
                  result.push(subsh_ch)
                }
              }
              _ => result.push(subsh_ch),
            }
          }
        }
      }
      _ => result.push(ch),
    }
  }
  result
}

#[derive(Debug)]
pub enum ParamExp {
  Len,                               // #var_name
  DefaultUnsetOrNull(String),        // :-
  DefaultUnset(String),              // -
  SetDefaultUnsetOrNull(String),     // :=
  SetDefaultUnset(String),           // =
  AltSetNotNull(String),             // :+
  AltNotNull(String),                // +
  ErrUnsetOrNull(String),            // :?
  ErrUnset(String),                  // ?
  Substr(usize),                     // :pos
  SubstrLen(usize, usize),           // :pos:len
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

    let parse_err = || {
      Err(ShErr::simple(
        ShErrKind::SyntaxErr,
        "Invalid parameter expansion",
      ))
    };

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
        Some(l) => SubstrLen(pos, l),
        None => Substr(pos),
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
  let vars = read_vars(|v| v.clone());
  let mut chars = raw.chars();
  let mut var_name = String::new();
  let mut rest = String::new();
  if raw.starts_with('#') {
    return Ok(
      vars
        .get_var(raw.strip_prefix('#').unwrap())
        .len()
        .to_string(),
    );
  }

  while let Some(ch) = chars.next() {
    match ch {
      '!' | '#' | '%' | ':' | '-' | '+' | '=' | '/' | '?' => {
        rest.push(ch);
        rest.push_str(&chars.collect::<String>());
        break;
      }
      _ => var_name.push(ch),
    }
  }

  if let Ok(expansion) = rest.parse::<ParamExp>() {
    match expansion {
      ParamExp::Len => unreachable!(),
      ParamExp::DefaultUnsetOrNull(default) => {
        match vars.try_get_var(&var_name).filter(|v| !v.is_empty()) {
          Some(val) => Ok(val),
          None => expand_raw(&mut default.chars().peekable()),
        }
      }
      ParamExp::DefaultUnset(default) => match vars.try_get_var(&var_name) {
        Some(val) => Ok(val),
        None => expand_raw(&mut default.chars().peekable()),
      },
      ParamExp::SetDefaultUnsetOrNull(default) => {
        match vars.try_get_var(&var_name).filter(|v| !v.is_empty()) {
          Some(val) => Ok(val),
          None => {
            let expanded = expand_raw(&mut default.chars().peekable())?;
            write_vars(|v| v.set_var(&var_name, VarKind::Str(expanded.clone()), VarFlags::NONE))?;
            Ok(expanded)
          }
        }
      }
      ParamExp::SetDefaultUnset(default) => match vars.try_get_var(&var_name) {
        Some(val) => Ok(val),
        None => {
          let expanded = expand_raw(&mut default.chars().peekable())?;
          write_vars(|v| v.set_var(&var_name, VarKind::Str(expanded.clone()), VarFlags::NONE))?;
          Ok(expanded)
        }
      },
      ParamExp::AltSetNotNull(alt) => match vars.try_get_var(&var_name).filter(|v| !v.is_empty()) {
        Some(_) => expand_raw(&mut alt.chars().peekable()),
        None => Ok("".into()),
      },
      ParamExp::AltNotNull(alt) => match vars.try_get_var(&var_name) {
        Some(_) => expand_raw(&mut alt.chars().peekable()),
        None => Ok("".into()),
      },
      ParamExp::ErrUnsetOrNull(err) => {
        match vars.try_get_var(&var_name).filter(|v| !v.is_empty()) {
          Some(val) => Ok(val),
          None => {
            let expanded = expand_raw(&mut err.chars().peekable())?;
            Err(ShErr::simple(ShErrKind::ExecFail, expanded))
          }
        }
      }
      ParamExp::ErrUnset(err) => match vars.try_get_var(&var_name) {
        Some(val) => Ok(val),
        None => {
          let expanded = expand_raw(&mut err.chars().peekable())?;
          Err(ShErr::simple(ShErrKind::ExecFail, expanded))
        }
      },
      ParamExp::Substr(pos) => {
        let value = vars.get_var(&var_name);
        if let Some(substr) = value.get(pos..) {
          Ok(substr.to_string())
        } else {
          Ok(value)
        }
      }
      ParamExp::SubstrLen(pos, len) => {
        let value = vars.get_var(&var_name);
        let end = pos.saturating_add(len);
        if let Some(substr) = value.get(pos..end) {
          Ok(substr.to_string())
        } else {
          Ok(value)
        }
      }
      ParamExp::RemShortestPrefix(prefix) => {
        let value = vars.get_var(&var_name);
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
        let value = vars.get_var(&var_name);
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
        let value = vars.get_var(&var_name);
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
        let value = vars.get_var(&var_name);
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
        let value = vars.get_var(&var_name);
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
        let value = vars.get_var(&var_name);
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
        let value = vars.get_var(&var_name);
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
        let value = vars.get_var(&var_name);
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
        let mut match_vars = vec![];
        for var in vars.flatten_vars().keys() {
          if var.starts_with(&prefix) {
            match_vars.push(var.clone())
          }
        }
        Ok(match_vars.join(" "))
      }
      ParamExp::ExpandInnerVar(var_name) => {
        let value = vars.get_var(&var_name);
        Ok(vars.get_var(&value))
      }
    }
  } else {
    Ok(vars.get_var(&var_name))
  }
}

/// Expand a case pattern: performs variable/command expansion while preserving
/// glob metacharacters that were inside quotes as literals (by backslash-escaping them).
/// Unquoted glob chars (*, ?, [) pass through for glob_to_regex to interpret.
pub fn expand_case_pattern(raw: &str) -> ShResult<String> {
  let unescaped = unescape_str(raw);
  let expanded = expand_raw(&mut unescaped.chars().peekable())?;

  let mut result = String::new();
  let mut in_quote = false;
  let mut chars = expanded.chars();

  while let Some(ch) = chars.next() {
    match ch {
      markers::DUB_QUOTE | markers::SNG_QUOTE => {
        in_quote = !in_quote;
      }
      markers::ESCAPE => {
        if let Some(next_ch) = chars.next() {
          result.push(next_ch);
        }
      }
      '*' | '?' | '[' | ']' if in_quote => {
        result.push('\\');
        result.push(ch);
      }
      _ => result.push(ch),
    }
  }
  Ok(result)
}

pub fn glob_to_regex(glob: &str, anchored: bool) -> Regex {
  let mut regex = String::new();
  if anchored {
    regex.push('^');
  }
  let mut chars = glob.chars();
  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        // Shell escape: next char is literal
        if let Some(esc) = chars.next() {
          // Some characters have special meaning after \ in regex
          // (e.g. \< is word boundary), so use hex escape for safety
          regex.push_str(&format!("\\x{:02x}", esc as u32));
        }
      }
      '*' => regex.push_str(".*"),
      '?' => regex.push('.'),
      '[' => {
        // Pass through character class [...] as-is (glob and regex syntax match)
        regex.push('[');
        while let Some(bc) = chars.next() {
          regex.push(bc);
          if bc == ']' {
            break;
          }
        }
      }
      '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' => {
        regex.push('\\');
        regex.push(ch);
      }
      _ => regex.push(ch),
    }
  }
  if anchored {
    regex.push('$');
  }
  Regex::new(&regex).unwrap()
}

#[derive(Debug)]
pub enum PromptTk {
  AsciiOct(i32),
  Text(String),
  AnsiSeq(String),
  Function(String), // Expands to the output of any defined shell function
  VisGrp,
  UserSeq,
  RuntimeMillis,
  RuntimeFormatted,
  Weekday,
  Dquote,
  Squote,
  Return,
  Newline,
  Pwd,
  PwdShort,
  Hostname,
  HostnameShort,
  ShellName,
  Username,
  PromptSymbol,
  ExitCode,
  SuccessSymbol,
  FailureSymbol,
  JobCount,
  VisGroupOpen,
  VisGroupClose,
}

pub fn format_cmd_runtime(dur: std::time::Duration) -> String {
  const ETERNITY: u128 = f32::INFINITY as u128;
  let mut micros = dur.as_micros();
  let mut millis = 0;
  let mut seconds = 0;
  let mut minutes = 0;
  let mut hours = 0;
  let mut days = 0;
  let mut weeks = 0;
  let mut months = 0;
  let mut years = 0;
  let mut decades = 0;
  let mut centuries = 0;
  let mut millennia = 0;
  let mut epochs = 0;
  let mut aeons = 0;
  let mut eternities = 0;

  if micros >= 1000 {
    millis = micros / 1000;
    micros %= 1000;
  }
  if millis >= 1000 {
    seconds = millis / 1000;
    millis %= 1000;
  }
  if seconds >= 60 {
    minutes = seconds / 60;
    seconds %= 60;
  }
  if minutes >= 60 {
    hours = minutes / 60;
    minutes %= 60;
  }
  if hours >= 24 {
    days = hours / 24;
    hours %= 24;
  }
  if days >= 7 {
    weeks = days / 7;
    days %= 7;
  }
  if weeks >= 4 {
    months = weeks / 4;
    weeks %= 4;
  }
  if months >= 12 {
    years = months / 12;
    weeks %= 12;
  }
  if years >= 10 {
    decades = years / 10;
    years %= 10;
  }
  if decades >= 10 {
    centuries = decades / 10;
    decades %= 10;
  }
  if centuries >= 10 {
    millennia = centuries / 10;
    centuries %= 10;
  }
  if millennia >= 1000 {
    epochs = millennia / 1000;
    millennia %= 1000;
  }
  if epochs >= 1000 {
    aeons = epochs / 1000;
    epochs %= aeons;
  }
  if aeons == ETERNITY {
    eternities = aeons / ETERNITY;
    aeons %= ETERNITY;
  }

  // Format the result
  let mut result = Vec::new();
  if eternities > 0 {
    let mut string = format!("{} eternit", eternities);
    if eternities > 1 {
      string.push_str("ies");
    } else {
      string.push('y');
    }
    result.push(string)
  }
  if aeons > 0 {
    let mut string = format!("{} aeon", aeons);
    if aeons > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if epochs > 0 {
    let mut string = format!("{} epoch", epochs);
    if epochs > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if millennia > 0 {
    let mut string = format!("{} millenni", millennia);
    if millennia > 1 {
      string.push_str("um")
    } else {
      string.push('a')
    }
    result.push(string)
  }
  if centuries > 0 {
    let mut string = format!("{} centur", centuries);
    if centuries > 1 {
      string.push_str("ies")
    } else {
      string.push('y')
    }
    result.push(string)
  }
  if decades > 0 {
    let mut string = format!("{} decade", decades);
    if decades > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if years > 0 {
    let mut string = format!("{} year", years);
    if years > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if months > 0 {
    let mut string = format!("{} month", months);
    if months > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if weeks > 0 {
    let mut string = format!("{} week", weeks);
    if weeks > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if days > 0 {
    let mut string = format!("{} day", days);
    if days > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if hours > 0 {
    let string = format!("{}h", hours);
    result.push(string);
  }
  if minutes > 0 {
    let string = format!("{}m", minutes);
    result.push(string);
  }
  if seconds > 0 {
    let string = format!("{}s", seconds);
    result.push(string);
  }
  if result.is_empty() && millis > 0 {
    let string = format!("{}ms", millis);
    result.push(string);
  }
  if result.is_empty() && micros > 0 {
    let string = format!("{}µs", micros);
    result.push(string);
  }

  result.join(" ")
}

fn tokenize_prompt(raw: &str) -> Vec<PromptTk> {
  let mut chars = raw.chars().peekable();
  let mut tk_text = String::new();
  let mut tokens = vec![];

  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        // Push any accumulated text as a token
        if !tk_text.is_empty() {
          tokens.push(PromptTk::Text(std::mem::take(&mut tk_text)));
        }

        // Handle the escape sequence
        if let Some(ch) = chars.next() {
          match ch {
            'w' => tokens.push(PromptTk::Pwd),
            'W' => tokens.push(PromptTk::PwdShort),
            'h' => tokens.push(PromptTk::Hostname),
            'H' => tokens.push(PromptTk::HostnameShort),
            's' => tokens.push(PromptTk::ShellName),
            'u' => tokens.push(PromptTk::Username),
            '$' => tokens.push(PromptTk::PromptSymbol),
            'n' => tokens.push(PromptTk::Text("\n".into())),
            'r' => tokens.push(PromptTk::Text("\r".into())),
            't' => tokens.push(PromptTk::RuntimeMillis),
            'j' => tokens.push(PromptTk::JobCount),
            'T' => tokens.push(PromptTk::RuntimeFormatted),
            '\\' => tokens.push(PromptTk::Text("\\".into())),
            '"' => tokens.push(PromptTk::Text("\"".into())),
            '\'' => tokens.push(PromptTk::Text("'".into())),
            '(' => tokens.push(PromptTk::VisGroupOpen),
            ')' => tokens.push(PromptTk::VisGroupClose),
            '@' => {
              let mut func_name = String::new();
              let is_braced = chars.peek() == Some(&'{');
              let mut handled = false;
              while let Some(ch) = chars.peek() {
                match ch {
                  '}' if is_braced => {
                    chars.next();
                    handled = true;
                    break;
                  }
                  'A'..='Z' | 'a'..='z' | '0'..='9' | '_' => {
                    func_name.push(*ch);
                    chars.next();
                  }
                  _ => {
                    handled = true;
                    if is_braced {
                      // Invalid character in braced function name
                      tokens.push(PromptTk::Text(format!("\\@{{{func_name}")));
                    } else {
                      // End of unbraced function name
                      let func_exists = read_logic(|l| l.get_func(&func_name).is_some());
                      if func_exists {
                        tokens.push(PromptTk::Function(func_name.clone()));
                      } else {
                        tokens.push(PromptTk::Text(format!("\\@{func_name}")));
                      }
                    }
                    break;
                  }
                }
              }
              // Handle end-of-input: function name collected but loop ended without pushing
              if !handled && !func_name.is_empty() {
                let func_exists = read_logic(|l| l.get_func(&func_name).is_some());
                if func_exists {
                  tokens.push(PromptTk::Function(func_name));
                } else {
                  tokens.push(PromptTk::Text(format!("\\@{func_name}")));
                }
              }
            }
            'e' => {
              if chars.next() == Some('[') {
                let mut params = String::new();

                // Collect parameters and final character
                while let Some(ch) = chars.next() {
                  match ch {
                    '0'..='9' | ';' | '?' | ':' => params.push(ch), // Valid parameter characters
                    'A'..='Z' | 'a'..='z' => {
                      // Final character (letter)
                      params.push(ch);
                      break;
                    }
                    _ => {
                      // Invalid character in ANSI sequence
                      tokens.push(PromptTk::Text(format!("\x1b[{params}")));
                      break;
                    }
                  }
                }

                tokens.push(PromptTk::AnsiSeq(format!("\x1b[{params}")));
              } else {
                // Handle case where 'e' is not followed by '['
                tokens.push(PromptTk::Text("\\e".into()));
              }
            }
            '0'..='7' => {
              // Handle octal escape
              let mut octal_str = String::new();
              octal_str.push(ch);

              // Collect up to 2 more octal digits
              for _ in 0..2 {
                if let Some(&next_ch) = chars.peek() {
                  if ('0'..='7').contains(&next_ch) {
                    octal_str.push(chars.next().unwrap());
                  } else {
                    break;
                  }
                } else {
                  break;
                }
              }

              // Parse the octal string into an integer
              if let Ok(octal) = i32::from_str_radix(&octal_str, 8) {
                tokens.push(PromptTk::AsciiOct(octal));
              } else {
                // Fallback: treat as raw text
                tokens.push(PromptTk::Text(format!("\\{octal_str}")));
              }
            }
            _ => {
              // Unknown escape sequence: treat as raw text
              tokens.push(PromptTk::Text(format!("\\{ch}")));
            }
          }
        } else {
          // Handle trailing backslash
          tokens.push(PromptTk::Text("\\".into()));
        }
      }
      _ => {
        // Accumulate non-escape characters
        tk_text.push(ch);
      }
    }
  }

  // Push any remaining text as a token
  if !tk_text.is_empty() {
    tokens.push(PromptTk::Text(tk_text));
  }

  tokens
}

pub fn expand_prompt(raw: &str) -> ShResult<String> {
  let mut tokens = tokenize_prompt(raw).into_iter();
  let mut result = String::new();

  while let Some(token) = tokens.next() {
    match token {
      PromptTk::AsciiOct(_) => todo!(),
      PromptTk::Text(txt) => result.push_str(&txt),
      PromptTk::AnsiSeq(params) => result.push_str(&params),
      PromptTk::RuntimeMillis => {
        if let Some(runtime) = write_meta(|m| m.get_time()) {
          let runtime_millis = runtime.as_millis().to_string();
          result.push_str(&runtime_millis);
        }
      }
      PromptTk::RuntimeFormatted => {
        if let Some(runtime) = write_meta(|m| m.get_time()) {
          let runtime_fmt = format_cmd_runtime(runtime);
          result.push_str(&runtime_fmt);
        }
      }
      PromptTk::Pwd => {
        let mut pwd = std::env::var("PWD").unwrap();
        let home = std::env::var("HOME").unwrap();
        if pwd.starts_with(&home) {
          pwd = pwd.replacen(&home, "~", 1);
        }
        result.push_str(&pwd);
      }
      PromptTk::PwdShort => {
        let mut path = std::env::var("PWD").unwrap();
        let home = std::env::var("HOME").unwrap();
        if path.starts_with(&home) {
          path = path.replacen(&home, "~", 1);
        }
        let pathbuf = PathBuf::from(&path);
        let mut segments = pathbuf.iter().count();
        let mut path_iter = pathbuf.iter();
        let max_segments = crate::state::read_shopts(|s| s.prompt.trunc_prompt_path);
        while segments > max_segments {
          path_iter.next();
          segments -= 1;
        }
        let path_rebuilt: PathBuf = path_iter.collect();
        let mut path_rebuilt = path_rebuilt.to_str().unwrap().to_string();
        if path_rebuilt.starts_with(&home) {
          path_rebuilt = path_rebuilt.replacen(&home, "~", 1);
        }
        result.push_str(&path_rebuilt);
      }
      PromptTk::Hostname => {
        let hostname = std::env::var("HOST").unwrap();
        result.push_str(&hostname);
      }
      PromptTk::HostnameShort => todo!(),
      PromptTk::ShellName => result.push_str("shed"),
      PromptTk::Username => {
        let username = std::env::var("USER").unwrap();
        result.push_str(&username);
      }
      PromptTk::PromptSymbol => {
        let uid = std::env::var("UID").unwrap();
        let symbol = if &uid == "0" { '#' } else { '$' };
        result.push(symbol);
      }
      PromptTk::ExitCode => todo!(),
      PromptTk::SuccessSymbol => todo!(),
      PromptTk::FailureSymbol => todo!(),
      PromptTk::JobCount => {
        let count = read_jobs(|j| {
          j.jobs()
            .iter()
            .filter(|j| {
              j.as_ref().is_some_and(|j| {
                j.get_stats()
                  .iter()
                  .all(|st| matches!(st, WtStat::StillAlive))
              })
            })
            .count()
        });
        result.push_str(&count.to_string());
      }
      PromptTk::Function(f) => {
        let output = expand_cmd_sub(&f)?;
        result.push_str(&output);
      }
      PromptTk::VisGrp => todo!(),
      PromptTk::UserSeq => todo!(),
      PromptTk::Weekday => todo!(),
      PromptTk::Dquote => todo!(),
      PromptTk::Squote => todo!(),
      PromptTk::Return => todo!(),
      PromptTk::Newline => todo!(),
      PromptTk::VisGroupOpen => todo!(),
      PromptTk::VisGroupClose => todo!(),
    }
  }

  Ok(result)
}

/// Expand aliases in the given input string
///
/// Recursively calls itself until all aliases are expanded
pub fn expand_aliases(
  input: String,
  mut already_expanded: HashSet<String>,
  log_tab: &LogTab,
) -> String {
  let mut result = input.clone();
  let tokens: Vec<_> = LexStream::new(Arc::new(input), LexFlags::empty()).collect();
  let mut expanded_this_iter: Vec<String> = vec![];

  for token_result in tokens.into_iter().rev() {
    let Ok(tk) = token_result else { continue };

    if !tk.flags.contains(TkFlags::IS_CMD) {
      continue;
    }
    if tk.flags.contains(TkFlags::KEYWORD) {
      continue;
    }

    let raw_tk = tk.span.as_str().to_string();

    if already_expanded.contains(&raw_tk) {
      continue;
    }

    if let Some(alias) = log_tab.get_alias(&raw_tk) {
      result.replace_range(tk.span.range(), &alias.to_string());
      expanded_this_iter.push(raw_tk);
    }
  }

  if expanded_this_iter.is_empty() {
    result
  } else {
    already_expanded.extend(expanded_this_iter);
    expand_aliases(result, already_expanded, log_tab)
  }
}

pub fn expand_keymap(s: &str) -> Vec<KeyEvent> {
  let mut keys = Vec::new();
  let mut chars = s.chars().collect::<VecDeque<char>>();
  while let Some(ch) = chars.pop_front() {
    match ch {
      '\\' => {
        if let Some(next_ch) = chars.pop_front() {
          keys.push(KeyEvent(KeyCode::Char(next_ch), ModKeys::NONE));
        }
      }
      '<' => {
        let mut alias = String::new();
        while let Some(a_ch) = chars.pop_front() {
          match a_ch {
            '\\' => {
              if let Some(esc_ch) = chars.pop_front() {
                alias.push(esc_ch);
              }
            }
            '>' => {
              if alias.eq_ignore_ascii_case("leader") {
                let mut leader = read_shopts(|o| o.prompt.leader.clone());
                if leader == "\\" {
                  leader.push('\\');
                }
                keys.extend(expand_keymap(&leader));
              } else if let Some(key) = parse_key_alias(&alias) {
                keys.push(key);
              }
              break;
            }
            _ => alias.push(a_ch),
          }
        }
      }
      _ => {
        keys.push(KeyEvent(KeyCode::Char(ch), ModKeys::NONE));
      }
    }
  }

  keys
}

pub fn parse_key_alias(alias: &str) -> Option<KeyEvent> {
  let parts: Vec<&str> = alias.split('-').collect();
  let (mods_parts, key_name) = parts.split_at(parts.len() - 1);
  let mut mods = ModKeys::NONE;
  for m in mods_parts {
    match m.to_uppercase().as_str() {
      "C" => mods |= ModKeys::CTRL,
      "A" | "M" => mods |= ModKeys::ALT,
      "S" => mods |= ModKeys::SHIFT,
      _ => return None,
    }
  }

  let key = match key_name.first()?.to_uppercase().as_str() {
    "CR" => KeyCode::Char('\r'),
    "ENTER" | "RETURN" => KeyCode::Enter,
    "ESC" | "ESCAPE" => KeyCode::Esc,
    "TAB" => KeyCode::Tab,
    "BS" | "BACKSPACE" => KeyCode::Backspace,
    "DEL" | "DELETE" => KeyCode::Delete,
    "INS" | "INSERT" => KeyCode::Insert,
    "SPACE" => KeyCode::Char(' '),
    "UP" => KeyCode::Up,
    "DOWN" => KeyCode::Down,
    "LEFT" => KeyCode::Left,
    "RIGHT" => KeyCode::Right,
    "HOME" => KeyCode::Home,
    "END" => KeyCode::End,
    "CMD" => KeyCode::ExMode,
    "PGUP" | "PAGEUP" => KeyCode::PageUp,
    "PGDN" | "PAGEDOWN" => KeyCode::PageDown,
    k if k.len() == 1 => KeyCode::Char(k.chars().next().unwrap()),
    _ => return None,
  };

  Some(KeyEvent(key, mods))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parse::lex::Span;
  use crate::readline::keys::{KeyCode, KeyEvent, ModKeys};
  use crate::state::{ArrIndex, VarFlags, VarKind, read_vars, write_vars};
  use crate::testutil::{TestGuard, test_input};
  use std::time::Duration;

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

  // ===================== Arithmetic =====================

  #[test]
  fn arith_addition() {
    assert_eq!(expand_arithmetic("(1+2)").unwrap().unwrap(), "3");
  }

  #[test]
  fn arith_subtraction() {
    assert_eq!(expand_arithmetic("(10-3)").unwrap().unwrap(), "7");
  }

  #[test]
  fn arith_multiplication() {
    assert_eq!(expand_arithmetic("(3*4)").unwrap().unwrap(), "12");
  }

  #[test]
  fn arith_division() {
    assert_eq!(expand_arithmetic("(10/2)").unwrap().unwrap(), "5");
  }

  #[test]
  fn arith_modulo() {
    assert_eq!(expand_arithmetic("(10%3)").unwrap().unwrap(), "1");
  }

  #[test]
  fn arith_precedence() {
    assert_eq!(expand_arithmetic("(2+3*4)").unwrap().unwrap(), "14");
  }

  #[test]
  fn arith_parens() {
    assert_eq!(expand_arithmetic("((2+3)*4)").unwrap().unwrap(), "20");
  }

  #[test]
  fn arith_nested_parens() {
    assert_eq!(expand_arithmetic("((1+2)*(3+4))").unwrap().unwrap(), "21");
  }

  #[test]
  fn arith_spaces() {
    assert_eq!(expand_arithmetic("( 1 + 2 )").unwrap().unwrap(), "3");
  }

  // ===================== glob_to_regex =====================

  #[test]
  fn glob_star_matches_anything() {
    let re = glob_to_regex("*", false);
    assert!(re.is_match("anything"));
    assert!(re.is_match(""));
  }

  #[test]
  fn glob_question_matches_single() {
    let re = glob_to_regex("?", true);
    assert!(re.is_match("a"));
    assert!(!re.is_match("ab"));
    assert!(!re.is_match(""));
  }

  #[test]
  fn glob_star_dot_ext() {
    let re = glob_to_regex("*.txt", true);
    assert!(re.is_match("hello.txt"));
    assert!(re.is_match(".txt"));
    assert!(!re.is_match("hello.rs"));
  }

  #[test]
  fn glob_char_class() {
    let re = glob_to_regex("[abc]", true);
    assert!(re.is_match("a"));
    assert!(re.is_match("b"));
    assert!(!re.is_match("d"));
  }

  #[test]
  fn glob_dot_escaped() {
    let re = glob_to_regex("foo.bar", true);
    assert!(re.is_match("foo.bar"));
    assert!(!re.is_match("fooXbar"));
  }

  #[test]
  fn glob_special_chars_escaped() {
    let re = glob_to_regex("a+b(c)", true);
    assert!(re.is_match("a+b(c)"));
    assert!(!re.is_match("ab"));
  }

  #[test]
  fn glob_anchored_vs_unanchored() {
    let anchored = glob_to_regex("hello", true);
    assert!(anchored.is_match("hello"));
    assert!(!anchored.is_match("say hello"));

    let unanchored = glob_to_regex("hello", false);
    assert!(unanchored.is_match("hello"));
    assert!(unanchored.is_match("say hello world"));
  }

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
    assert!(matches!(exp, ParamExp::Substr(2)));
  }

  #[test]
  fn param_exp_substr_len() {
    let exp: ParamExp = ":1:3".parse().unwrap();
    assert!(matches!(exp, ParamExp::SubstrLen(1, 3)));
  }

  // ===================== unescape_str =====================

  #[test]
  fn unescape_backslash() {
    let result = unescape_str("hello\\nworld");
    let expected = format!("hello{}nworld", markers::ESCAPE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_tilde_at_start() {
    let result = unescape_str("~/foo");
    assert!(result.starts_with(markers::TILDE_SUB));
    assert!(result.ends_with("/foo"));
  }

  #[test]
  fn unescape_tilde_not_at_start() {
    let result = unescape_str("a~b");
    assert!(!result.contains(markers::TILDE_SUB));
    assert!(result.contains('~'));
  }

  #[test]
  fn unescape_dollar_becomes_var_sub() {
    let result = unescape_str("$foo");
    assert!(result.starts_with(markers::VAR_SUB));
    assert!(result.ends_with("foo"));
  }

  #[test]
  fn unescape_single_quotes() {
    let result = unescape_str("'hello'");
    let expected = format!("{}hello{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_double_quotes() {
    let result = unescape_str("\"hello\"");
    let expected = format!("{}hello{}", markers::DUB_QUOTE, markers::DUB_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_newline() {
    let result = unescape_str("$'\\n'");
    let expected = format!("{}\n{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_tab() {
    let result = unescape_str("$'\\t'");
    let expected = format!("{}\t{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_escape() {
    let result = unescape_str("$'\\e'");
    let expected = format!("{}\x1b{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_hex() {
    let result = unescape_str("$'\\x41'");
    let expected = format!("{}A{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  #[test]
  fn unescape_dollar_single_quote_backslash() {
    let result = unescape_str("$'\\\\'");
    let expected = format!("{}\\{}", markers::SNG_QUOTE, markers::SNG_QUOTE);
    assert_eq!(result, expected);
  }

  // ===================== tokenize_prompt =====================

  #[test]
  fn prompt_username() {
    let tokens = tokenize_prompt("\\u");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Username));
  }

  #[test]
  fn prompt_hostname() {
    let tokens = tokenize_prompt("\\h");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Hostname));
  }

  #[test]
  fn prompt_pwd() {
    let tokens = tokenize_prompt("\\w");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Pwd));
  }

  #[test]
  fn prompt_pwd_short() {
    let tokens = tokenize_prompt("\\W");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::PwdShort));
  }

  #[test]
  fn prompt_symbol() {
    let tokens = tokenize_prompt("\\$");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::PromptSymbol));
  }

  #[test]
  fn prompt_newline() {
    let tokens = tokenize_prompt("\\n");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\n"));
  }

  #[test]
  fn prompt_shell_name() {
    let tokens = tokenize_prompt("\\s");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::ShellName));
  }

  #[test]
  fn prompt_literal_backslash() {
    let tokens = tokenize_prompt("\\\\");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\\"));
  }

  #[test]
  fn prompt_mixed() {
    let tokens = tokenize_prompt("\\u@\\h \\w\\$ ");
    // \u, Text("@"), \h, Text(" "), \w, \$, Text(" ")
    assert_eq!(tokens.len(), 7);
    assert!(matches!(tokens[0], PromptTk::Username));
    assert!(matches!(tokens[1], PromptTk::Text(ref t) if t == "@"));
    assert!(matches!(tokens[2], PromptTk::Hostname));
    assert!(matches!(tokens[3], PromptTk::Text(ref t) if t == " "));
    assert!(matches!(tokens[4], PromptTk::Pwd));
    assert!(matches!(tokens[5], PromptTk::PromptSymbol));
    assert!(matches!(tokens[6], PromptTk::Text(ref t) if t == " "));
  }

  #[test]
  fn prompt_ansi_sequence() {
    let tokens = tokenize_prompt("\\e[31m");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::AnsiSeq(ref s) if s == "\x1b[31m"));
  }

  #[test]
  fn prompt_octal() {
    let tokens = tokenize_prompt("\\141"); // 'a' in octal
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::AsciiOct(97)));
  }

  // ===================== format_cmd_runtime =====================

  #[test]
  fn runtime_millis() {
    let dur = Duration::from_millis(500);
    assert_eq!(format_cmd_runtime(dur), "500ms");
  }

  #[test]
  fn runtime_seconds() {
    let dur = Duration::from_secs(5);
    assert_eq!(format_cmd_runtime(dur), "5s");
  }

  #[test]
  fn runtime_minutes_and_seconds() {
    let dur = Duration::from_secs(125);
    assert_eq!(format_cmd_runtime(dur), "2m 5s");
  }

  #[test]
  fn runtime_hours() {
    let dur = Duration::from_secs(3661);
    assert_eq!(format_cmd_runtime(dur), "1h 1m 1s");
  }

  #[test]
  fn runtime_micros() {
    let dur = Duration::from_micros(500);
    assert_eq!(format_cmd_runtime(dur), "500µs");
  }

  // ===================== parse_key_alias =====================

  #[test]
  fn key_alias_cr() {
    let key = parse_key_alias("CR").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Char('\r'), ModKeys::NONE));
  }

  #[test]
  fn key_alias_enter() {
    let key = parse_key_alias("ENTER").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Enter, ModKeys::NONE));
  }

  #[test]
  fn key_alias_esc() {
    let key = parse_key_alias("ESC").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Esc, ModKeys::NONE));
  }

  #[test]
  fn key_alias_tab() {
    let key = parse_key_alias("TAB").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Tab, ModKeys::NONE));
  }

  #[test]
  fn key_alias_backspace() {
    let key = parse_key_alias("BS").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Backspace, ModKeys::NONE));
  }

  #[test]
  fn key_alias_space() {
    let key = parse_key_alias("SPACE").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Char(' '), ModKeys::NONE));
  }

  #[test]
  fn key_alias_arrows() {
    assert_eq!(
      parse_key_alias("UP").unwrap(),
      KeyEvent(KeyCode::Up, ModKeys::NONE)
    );
    assert_eq!(
      parse_key_alias("DOWN").unwrap(),
      KeyEvent(KeyCode::Down, ModKeys::NONE)
    );
    assert_eq!(
      parse_key_alias("LEFT").unwrap(),
      KeyEvent(KeyCode::Left, ModKeys::NONE)
    );
    assert_eq!(
      parse_key_alias("RIGHT").unwrap(),
      KeyEvent(KeyCode::Right, ModKeys::NONE)
    );
  }

  #[test]
  fn key_alias_ctrl_modifier() {
    let key = parse_key_alias("C-a").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Char('A'), ModKeys::CTRL));
  }

  #[test]
  fn key_alias_ctrl_shift_alt_modifier() {
    let key = parse_key_alias("C-S-A-b").unwrap();
    assert_eq!(
      key,
      KeyEvent(
        KeyCode::Char('B'),
        ModKeys::CTRL | ModKeys::SHIFT | ModKeys::ALT
      )
    );
  }

  #[test]
  fn key_alias_alt_modifier() {
    let key = parse_key_alias("M-x").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Char('X'), ModKeys::ALT));
  }

  #[test]
  fn key_alias_shift_modifier() {
    let key = parse_key_alias("S-TAB").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Tab, ModKeys::SHIFT));
  }

  #[test]
  fn key_alias_invalid() {
    assert!(parse_key_alias("INVALID_KEY").is_none());
  }

  // ===================== expand_keymap =====================

  #[test]
  fn keymap_single_char() {
    let keys = expand_keymap("a");
    assert_eq!(keys, vec![KeyEvent(KeyCode::Char('a'), ModKeys::NONE)]);
  }

  #[test]
  fn keymap_sequence() {
    let keys = expand_keymap("abc");
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0], KeyEvent(KeyCode::Char('a'), ModKeys::NONE));
    assert_eq!(keys[1], KeyEvent(KeyCode::Char('b'), ModKeys::NONE));
    assert_eq!(keys[2], KeyEvent(KeyCode::Char('c'), ModKeys::NONE));
  }

  #[test]
  fn keymap_ctrl_key() {
    let keys = expand_keymap("<C-a>");
    assert_eq!(keys, vec![KeyEvent(KeyCode::Char('A'), ModKeys::CTRL)]);
  }

  #[test]
  fn keymap_escaped_char() {
    let keys = expand_keymap("\\<");
    assert_eq!(keys, vec![KeyEvent(KeyCode::Char('<'), ModKeys::NONE)]);
  }

  #[test]
  fn keymap_mixed() {
    let keys = expand_keymap("a<CR>b");
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0], KeyEvent(KeyCode::Char('a'), ModKeys::NONE));
    assert_eq!(keys[1], KeyEvent(KeyCode::Char('\r'), ModKeys::NONE));
    assert_eq!(keys[2], KeyEvent(KeyCode::Char('b'), ModKeys::NONE));
  }

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

    // ${EMPTY-fallback} — EMPTY is set (even if null), so returns null
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

  // ===================== Command Substitution (TestGuard) =====================

  #[test]
  fn cmd_sub_echo() {
    let _guard = TestGuard::new();
    let result = expand_cmd_sub("echo hello").unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn cmd_sub_trailing_newlines_stripped() {
    let _guard = TestGuard::new();
    let result = expand_cmd_sub("printf 'hello\\n\\n'").unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn cmd_sub_arithmetic() {
    let result = expand_cmd_sub("(1+2)").unwrap();
    assert_eq!(result, "3");
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

  // ===================== Word Splitting (TestGuard) =====================

  #[test]
  fn word_split_default_ifs() {
    let _guard = TestGuard::new();

    let mut exp = Expander {
      raw: "hello world\tfoo".to_string(),
			flags: TkFlags::empty()
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
			flags: TkFlags::empty()
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
			flags: TkFlags::empty()
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
			flags: TkFlags::empty()
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
			flags: TkFlags::empty()
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
			flags: TkFlags::empty()
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
			flags: TkFlags::empty()
		};
    let words = exp.split_words();
    assert_eq!(words, vec!["a:b", "c"]);
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

  // ===================== Arithmetic with Variables (TestGuard) =====================

  #[test]
  fn arith_with_variable() {
    let _guard = TestGuard::new();
    write_vars(|v| v.set_var("x", VarKind::Str("5".into()), VarFlags::NONE)).unwrap();

    // expand_arithmetic processes the body after stripping outer parens
    // unescape_math converts $x into marker+x
    let body = "$x+3";
    let unescaped = unescape_math(body);
    let expanded = expand_raw(&mut unescaped.chars().peekable()).unwrap();
    let tokens = ArithTk::tokenize(&expanded).unwrap().unwrap();
    let rpn = ArithTk::to_rpn(tokens).unwrap();
    let result = ArithTk::eval_rpn(rpn).unwrap();
    assert_eq!(result, 8.0);
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

  // ===================== Alias Expansion (TestGuard) =====================

  #[test]
  fn alias_simple() {
    let _guard = TestGuard::new();
    let dummy_span = Span::default();
    crate::state::SHED.with(|s| {
      s.logic
        .borrow_mut()
        .insert_alias("ll", "ls -la", dummy_span.clone());
    });

    let log_tab = crate::state::SHED.with(|s| s.logic.borrow().clone());
    let result = expand_aliases("ll".to_string(), HashSet::new(), &log_tab);
    assert_eq!(result, "ls -la");
  }

  #[test]
  fn alias_circular_prevention() {
    let _guard = TestGuard::new();
    let dummy_span = Span::default();
    crate::state::SHED.with(|s| {
      s.logic
        .borrow_mut()
        .insert_alias("foo", "foo --verbose", dummy_span.clone());
    });

    let log_tab = crate::state::SHED.with(|s| s.logic.borrow().clone());
    let result = expand_aliases("foo".to_string(), HashSet::new(), &log_tab);
    // After first expansion: "foo --verbose", then "foo" is in already_expanded
    // so it won't expand again
    assert_eq!(result, "foo --verbose");
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

    test_input("echo $echo ${var:-${arr[$(($(echo ${i[0]}) + 1))]}}").unwrap();

    let out = guard.read_output();
    assert_eq!(out, "bar\n");
  }
}
