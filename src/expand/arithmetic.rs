use std::{cell::Cell, str::FromStr};

use crate::{state::vars::VarStr, util};

use super::{
  ShErr, ShResult,
  escape::unescape_math,
  match_loop, sherr,
  state::{
    Shed,
    vars::{VarFlags, VarKind},
  },
  try_var,
  var::expand_raw,
};

#[derive(Debug, Clone)]
enum ArithOp {
  // math
  Add,
  Sub,
  Mul,
  Div,
  Mod,
  Pow,
  // comparison
  Lt,
  Gt,
  Le,
  Ge,
  Eq,
  Ne,
  // logical
  And,
  Or,
  // bitwise
  BitAnd,
  BitOr,
  BitXor,
  ShiftL,
  ShiftR,
  // assign
  Assign,
  PlusAssign,
  MinusAssign,
  MulAssign,
  DivAssign,
  ModAssign,
  BitAndAssign,
  BitOrAssign,
  BitXorAssign,
  ShiftLAssign,
  ShiftRAssign,
}

impl FromStr for ArithOp {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "+" => Ok(Self::Add),
      "-" => Ok(Self::Sub),
      "*" => Ok(Self::Mul),
      "**" => Ok(Self::Pow),
      "/" => Ok(Self::Div),
      "%" => Ok(Self::Mod),
      "<" => Ok(Self::Lt),
      ">" => Ok(Self::Gt),
      "<=" => Ok(Self::Le),
      ">=" => Ok(Self::Ge),
      "==" => Ok(Self::Eq),
      "!=" => Ok(Self::Ne),
      "&&" => Ok(Self::And),
      "||" => Ok(Self::Or),
      "&" => Ok(Self::BitAnd),
      "|" => Ok(Self::BitOr),
      "^" => Ok(Self::BitXor),
      "<<" => Ok(Self::ShiftL),
      ">>" => Ok(Self::ShiftR),
      "=" => Ok(Self::Assign),
      "+=" => Ok(Self::PlusAssign),
      "-=" => Ok(Self::MinusAssign),
      "*=" => Ok(Self::MulAssign),
      "/=" => Ok(Self::DivAssign),
      "%=" => Ok(Self::ModAssign),
      "&=" => Ok(Self::BitAndAssign),
      "|=" => Ok(Self::BitOrAssign),
      "^=" => Ok(Self::BitXorAssign),
      "<<=" => Ok(Self::ShiftLAssign),
      ">>=" => Ok(Self::ShiftRAssign),
      _ => Err(sherr!(ParseErr, "Unknown operator: '{s}'")),
    }
  }
}

#[derive(Debug, Clone)]
enum ArithTk {
  Num(i64),
  Op(ArithOp),
  Comma,
  LParen,
  RParen,
  Inc,      // ++ (raw, resolved to prefix/postfix during to_rpn)
  Dec,      // -- (raw, resolved to prefix/postfix during to_rpn)
  Not,      // !
  Neg,      // unary -
  UPlus,    // unary +
  BitNot,   // ~
  Question, // ? (lex token)
  Colon,    // : (lex token)
  Var(String),

  // RPN-only opcodes for control flow (jump-based short-circuit + ternary)
  JumpIfZero(usize),        // pop, if 0 jump to offset
  JumpIfZeroPeek(usize),    // peek, if 0 jump (used by &&)
  JumpIfNonZeroPeek(usize), // peek, if non-zero jump (used by ||)
  Jump(usize),              // unconditional jump
  Pop,                      // discard top of stack
  Nez,                      // replace top with 0 if it's 0, else 1

  // Ops-stack-only pending markers (carry indices to patch when flushed)
  PendingAnd(usize),         // jump_idx of the placeholder JIFZ_PEEK
  PendingOr(usize),          // jump_idx of the placeholder JIFNZ_PEEK
  PendingTernaryThen(usize), // jump_idx of the placeholder JIFZ (cond→else)
  PendingTernaryElse(usize), // jump_idx of the placeholder JMP (then→end)
}

// Stack value used during eval_rpn, keeps Var names alive for assignment targets
enum StackVal {
  Num(i64),
  Var(String),
}

impl StackVal {
  fn to_num(&self) -> ShResult<i64> {
    match self {
      StackVal::Num(n) => Ok(*n),
      StackVal::Var(name) => resolve_var_num(name),
    }
  }
}

/// Digit value for a `base#digits` radix literal (bash rules): 0-9 → 0-9;
/// for base <= 36 letters are case-insensitive 10-35; for base > 36, `a-z` →
/// 10-35, `A-Z` → 36-61, `@` → 62, `_` → 63. Returns `None` if out of range.
fn radix_digit_value(c: char, base: u32) -> Option<u32> {
  let v = match c {
    '0'..='9' => c as u32 - '0' as u32,
    'a'..='z' => c as u32 - 'a' as u32 + 10,
    'A'..='Z' if base <= 36 => c as u32 - 'A' as u32 + 10,
    'A'..='Z' => c as u32 - 'A' as u32 + 36,
    '@' => 62,
    '_' => 63,
    _ => return None,
  };
  (v < base).then_some(v)
}

/// Depth limit for recursive arithmetic variable resolution.
/// `eval_rpn` has a huge stack frame so this can't be very large at the moment.
const MAX_ARITH_DEPTH: u32 = 16; // TODO: slim down eval_rpn's stack frame size

