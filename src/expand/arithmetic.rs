use crate::prelude::*;

use super::vars::expand_dquote;

#[derive(Clone,PartialEq,Debug)]
pub enum ExprToken {
	Number(f64),
	Operator(Op),
	OpenParen,
	CloseParen
}

#[derive(Clone,PartialEq,Debug)]
pub enum Op {
	Add,
	Sub,
	Mul,
	Div,
	Mod,
	Pow
}

impl Op {
	pub fn precedence(&self) -> u8 {
		match self {
			Op::Add | Op::Sub => 1,
			Op::Mul | Op::Div | Op::Mod => 2,
			Op::Pow => 3
		}
	}
	pub fn is_left_associative(&self) -> bool {
		*self != Op::Pow
	}
}

fn tokenize_expr(expr: &str) -> ShResult<Vec<ExprToken>> {
	let mut chars = expr.chars().peekable();
	let mut tokens = vec![];

	while let Some(ch) = chars.next() {
		match ch {
			'+' => tokens.push(ExprToken::Operator(Op::Add)),
			'-' => tokens.push(ExprToken::Operator(Op::Sub)),
			'*' => {
				if chars.peek() == Some(&'*') {
					chars.next();
					tokens.push(ExprToken::Operator(Op::Pow));
				} else {
					tokens.push(ExprToken::Operator(Op::Mul));
				}
			}
			'/' => tokens.push(ExprToken::Operator(Op::Div)),
			'%' => tokens.push(ExprToken::Operator(Op::Mod)),
			'(' => tokens.push(ExprToken::OpenParen),
			')' => tokens.push(ExprToken::CloseParen),
			'0'..='9' => {
				let mut number = ch.to_string();
				while let Some(next_ch) = chars.peek() {
					if next_ch.is_ascii_digit() {
						number.push(chars.next().unwrap());
					} else {
						break;
					}
				}
				let value = number.parse::<f64>().unwrap();
				tokens.push(ExprToken::Number(value));
			}
			' ' | '\t' => continue, // Skip whitespace
			_ => return Err(ShErr::simple(ShErrKind::ParseErr, format!("Unexpected character in arithmetic expansion: {}",ch))), // Handle unexpected characters
		}
	}

	Ok(tokens)
}

fn shunting_yard(tokens: Vec<ExprToken>) -> ShResult<Vec<ExprToken>> {
	let mut sorted = vec![];
	let mut operators = vec![];

	for token in tokens {
		match token {
			ExprToken::Number(_) => sorted.push(token.clone()),
			ExprToken::Operator(ref op) => {
				while let Some(top) = operators.last() {
					if let ExprToken::Operator(top_op) = top {
						if (op.is_left_associative() && op.precedence() <= top_op.precedence())
							|| (!op.is_left_associative() && op.precedence() < top_op.precedence())
						{
							sorted.push(operators.pop().unwrap())
						} else {
							break
						}
					} else {
						break
					}
				}
				operators.push(token.clone())
			}
			ExprToken::OpenParen => operators.push(token.clone()),
			ExprToken::CloseParen => {
				while let Some(top) = operators.pop() {
					if matches!(top, ExprToken::OpenParen) {
						break;
					}
					sorted.push(top);
				}
			}
		}
	}

	while let Some(op) = operators.pop() {
		if matches!(op, ExprToken::OpenParen | ExprToken::CloseParen) {
			return Err(ShErr::simple(ShErrKind::ParseErr, "Mismatched parenthesis in arithmetic expansion"))
		}
		sorted.push(op);
	}

	Ok(sorted)
}

pub fn eval_rpn(tokens: Vec<ExprToken>) -> ShResult<f64> {
	let mut stack = vec![];

	for token in tokens {
		match token {
			ExprToken::Number(num) => stack.push(num),
			ExprToken::Operator(op) => {
				if stack.len() < 2 {
					return Err(ShErr::simple(ShErrKind::ParseErr, "Not enough operands in arithmetic expansion"))
				}
				let rhs = stack.pop().unwrap();
				let lhs = stack.pop().unwrap();
				let result = match op {
					Op::Add => lhs + rhs,
					Op::Sub => lhs - rhs,
					Op::Mul => lhs * rhs,
					Op::Mod => lhs % rhs,
					Op::Pow => lhs.powf(rhs),
					Op::Div => {
						if rhs == 0.0 {
							return Err(ShErr::simple(ShErrKind::ParseErr, "Attempt to divide by zero in arithmetic expansion"))
						}
						lhs / rhs
					}
				};
				stack.push(result);
			}
			ExprToken::OpenParen => todo!(),
			ExprToken::CloseParen => todo!(),
		}
	}

	Ok(stack.pop().unwrap())
}

pub fn expand_arithmetic(token: Token, shenv: &mut ShEnv) -> ShResult<Token> {
	// I mean hey it works (I think)
	let dummy_token = Token::new(TkRule::DQuote, token.span());
	let expanded = expand_dquote(dummy_token, shenv);
	token.span().borrow_mut().expanded = false;
	let expanded_raw = expanded.as_raw(shenv);
	let arith_raw = expanded_raw.trim_matches('`');

	let expr_tokens = shunting_yard(tokenize_expr(arith_raw)?)?;
	log!(DEBUG,expr_tokens);
	let result = eval_rpn(expr_tokens)?.to_string();

	let mut final_expansion = shenv.expand_input(&result, token.span());

	Ok(final_expansion.pop().unwrap_or(token))
}
