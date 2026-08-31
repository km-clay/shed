use std::{fmt::Display, iter::Peekable};

use bstr::ByteSlice;

use crate::{
  eval::lex::{Span, Tk},
  sherr,
  state::vars::VarStr,
  util::error::ShResult,
  varstr,
};

pub(crate) enum Word {
  Arg(VarStr, Span),
  Opt(Opt),
  Sep(Span), // the '--' separator
}

/// The result of parsing a builtin's command line.
pub(crate) struct Parsed {
  pub words: Vec<Word>,   // the parsed arguments and options
  pub trace: Vec<VarStr>, // the flat word list used for `set -x` tracing
}

impl From<Vec<(VarStr, Span)>> for Parsed {
  fn from(words: Vec<(VarStr, Span)>) -> Self {
    let words = words
      .into_iter()
      .map(|(word, span)| Word::Arg(word, span))
      .collect::<Vec<_>>();
    let trace = words
      .iter()
      .filter_map(|w| match w {
        Word::Arg(word, _) => Some(word.clone()),
        _ => None,
      })
      .collect();
    Parsed { words, trace }
  }
}

pub(crate) struct Opt {
  key: VarStr,
  span: Span,
  args: Vec<(VarStr, Span)>,
}

impl Opt {
  pub fn args(&self) -> &[(VarStr, Span)] {
    &self.args
  }
  pub fn span(&self) -> Span {
    self.span.clone()
  }
  pub fn key(&self) -> &str {
    self.key.to_str().unwrap_or_default()
  }
  pub fn value(&self) -> ShResult<&str> {
    if self.args.len() == 1 {
      Ok(self.args[0].0.to_str().unwrap_or_default())
    } else {
      Err(sherr!(ParseErr @ self.span(), "option '{self}' requires an argument"))
    }
  }
}

#[cfg(test)]
impl Opt {
  /// Construct an `Opt` directly for unit tests, bypassing the parser. `key` is
  /// the canonical option key; `args` are its argument values (empty for a flag).
  pub(crate) fn for_test(key: &str, args: &[&str]) -> Self {
    Opt {
      key: key.into(),
      span: Span::default(),
      args: args.iter().map(|&a| (a.into(), Span::default())).collect(),
    }
  }
}

impl Display for Opt {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let mut span = self.span.clone();
    if let Some(arg_span) = self.args.last().map(|(_, s)| s) {
      span.merge_inplace(arg_span);
    }

    write!(f, "{}", span.to_str_lossy())
  }
}

#[derive(Default, Debug)]
pub struct OptSpec {
  short: Option<u8>,    // form like '-a'
  long: Option<VarStr>, // form like '--arg'
  key: VarStr,          // internal name used for matching
  argc: usize,          // number of arguments the option takes
}

impl OptSpec {
  pub fn new(key: &str) -> Self {
    Self {
      key: key.into(),
      ..Default::default()
    }
  }
  pub fn new_long(key: &str) -> Self {
    Self {
      key: key.into(),
      long: Some(key.into()),
      ..Default::default()
    }
  }
  pub fn new_short(key: &str, short: u8) -> Self {
    Self {
      key: key.into(),
      short: Some(short),
      ..Default::default()
    }
  }
  pub fn short(mut self, short: u8) -> Self {
    self.short = Some(short);
    self
  }
  pub fn long(mut self, long: &str) -> Self {
    self.long = Some(long.into());
    self
  }
  pub fn argc(mut self, argc: usize) -> Self {
    self.argc = argc;
    self
  }

  pub fn is_long_match(&self, other: &str) -> bool {
    other
      .strip_prefix("--")
      .is_some_and(|name| self.long.as_deref() == Some(name.as_bytes()))
  }

  pub fn is_short_match(&self, other: u8) -> bool {
    if let Some(short) = self.short
      && short == other
    {
      return true;
    }
    false
  }
}

/// Concisely define an `OptSpec`.
///
/// The invocation is in two parts: The first part is the long/short option names. Short is optional, long is not.
/// Long-only form is `opt!("long")`
/// Long+short form is `opt!("long" | 's')`
///
/// The second part is the number of arguments. This is optional, and defaults to 0.
/// Long-only form is `opt!("long", 1)`
/// Long+short form is `opt!("long" | 's', 1)`
#[macro_export]
macro_rules! opt {
  ($long:literal) => {
    OptSpec::new_long($long)
  };
  ($long:literal | $short:literal) => {
    OptSpec::new_long($long).short($short)
  };
  ($long:literal, $count:literal) => {
    OptSpec::new_long($long).argc($count)
  };
  ($long:literal | $short:literal, $count:literal) => {
    OptSpec::new_long($long).short($short).argc($count)
  };
}

pub fn parse_opts(tokens: &[Tk], specs: &[OptSpec]) -> ShResult<Parsed> {
  parse_opts_inner(tokens, specs, false, false)
}