thread_local! {
  static ARITH_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Restores the recursion depth counter on drop, even on the error path.
struct DepthGuard;
impl Drop for DepthGuard {
  fn drop(&mut self) {
    ARITH_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
  }
}

/// Resolve a variable to an integer for arithmetic. If its value is not already
/// a literal integer, evaluate it *as an arithmetic expression* (POSIX 2.6.4 /
/// bash), so a variable holding another name (`y=x`) or a sub-expression
/// (`b="a+1"`) resolves transitively. Unset/empty resolves to 0; cyclic
/// references are cut off by [`MAX_ARITH_DEPTH`].
fn resolve_var_num(name: &str) -> ShResult<i64> {
  let val = try_var!(name).unwrap_or_default();
  let val = val.to_str_lossy();
  let trimmed = val.trim();
  if trimmed.is_empty() {
    return Ok(0);
  }
  if let Ok(n) = trimmed.parse::<i64>() {
    return Ok(n);
  }
  if ARITH_DEPTH.with(Cell::get) >= MAX_ARITH_DEPTH {
    return Err(sherr!(
      ParseErr,
      "arithmetic recursion limit exceeded resolving '{name}'"
    ));
  }
  ARITH_DEPTH.with(|d| d.set(d.get() + 1));
  let _guard = DepthGuard;
  let result = expand_arithmetic(trimmed)?;
  result
    .to_str_lossy()
    .parse::<i64>()
    .map_err(|_| sherr!(ParseErr, "Variable '{name}' does not contain an integer"))
}

fn read_var_as_i64(name: &str) -> ShResult<i64> {
  resolve_var_num(name)
}

impl ArithTk {
  #[expect(clippy::too_many_lines)]
  pub fn tokenize(raw: &str) -> ShResult<Vec<Self>> {
    let mut tokens = Vec::new();
    let mut chars = raw.chars().peekable();
    // Track whether the last emitted token was an operand, to distinguish
    // unary minus from binary subtraction.
    let mut last_was_operand = false;

    match_loop!(chars.peek() => &ch => ch, {
      ' ' | '\t' => { chars.next(); }

      '0'..='9' => {
        let mut num = util::scratch_buf();
        let first = chars.next().unwrap();
        num.push(first);

        // Hex (0x... / 0X...) or octal (0NNN); otherwise decimal.
        let parsed: i64 = if first == '0' && matches!(chars.peek(), Some('x' | 'X')) {
          chars.next(); // consume x/X
          let mut hex = util::scratch_buf();
          while let Some(&d) = chars.peek() {
            if d.is_ascii_hexdigit() {
              hex.push(d);
              chars.next();
            } else {
              break;
            }
          }
          if hex.is_empty() {
            return Err(sherr!(ParseErr, "Invalid hex literal '0{}'", first));
          }
          i64::from_str_radix(&hex, 16).map_err(|_| sherr!(
            ParseErr, "Invalid hex literal: '0x{}'", hex,
          ))?
        } else if first == '0' && chars.peek().is_some_and(char::is_ascii_digit) {
          // Octal, collect remaining octal digits.
          let mut oct = util::scratch_buf();
          while let Some(&d) = chars.peek() {
            if matches!(d, '0'..='7') {
              oct.push(d);
              chars.next();
            } else if d.is_ascii_digit() {
              return Err(sherr!(ParseErr, "Invalid digit '{}' in octal literal", d));
            } else {
              break;
            }
          }
          i64::from_str_radix(&oct, 8).map_err(|_| sherr!(
            ParseErr, "Invalid octal literal: '0{}'", oct,
          ))?
        } else {
          while let Some(&d) = chars.peek() {
            if d.is_ascii_digit() {
              num.push(d);
              chars.next();
            } else {
              break;
            }
          }
          // `base#digits` radix literal (bash), base 2..=64.
          if chars.peek() == Some(&'#') {
            chars.next(); // consume '#'
            let base: u32 = num
              .parse()
              .ok()
              .filter(|b| (2..=64).contains(b))
              .ok_or_else(|| sherr!(ParseErr, "Invalid arithmetic base '{num}' (must be 2..64)"))?;
            let mut digits = util::scratch_buf();
            while let Some(&d) = chars.peek() {
              if d.is_ascii_alphanumeric() || d == '@' || d == '_' {
                digits.push(d);
                chars.next();
              } else {
                break;
              }
            }
            if digits.is_empty() {
              return Err(sherr!(ParseErr, "Missing digits after base '{base}#'"));
            }
            let mut result: i64 = 0;
            for c in digits.chars() {
              let d = radix_digit_value(c, base).ok_or_else(|| {
                sherr!(ParseErr, "Invalid digit '{c}' for base {base}")
              })?;
              result = result * i64::from(base) + i64::from(d);
            }
            result
          } else {
            num.parse::<i64>().map_err(|_| sherr!(
              ParseErr, "Invalid number in arithmetic expression: '{}'", num,
            ))?
          }
        };

        tokens.push(Self::Num(parsed));
        last_was_operand = true;
      }

      '-' => {
        chars.next();
        if chars.peek() == Some(&'-') {
          chars.next();
          tokens.push(Self::Dec);
          // postfix Dec: last_was_operand stays true if it was; prefix Dec: next is a var
        } else if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::MinusAssign));
          last_was_operand = false;
        } else if last_was_operand {
          tokens.push(Self::Op(ArithOp::Sub));
          last_was_operand = false;
        } else {
          tokens.push(Self::Neg);
          // last_was_operand stays false, Neg is unary prefix
        }
      }

      '+' => {
        chars.next();
        if chars.peek() == Some(&'+') {
          chars.next();
          tokens.push(Self::Inc);
        } else if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::PlusAssign));
          last_was_operand = false;
        } else if last_was_operand {
          tokens.push(Self::Op(ArithOp::Add));
          last_was_operand = false;
        } else {
          tokens.push(Self::UPlus);
          // last_was_operand stays false, UPlus is unary prefix
        }
      }

      '*' => {
        chars.next();
        if chars.peek() == Some(&'*') {
          chars.next();
          tokens.push(Self::Op(ArithOp::Pow));
        } else if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::MulAssign));
        } else {
          tokens.push(Self::Op(ArithOp::Mul));
        }
        last_was_operand = false;
      }

      '/' => {
        chars.next();
        if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::DivAssign));
        } else {
          tokens.push(Self::Op(ArithOp::Div));
        }
        last_was_operand = false;
      }

      '%' => {
        chars.next();
        if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::ModAssign));
        } else {
          tokens.push(Self::Op(ArithOp::Mod));
        }
        last_was_operand = false;
      }

      '<' => {
        chars.next();
        if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::Le));
        } else if chars.peek() == Some(&'<') {
          chars.next();
          if chars.peek() == Some(&'=') {
            chars.next();
            tokens.push(Self::Op(ArithOp::ShiftLAssign));
          } else {
            tokens.push(Self::Op(ArithOp::ShiftL));
          }
        } else {
          tokens.push(Self::Op(ArithOp::Lt));
        }
        last_was_operand = false;
      }

      '>' => {
        chars.next();
        if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::Ge));
        } else if chars.peek() == Some(&'>') {
          chars.next();
          if chars.peek() == Some(&'=') {
            chars.next();
            tokens.push(Self::Op(ArithOp::ShiftRAssign));
          } else {
            tokens.push(Self::Op(ArithOp::ShiftR));
          }
        } else {
          tokens.push(Self::Op(ArithOp::Gt));
        }
        last_was_operand = false;
      }

      '=' => {
        chars.next();
        if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::Eq));
        } else {
          tokens.push(Self::Op(ArithOp::Assign));
        }
        last_was_operand = false;
      }

      '!' => {
        chars.next();
        if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::Ne));
          last_was_operand = false;
        } else {
          tokens.push(Self::Not);
          last_was_operand = false;
        }
      }

      '&' => {
        chars.next();
        if chars.peek() == Some(&'&') {
          chars.next();
          tokens.push(Self::Op(ArithOp::And));
        } else if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::BitAndAssign));
        } else {
          tokens.push(Self::Op(ArithOp::BitAnd));
        }
        last_was_operand = false;
      }

      '|' => {
        chars.next();
        if chars.peek() == Some(&'|') {
          chars.next();
          tokens.push(Self::Op(ArithOp::Or));
        } else if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::BitOrAssign));
        } else {
          tokens.push(Self::Op(ArithOp::BitOr));
        }
        last_was_operand = false;
      }

      '^' => {
        chars.next();
        if chars.peek() == Some(&'=') {
          chars.next();
          tokens.push(Self::Op(ArithOp::BitXorAssign));
        } else {
          tokens.push(Self::Op(ArithOp::BitXor));
        }
        last_was_operand = false;
      }

      '~' => {
        chars.next();
        tokens.push(Self::BitNot);
        // last_was_operand stays false, BitNot is unary prefix
      }

      ',' => {
        tokens.push(Self::Comma);
        chars.next();
        last_was_operand = false;
      }

      '?' => {
        tokens.push(Self::Question);
        chars.next();
        last_was_operand = false;
      }

      ':' => {
        tokens.push(Self::Colon);
        chars.next();
        last_was_operand = false;
      }

      '(' => {
        tokens.push(Self::LParen);
        chars.next();
        last_was_operand = false;
      }

      ')' => {
        tokens.push(Self::RParen);
        chars.next();
        last_was_operand = true;
      }

      _ if ch.is_alphabetic() || ch == '_' => {
        chars.next();
        let mut var_name = ch.to_string();
        while let Some(ch) = chars.peek() {
          match ch {
            _ if ch.is_alphabetic() || *ch == '_' || ch.is_ascii_digit() => {
              var_name.push(*ch);
              chars.next();
            }
            _ => break,
          }
        }
        tokens.push(Self::Var(var_name));
        last_was_operand = true;
      }

      _ => {
        return Err(sherr!(
          ParseErr,
          "Unexpected character in arithmetic expression: '{ch}'",
        ));
      }
    });

    Ok(tokens)
  }
  fn precedence(tk: &ArithTk) -> usize {
    match tk {
      // Pending markers participate in flushing at their op's precedence.
      ArithTk::PendingTernaryElse(_) => 1,
      ArithTk::PendingOr(_) => 2,
      ArithTk::PendingAnd(_) => 3,
      ArithTk::Op(op) => match op {
        ArithOp::Assign
        | ArithOp::PlusAssign
        | ArithOp::MinusAssign
        | ArithOp::MulAssign
        | ArithOp::DivAssign
        | ArithOp::ModAssign
        | ArithOp::BitAndAssign
        | ArithOp::BitOrAssign
        | ArithOp::BitXorAssign
        | ArithOp::ShiftLAssign
        | ArithOp::ShiftRAssign => 1,
        ArithOp::Or => 2,
        ArithOp::And => 3,
        ArithOp::BitOr => 4,
        ArithOp::BitXor => 5,
        ArithOp::BitAnd => 6,
        ArithOp::Eq | ArithOp::Ne => 7,
        ArithOp::Lt | ArithOp::Gt | ArithOp::Le | ArithOp::Ge => 8,
        ArithOp::ShiftL | ArithOp::ShiftR => 9,
        ArithOp::Add | ArithOp::Sub => 10,
        ArithOp::Mul | ArithOp::Div | ArithOp::Mod => 11,
        // `**` binds tighter than `* / %` but looser than the unary operators
        // (bash: `-2**2` == `(-2)**2` == 4).
        ArithOp::Pow => 12,
      },
      ArithTk::Not | ArithTk::Neg | ArithTk::UPlus | ArithTk::BitNot => 13,
      _ => 0,
    }
  }

  fn is_right_assoc(tk: &ArithTk) -> bool {
    matches!(
      tk,
      ArithTk::Not
        | ArithTk::Neg
        | ArithTk::UPlus
        | ArithTk::BitNot
        | ArithTk::PendingTernaryElse(_)
        | ArithTk::Op(
          ArithOp::Pow
            | ArithOp::Assign
            | ArithOp::PlusAssign
            | ArithOp::MinusAssign
            | ArithOp::MulAssign
            | ArithOp::DivAssign
            | ArithOp::ModAssign
            | ArithOp::BitAndAssign
            | ArithOp::BitOrAssign
            | ArithOp::BitXorAssign
            | ArithOp::ShiftLAssign
            | ArithOp::ShiftRAssign
        )
    )
  }

  // Pop one op off the ops stack, translating control-flow markers into
  // their final RPN form (and patching the corresponding jump placeholders).
  fn pop_to_output(ops: &mut Vec<ArithTk>, output: &mut Vec<ArithTk>) -> ShResult<()> {
    match ops.pop().unwrap() {
      ArithTk::PendingAnd(jump_idx) => {
        // After RHS of && is built. Patch JIFZ_PEEK to land at the Nez we
        // emit now, so the short-circuit jump produces 0 directly.
        let target = output.len();
        output.push(ArithTk::Nez);
        output[jump_idx] = ArithTk::JumpIfZeroPeek(target);
      }
      ArithTk::PendingOr(jump_idx) => {
        let target = output.len();
        output.push(ArithTk::Nez);
        output[jump_idx] = ArithTk::JumpIfNonZeroPeek(target);
      }
      ArithTk::PendingTernaryElse(jump_idx) => {
        // After else-arm. Patch the JMP that skips past the else.
        let target = output.len();
        output[jump_idx] = ArithTk::Jump(target);
      }
      ArithTk::PendingTernaryThen(_) | ArithTk::Question => {
        return Err(sherr!(
          ParseErr,
          "'?' without matching ':' in arithmetic expression"
        ));
      }
      other => output.push(other),
    }
    Ok(())
  }

  // True if `top` is a barrier that flushing must stop at.
  fn is_barrier(top: &ArithTk) -> bool {
    matches!(top, ArithTk::LParen | ArithTk::Question)
  }

  fn flush_ops(
    ops: &mut Vec<ArithTk>,
    output: &mut Vec<ArithTk>,
    until_paren: bool,
  ) -> ShResult<()> {
    while let Some(top) = ops.last() {
      if matches!(top, ArithTk::LParen) {
        break;
      }
      Self::pop_to_output(ops, output)?;
    }
    if until_paren {
      ops.pop(); // remove the LParen
    }
    Ok(())
  }

  #[expect(clippy::too_many_lines)]
  fn to_rpn(tokens: Vec<ArithTk>) -> ShResult<Vec<ArithTk>> {
    let mut output: Vec<ArithTk> = Vec::new();
    let mut ops: Vec<ArithTk> = Vec::new();
    let mut tokens = tokens.into_iter().peekable();

    match_loop!(tokens.next() => token, {
      ArithTk::Num(_) => output.push(token),

      ArithTk::Var(ref var) => {
        // Check for postfix inc/dec
        if tokens.peek().is_some_and(|tk| matches!(tk, ArithTk::Inc | ArithTk::Dec)) {
          let op = tokens.next().unwrap();
          let val = read_var_as_i64(var)?;
          let delta: i64 = if matches!(op, ArithTk::Inc) { 1 } else { -1 };
          Shed::vars_mut(|v| v.set_var(var, VarKind::string((val + delta).to_string().into()), VarFlags::empty())).unwrap();
          output.push(ArithTk::Num(val)); // push old value (postfix)
        } else {
          output.push(token); // keep as Var, may be assignment target
        }
      }

      op @ (ArithTk::Inc | ArithTk::Dec) => {
        let Some(ArithTk::Var(_)) = tokens.peek() else {
          return Err(sherr!(
            ParseErr,
            "Expected variable name after '{}' operator",
            if matches!(op, ArithTk::Inc) { "++" } else { "--" },
          ));
        };
        let Some(ArithTk::Var(var)) = tokens.next() else { unreachable!() };
        let val = read_var_as_i64(&var)?;
        let delta: i64 = if matches!(op, ArithTk::Inc) { 1 } else { -1 };
        let new_val = val + delta;
        Shed::vars_mut(|v| v.set_var(&var, VarKind::string(new_val.to_string().into()), VarFlags::empty())).unwrap();
        output.push(ArithTk::Num(new_val)); // push new value (prefix)
      }

      ArithTk::Not | ArithTk::Neg | ArithTk::UPlus | ArithTk::BitNot => {
        // Unary right-associative
        // push to ops stack
        ops.push(token);
      }

      ArithTk::Comma => {
        // Lowest-precedence binary op
        // push to ops stack so both operands
        // are fully evaluated before Comma is applied. Question is a barrier
        // (commas inside the then-arm of a ternary stay there).
        while let Some(top) = ops.last() {
          if Self::is_barrier(top) { break; }
          Self::pop_to_output(&mut ops, &mut output)?;
        }
        ops.push(ArithTk::Comma);
      }

      ArithTk::Op(ref op) => {
        // Intercept && and || for short-circuit emission.
        if matches!(op, ArithOp::And | ArithOp::Or) {
          // Flush higher-precedence ops first (LHS finished).
          let cur_prec = if matches!(op, ArithOp::And) { 3 } else { 2 };
          while let Some(top) = ops.last() {
            if Self::is_barrier(top) { break; }
            let top_prec = Self::precedence(top);
            if top_prec >= cur_prec {
              Self::pop_to_output(&mut ops, &mut output)?;
            } else {
              break;
            }
          }
          // Reserve placeholder jump and push pending marker.
          let jump_idx = output.len();
          if matches!(op, ArithOp::And) {
            output.push(ArithTk::JumpIfZeroPeek(0));
            output.push(ArithTk::Pop);
            ops.push(ArithTk::PendingAnd(jump_idx));
          } else {
            output.push(ArithTk::JumpIfNonZeroPeek(0));
            output.push(ArithTk::Pop);
            ops.push(ArithTk::PendingOr(jump_idx));
          }
        } else {
          let right_assoc = Self::is_right_assoc(&token);
          let cur_prec = Self::precedence(&token);
          while let Some(top) = ops.last() {
            if Self::is_barrier(top) { break; }
            let top_prec = Self::precedence(top);
            if top_prec > cur_prec || (top_prec == cur_prec && !right_assoc) {
              Self::pop_to_output(&mut ops, &mut output)?;
            } else {
              break;
            }
          }
          ops.push(token);
        }
      }

      ArithTk::Question => {
        // Cond is fully built. Flush higher-prec ops, then emit a placeholder
        // JIFZ that will jump to the else-arm once we know its position.
        let cur_prec = 1;
        while let Some(top) = ops.last() {
          if Self::is_barrier(top) { break; }
          let top_prec = Self::precedence(top);
          if top_prec > cur_prec {
            Self::pop_to_output(&mut ops, &mut output)?;
          } else {
            break;
          }
        }
        let jump_idx = output.len();
        output.push(ArithTk::JumpIfZero(0));
        ops.push(ArithTk::PendingTernaryThen(jump_idx));
      }

      ArithTk::Colon => {
        // Flush ops down to the matching PendingTernaryThen.
        while let Some(top) = ops.last() {
          if matches!(top, ArithTk::PendingTernaryThen(_)) { break; }
          if matches!(top, ArithTk::LParen) {
            return Err(sherr!(ParseErr, "Unexpected ':' before matching '?' in arithmetic"));
          }
          Self::pop_to_output(&mut ops, &mut output)?;
        }
        let Some(ArithTk::PendingTernaryThen(jifz_idx)) = ops.pop() else {
          return Err(sherr!(ParseErr, "':' without matching '?' in arithmetic expression"));
        };
        // Emit a placeholder JMP (then→end), then patch the JIFZ to point at
        // the start of the else-arm (right after this JMP).
        let jmp_idx = output.len();
        output.push(ArithTk::Jump(0));
        output[jifz_idx] = ArithTk::JumpIfZero(output.len());
        ops.push(ArithTk::PendingTernaryElse(jmp_idx));
      }

      ArithTk::LParen => ops.push(token),

      ArithTk::RParen => Self::flush_ops(&mut ops, &mut output, true)?,

      // Control-flow opcodes are only emitted by to_rpn, never lexed.
      ArithTk::JumpIfZero(_)
      | ArithTk::JumpIfZeroPeek(_)
      | ArithTk::JumpIfNonZeroPeek(_)
      | ArithTk::Jump(_)
      | ArithTk::Pop
      | ArithTk::Nez
      | ArithTk::PendingAnd(_)
      | ArithTk::PendingOr(_)
      | ArithTk::PendingTernaryThen(_)
      | ArithTk::PendingTernaryElse(_) => {
        unreachable!("control-flow opcodes are never produced by the tokenizer")
      }
    });

    while !ops.is_empty() {
      Self::pop_to_output(&mut ops, &mut output)?;
    }

    Ok(output)
  }

  #[expect(clippy::too_many_lines, clippy::cast_sign_loss)]
  pub fn eval_rpn(tokens: &[ArithTk]) -> ShResult<i64> {
    let mut stack: Vec<StackVal> = Vec::new();

    macro_rules! pop_num {
      () => {
        stack
          .pop()
          .ok_or_else(|| sherr!(ParseErr, "Missing operand in arithmetic expression"))?
          .to_num()?
      };
    }

    macro_rules! pop_var {
      () => {
        match stack
          .pop()
          .ok_or_else(|| sherr!(ParseErr, "Missing operand in arithmetic expression"))?
        {
          StackVal::Var(name) => name,
          StackVal::Num(_) => return Err(sherr!(ParseErr, "Assignment target must be a variable")),
        }
      };
    }

    let mut i = 0;
    while i < tokens.len() {
      match tokens[i].clone() {
        ArithTk::Num(n) => stack.push(StackVal::Num(n)),

        ArithTk::Var(name) => stack.push(StackVal::Var(name)),

        // Control flow
        // set i directly and continue (skip auto-increment).
        ArithTk::Jump(target) => {
          i = target;
          continue;
        }
        ArithTk::JumpIfZero(target) => {
          let val = pop_num!();
          if val == 0 {
            i = target;
            continue;
          }
        }
        ArithTk::JumpIfZeroPeek(target) => {
          let val = stack
            .last()
            .ok_or_else(|| sherr!(ParseErr, "Empty stack at conditional jump"))?
            .to_num()?;
          if val == 0 {
            i = target;
            continue;
          }
        }
        ArithTk::JumpIfNonZeroPeek(target) => {
          let val = stack
            .last()
            .ok_or_else(|| sherr!(ParseErr, "Empty stack at conditional jump"))?
            .to_num()?;
          if val != 0 {
            i = target;
            continue;
          }
        }
        ArithTk::Pop => {
          stack.pop();
        }
        ArithTk::Nez => {
          let val = pop_num!();
          stack.push(StackVal::Num(i64::from(val != 0)));
        }

        ArithTk::Not => {
          let val = pop_num!();
          stack.push(StackVal::Num(i64::from(val == 0)));
        }

        ArithTk::Neg => {
          let val = pop_num!();
          stack.push(StackVal::Num(-val));
        }

        ArithTk::UPlus => {
          let val = pop_num!();
          stack.push(StackVal::Num(val));
        }

        ArithTk::BitNot => {
          let val = pop_num!();
          stack.push(StackVal::Num(!val));
        }

        ArithTk::Comma => {
          // Discard LHS, keep RHS already on stack
          let rhs = stack
            .pop()
            .ok_or_else(|| sherr!(ParseErr, "Missing operand after ','"))?;
          let _lhs = stack
            .pop()
            .ok_or_else(|| sherr!(ParseErr, "Missing operand before ','"))?;
          stack.push(rhs);
        }

        ArithTk::Op(op) => {
          match op {
            // Assignment ops
            // LHS must be a Var
            ArithOp::Assign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              Shed::vars_mut(|v| {
                v.set_var(
                  &lhs,
                  VarKind::string(rhs.to_string().into()),
                  VarFlags::empty(),
                )
              })
              .unwrap();
              stack.push(StackVal::Num(rhs));
            }
            ArithOp::PlusAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)? + rhs;
              Shed::vars_mut(|v| {
                v.set_var(
                  &lhs,
                  VarKind::string(new_val.to_string().into()),
                  VarFlags::empty(),
                )
              })
              .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::MinusAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)? - rhs;
              Shed::vars_mut(|v| {
                v.set_var(
                  &lhs,
                  VarKind::string(new_val.to_string().into()),
                  VarFlags::empty(),
                )
              })
              .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::MulAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)? * rhs;
              Shed::vars_mut(|v| {
                v.set_var(
                  &lhs,
                  VarKind::string(new_val.to_string().into()),
                  VarFlags::empty(),
                )
              })
              .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::DivAssign => {
              let rhs = pop_num!();
              if rhs == 0 {
                return Err(sherr!(InternalErr, "Division by zero"));
              }
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)? / rhs;
              Shed::vars_mut(|v| {
                v.set_var(
                  &lhs,
                  VarKind::string(new_val.to_string().into()),
                  VarFlags::empty(),
                )
              })
              .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::ModAssign => {
              let rhs = pop_num!();
              if rhs == 0 {
                return Err(sherr!(InternalErr, "Modulo by zero"));
              }
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)? % rhs;
              Shed::vars_mut(|v| {
                v.set_var(
                  &lhs,
                  VarKind::string(new_val.to_string().into()),
                  VarFlags::empty(),
                )
              })
              .unwrap();
              stack.push(StackVal::Num(new_val));
            }

            // Binary math
            ArithOp::Add => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(lhs + rhs));
            }
            ArithOp::Sub => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(lhs - rhs));
            }
            ArithOp::Mul => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(lhs * rhs));
            }
            ArithOp::Div => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              if rhs == 0 {
                return Err(sherr!(InternalErr, "Division by zero"));
              }
              stack.push(StackVal::Num(lhs / rhs));
            }
            ArithOp::Pow => {
              let exp = pop_num!();
              let base = pop_num!();
              if exp < 0 {
                return Err(sherr!(InternalErr, "exponent less than 0"));
              }
              stack.push(StackVal::Num(base.wrapping_pow(exp as u32)));
            }
            ArithOp::Mod => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              if rhs == 0 {
                return Err(sherr!(InternalErr, "Modulo by zero"));
              }
              stack.push(StackVal::Num(lhs % rhs));
            }

            // Comparison (result is 1 or 0)
            ArithOp::Lt => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(i64::from(lhs < rhs)));
            }
            ArithOp::Gt => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(i64::from(lhs > rhs)));
            }
            ArithOp::Le => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(i64::from(lhs <= rhs)));
            }
            ArithOp::Ge => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(i64::from(lhs >= rhs)));
            }
            ArithOp::Eq => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(i64::from(lhs == rhs)));
            }
            ArithOp::Ne => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(i64::from(lhs != rhs)));
            }

            // && and || are decomposed into JIFZ_PEEK/JIFNZ_PEEK + Pop + Nez at
            // to_rpn time (see the ArithTk::Op handler). They never reach eval.
            ArithOp::And | ArithOp::Or => {
              return Err(sherr!(
                ParseErr,
                "Internal: && / || should have been decomposed before eval"
              ));
            }

            // Bitwise
            ArithOp::BitAnd => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(lhs & rhs));
            }
            ArithOp::BitOr => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(lhs | rhs));
            }
            ArithOp::BitXor => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(lhs ^ rhs));
            }
            ArithOp::ShiftL => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(lhs.wrapping_shl(rhs as u32)));
            }
            ArithOp::ShiftR => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(lhs.wrapping_shr(rhs as u32)));
            }

            // Bitwise/shift compound assignment
            ArithOp::BitAndAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)? & rhs;
              Shed::vars_mut(|v| {
                v.set_var(
                  &lhs,
                  VarKind::string(new_val.to_string().into()),
                  VarFlags::empty(),
                )
              })
              .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::BitOrAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)? | rhs;
              Shed::vars_mut(|v| {
                v.set_var(
                  &lhs,
                  VarKind::string(new_val.to_string().into()),
                  VarFlags::empty(),
                )
              })
              .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::BitXorAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)? ^ rhs;
              Shed::vars_mut(|v| {
                v.set_var(
                  &lhs,
                  VarKind::string(new_val.to_string().into()),
                  VarFlags::empty(),
                )
              })
              .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::ShiftLAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)?.wrapping_shl(rhs as u32);
              Shed::vars_mut(|v| v.set_var(&lhs, VarKind::Int(new_val as i32), VarFlags::empty()))
                .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::ShiftRAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_i64(&lhs)?.wrapping_shr(rhs as u32);
              Shed::vars_mut(|v| v.set_var(&lhs, VarKind::Int(new_val as i32), VarFlags::empty()))
                .unwrap();
              stack.push(StackVal::Num(new_val));
            }
          }
        }

        ArithTk::Inc
        | ArithTk::Dec
        | ArithTk::LParen
        | ArithTk::RParen
        | ArithTk::Question
        | ArithTk::Colon
        | ArithTk::PendingAnd(_)
        | ArithTk::PendingOr(_)
        | ArithTk::PendingTernaryThen(_)
        | ArithTk::PendingTernaryElse(_) => {
          return Err(sherr!(
            ParseErr,
            "Unexpected token during arithmetic evaluation: '{:?}'",
            &tokens[i],
          ));
        }
      }
      i += 1;
    }

    if stack.len() != 1 {
      return Err(sherr!(ParseErr, "Invalid arithmetic expression"));
    }

    stack.pop().unwrap().to_num()
  }
}

