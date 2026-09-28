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
  pub(crate) fn args(&self) -> &[(VarStr, Span)] {
    &self.args
  }
  pub(crate) fn span(&self) -> Span {
    self.span
  }
  pub(crate) fn key(&self) -> &str {
    self.key.to_str().unwrap_or_default()
  }
  pub(crate) fn value(&self) -> ShResult<VarStr> {
    if self.args.len() == 1 {
      Ok(self.args[0].0.clone())
    } else {
      Err(sherr!(ParseErr @ self.span(), "option '{self}' requires an argument").with_code(2))
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
    let mut span = self.span;
    if let Some(arg_span) = self.args.last().map(|(_, s)| s) {
      span.merge_inplace(*arg_span);
    }

    write!(f, "{}", span.try_slice().unwrap_or_default().to_str_lossy())
  }
}

#[derive(Default, Debug)]
pub(crate) struct OptSpec {
  short: Option<u8>,    // form like '-a'
  long: Option<VarStr>, // form like '--arg'
  key: VarStr,          // internal name used for matching
  argc: usize,          // number of arguments the option takes
}

impl OptSpec {
  pub(crate) fn new(key: &str) -> Self {
    Self {
      key: key.into(),
      ..Default::default()
    }
  }
  pub(crate) fn new_long(key: &str) -> Self {
    Self {
      key: key.into(),
      long: Some(key.into()),
      ..Default::default()
    }
  }
  pub(crate) fn new_short(key: &str, short: u8) -> Self {
    Self {
      key: key.into(),
      short: Some(short),
      ..Default::default()
    }
  }
  pub(crate) fn short(mut self, short: u8) -> Self {
    self.short = Some(short);
    self
  }
  pub(crate) fn long(mut self, long: &str) -> Self {
    self.long = Some(long.into());
    self
  }
  pub(crate) fn argc(mut self, argc: usize) -> Self {
    self.argc = argc;
    self
  }

  pub(crate) fn is_long_match(&self, other: &str) -> bool {
    other
      .strip_prefix("--")
      .is_some_and(|name| self.long.as_deref() == Some(name.as_bytes()))
  }

  pub(crate) fn is_short_match(&self, other: u8) -> bool {
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

pub(super) fn parse_opts(tokens: &[Tk], specs: &[OptSpec]) -> ShResult<Parsed> {
  parse_opts_inner(tokens, specs, false, false)
}

/// Like [`parse_opts`], but `keep_double_dash` controls whether `--` acts as an
/// end-of-options separator (`false`) or is kept as a literal operand (`true`).
/// `echo`/`printf` have no `--` terminator in operand position and pass `true`.
pub(super) fn parse_opts_with(
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
    let span = tk.span;
    for word in tk.expand_to_words()? {
      expanded_words.push((word, span));
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
          let args = take_args(&mut words_iter, spec.argc, span, &word.to_str_lossy())?;
          words.push(Word::Opt(Opt {
            key: spec.key.clone(),
            span,
            args,
          }));
        }
        None if strict => {
          return Err(sherr!(ParseErr @ span, "Unknown option '{word}'").with_code(2));
        }
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
            span,
            &varstr!("-{}", byte as char).to_str_lossy(),
          )?;
          words.push(Word::Opt(Opt {
            key: spec.key.clone(),
            span,
            args,
          }));
        }
      } else if strict {
        let unknown = cluster
          .bytes()
          .find(|ch| !specs.iter().any(|s| s.is_short_match(*ch)))
          .unwrap();
        return Err(sherr!(ParseErr @ span, "Unknown option '-{}'", unknown as char).with_code(2));
      } else {
        words.push(Word::Arg(word, span));
      }
    }
  }

  Ok(Parsed { words, trace })
}

