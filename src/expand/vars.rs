use crate::{parse::lex::Token, prelude::*};

pub fn expand_var(var_sub: Token, shenv: &mut ShEnv) -> Vec<Token> {
	let var_name = var_sub.as_raw(shenv);
	let var_name = var_name.trim_start_matches('$').trim_matches(['{','}']);
	let value = shenv.vars().get_var(var_name).to_string();

	shenv.expand_input(&value, var_sub.span())
}

pub fn expand_string(s: String, shenv: &mut ShEnv) -> String {
	let mut result = String::new();
	let mut var_name = String::new();
	let mut chars = s.chars().peekable();
	let mut in_brace = false;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				if let Some(next_ch) = chars.next() {
					result.push(next_ch)
				}
			}
			'$' => {
				let mut expanded = false;
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
							let value = shenv.vars().get_var(&var_name);
							result.push_str(value);
							expanded = true;
							break
						}
						_ if ch.is_ascii_digit() && var_name.is_empty() && !in_brace => {
							var_name.push(ch);
							let value = shenv.vars().get_var(&var_name);
							result.push_str(value);
							expanded = true;
							break
						}
						'@' | '#' | '*' | '-' | '?' | '!' | '$' if var_name.is_empty() => {
							var_name.push(ch);
							let value = shenv.vars().get_var(&var_name);
							result.push_str(value);
							expanded = true;
							break
						}
						' ' | '\t' => {
							let value = shenv.vars().get_var(&var_name);
							result.push_str(value);
							result.push(ch);
							expanded = true;
							break
						}
						_ => var_name.push(ch)
					}
				}
				if !expanded {
					let value = shenv.vars().get_var(&var_name);
					result.push_str(value);
				}
			}
			_ => result.push(ch)
		}
	}
	result
}
