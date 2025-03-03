use crate::{parse::lex::Token, prelude::*};

pub fn expand_var(var_sub: Token, shenv: &mut ShEnv) -> Vec<Token> {
	let var_name = var_sub.to_string();
	let var_name = var_name.trim_start_matches('$').trim_matches(['{','}']);
	let value = Rc::new(
			shenv.vars()
				.get_var(var_name)
				.to_string()
	);
	Lexer::new(value).lex() // Automatically handles word splitting for us
}

pub fn expand_dquote(dquote: Token, shenv: &mut ShEnv) -> Token {
	let dquote_raw = dquote.to_string();
	let mut result = String::new();
	let mut var_name = String::new();
	let mut chars = dquote_raw.chars().peekable();
	let mut in_brace = false;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				if let Some(next_ch) = chars.next() {
					result.push(next_ch)
				}
			}
			'$' => {
				while let Some(ch) = chars.peek() {
					if *ch == '"' {
						break
					}
					let ch = chars.next().unwrap();
					match ch {
						'{' => {
							in_brace = true;
						}
						'}' if in_brace => {
							break
						}
						_ if ch.is_ascii_digit() && var_name.is_empty() && !in_brace => {
							var_name.push(ch);
							break
						}
						'@' | '#' | '*' | '-' | '?' | '!' | '$' if var_name.is_empty() => {
							var_name.push(ch);
							break
						}
						' ' | '\t' => {
							break
						}
						_ => var_name.push(ch)
					}
				}
				log!(TRACE, var_name);
				let value = shenv.vars().get_var(&var_name);
				log!(TRACE, value);
				result.push_str(value);
			}
			_ => result.push(ch)
		}
	}

	log!(DEBUG, result);

	Lexer::new(Rc::new(result)).lex().pop().unwrap_or(dquote)
}
