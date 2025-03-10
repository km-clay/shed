use crate::{expand::vars::expand_string, prelude::*};

pub fn expand_brace_token(token: Token, shenv: &mut ShEnv) -> ShResult<Vec<Token>> {
	let raw = token.as_raw(shenv);
	let raw_exp = expand_string(&raw, shenv)?;
	log!(DEBUG, raw_exp);
	let expanded = expand_brace_string(&raw_exp);
	log!(DEBUG, expanded);
	let mut new_tokens = shenv.expand_input(&expanded, token.span());
	new_tokens.retain(|tk| tk.rule() != TkRule::Whitespace);
	log!(DEBUG, new_tokens);
	Ok(new_tokens)
}

pub fn expand_brace_string(raw: &str) -> String {
	let mut result = VecDeque::new();
	let mut stack = vec![];
	stack.push(raw.to_string());

	while let Some(current) = stack.pop() {
		if let Some((prefix,braces,suffix)) = get_brace_positions(&current) {
			let expanded = expand_brace_inner(&braces);
			for part in expanded {
				let formatted = format!("{prefix}{part}{suffix}");
				stack.push(formatted);
			}
		} else {
			result.fpush(current);
		}
	}

	result.into_iter().collect::<Vec<_>>().join(" ")
}

pub fn get_brace_positions(slice: &str) -> Option<(String, String, String)> {
	let mut chars = slice.chars().enumerate();
	let mut start = None;
	let mut brc_count = 0;
	while let Some((i,ch)) = chars.next() {
		match ch {
			'{' => {
				if brc_count == 0 {
					start = Some(i);
				}
				brc_count += 1;
			}
			'}' => {
				brc_count -= 1;
				if brc_count == 0 {
					if let Some(start) = start {
						let prefix = slice[..start].to_string();
						let braces = slice[start+1..i].to_string();
						let suffix = slice[i+1..].to_string();
						return Some((prefix,braces,suffix))
					}
				}
			}
			_ => continue
		}
	}
	None
}

fn expand_brace_inner(inner: &str) -> Vec<String> {
	if inner.split_once("..").is_some() && !inner.contains(['{','}']) {
		expand_range(inner)
	} else {
		split_list(inner)
	}
}

fn split_list(list: &str) -> Vec<String> {
	log!(DEBUG, list);
	let mut chars = list.chars();
	let mut items = vec![];
	let mut curr_item = String::new();
	let mut brc_count = 0;

	while let Some(ch) = chars.next() {
		match ch {
			',' if brc_count == 0 => {
				if !curr_item.is_empty() {
					items.push(std::mem::take(&mut curr_item));
				}
			}
			'{' => {
				brc_count += 1;
				curr_item.push(ch);
			}
			'}' => {
				if brc_count == 0 {
					return vec![list.to_string()];
				}
				brc_count -= 1;
				curr_item.push(ch);
			}
			_ => curr_item.push(ch),
		}
	}
	if !curr_item.is_empty() {
		items.push(std::mem::take(&mut curr_item))
	}
	log!(DEBUG,items);
	items
}

fn expand_range(range: &str) -> Vec<String> {
	if let Some((left,right)) = range.split_once("..") {
		// I know, I know
		// This is checking to see if the range looks like "a..b" or "A..B"
		// one character on both sides, both are letters, and both are uppercase OR both are lowercase
		if (left.len() == 1 && right.len() == 1) &&
			(left.chars().all(|ch| ch.is_ascii_alphanumeric() && right.chars().all(|ch| ch.is_ascii_alphanumeric()))) &&
			(
				(left.chars().all(|ch| ch.is_uppercase()) && right.chars().all(|ch| ch.is_uppercase())) ||
				(left.chars().all(|ch| ch.is_lowercase()) && right.chars().all(|ch| ch.is_lowercase()))
			)
		{
			expand_range_alpha(left, right)
		}
		else if right.chars().all(|ch| ch.is_ascii_digit()) && left.chars().all(|ch| ch.is_ascii_digit())
		{
			expand_range_numeric(left, right)
		}
		else
		{
			vec![range.to_string()]
		}
	} else {
		vec![range.to_string()]
	}
}

fn expand_range_alpha(left: &str, right: &str) -> Vec<String> {
	let start = left.chars().next().unwrap() as u8;
	let end = right.chars().next().unwrap() as u8;

	if start > end {
		(end..=start).rev().map(|c| (c as char).to_string()).collect()
	} else {
		(start..=end).map(|c| (c as char).to_string()).collect()
	}
}

fn expand_range_numeric(left: &str, right: &str) -> Vec<String> {
	let start = left.parse::<i32>().unwrap();
	let end = right.parse::<i32>().unwrap();
	(start..=end).map(|i| i.to_string()).collect()
}
