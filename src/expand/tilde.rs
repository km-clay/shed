use crate::prelude::*;

pub fn expand_tilde(tilde_sub: Token, shenv: &mut ShEnv) -> Token {
	let mut tilde_sub_raw = tilde_sub.as_raw(shenv);
	if tilde_sub_raw.starts_with('~') {
		let home = std::env::var("HOME").unwrap_or_default();
		tilde_sub_raw = tilde_sub_raw.replacen('~', &home, 1);
		let mut tokens = Lexer::new(tilde_sub_raw,shenv).lex();
		tokens.pop().unwrap_or(tilde_sub)
	} else {
		tilde_sub
	}
}
