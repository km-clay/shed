pub mod vars;
pub mod tilde;
pub mod alias;
pub mod cmdsub;

use vars::{expand_dquote, expand_var};
use tilde::expand_tilde;

use crate::prelude::*;

pub fn expand_argv(argv: Vec<Token>, shenv: &mut ShEnv) -> Vec<Token> {
	let mut processed = vec![];
	for arg in argv {
		log!(DEBUG, "{}",arg.as_raw(shenv));
		log!(DEBUG, processed);
		let mut expanded = expand_token(arg, shenv);
		processed.append(&mut expanded);
	}
	processed
}

pub fn expand_token(token: Token, shenv: &mut ShEnv) -> Vec<Token> {
	let mut processed = vec![];
	match token.rule() {
		TkRule::DQuote => {
			let dquote_exp = expand_dquote(token.clone(), shenv);
			processed.push(dquote_exp);
		}
		TkRule::VarSub => {
			let mut varsub_exp = expand_var(token.clone(), shenv);
			processed.append(&mut varsub_exp);
		}
		TkRule::TildeSub => {
			let tilde_exp = expand_tilde(token.clone(), shenv);
			processed.push(tilde_exp);
		}
		_ => {
			if token.rule() != TkRule::Ident {
				log!(WARN, "found this in expand_token: {:?}", token.rule());
			}
			processed.push(token.clone())
		}
	}
	processed
}
