use std::{ops::Deref, str::FromStr, sync::{Arc, LazyLock}};

use fmt::Display;

use crate::{libsh::error::ShResult, parse::lex::Tk, prelude::*};

type OptSet = Arc<[Opt]>;

pub static ECHO_OPTS: LazyLock<OptSet> = LazyLock::new(|| {[
	Opt::Short('n'),
	Opt::Short('E'),
	Opt::Short('e'),
	Opt::Short('r'),
].into()});

#[derive(Clone,PartialEq,Eq,Debug)]
pub enum Opt {
	Long(String),
	Short(char)
}

impl Opt {
	pub fn parse(s: &str) -> Vec<Self> {
		flog!(DEBUG, s);
		let mut opts = vec![];

		if s.starts_with("--") {
			opts.push(Opt::Long(s.trim_start_matches('-').to_string()))
		} else if s.starts_with('-') {
			let mut chars = s.trim_start_matches('-').chars();
			while let Some(ch) = chars.next() {
				opts.push(Self::Short(ch))
			}
		}
		flog!(DEBUG,opts);

		opts
	}
}

impl Display for Opt {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Long(opt) => write!(f,"--{}",opt),
			Self::Short(opt) => write!(f,"-{}",opt),
		}
	}
}

pub fn get_opts(words: Vec<String>) -> (Vec<String>,Vec<Opt>) {
	let mut words_iter = words.into_iter();
	let mut opts = vec![];
	let mut non_opts = vec![];

	while let Some(word) = words_iter.next() {
		flog!(DEBUG, opts,non_opts);
		if &word == "--" {
			non_opts.extend(words_iter);
			break
		}
		let parsed_opts = Opt::parse(&word);
		if parsed_opts.is_empty() {
			non_opts.push(word)
		} else {
			opts.extend(parsed_opts);
		}
	}
	(non_opts,opts)
}

pub fn get_opts_from_tokens(tokens: Vec<Tk>) -> (Vec<Tk>, Vec<Opt>) {
	let mut tokens_iter = tokens.into_iter();
	let mut opts = vec![];
	let mut non_opts = vec![];

	while let Some(token) = tokens_iter.next() {
		if &token.to_string() == "--" {
			non_opts.extend(tokens_iter);
			break
		}
		let parsed_opts = Opt::parse(&token.to_string());
		if parsed_opts.is_empty() {
			non_opts.push(token)
		} else {
			opts.extend(parsed_opts);
		}
	}
	(non_opts,opts)
}