/// Evaluate an arithmetic expression string, returning the result.
/// The caller is responsible for stripping any `((...))` or `(...)` wrappers.
pub fn expand_arithmetic(expr: &str) -> ShResult<VarStr> {
  let unescaped = unescape_math(expr)?;
  let expanded = expand_raw(&mut unescaped.chars().peekable())?;
  let tokens = ArithTk::tokenize(&expanded)?;
  let rpn = ArithTk::to_rpn(tokens)?;
  let result = ArithTk::eval_rpn(&rpn)?;
  Ok(result.into())
}

/// Strip `((...))` or `(...)` wrappers and evaluate. Convenience for call sites
/// that receive the raw token including its delimiters.
pub fn expand_arithmetic_wrapped(raw: &str) -> ShResult<VarStr> {
  let mut expr = raw.trim();
  while let Some(inner) = strip_enclosing_parens(expr) {
    expr = inner.trim();
  }
  expand_arithmetic(expr)
}

fn strip_enclosing_parens(s: &str) -> Option<&str> {
  let inner = s.strip_prefix('(')?;
  let mut chars = inner.chars().peekable();
  let mut pos = 0;
  // The opening `(` is already consumed (depth 1); scan to its matching `)`.
  // `pos` lands just past that `)`; if that is the end of `inner`, the opening
  // paren enclosed the whole string.
  (util::scan_parens(&mut chars, &mut pos, 1) && pos == inner.len()).then(|| &inner[..pos - 1])
}

