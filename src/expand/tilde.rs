use crate::prelude::*;

pub fn expand_tilde_token(tilde_sub: Token, shenv: &mut ShEnv) -> Token {
	let tilde_sub_raw = tilde_sub.as_raw(shenv);
	let result = expand_tilde_string(&tilde_sub_raw);
	if result == tilde_sub_raw {
		return tilde_sub
	}
	let mut tokens = Lexer::new(result,shenv).lex();
	tokens.pop().unwrap_or(tilde_sub)
}

pub fn expand_tilde_string(s: &str) -> String {
	if s.starts_with('~') {
		let home = std::env::var("HOME").unwrap_or_default();
		s.replacen('~', &home, 1)
	} else {
		s.to_string()
	}
}
