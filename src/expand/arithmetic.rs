use std::str::FromStr;

use ariadne::Fmt;

use crate::expand::escape::unescape_math;
use crate::expand::var::expand_raw;
use crate::libsh::error::{ShErr, ShResult, next_color};
use crate::state::{VarFlags, VarKind, read_vars, write_vars};
use crate::{match_loop, sherr};

#[derive(Debug)]
enum ArithOp {
  // math
  Add,
  Sub,
  Mul,
  Div,
  Mod,
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
  // assign
  Assign,
  PlusAssign,
  MinusAssign,
  MulAssign,
  DivAssign,
  ModAssign,
}

impl FromStr for ArithOp {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "+" => Ok(Self::Add),
      "-" => Ok(Self::Sub),
      "*" => Ok(Self::Mul),
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
      "=" => Ok(Self::Assign),
      "+=" => Ok(Self::PlusAssign),
      "-=" => Ok(Self::MinusAssign),
      "*=" => Ok(Self::MulAssign),
      "/=" => Ok(Self::DivAssign),
      "%=" => Ok(Self::ModAssign),
      _ => Err(sherr!(ParseErr, "Unknown operator: '{s}'")),
    }
  }
}

#[derive(Debug)]
enum ArithTk {
  Num(f64),
  Op(ArithOp),
  Comma,
  LParen,
  RParen,
  Inc, // ++ (raw, resolved to prefix/postfix during to_rpn)
  Dec, // -- (raw, resolved to prefix/postfix during to_rpn)
  Not, // !
  Neg, // unary -
  Var(String),
}

// Stack value used during eval_rpn — keeps Var names alive for assignment targets
enum StackVal {
  Num(f64),
  Var(String),
}

impl StackVal {
  fn to_num(&self) -> ShResult<f64> {
    match self {
      StackVal::Num(n) => Ok(*n),
      StackVal::Var(name) => {
        let val = read_vars(|v| v.try_get_var(name)).unwrap_or_else(|| "0".into());
        val.parse::<f64>().map_err(|_| {
          sherr!(
            ParseErr,
            "Variable '{}' does not contain a number",
            name.fg(next_color()),
          )
        })
      }
    }
  }
}

fn read_var_as_f64(name: &str) -> ShResult<f64> {
  let val = read_vars(|v| v.try_get_var(name)).unwrap_or_else(|| "0".into());
  val.parse::<f64>().map_err(|_| {
    sherr!(
      ParseErr,
      "Variable '{}' does not contain a number",
      name.fg(next_color()),
    )
  })
}