#[cfg(test)]
#[expect(clippy::float_cmp)]
mod tests {
  use super::*;
  use crate::state::{Shed, vars::VarFlags, vars::VarKind};
  use crate::tests::testutil::TestGuard;

  fn arith(s: &str) -> f64 {
    // Tests pass raw expressions - no outer ((...)) wrapper stripping
    expand_arithmetic(s)
      .unwrap()
      .to_str_lossy()
      .parse::<f64>()
      .unwrap()
  }

  // ===================== Wrapper stripping =====================

  #[test]
  fn strip_enclosing_parens_only_full_enclosure() {
    // Fully enclosing -> inner contents returned.
    assert_eq!(strip_enclosing_parens("(a+b)"), Some("a+b"));
    assert_eq!(strip_enclosing_parens("(2*(3))"), Some("2*(3)"));
    assert_eq!(strip_enclosing_parens("((x))"), Some("(x)"));
    // Leading `(` closes before the end -> NOT enclosing, left intact.
    assert_eq!(strip_enclosing_parens("(1+2)+(3)"), None);
    // No outer parens at all.
    assert_eq!(strip_enclosing_parens("a+b"), None);
  }

  #[test]
  fn wrapped_handles_body_ending_in_paren() {
    // Regression: greedy `((`/`))` stripping dropped a real paren when the body
    // began/ended with one, so these all raised a parse error.
    let w = |s: &str| {
      expand_arithmetic_wrapped(s)
        .unwrap()
        .to_str_lossy()
        .parse::<f64>()
        .unwrap()
    };
    assert_eq!(w("(5-(2))"), 3.0);
    assert_eq!(w("(2*(3))"), 6.0);
    assert_eq!(w("((1+2)+(3))"), 6.0);
    assert_eq!(w("((1+2)*3)"), 9.0);
    // Preserved: one pair, two pairs, and the comma operator inside a group.
    assert_eq!(w("(2+3)"), 5.0);
    assert_eq!(w("((2+3))"), 5.0);
    assert_eq!(w("(a = 5, a * 2)"), 10.0);
  }

