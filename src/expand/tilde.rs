use crate::prelude::*;

pub fn expand_tilde(tilde_sub: Token) -> String {
	let tilde_sub_raw = tilde_sub.to_string();
	if tilde_sub_raw.starts_with('~') {
		let home = std::env::var("HOME").unwrap_or_default();
		tilde_sub_raw.replacen('~', &home, 1)
	} else {
		tilde_sub_raw
	}
}
