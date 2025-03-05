use crate::{parse::lex::SEPARATORS, prelude::*};

pub fn expand_alias(candidate: Token, shenv: &mut ShEnv) -> Vec<Token> {
	let mut tokens = vec![];
	let mut work_stack = VecDeque::new();
	let mut expanded_aliases = vec![];
	let logic = shenv.logic().clone();

	// Start with the candidate token in the work queue
	work_stack.bpush(candidate);

	// Process until there are no more tokens in the queue
	while let Some(token) = work_stack.fpop() {
		if token.rule() == TkRule::Ident {
			let candidate_str = token.as_raw(shenv);
			if let Some(alias) = logic.get_alias(&candidate_str) {
				// Expand the alias only if it hasn't been expanded yet
				if !expanded_aliases.contains(&candidate_str) {
					expanded_aliases.push(candidate_str);
					let mut new_tokens = shenv.expand_input(alias, token.span());
					for token in new_tokens.iter_mut() {
						work_stack.bpush(token.clone());
					}
				} else {
					// If already expanded, just add the token to the output
					tokens.push(token);
				}
			} else {
				tokens.push(token);
			}
		} else {
			tokens.push(token);
		}
	}
	tokens
}

pub fn expand_aliases(tokens: Vec<Token>, shenv: &mut ShEnv) -> Vec<Token> {
	let mut stream = tokens.iter();
	let mut processed = vec![];
	let mut is_command = true;
	while let Some(token) = stream.next() {
		match token.rule() {
			_ if SEPARATORS.contains(&token.rule()) => {
				is_command = true;
				processed.push(token.clone());
			}
			TkRule::Ident if is_command => {
				is_command = false;
				let mut alias_tokens = expand_alias(token.clone(), shenv);
				log!(DEBUG, alias_tokens);
				if !alias_tokens.is_empty() {
					processed.append(&mut alias_tokens);
				} else {
					processed.push(token.clone());
				}
			}
			_ => processed.push(token.clone()),
		}
	}
	processed
}