  // ===================== Basic math =====================

  #[test]
  fn arith_addition() {
    assert_eq!(arith("(1+2)"), 3.0);
  }

  #[test]
  fn arith_subtraction() {
    assert_eq!(arith("(10-3)"), 7.0);
  }

  #[test]
  fn arith_multiplication() {
    assert_eq!(arith("(3*4)"), 12.0);
  }

  #[test]
  fn arith_division() {
    assert_eq!(arith("(10/2)"), 5.0);
  }

  #[test]
  fn arith_modulo() {
    assert_eq!(arith("(10%3)"), 1.0);
  }

  #[test]
  fn arith_pow() {
    assert_eq!(arith("2**10"), 1024.0);
    assert_eq!(arith("2**0"), 1.0);
    // right-associative
    assert_eq!(arith("2**2**3"), 256.0);
    // binds tighter than `*`, looser than unary minus (bash: -2**2 == 4)
    assert_eq!(arith("2*3**2"), 18.0);
    assert_eq!(arith("-2**2"), 4.0);
    assert_eq!(arith("-2**3"), -8.0);
  }

  #[test]
  fn arith_radix_literals() {
    assert_eq!(arith("2#101"), 5.0);
    assert_eq!(arith("16#ff"), 255.0);
    assert_eq!(arith("8#17"), 15.0);
    assert_eq!(arith("36#z"), 35.0); // case-insensitive for base <= 36
    assert_eq!(arith("36#Z"), 35.0);
    assert_eq!(arith("64#A"), 36.0); // base > 36: A..Z are 36..61
    assert_eq!(arith("64#_"), 63.0);
    assert_eq!(arith("2#10+16#f"), 17.0);
  }

