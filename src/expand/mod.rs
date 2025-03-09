pub mod vars;
pub mod tilde;
pub mod alias;
pub mod cmdsub;
pub mod arithmetic;
pub mod prompt;

use arithmetic::expand_arith_token;
use cmdsub::expand_cmdsub_token;
use vars::{expand_string, expand_var};
use tilde::expand_tilde_token;

use crate::prelude::*;

pub fn expand_argv(argv: Vec<Token>, shenv: &mut ShEnv) -> ShResult<Vec<Token>> {
	let mut processed = vec![];
	for arg in argv {
		log!(TRACE, "{}",arg.as_raw(shenv));
		log!(TRACE, processed);
		let mut expanded = expand_token(arg, shenv)?;
		processed.append(&mut expanded);
	}
	Ok(processed)
}

pub fn expand_token(token: Token, shenv: &mut ShEnv) -> ShResult<Vec<Token>> {
	let mut processed = vec![];
	match token.rule() {
		TkRule::DQuote => {
			let dquote_exp = expand_string(&token.as_raw(shenv), shenv)?;
			let mut expanded = shenv.expand_input(&dquote_exp, token.span());
			processed.append(&mut expanded);
		}
		TkRule::VarSub => {
			let mut varsub_exp = expand_var(token.clone(), shenv);
			processed.append(&mut varsub_exp);
		}
		TkRule::TildeSub => {
			let tilde_exp = expand_tilde_token(token.clone(), shenv);
			processed.push(tilde_exp);
		}
		TkRule::ArithSub => {
			let arith_exp = expand_arith_token(token.clone(), shenv)?;
			processed.push(arith_exp);
		}
		TkRule::CmdSub => {
			let mut cmdsub_exp = expand_cmdsub_token(token.clone(), shenv)?;
			processed.append(&mut cmdsub_exp);
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