/// Split `tokens` into parsed options and the *raw*, unexpanded operand tokens.
pub(super) fn parse_opts_raw(tokens: &[Tk], specs: &[OptSpec]) -> (Vec<Opt>, Vec<Tk>) {
  let mut opts = vec![];
  let mut operands = vec![];
  let mut end_of_opts = false;

  for tk in tokens {
    let raw = tk.span.slice();

    if !end_of_opts && *raw == *b"--" {
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
            span: tk.span,
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

/// How a caller of [`scan_options`] classifies a short flag character.
pub(crate) enum Role<F> {
  /// A toggle flag carrying the caller's own flag value.
  Set(F),
  /// An invocation-only flag that takes no argument.
  Invocation,
  /// An invocation flag that consumes an argument.
  InvocationArg,
  /// Not a recognized flag.
  Unknown,
}

/// Outcome of scanning a run of `set`-style option words.
pub(crate) struct ScanOutcome {
  /// A `--` terminator was consumed; operands follow.
  pub terminated: bool,
}

/// Scan and dispatch a leading run of `set`-style option words from `words`.
///
/// Handles polarity (`-`/`+`), bundled shorts (`-ex`, `+ex`), `-o NAME` /
/// `+o NAME` (including the attached `-oNAME` form, several names after one
/// `-o`, and the no-name "print" form), and the `--` terminator. Stops —
/// *without consuming* — at the first operand, a lone `-`, or a `--long` word,
/// leaving it in `words` for the caller. `--` is consumed and reported via
/// [`ScanOutcome::terminated`].
///
/// The grammar is generic; the caller supplies the meaning:
/// - `classify` decides what each short char is ([`Role`]).
/// - `set_flag` applies one toggle flag. A word's toggle flags are collected
///   and applied only after the whole cluster parses, so a later error in the
///   same word applies none of them.
/// - `long_opt` handles a `-o`/`+o` name (`Some`) or the no-name print form
///   (`None`).
/// - `invocation` handles a char classified as [`Role::Invocation`] /
///   [`Role::InvocationArg`], receiving the attached argument (leftover cluster
///   chars) if present and the remaining `words` so it can pull a separate one.
/// - `strict` makes an unknown short flag an error rather than a stop.
pub(crate) fn scan_options<I, F>(
  words: &mut Peekable<I>,
  classify: impl Fn(char) -> Role<F>,
  mut set_flag: impl FnMut(bool, F, Span) -> ShResult<()>,
  mut long_opt: impl FnMut(bool, Option<&str>, Span) -> ShResult<()>,
  mut invocation: impl FnMut(char, Option<VarStr>, &mut Peekable<I>, Span) -> ShResult<()>,
  strict: bool,
) -> ShResult<ScanOutcome>
where
  I: Iterator<Item = (VarStr, Span)>,
{
  while let Some((word, span)) = words.peek().cloned() {
    let word = word.to_str_lossy();
    match word.chars().next() {
      Some('-' | '+') => {}
      _ => break, // first operand — leave it in `words`
    }
    if word == "-" {
      break; // a lone `-` is an operand, not an option
    }
    if word.starts_with("--") {
      if word == "--" {
        words.next();
        return Ok(ScanOutcome { terminated: true });
      }
      break; // `--long` word: caller handles it; don't consume
    }

    words.next(); // commit: it's a short cluster or `-o`
    let on = word.starts_with('-');
    let mut cluster = word[1..].chars().collect::<Vec<_>>().into_iter().peekable();
    let mut pending: Vec<F> = vec![];

    while let Some(ch) = cluster.next() {
      if ch == 'o' {
        scan_long(on, &mut cluster, words, span, &mut long_opt)?;
        continue;
      }
      match classify(ch) {
        Role::Set(f) => pending.push(f),
        Role::Invocation => invocation(ch, None, words, span)?,
        Role::InvocationArg => {
          // getopt rule: leftover cluster chars are this option's argument.
          let attached: String = cluster.by_ref().collect();
          let attached = (!attached.is_empty()).then(|| VarStr::from(attached));
          invocation(ch, attached, words, span)?;
          break; // an arg-taking option ends the cluster
        }
        Role::Unknown if strict => {
          return Err(sherr!(ParseErr @ span, "invalid option: -{ch}").with_code(2));
        }
        Role::Unknown => break,
      }
    }

    for f in pending {
      set_flag(on, f, span)?;
    }
  }

  Ok(ScanOutcome { terminated: false })
}

/// Handle a `-o` / `+o` occurrence. The name is taken from the rest of the
/// current cluster if present (`-oname`), otherwise from following operand
/// words (`-o name`, several allowed). With no name at all, `long_opt` is
/// called once with `None` to print the current settings.
fn scan_long<I>(
  on: bool,
  cluster: &mut Peekable<impl Iterator<Item = char>>,
  words: &mut Peekable<I>,
  span: Span,
  long_opt: &mut impl FnMut(bool, Option<&str>, Span) -> ShResult<()>,
) -> ShResult<()>
where
  I: Iterator<Item = (VarStr, Span)>,
{
  let attached: String = cluster.by_ref().collect();
  if !attached.is_empty() {
    return long_opt(on, Some(&attached), span);
  }

  let mut found = false;
  while let Some((word, _)) = words.peek() {
    let word = word.to_str_lossy();
    if word.starts_with('-') || word.starts_with('+') {
      break;
    }
    found = true;
    let (name, name_span) = words.next().unwrap();
    long_opt(on, Some(&name.to_str_lossy()), name_span)?;
  }

  if !found {
    long_opt(on, None, span)?;
  }
  Ok(())
}