  #[test]
  fn arith_radix_invalid() {
    // base out of range, and a digit out of range for the base
    assert!(expand_arithmetic("1#0").is_err());
    assert!(expand_arithmetic("2#5").is_err());
  }

  #[test]
  fn arith_recursive_variable_resolution() {
    let _g = TestGuard::new();
    // A variable holding another name resolves transitively.
    Shed::vars_mut(|v| v.set_var("x", VarKind::string("3".into()), VarFlags::empty())).unwrap();
    Shed::vars_mut(|v| v.set_var("y", VarKind::string("x".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("y+1"), 4.0);
    // A variable holding a sub-expression is evaluated.
    Shed::vars_mut(|v| v.set_var("b", VarKind::string("x+1".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("b*2"), 8.0);
    // A chain resolves to the end.
    Shed::vars_mut(|v| v.set_var("a", VarKind::string("b".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("a"), 4.0);
  }

  #[test]
  fn arith_recursion_cycle_errors_not_crashes() {
    // Self-reference must error (via the depth cap), not overflow the stack.
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("z", VarKind::string("z".into()), VarFlags::empty())).unwrap();
    assert!(expand_arithmetic("z").is_err());
    // Mutual reference likewise.
    Shed::vars_mut(|v| v.set_var("p", VarKind::string("q".into()), VarFlags::empty())).unwrap();
    Shed::vars_mut(|v| v.set_var("q", VarKind::string("p".into()), VarFlags::empty())).unwrap();
    assert!(expand_arithmetic("p").is_err());
  }

  #[test]
  fn arith_unset_variable_is_zero() {
    let _g = TestGuard::new();
    assert_eq!(arith("no_such_var_zzz + 7"), 7.0);
  }

  #[test]
  fn arith_precedence() {
    assert_eq!(arith("(2+3*4)"), 14.0);
  }

  #[test]
  fn arith_parens() {
    assert_eq!(arith("(2+3)*4"), 20.0);
  }

  #[test]
  fn arith_nested_parens() {
    assert_eq!(arith("(1+2)*(3+4)"), 21.0);
  }

  #[test]
  fn arith_spaces() {
    assert_eq!(arith("( 1 + 2 )"), 3.0);
  }

  #[test]
  fn arith_unary_neg() {
    assert_eq!(arith("(-5)"), -5.0);
  }

  #[test]
  fn arith_unary_neg_in_expr() {
    assert_eq!(arith("(10 + -3)"), 7.0);
  }

  // ===================== Comparison =====================

  #[test]
  fn arith_lt_true() {
    assert_eq!(arith("(3 < 5)"), 1.0);
  }

  #[test]
  fn arith_lt_false() {
    assert_eq!(arith("(5 < 3)"), 0.0);
  }

  #[test]
  fn arith_eq_true() {
    assert_eq!(arith("(4 == 4)"), 1.0);
  }

  #[test]
  fn arith_ne_true() {
    assert_eq!(arith("(3 != 4)"), 1.0);
  }

  #[test]
  fn arith_le_equal() {
    assert_eq!(arith("(5 <= 5)"), 1.0);
  }

  // ===================== Logical =====================

  #[test]
  fn arith_logical_and_true() {
    assert_eq!(arith("(1 && 1)"), 1.0);
  }

  #[test]
  fn arith_logical_and_false() {
    assert_eq!(arith("(1 && 0)"), 0.0);
  }

  #[test]
  fn arith_logical_or_true() {
    assert_eq!(arith("(0 || 1)"), 1.0);
  }

  #[test]
  fn arith_not_true() {
    assert_eq!(arith("(!0)"), 1.0);
  }

  #[test]
  fn arith_not_false() {
    assert_eq!(arith("(!1)"), 0.0);
  }

  // ===================== Assignment =====================

  #[test]
  fn arith_assign() {
    let _g = TestGuard::new();
    arith("(x = 5)");
    let val = try_var!("x").unwrap();
    assert_eq!(val, "5");
  }

  #[test]
  fn arith_plus_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("3".into()), VarFlags::empty())).unwrap();
    arith("(x += 2)");
    let val = try_var!("x").unwrap();
    assert_eq!(val, "5");
  }

  #[test]
  fn arith_chained_assign() {
    let _g = TestGuard::new();
    arith("(a = b = 7)");
    let a = try_var!("a").unwrap();
    let b = try_var!("b").unwrap();
    assert_eq!(a, "7");
    assert_eq!(b, "7");
  }

  // ===================== Inc/Dec =====================

  #[test]
  fn arith_postfix_inc() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("i", VarKind::Str("5".into()), VarFlags::empty())).unwrap();
    let result = arith("(i++)");
    assert_eq!(result, 5.0); // returns old value
    let val = try_var!("i").unwrap();
    assert_eq!(val, "6");
  }

