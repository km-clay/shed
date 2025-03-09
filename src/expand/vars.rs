use crate::{parse::lex::Token, prelude::*};

use super::cmdsub::expand_cmdsub_string;

pub fn expand_var(var_sub: Token, shenv: &mut ShEnv) -> Vec<Token> {
	let var_name = var_sub.as_raw(shenv);
	let var_name = var_name.trim_start_matches('$').trim_matches(['{','}']);
	let value = shenv.vars().get_var(var_name).to_string();

	shenv.expand_input(&value, var_sub.span())
}

pub fn expand_string(s: &str, shenv: &mut ShEnv) -> ShResult<String> {
	log!(DEBUG, s);
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
					if *ch == '"' || *ch == '`' {
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
						'(' if var_name.is_empty() => {
							let mut paren_count = 1;
							var_name.push_str("$(");
							while let Some(ch) = chars.next() {
								match ch {
									'(' => {
										paren_count += 1;
										var_name.push(ch);
									}
									')' => {
										paren_count -= 1;
										var_name.push(ch);
										if paren_count == 0 {
											break
										}
									}
									_ => var_name.push(ch)
								}
							}
							let value = expand_cmdsub_string(&var_name, shenv)?;
							result.push_str(&value);
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
						' ' | '\t' | '\n' | ';' => {
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
				var_name.clear();
			}
			_ => result.push(ch)
		}
	}
	Ok(result)
}