impl ArithTk {
  pub fn tokenize(raw: &str) -> ShResult<Vec<Self>> {
    let mut tokens = Vec::new();
    let mut chars = raw.chars().peekable();
    // Track whether the last emitted token was an operand, to distinguish
    // unary minus from binary subtraction.
    let mut last_was_operand = false;

    match_loop!(chars.peek() => &ch => ch, {
      ' ' | '\t' => { chars.next(); }

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
        let num = num.parse::<f64>().map_err(|_| sherr!(
          ParseErr, "Invalid number in arithmetic expression: '{}'", num,
        ))?;
        tokens.push(Self::Num(num));
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
          // last_was_operand stays false — Neg is unary prefix
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
        } else {
          tokens.push(Self::Op(ArithOp::Add));
          last_was_operand = false;
        }
      }

      '*' => {
        chars.next();
        if chars.peek() == Some(&'=') {
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
        } else {
          return Err(sherr!(ParseErr, "Expected '&&' in arithmetic expression"));
        }
        last_was_operand = false;
      }

      '|' => {
        chars.next();
        if chars.peek() == Some(&'|') {
          chars.next();
          tokens.push(Self::Op(ArithOp::Or));
        } else {
          return Err(sherr!(ParseErr, "Expected '||' in arithmetic expression"));
        }
        last_was_operand = false;
      }

      ',' => {
        tokens.push(Self::Comma);
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
          "Unexpected character in arithmetic expression: '{}'",
          ch.fg(next_color()),
        ));
      }
    });

    Ok(tokens)
  }

  fn to_rpn(tokens: Vec<ArithTk>) -> ShResult<Vec<ArithTk>> {
    let mut output: Vec<ArithTk> = Vec::new();
    let mut ops: Vec<ArithTk> = Vec::new();
    let mut tokens = tokens.into_iter().peekable();

    fn precedence(tk: &ArithTk) -> usize {
      match tk {
        ArithTk::Comma => 0,
        ArithTk::Op(op) => match op {
          ArithOp::Assign
          | ArithOp::PlusAssign
          | ArithOp::MinusAssign
          | ArithOp::MulAssign
          | ArithOp::DivAssign
          | ArithOp::ModAssign => 1,
          ArithOp::Or => 2,
          ArithOp::And => 3,
          ArithOp::Eq | ArithOp::Ne => 4,
          ArithOp::Lt | ArithOp::Gt | ArithOp::Le | ArithOp::Ge => 5,
          ArithOp::Add | ArithOp::Sub => 6,
          ArithOp::Mul | ArithOp::Div | ArithOp::Mod => 7,
        },
        ArithTk::Not | ArithTk::Neg => 8,
        _ => 0,
      }
    }

    fn is_right_assoc(tk: &ArithTk) -> bool {
      matches!(
        tk,
        ArithTk::Not
          | ArithTk::Neg
          | ArithTk::Op(
            ArithOp::Assign
              | ArithOp::PlusAssign
              | ArithOp::MinusAssign
              | ArithOp::MulAssign
              | ArithOp::DivAssign
              | ArithOp::ModAssign
          )
      )
    }

    fn flush_ops(ops: &mut Vec<ArithTk>, output: &mut Vec<ArithTk>, until_paren: bool) {
      while let Some(top) = ops.last() {
        if matches!(top, ArithTk::LParen) {
          break;
        }
        output.push(ops.pop().unwrap());
      }
      if until_paren {
        ops.pop(); // remove the LParen
      }
    }

    match_loop!(tokens.next() => token, {
      ArithTk::Num(_) => output.push(token),

      ArithTk::Var(ref var) => {
        // Check for postfix inc/dec
        if tokens.peek().is_some_and(|tk| matches!(tk, ArithTk::Inc | ArithTk::Dec)) {
          let op = tokens.next().unwrap();
          let val = read_var_as_f64(var)?;
          let delta = if matches!(op, ArithTk::Inc) { 1.0 } else { -1.0 };
          write_vars(|v| v.set_var(var, VarKind::Str((val + delta).to_string()), VarFlags::NONE)).unwrap();
          output.push(ArithTk::Num(val)); // push old value (postfix)
        } else {
          output.push(token); // keep as Var — may be assignment target
        }
      }

      op @ (ArithTk::Inc | ArithTk::Dec) => {
        // Prefix inc/dec — must be followed by a Var
        let Some(ArithTk::Var(_)) = tokens.peek() else {
          return Err(sherr!(
            ParseErr,
            "Expected variable name after '{}' operator",
            if matches!(op, ArithTk::Inc) { "++" } else { "--" },
          ));
        };
        let Some(ArithTk::Var(var)) = tokens.next() else { unreachable!() };
        let val = read_var_as_f64(&var)?;
        let delta = if matches!(op, ArithTk::Inc) { 1.0 } else { -1.0 };
        let new_val = val + delta;
        write_vars(|v| v.set_var(&var, VarKind::Str(new_val.to_string()), VarFlags::NONE)).unwrap();
        output.push(ArithTk::Num(new_val)); // push new value (prefix)
      }

      ArithTk::Not | ArithTk::Neg => {
        // Unary right-associative — push to ops stack
        ops.push(token);
      }

      ArithTk::Comma => {
        // Lowest-precedence binary op — push to ops stack so both operands
        // are fully evaluated before Comma is applied
        while let Some(top) = ops.last() {
          if matches!(top, ArithTk::LParen) { break; }
          output.push(ops.pop().unwrap());
        }
        ops.push(ArithTk::Comma);
      }

      ArithTk::Op(_) => {
        let right_assoc = is_right_assoc(&token);
        let cur_prec = precedence(&token);
        while let Some(top) = ops.last() {
          if matches!(top, ArithTk::LParen) { break; }
          let top_prec = precedence(top);
          if top_prec > cur_prec || (top_prec == cur_prec && !right_assoc) {
            output.push(ops.pop().unwrap());
          } else {
            break;
          }
        }
        ops.push(token);
      }

      ArithTk::LParen => ops.push(token),

      ArithTk::RParen => flush_ops(&mut ops, &mut output, true),
    });

    while let Some(op) = ops.pop() {
      output.push(op);
    }

    Ok(output)
  }

  pub fn eval_rpn(tokens: Vec<ArithTk>) -> ShResult<f64> {
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

    for token in tokens {
      match token {
        ArithTk::Num(n) => stack.push(StackVal::Num(n)),

        ArithTk::Var(name) => stack.push(StackVal::Var(name)),

        ArithTk::Not => {
          let val = pop_num!();
          stack.push(StackVal::Num(if val == 0.0 { 1.0 } else { 0.0 }));
        }

        ArithTk::Neg => {
          let val = pop_num!();
          stack.push(StackVal::Num(-val));
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
            // Assignment ops — LHS must be a Var
            ArithOp::Assign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              write_vars(|v| v.set_var(&lhs, VarKind::Str(rhs.to_string()), VarFlags::NONE))
                .unwrap();
              stack.push(StackVal::Num(rhs));
            }
            ArithOp::PlusAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_f64(&lhs)? + rhs;
              write_vars(|v| v.set_var(&lhs, VarKind::Str(new_val.to_string()), VarFlags::NONE))
                .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::MinusAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_f64(&lhs)? - rhs;
              write_vars(|v| v.set_var(&lhs, VarKind::Str(new_val.to_string()), VarFlags::NONE))
                .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::MulAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_f64(&lhs)? * rhs;
              write_vars(|v| v.set_var(&lhs, VarKind::Str(new_val.to_string()), VarFlags::NONE))
                .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::DivAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_f64(&lhs)? / rhs;
              write_vars(|v| v.set_var(&lhs, VarKind::Str(new_val.to_string()), VarFlags::NONE))
                .unwrap();
              stack.push(StackVal::Num(new_val));
            }
            ArithOp::ModAssign => {
              let rhs = pop_num!();
              let lhs = pop_var!();
              let new_val = read_var_as_f64(&lhs)? % rhs;
              write_vars(|v| v.set_var(&lhs, VarKind::Str(new_val.to_string()), VarFlags::NONE))
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
              stack.push(StackVal::Num(lhs / rhs));
            }
            ArithOp::Mod => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(lhs % rhs));
            }

            // Comparison (result is 1.0 or 0.0)
            ArithOp::Lt => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(if lhs < rhs { 1.0 } else { 0.0 }));
            }
            ArithOp::Gt => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(if lhs > rhs { 1.0 } else { 0.0 }));
            }
            ArithOp::Le => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(if lhs <= rhs { 1.0 } else { 0.0 }));
            }
            ArithOp::Ge => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(if lhs >= rhs { 1.0 } else { 0.0 }));
            }
            ArithOp::Eq => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(if (lhs - rhs).abs() < f64::EPSILON {
                1.0
              } else {
                0.0
              }));
            }
            ArithOp::Ne => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(if (lhs - rhs).abs() >= f64::EPSILON {
                1.0
              } else {
                0.0
              }));
            }

            // Logical (short-circuit semantics not possible in RPN, but side effects already done)
            ArithOp::And => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(if lhs != 0.0 && rhs != 0.0 {
                1.0
              } else {
                0.0
              }));
            }
            ArithOp::Or => {
              let rhs = pop_num!();
              let lhs = pop_num!();
              stack.push(StackVal::Num(if lhs != 0.0 || rhs != 0.0 {
                1.0
              } else {
                0.0
              }));
            }
          }
        }

        ArithTk::Inc | ArithTk::Dec | ArithTk::LParen | ArithTk::RParen => {
          return Err(sherr!(
            ParseErr,
            "Unexpected token during arithmetic evaluation: '{token:?}'"
          ));
        }
      }
    }

    if stack.len() != 1 {
      return Err(sherr!(ParseErr, "Invalid arithmetic expression"));
    }

    stack.pop().unwrap().to_num()
  }
}

