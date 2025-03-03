pub mod expand_vars;
pub mod tilde;
pub mod alias;

use alias::expand_aliases;
use expand_vars::{expand_dquote, expand_var};
use tilde::expand_tilde;

use crate::prelude::*;

pub fn expand_argv(argv: Vec<Token>, shenv: &mut ShEnv) -> Vec<Token> {
	let mut processed = vec![];
	for arg in argv {
		log!(DEBUG, arg);
		log!(DEBUG, processed);
		match arg.rule() {
			TkRule::DQuote => {
				let dquote_exp = expand_dquote(arg.clone(), shenv);
				processed.push(dquote_exp);
			}
			TkRule::VarSub => {
				let mut varsub_exp = expand_var(arg.clone(), shenv);
				processed.append(&mut varsub_exp);
			}
			TkRule::TildeSub => {
				let tilde_exp = expand_tilde(arg.clone());
				processed.push(tilde_exp);
			}
			_ => {
				if arg.rule() != TkRule::Ident {
					log!(WARN, "found this in expand_argv: {:?}", arg.rule());
				}
				processed.push(arg.clone())
			}
		}
	}
	processed
}
