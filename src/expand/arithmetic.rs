use std::str::FromStr;

use ariadne::Fmt;

use crate::expand::escape::unescape_math;
use crate::expand::var::expand_raw;
use crate::libsh::error::{ShErr, ShResult, next_color};
use crate::sherr;
use crate::state::read_vars;

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
            return Err(sherr!(
              NotFound,
              "Undefined variable in arithmetic expression: '{}'",
              var.fg(next_color()),
            ));
          };
          let Ok(num) = val.parse::<f64>() else {
            return Err(sherr!(
              ParseErr,
              "Variable '{}' does not contain a number",
              var.fg(next_color()),
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
          let rhs = stack
            .pop()
            .ok_or(sherr!(ParseErr, "Missing right-hand operand",))?;
          let lhs = stack
            .pop()
            .ok_or(sherr!(ParseErr, "Missing left-hand operand",))?;
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
          return Err(sherr!(ParseErr, "Unexpected token during evaluation",));
        }
      }
    }

    if stack.len() != 1 {
      return Err(sherr!(ParseErr, "Invalid arithmetic expression",));
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
      _ => Err(sherr!(ParseErr, "Invalid arithmetic operator",)),
    }
  }
}

pub fn expand_arithmetic(raw: &str) -> ShResult<Option<String>> {
  let body = raw.strip_prefix('(').unwrap_or(raw).strip_suffix(')').unwrap_or(raw); // Unwraps are safe here, we already checked for the parens
  let unescaped = unescape_math(body);
  let expanded = expand_raw(&mut unescaped.chars().peekable())?;
  let Some(tokens) = ArithTk::tokenize(&expanded)? else {
    return Ok(None);
  };
  let rpn = ArithTk::to_rpn(tokens)?;
  let result = ArithTk::eval_rpn(rpn)?;
  Ok(Some(result.to_string()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::expand::escape::unescape_math;
  use crate::expand::var::expand_raw;
  use crate::state::{VarFlags, VarKind, write_vars};
  use crate::testutil::TestGuard;

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
}