/// Evaluate an arithmetic expression string, returning the result.
/// The caller is responsible for stripping any `((...))` or `(...)` wrappers.
pub fn expand_arithmetic(expr: &str) -> ShResult<String> {
  let unescaped = unescape_math(expr);
  let expanded = expand_raw(&mut unescaped.chars().peekable())?;
  let tokens = ArithTk::tokenize(&expanded)?;
  let rpn = ArithTk::to_rpn(tokens)?;
  let result = ArithTk::eval_rpn(rpn)?;
  Ok(result.to_string())
}

/// Strip `((...))` or `(...)` wrappers and evaluate. Convenience for call sites
/// that receive the raw token including its delimiters.
pub fn expand_arithmetic_wrapped(raw: &str) -> ShResult<String> {
  let mut expr = raw;
  if expr.starts_with("((") {
    expr = &expr[2..];
  }
  if expr.ends_with("))") {
    expr = &expr[..expr.len() - 2];
  }
  if expr.starts_with('(') {
    expr = &expr[1..];
  }
  if expr.ends_with(')') {
    expr = &expr[..expr.len() - 1];
  }
  expand_arithmetic(expr)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::state::{VarFlags, VarKind, write_vars};
  use crate::testutil::TestGuard;

  fn arith(s: &str) -> f64 {
    // Tests pass raw expressions — no outer ((...)) wrapper stripping
    expand_arithmetic(s).unwrap().parse::<f64>().unwrap()
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
    let val = read_vars(|v| v.try_get_var("x")).unwrap();
    assert_eq!(val, "5");
  }

  #[test]
  fn arith_plus_assign() {
    let _g = TestGuard::new();
    write_vars(|v| v.set_var("x", VarKind::Str("3".into()), VarFlags::NONE)).unwrap();
    arith("(x += 2)");
    let val = read_vars(|v| v.try_get_var("x")).unwrap();
    assert_eq!(val, "5");
  }

  #[test]
  fn arith_chained_assign() {
    let _g = TestGuard::new();
    arith("(a = b = 7)");
    let a = read_vars(|v| v.try_get_var("a")).unwrap();
    let b = read_vars(|v| v.try_get_var("b")).unwrap();
    assert_eq!(a, "7");
    assert_eq!(b, "7");
  }

  // ===================== Inc/Dec =====================

  #[test]
  fn arith_postfix_inc() {
    let _g = TestGuard::new();
    write_vars(|v| v.set_var("i", VarKind::Str("5".into()), VarFlags::NONE)).unwrap();
    let result = arith("(i++)");
    assert_eq!(result, 5.0); // returns old value
    let val = read_vars(|v| v.try_get_var("i")).unwrap();
    assert_eq!(val, "6");
  }

  #[test]
  fn arith_prefix_inc() {
    let _g = TestGuard::new();
    write_vars(|v| v.set_var("i", VarKind::Str("5".into()), VarFlags::NONE)).unwrap();
    let result = arith("(++i)");
    assert_eq!(result, 6.0); // returns new value
    let val = read_vars(|v| v.try_get_var("i")).unwrap();
    assert_eq!(val, "6");
  }

  // ===================== Comma =====================

  #[test]
  fn arith_comma_returns_last() {
    let _g = TestGuard::new();
    // (j=2, j+1) should set j=2 and return 3
    let result = arith("(j=2, j+1)");
    assert_eq!(result, 3.0);
    let val = read_vars(|v| v.try_get_var("j")).unwrap();
    assert_eq!(val, "2");
  }

  #[test]
  fn arith_nested_comma() {
    let _g = TestGuard::new();
    // i=(j=2,j+1) sets j=2, evaluates j+1=3, assigns i=3
    arith("(i=(j=2,j+1))");
    let i = read_vars(|v| v.try_get_var("i")).unwrap();
    let j = read_vars(|v| v.try_get_var("j")).unwrap();
    assert_eq!(i, "3");
    assert_eq!(j, "2");
  }

  // ===================== Variable reads =====================

  #[test]
  fn arith_with_variable() {
    let _g = TestGuard::new();
    write_vars(|v| v.set_var("x", VarKind::Str("5".into()), VarFlags::NONE)).unwrap();
    assert_eq!(arith("(x + 3)"), 8.0);
  }

  #[test]
  fn arith_undefined_var_is_zero() {
    let _g = TestGuard::new();
    assert_eq!(arith("(undef_var + 1)"), 1.0);
  }
}