  #[test]
  fn arith_prefix_inc() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("i", VarKind::Str("5".into()), VarFlags::empty())).unwrap();
    let result = arith("(++i)");
    assert_eq!(result, 6.0); // returns new value
    let val = try_var!("i").unwrap();
    assert_eq!(val, "6");
  }

  // ===================== Comma =====================

  #[test]
  fn arith_comma_returns_last() {
    let _g = TestGuard::new();
    // (j=2, j+1) should set j=2 and return 3
    let result = arith("(j=2, j+1)");
    assert_eq!(result, 3.0);
    let val = try_var!("j").unwrap();
    assert_eq!(val, "2");
  }

  #[test]
  fn arith_nested_comma() {
    let _g = TestGuard::new();
    // i=(j=2,j+1) sets j=2, evaluates j+1=3, assigns i=3
    arith("(i=(j=2,j+1))");
    let i = try_var!("i").unwrap();
    let j = try_var!("j").unwrap();
    assert_eq!(i, "3");
    assert_eq!(j, "2");
  }

  // ===================== Variable reads =====================

  #[test]
  fn arith_with_variable() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("5".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x + 3)"), 8.0);
  }

  #[test]
  fn arith_undefined_var_is_zero() {
    let _g = TestGuard::new();
    assert_eq!(arith("(undef_var + 1)"), 1.0);
  }

  // ===================== Comparison: Gt / Ge =====================

  #[test]
  fn arith_gt_true() {
    let _g = TestGuard::new();
    assert_eq!(arith("(5 > 3)"), 1.0);
  }

  #[test]
  fn arith_gt_false() {
    let _g = TestGuard::new();
    assert_eq!(arith("(3 > 5)"), 0.0);
  }

  #[test]
  fn arith_ge_equal() {
    let _g = TestGuard::new();
    assert_eq!(arith("(5 >= 5)"), 1.0);
  }

  #[test]
  fn arith_ge_false() {
    let _g = TestGuard::new();
    assert_eq!(arith("(3 >= 5)"), 0.0);
  }

  // ===================== Bitwise =====================

  #[test]
  fn arith_bit_and() {
    let _g = TestGuard::new();
    assert_eq!(arith("(12 & 10)"), 8.0); // 1100 & 1010 = 1000
  }

  #[test]
  fn arith_bit_or() {
    let _g = TestGuard::new();
    assert_eq!(arith("(12 | 10)"), 14.0); // 1100 | 1010 = 1110
  }

  #[test]
  fn arith_bit_xor() {
    let _g = TestGuard::new();
    assert_eq!(arith("(12 ^ 10)"), 6.0); // 1100 ^ 1010 = 0110
  }

  #[test]
  fn arith_bit_not() {
    let _g = TestGuard::new();
    assert_eq!(arith("(~0)"), -1.0);
    assert_eq!(arith("(~5)"), -6.0);
  }

  #[test]
  fn arith_shift_left() {
    let _g = TestGuard::new();
    assert_eq!(arith("(1 << 4)"), 16.0);
  }

  #[test]
  fn arith_shift_right() {
    let _g = TestGuard::new();
    assert_eq!(arith("(64 >> 2)"), 16.0);
  }

  // ===================== Unary plus =====================

  #[test]
  fn arith_unary_plus() {
    let _g = TestGuard::new();
    assert_eq!(arith("(+5)"), 5.0);
    assert_eq!(arith("(3 + +2)"), 5.0);
  }

  // ===================== Compound assignment ops =====================

  #[test]
  fn arith_minus_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("10".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x -= 3)"), 7.0);
    assert_eq!(try_var!("x").unwrap(), "7");
  }

  #[test]
  fn arith_mul_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("6".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x *= 7)"), 42.0);
    assert_eq!(try_var!("x").unwrap(), "42");
  }

  #[test]
  fn arith_div_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("20".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x /= 4)"), 5.0);
    assert_eq!(try_var!("x").unwrap(), "5");
  }

  #[test]
  fn arith_mod_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("17".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x %= 5)"), 2.0);
    assert_eq!(try_var!("x").unwrap(), "2");
  }

  #[test]
  fn arith_bit_and_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("12".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x &= 10)"), 8.0);
  }

  #[test]
  fn arith_bit_or_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("12".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x |= 3)"), 15.0);
  }

  #[test]
  fn arith_bit_xor_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("12".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x ^= 10)"), 6.0);
  }

  #[test]
  fn arith_shift_l_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("1".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x <<= 3)"), 8.0);
  }

  #[test]
  fn arith_shift_r_assign() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("32".into()), VarFlags::empty())).unwrap();
    assert_eq!(arith("(x >>= 2)"), 8.0);
  }

  // ===================== Postfix/Prefix decrement =====================

  #[test]
  fn arith_postfix_dec() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("5".into()), VarFlags::empty())).unwrap();
    // post-dec yields the pre-decrement value
    assert_eq!(arith("(x--)"), 5.0);
    assert_eq!(try_var!("x").unwrap(), "4");
  }

  #[test]
  fn arith_prefix_dec() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("5".into()), VarFlags::empty())).unwrap();
    // pre-dec yields the new value
    assert_eq!(arith("(--x)"), 4.0);
    assert_eq!(try_var!("x").unwrap(), "4");
  }

  // ===================== Ternary =====================

  #[test]
  fn arith_ternary_true_branch() {
    let _g = TestGuard::new();
    assert_eq!(arith("(1 ? 42 : 99)"), 42.0);
  }

  #[test]
  fn arith_ternary_false_branch() {
    let _g = TestGuard::new();
    assert_eq!(arith("(0 ? 42 : 99)"), 99.0);
  }

  #[test]
  fn arith_ternary_short_circuits_unevaluated_side() {
    // x in the true branch should NOT be evaluated when the condition
    // is false. If it were, an undefined var would still resolve to 0,
    // so this is more about exercising the Jump opcode than detecting
    // side effects, but it confirms the control-flow path.
    let _g = TestGuard::new();
    assert_eq!(arith("(0 ? 1/0 : 7)"), 7.0); // 1/0 would error if evaluated
  }

  // ===================== Logical && / || short-circuit =====================

  #[test]
  fn arith_and_short_circuits_on_zero() {
    let _g = TestGuard::new();
    // 0 && (1/0) — should not evaluate the RHS, so no div-by-zero.
    assert_eq!(arith("(0 && 1/0)"), 0.0);
  }

  #[test]
  fn arith_or_short_circuits_on_nonzero() {
    let _g = TestGuard::new();
    assert_eq!(arith("(1 || 1/0)"), 1.0);
  }

  // ===================== Error paths =====================

  #[test]
  fn arith_division_by_zero_errors() {
    let _g = TestGuard::new();
    assert!(expand_arithmetic("(5 / 0)").is_err());
  }

  #[test]
  fn arith_modulo_by_zero_errors() {
    let _g = TestGuard::new();
    assert!(expand_arithmetic("(5 % 0)").is_err());
  }

  #[test]
  fn arith_div_assign_by_zero_errors() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("5".into()), VarFlags::empty())).unwrap();
    assert!(expand_arithmetic("(x /= 0)").is_err());
  }

  #[test]
  fn arith_mod_assign_by_zero_errors() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("5".into()), VarFlags::empty())).unwrap();
    assert!(expand_arithmetic("(x %= 0)").is_err());
  }

  // ===================== ArithOp::from_str =====================
  //
  // One test per operator string, plus the error path. Uses `matches!` to
  // avoid requiring a PartialEq impl on ArithOp (the enum is module-private
  // and doesn't have one).

  macro_rules! parse_op {
    ($name:ident, $input:expr, $variant:pat) => {
      #[test]
      fn $name() {
        let parsed = $input.parse::<ArithOp>().expect("should parse");
        assert!(
          matches!(parsed, $variant),
          "got {parsed:?}, expected to match {}",
          stringify!($variant)
        );
      }
    };
  }

  // Math
  parse_op!(arithop_add, "+", ArithOp::Add);
  parse_op!(arithop_sub, "-", ArithOp::Sub);
  parse_op!(arithop_mul, "*", ArithOp::Mul);
  parse_op!(arithop_div, "/", ArithOp::Div);
  parse_op!(arithop_mod, "%", ArithOp::Mod);

  // Comparison
  parse_op!(arithop_lt, "<", ArithOp::Lt);
  parse_op!(arithop_gt, ">", ArithOp::Gt);
  parse_op!(arithop_le, "<=", ArithOp::Le);
  parse_op!(arithop_ge, ">=", ArithOp::Ge);
  parse_op!(arithop_eq, "==", ArithOp::Eq);
  parse_op!(arithop_ne, "!=", ArithOp::Ne);

  // Logical
  parse_op!(arithop_and, "&&", ArithOp::And);
  parse_op!(arithop_or, "||", ArithOp::Or);

  // Bitwise
  parse_op!(arithop_bit_and, "&", ArithOp::BitAnd);
  parse_op!(arithop_bit_or, "|", ArithOp::BitOr);
  parse_op!(arithop_bit_xor, "^", ArithOp::BitXor);
  parse_op!(arithop_shift_l, "<<", ArithOp::ShiftL);
  parse_op!(arithop_shift_r, ">>", ArithOp::ShiftR);

  // Assignment
  parse_op!(arithop_assign, "=", ArithOp::Assign);
  parse_op!(arithop_plus_assign, "+=", ArithOp::PlusAssign);
  parse_op!(arithop_minus_assign, "-=", ArithOp::MinusAssign);
  parse_op!(arithop_mul_assign, "*=", ArithOp::MulAssign);
  parse_op!(arithop_div_assign, "/=", ArithOp::DivAssign);
  parse_op!(arithop_mod_assign, "%=", ArithOp::ModAssign);
  parse_op!(arithop_bit_and_assign, "&=", ArithOp::BitAndAssign);
  parse_op!(arithop_bit_or_assign, "|=", ArithOp::BitOrAssign);
  parse_op!(arithop_bit_xor_assign, "^=", ArithOp::BitXorAssign);
  parse_op!(arithop_shift_l_assign, "<<=", ArithOp::ShiftLAssign);
  parse_op!(arithop_shift_r_assign, ">>=", ArithOp::ShiftRAssign);

  // Error path
  #[test]
  fn arithop_unknown_string_errors() {
    assert!("@@".parse::<ArithOp>().is_err());
    assert!("===".parse::<ArithOp>().is_err());
    assert!("".parse::<ArithOp>().is_err());
    assert!("foo".parse::<ArithOp>().is_err());
  }

  // Regressions: expansion errors used to be silently dropped in the
  // regular-word arm of `Expander::expand`, so a malformed arith
  // expansion would assign the raw token text (with markers stripped
  // by split_words) instead of erroring.

  #[test]
  fn bad_arith_in_assignment_errors_not_leaks() {
    use crate::tests::testutil::test_input;
    let _g = TestGuard::new();
    let result = test_input("foo=$((1 k 2))");
    assert!(
      result.is_err() || Shed::get_status() != 0,
      "bad arith should error or set non-zero status; got Ok with status=0"
    );
    // The pre-fix bug: foo would end up as "(1 k 2)".
    let foo = crate::var!("foo");
    assert!(
      foo.is_empty() || !foo.to_str_lossy().contains('k'),
      "foo should not contain the raw arith body; got {foo:?}"
    );
  }

  #[test]
  fn bad_arith_in_command_errors_not_leaks() {
    use crate::tests::testutil::test_input;
    let _g = TestGuard::new();
    // Capture output into a known sink (the `foo` variable) so we don't
    // have to disentangle stdout from the rendered error block (which
    // reproduces the source line and would false-positive any check
    // against the raw arith text).
    test_input("foo=$(echo $((1 k 2)))").ok();
    let foo = crate::var!("foo");
    assert!(
      foo.is_empty() || !foo.to_str_lossy().contains("(1 k 2)"),
      "raw arith body leaked into echo output: {foo:?}"
    );
  }
}
