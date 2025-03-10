use crate::{parse::lex::SEPARATORS, prelude::*};

pub fn expand_alias(candidate: Token, shenv: &mut ShEnv) -> Vec<Token> {
	let mut tokens = vec![];
	let mut work_stack = VecDeque::new();
	let logic = shenv.logic().clone();
	let mut done = false;

	// Start with the candidate token in the work queue
	work_stack.bpush(candidate);

	// Process until there are no more tokens in the queue
	while !done {
		done = true;
		while let Some(token) = work_stack.fpop() {
			if token.rule() == TkRule::Ident {
				let cand_str = token.as_raw(shenv);
				if let Some(alias) = logic.get_alias(&cand_str) {
					done = false;
					if !token.span().borrow().expanded {
						let mut new_tokens = shenv.expand_input(alias, token.span());
						new_tokens.retain(|tk| tk.rule() != TkRule::Whitespace);
						for token in &new_tokens {
							tokens.push(token.clone());
						}
					}
				} else {
					tokens.push(token);
				}
			} else {
				tokens.push(token);
			}
		}
		if !done {
			work_stack.extend(tokens.drain(..));
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
			TkRule::Case | TkRule::For => {
				processed.push(token.clone());
				while let Some(token) = stream.next() {
					processed.push(token.clone());
					if token.rule() == TkRule::Sep {
						break
					}
				}
			}
			TkRule::Ident if is_command => {
				is_command = false;
				let mut alias_tokens = expand_alias(token.clone(), shenv);
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
