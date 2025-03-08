pub mod vars;
pub mod tilde;
pub mod alias;
pub mod cmdsub;
pub mod arithmetic;

use arithmetic::expand_arithmetic;
use vars::{expand_string, expand_var};
use tilde::expand_tilde;

use crate::prelude::*;

pub fn expand_argv(argv: Vec<Token>, shenv: &mut ShEnv) -> ShResult<Vec<Token>> {
	let mut processed = vec![];
	for arg in argv {
		log!(DEBUG, "{}",arg.as_raw(shenv));
		log!(DEBUG, processed);
		let mut expanded = expand_token(arg, shenv)?;
		processed.append(&mut expanded);
	}
	Ok(processed)
}

pub fn expand_token(token: Token, shenv: &mut ShEnv) -> ShResult<Vec<Token>> {
	let mut processed = vec![];
	match token.rule() {
		TkRule::DQuote => {
			let dquote_exp = expand_string(token.as_raw(shenv), shenv);
			let mut expanded = shenv.expand_input(&dquote_exp, token.span());
			processed.append(&mut expanded);
		}
		TkRule::VarSub => {
			let mut varsub_exp = expand_var(token.clone(), shenv);
			processed.append(&mut varsub_exp);
		}
		TkRule::TildeSub => {
			let tilde_exp = expand_tilde(token.clone(), shenv);
			processed.push(tilde_exp);
		}
		TkRule::ArithSub => {
			let arith_exp = expand_arithmetic(token.clone(), shenv)?;
			processed.push(arith_exp);
		}
		_ => {
			if token.rule() != TkRule::Ident {
				log!(WARN, "found this in expand_token: {:?}", token.rule());
			}
			processed.push(token.clone())
		}
	}
	Ok(processed)
}
