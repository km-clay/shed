use crate::{parse::lex::SEPARATORS, prelude::*};

pub fn expand_aliases(input: &str, shenv: &mut ShEnv) -> Option<String> {
	let mut result = input.to_string();
	let mut expanded_aliases = Vec::new();
	let mut found_in_iteration = true;

	// Loop until no new alias expansion happens.
	while found_in_iteration {
		found_in_iteration = false;
		let mut new_result = String::new();
		let mut chars = result.chars().peekable();
		let mut alias_cand = String::new();
		let mut is_cmd = true;

		while let Some(ch) = chars.next() {
			match ch {
				';' | '\n' => {
					new_result.push(ch);
					is_cmd = true;
					// Consume any extra whitespace or delimiters.
					while let Some(&next_ch) = chars.peek() {
						if matches!(next_ch, ' ' | '\t' | ';' | '\n') {
							new_result.push(next_ch);
							chars.next();
						} else {
							break;
						}
					}
				}
				' ' | '\t' => {
					is_cmd = false;
					new_result.push(ch);
				}
				_ if is_cmd => {
					// Accumulate token characters.
					alias_cand.push(ch);
					while let Some(&next_ch) = chars.peek() {
						if matches!(next_ch, ' ' | '\t' | ';' | '\n') {
							break;
						} else {
							alias_cand.push(next_ch);
							chars.next();
						}
					}
					// Check for an alias expansion.
					if let Some(alias) = shenv.logic().get_alias(&alias_cand) {
						// Only expand if we haven't already done so.
						if !expanded_aliases.contains(&alias_cand) {
							new_result.push_str(alias);
							expanded_aliases.push(alias_cand.clone());
							found_in_iteration = true;
						} else {
							new_result.push_str(&alias_cand);
						}
					} else {
						new_result.push_str(&alias_cand);
					}
					alias_cand.clear();
				}
				_ => {
					new_result.push(ch);
				}
			}
		}
		result = new_result;
		log!(DEBUG, result);
	}

	if expanded_aliases.is_empty() {
		None
	} else {
		Some(result)
	}
}