/// Like [`parse_opts`], but `keep_double_dash` controls whether `--` acts as an
/// end-of-options separator (`false`) or is kept as a literal operand (`true`).
/// `echo`/`printf` have no `--` terminator in operand position and pass `true`.
pub fn parse_opts_with(
  tokens: &[Tk],
  specs: &[OptSpec],
  strict: bool,
  keep_double_dash: bool,
) -> ShResult<Parsed> {
  parse_opts_inner(tokens, specs, strict, keep_double_dash)
}

fn parse_opts_inner(
  tokens: &[Tk],
  specs: &[OptSpec],
  strict: bool,
  keep_double_dash: bool,
) -> ShResult<Parsed> {
  // Expand tokens and flatten via get_words, preserving spans
  let mut expanded_words = vec![];
  for tk in tokens {
    let tk = tk.clone();
    let span = tk.span.clone();
    let expanded = tk.expand()?;
    for word in expanded.get_words().iter() {
      expanded_words.push((word.clone(), span.clone()));
    }
  }

  // Snapshot the flat expansion for tracing before classification consumes it.
  let trace: Vec<VarStr> = expanded_words
    .iter()
    .map(|(word, _)| word.clone())
    .collect();

  let mut words_iter = expanded_words.into_iter().peekable();
  let mut words = vec![];

  while let Some((word, span)) = words_iter.next() {
    // separator, denotes end of options (unless the builtin keeps `--` literal)
    if word == "--" && !keep_double_dash {
      if words_iter.peek().is_none() {
        // it's the last word, push it as an arg
        words.push(Word::Arg(word, span));
      } else {
        // push it as a separator and collect the remaining words as args
        words.push(Word::Sep(span));
        words.extend(words_iter.map(|(word, span)| Word::Arg(word, span)));
      }
      break;
    }

    if !word.to_str_lossy().starts_with('-')
      || word == "-"
      || word.to_str_lossy().starts_with("---")
    {
      // it's not an option
      words.push(Word::Arg(word, span));
      continue;
    }

    if word.to_str_lossy().starts_with("--") {
      // long option
      match specs.iter().find(|s| s.is_long_match(&word.to_str_lossy())) {
        Some(spec) => {
          let args = take_args(
            &mut words_iter,
            spec.argc,
            span.clone(),
            &word.to_str_lossy(),
          )?;
          words.push(Word::Opt(Opt {
            key: spec.key.clone(),
            span,
            args,
          }));
        }
        None if strict => return Err(sherr!(ParseErr @ span, "Unknown option '{word}'")),
        None => words.push(Word::Arg(word, span)),
      }
    } else if let Some(cluster) = word.to_str_lossy().strip_prefix('-') {
      if cluster
        .bytes()
        .all(|ch| specs.iter().any(|s| s.is_short_match(ch)))
      {
        for byte in cluster.bytes() {
          let spec = specs.iter().find(|s| s.is_short_match(byte)).unwrap();
          let args = take_args(
            &mut words_iter,
            spec.argc,
            span.clone(),
            &varstr!("-{}", byte as char).to_str_lossy(),
          )?;
          words.push(Word::Opt(Opt {
            key: spec.key.clone(),
            span: span.clone(),
            args,
          }));
        }
      } else if strict {
        let unknown = cluster
          .bytes()
          .find(|ch| !specs.iter().any(|s| s.is_short_match(*ch)))
          .unwrap();
        return Err(sherr!(ParseErr @ span, "Unknown option '-{unknown}'"));
      } else {
        words.push(Word::Arg(word, span));
      }
    }
  }

  Ok(Parsed { words, trace })
}

/// Split `tokens` into parsed options and the *raw*, unexpanded operand tokens.
pub fn parse_opts_raw(tokens: &[Tk], specs: &[OptSpec]) -> (Vec<Opt>, Vec<Tk>) {
  let mut opts = vec![];
  let mut operands = vec![];
  let mut end_of_opts = false;

  for tk in tokens {
    let raw = tk.span.as_bytes();

    if !end_of_opts && raw == b"--" {
      end_of_opts = true;
      continue;
    }

    // A short-flag cluster is a single `-` followed by chars that are ALL
    // recognized flags.
    let cluster = (!end_of_opts)
      .then(|| raw.strip_prefix(b"-"))
      .flatten()
      .filter(|c| !c.is_empty() && !c.starts_with(b"-"));

    match cluster {
      Some(c)
        if c
          .bytes()
          .all(|ch| specs.iter().any(|s| s.is_short_match(ch))) =>
      {
        for byte in c.bytes() {
          let spec = specs.iter().find(|s| s.is_short_match(byte)).unwrap();
          opts.push(Opt {
            key: spec.key.clone(),
            span: tk.span.clone(),
            args: vec![],
          });
        }
      }
      _ => operands.push(tk.clone()),
    }
  }

  (opts, operands)
}

fn take_args(
  iter: &mut Peekable<impl Iterator<Item = (VarStr, Span)>>,
  count: usize,
  span: Span,
  label: &str,
) -> ShResult<Vec<(VarStr, Span)>> {
  let mut args = vec![];
  for _ in 0..count {
    if let Some(arg) = iter.next() {
      args.push(arg);
    } else {
      return Err(sherr!(ParseErr @ span, "Option '{label}' requires {count} argument(s)"));
    }
  }
  Ok(args)
}
