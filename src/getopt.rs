use std::sync::Arc;

use fmt::Display;

use crate::{libsh::error::ShResult, parse::lex::Tk, prelude::*};

pub type OptSet = Arc<[Opt]>;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Opt {
  Long(String),
  LongWithArg(String, String),
  Short(char),
  ShortWithArg(char, String),
}

pub struct OptSpec {
  pub opt: Opt,
  pub takes_arg: bool,
}

impl Opt {
  pub fn parse(s: &str) -> Vec<Self> {
    let mut opts = vec![];

    if s.starts_with("--") {
      opts.push(Opt::Long(s.trim_start_matches('-').to_string()))
    } else if s.starts_with('-') {
      let mut chars = s.trim_start_matches('-').chars();
      while let Some(ch) = chars.next() {
        opts.push(Self::Short(ch))
      }
    }

    opts
  }
}

impl Display for Opt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Long(opt) => write!(f, "--{}", opt),
      Self::Short(opt) => write!(f, "-{}", opt),
      Self::LongWithArg(opt, arg) => write!(f, "--{} {}", opt, arg),
      Self::ShortWithArg(opt, arg) => write!(f, "-{} {}", opt, arg),
    }
  }
}

pub fn get_opts(words: Vec<String>) -> (Vec<String>, Vec<Opt>) {
  let mut words_iter = words.into_iter();
  let mut opts = vec![];
  let mut non_opts = vec![];

  while let Some(word) = words_iter.next() {
    if &word == "--" {
      non_opts.extend(words_iter);
      break;
    }
    let parsed_opts = Opt::parse(&word);
    if parsed_opts.is_empty() {
      non_opts.push(word)
    } else {
      opts.extend(parsed_opts);
    }
  }
  (non_opts, opts)
}

pub fn get_opts_from_tokens(tokens: Vec<Tk>, opt_specs: &[OptSpec]) -> ShResult<(Vec<Tk>, Vec<Opt>)> {
  let mut tokens_iter = tokens
		.into_iter()
		.map(|t| t.expand())
		.collect::<ShResult<Vec<_>>>()?
		.into_iter();
  let mut opts = vec![];
  let mut non_opts = vec![];

  while let Some(token) = tokens_iter.next() {
    if &token.to_string() == "--" {
      non_opts.extend(tokens_iter);
      break;
    }
    let parsed_opts = Opt::parse(&token.to_string());

    if parsed_opts.is_empty() {
      non_opts.push(token)
    } else {
      for opt in parsed_opts {
        let mut pushed = false;
        for opt_spec in opt_specs {
          if opt_spec.opt == opt {
            if opt_spec.takes_arg {
              let arg = tokens_iter
                .next()
                .map(|t| t.to_string())
                .unwrap_or_default();

              let opt = match opt {
                Opt::Long(ref opt) => Opt::LongWithArg(opt.to_string(), arg),
                Opt::Short(opt) => Opt::ShortWithArg(opt, arg),
                _ => unreachable!(),
              };
              opts.push(opt);
              pushed = true;
            } else {
              opts.push(opt.clone());
              pushed = true;
            }
          }
        }
        if !pushed {
          non_opts.push(token.clone());
        }
      }
    }
  }
  Ok((non_opts, opts))
}
