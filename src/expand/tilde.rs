use crate::prelude::*;

pub fn expand_tilde(tilde_sub: Token) -> Token {
	let tilde_sub_raw = tilde_sub.to_string();
	if tilde_sub_raw.starts_with('~') {
		let home = std::env::var("HOME").unwrap_or_default();
		tilde_sub_raw.replacen('~', &home, 1);
		let lex_input = Rc::new(tilde_sub_raw);
		let mut tokens = Lexer::new(lex_input).lex();
		tokens.pop().unwrap_or(tilde_sub)
	} else {
		tilde_sub
	}
}
