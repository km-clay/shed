use ariadne::Span as ASpan;
use itertools::Itertools;

use crate::{eval::lex::Span, state::vars::VarStr, varstr};

use super::opt::{Opt, Word};

/// The arguments for a builtin.
///
/// Contains the argument vector (`argv`), the parsed options (`opts`), the
/// `span` of the entire command for error reporting, and `stdin` piped in
/// from a previous builtin in an in-process pipeline.
pub struct BuiltinArgs {
  argv: Vec<Word>,
  /// The span of the entire builtin call
  span: Span,
  /// The span of just the command
  cmd_span: Span,
}

impl BuiltinArgs {
  pub fn new(argv: Vec<Word>, span: Span, cmd_span: Span) -> Self {
    Self {
      argv,
      span,
      cmd_span,
    }
  }
  pub fn span(&self) -> Span {
    // cloning spans is cheap
    self.span.clone()
  }
  pub fn cmd_span(&self) -> Span {
    self.cmd_span.clone()
  }

  /// The arg vector of the builtin, including *BOTH* options and arguments.
  ///
  /// If you want just the arguments, use `arguments()`. If you want just the options, use `options()`.
  pub fn argv(&self) -> &[Word] {
    &self.argv
  }

  /// Get an iterator over the arguments (non-option words) of the builtin.
  pub fn arguments(&self) -> impl Iterator<Item = (&VarStr, &Span)> {
    self.argv.iter().filter_map(|word| match word {
      Word::Arg(value, span) => Some((value, span)),
      _ => None,
    })
  }
  /// Get an iterator over the options of the builtin.
  pub fn options(&self) -> impl Iterator<Item = &Opt> {
    self.argv.iter().filter_map(|word| match word {
      Word::Opt(opt) => Some(opt),
      _ => None,
    })
  }
  /// Check if the builtin has an option with the given key.
  pub fn has_opt(&self, key: &str) -> bool {
    self.options().any(|o| o.key() == key)
  }
  /// Get the value of an option with the given key, if it exists.
  pub fn opt_value(&self, key: &str) -> Option<VarStr> {
    self
      .options()
      .find_map(|o| {
        let opt_key = o.key();
        (opt_key == key).then(|| o.value().ok()).flatten()
      })
      .map(VarStr::from)
  }
  pub fn no_arguments(&self) -> bool {
    self
      .argv
      .iter()
      .all(|word| !matches!(word, Word::Arg(_, _)))
  }
  pub fn no_options(&self) -> bool {
    self.argv.iter().all(|word| !matches!(word, Word::Opt(_)))
  }
  /// Take the arguments and options from the builtin, leaving `argv` empty.
  /// Splits `argv` into a vector of `(VarStr, Span)` for arguments and a vector of `Opt` for options.
  pub fn take_argv(&mut self) -> (Vec<(VarStr, Span)>, Vec<Opt>) {
    self
      .argv
      .drain(..)
      .filter(|word| !matches!(word, Word::Sep(_)))
      .partition_map(|word| match word {
        Word::Arg(var_str, span) => itertools::Either::Left((var_str, span)),
        Word::Opt(opt) => itertools::Either::Right(opt),
        Word::Sep(_) => unreachable!(),
      })
  }
}

/// Join all of the word-split arguments into a single string
/// Preserve the span too
pub fn join_raw_args(args: Vec<(VarStr, Span)>) -> (VarStr, Span) {
  join_raw_arg_iter(args.into_iter())
}

/// Join all of the word-split arguments into a single string
/// Preserve the span too
pub fn join_raw_arg_iter(args: impl Iterator<Item = (VarStr, Span)>) -> (VarStr, Span) {
  args.fold((VarStr::default(), Span::default()), |mut acc, arg| {
    if acc.1 == Span::default() {
      acc.1 = arg.1.clone();
    } else {
      let new_end = arg.1.end();
      let start = acc.1.start();
      acc.1.set_range(start..new_end);
    }

    if acc.0.is_empty() {
      acc.0 = arg.0;
    } else {
      acc.0 = varstr!("{} {}", acc.0, arg.0);
    }
    acc
  })
}
