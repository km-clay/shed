//! `shed`'s internal opt parsing functions
//!
//! not be be confused with the `getopts` builtin

use std::fmt;

use fmt::Display;

use super::{
  eval::lex::{Span, Tk},
  sherr,
  state::shopt::xtrace_print,
  util::ShResult,
};

pub(crate) trait AsOpt {
  fn as_opt(&self) -> Opt;
}

impl AsOpt for char {
  fn as_opt(&self) -> Opt {
    Opt::Short(*self)
  }
}

impl AsOpt for String {
  fn as_opt(&self) -> Opt {
    Opt::Long(self.clone())
  }
}

impl AsOpt for &str {
  fn as_opt(&self) -> Opt {
    Opt::Long(self.to_string())
  }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Opt {
  Long(String),
  LongWithArg(String, String),
  LongWithList(String, Vec<String>),
  Short(char),
  ShortWithArg(char, String),
  ShortWithList(char, Vec<String>),
}

impl Opt {
  pub fn parse(s: &str) -> Vec<Self> {
    let mut opts = vec![];

    if s.starts_with("--") {
      opts.push(Opt::Long(s.trim_start_matches('-').to_string()));
    } else if s.starts_with('-') {
      let mut chars = s.trim_start_matches('-').chars();
      while let Some(ch) = chars.next() {
        opts.push(Self::Short(ch));
      }
    }

    opts
  }
}

impl Display for Opt {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Long(opt) => write!(f, "--{opt}"),
      Self::Short(opt) => write!(f, "-{opt}"),
      Self::LongWithArg(opt, arg) => write!(f, "--{opt} {arg}"),
      Self::ShortWithArg(opt, arg) => write!(f, "-{opt} {arg}"),
      Self::LongWithList(opt, args) => write!(f, "--{} {}", opt, args.join(" ")),
      Self::ShortWithList(opt, args) => write!(f, "-{} {}", opt, args.join(" ")),
    }
  }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum OptArg {
  None,
  Single,
  Exact(usize),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct OptSpec {
  pub opt: Opt,
  pub takes_arg: OptArg,
}

#[expect(clippy::needless_pass_by_value)]
impl OptSpec {
  pub fn flag(opt: impl AsOpt) -> Self {
    Self {
      opt: opt.as_opt(),
      takes_arg: OptArg::None,
    }
  }
  pub fn single_arg(opt: impl AsOpt) -> Self {
    Self {
      opt: opt.as_opt(),
      takes_arg: OptArg::Single,
    }
  }
  pub fn exact_args(opt: impl AsOpt, n: usize) -> Self {
    Self {
      opt: opt.as_opt(),
      takes_arg: OptArg::Exact(n),
    }
  }
}

type GetOptResult = ShResult<(Vec<(String, Span)>, Vec<Opt>)>;
pub(crate) fn get_opts_from_tokens_strict(tokens: &[Tk], opt_specs: &[OptSpec]) -> GetOptResult {
  sort_tks(tokens, opt_specs, true)
}

pub(crate) fn get_opts_from_tokens(tokens: &[Tk], opt_specs: &[OptSpec]) -> GetOptResult {
  sort_tks(tokens, opt_specs, false)
}

/// Variant that returns raw Tk values for callsites that need
/// pre-expansion token operations (e.g. `split_tk_at`).
pub(crate) fn get_opts_from_tokens_raw(
  tokens: &[Tk],
  opt_specs: &[OptSpec],
) -> ShResult<(Vec<Tk>, Vec<Opt>)> {
  sort_tks_raw(tokens, opt_specs, false)
}

/// Like `get_opts_from_tokens_raw`, but pre-expands tokens without word
/// splitting. Use for builtins (declare/local/export/readonly) that need
/// to preserve whitespace in assignment RHS values.
pub(crate) fn get_opts_from_tokens_raw_no_split(
  tokens: &[Tk],
  opt_specs: &[OptSpec],
) -> ShResult<(Vec<Tk>, Vec<Opt>)> {
  sort_tks_raw_no_split(tokens, opt_specs, false)
}

pub(crate) fn sort_tks(tokens: &[Tk], opt_specs: &[OptSpec], strict: bool) -> GetOptResult {
  // Expand tokens and flatten via get_words, preserving spans
  let mut words: Vec<(String, Span)> = vec![];
  for tk in tokens {
    let tk = tk.clone();
    let span = tk.span.clone();
    let expanded = tk.expand()?;
    for word in expanded.get_words() {
      words.push((word, span.clone()));
    }
  }

  xtrace_print(&words);

  let mut words_iter = words.into_iter().peekable();
  let mut opts = vec![];
  let mut non_opts = vec![];

  while let Some((word, span)) = words_iter.next() {
    if word == "--" {
      let rest = words_iter.collect::<Vec<_>>();
      if rest.is_empty() {
        non_opts.push((word, span));
      } else {
        non_opts.extend(rest);
      }
      break;
    }
    let parsed_opts = Opt::parse(&word);

    if parsed_opts.is_empty() {
      non_opts.push((word, span));
      continue;
    }

    let all_recognized = parsed_opts
      .iter()
      .all(|o| opt_specs.iter().any(|s| s.opt == *o));

    if !all_recognized {
      if strict {
        let unknown = parsed_opts
          .iter()
          .find(|o| !opt_specs.iter().any(|s| s.opt == **o))
          .unwrap();
        return Err(sherr!(ParseErr, "Unknown option: '{unknown}'"));
      }
      non_opts.push((word, span));
      continue;
    }

    for opt in parsed_opts {
      for opt_spec in opt_specs {
        if opt_spec.opt == opt {
          match &opt_spec.takes_arg {
            OptArg::Single => {
              let arg = words_iter.next().map(|(w, _)| w).unwrap_or_default();
              let opt = match opt {
                Opt::Long(ref opt) => Opt::LongWithArg(opt.clone(), arg),
                Opt::Short(opt) => Opt::ShortWithArg(opt, arg),
                _ => unreachable!(),
              };
              opts.push(opt);
            }
            OptArg::Exact(n) => {
              let mut args = vec![];
              while let Some((w, _)) = words_iter.peek() {
                if w.starts_with('-') || args.len() >= *n {
                  break;
                }
                args.push(words_iter.next().unwrap().0);
              }
              if args.len() != *n {
                return Err(sherr!(
                  ParseErr,
                  "Option {} expects exactly {} arguments, but got {}",
                  opt.to_string(),
                  n,
                  args.len()
                ));
              }
              let opt = match opt {
                Opt::Long(ref opt) => Opt::LongWithList(opt.clone(), args),
                Opt::Short(opt) => Opt::ShortWithList(opt, args),
                _ => unreachable!(),
              };
              opts.push(opt);
            }
            OptArg::None => {
              opts.push(opt.clone());
            }
          }
          break;
        }
      }
    }
  }
  Ok((non_opts, opts))
}

fn sort_tks_raw(
  tokens: &[Tk],
  opt_specs: &[OptSpec],
  strict: bool,
) -> ShResult<(Vec<Tk>, Vec<Opt>)> {
  sort_tks_raw_inner(tokens, opt_specs, strict, true)
}

fn sort_tks_raw_no_split(
  tokens: &[Tk],
  opt_specs: &[OptSpec],
  strict: bool,
) -> ShResult<(Vec<Tk>, Vec<Opt>)> {
  sort_tks_raw_inner(tokens, opt_specs, strict, false)
}

fn sort_tks_raw_inner(
  tokens: &[Tk],
  opt_specs: &[OptSpec],
  strict: bool,
  split_words: bool,
) -> ShResult<(Vec<Tk>, Vec<Opt>)> {
  let mut tokens_iter = tokens
    .iter()
    .map(|tk| {
      if split_words {
        tk.expand()
      } else {
        tk.expand_unsplit()
      }
    })
    .collect::<ShResult<Vec<_>>>()?
    .into_iter()
    .peekable();
  let mut opts = vec![];
  let mut non_opts = vec![];

  while let Some(token) = tokens_iter.next() {
    if token.as_str() == "--" {
      let rest = tokens_iter.collect::<Vec<_>>();
      if rest.is_empty() {
        non_opts.push(token);
      } else {
        non_opts.extend(rest);
      }
      break;
    }
    let parsed_opts = Opt::parse(&token.to_string());

    if parsed_opts.is_empty() {
      non_opts.push(token);
      continue;
    }

    let all_recognized = parsed_opts
      .iter()
      .all(|o| opt_specs.iter().any(|s| s.opt == *o));

    if !all_recognized {
      if strict {
        let unknown = parsed_opts
          .iter()
          .find(|o| !opt_specs.iter().any(|s| s.opt == **o))
          .unwrap();
        return Err(sherr!(ParseErr, "Unknown option: {}", unknown.to_string(),));
      }
      non_opts.push(token);
      continue;
    }

    for opt in parsed_opts {
      for opt_spec in opt_specs {
        if opt_spec.opt == opt {
          match &opt_spec.takes_arg {
            OptArg::Single => {
              let arg = tokens_iter
                .next()
                .map(|t| t.to_string())
                .unwrap_or_default();
              let opt = match opt {
                Opt::Long(ref opt) => Opt::LongWithArg(opt.clone(), arg),
                Opt::Short(opt) => Opt::ShortWithArg(opt, arg),
                _ => unreachable!(),
              };
              opts.push(opt);
            }
            OptArg::Exact(n) => {
              let mut args = vec![];
              while let Some(tk) = tokens_iter.peek() {
                if tk.as_str().starts_with('-') || args.len() >= *n {
                  break;
                }
                args.push(tokens_iter.next().unwrap().to_string());
              }
              if args.len() != *n {
                return Err(sherr!(
                  ParseErr,
                  "Option {} expects exactly {} arguments, but got {}",
                  opt.to_string(),
                  n,
                  args.len()
                ));
              }
              let opt = match opt {
                Opt::Long(ref opt) => Opt::LongWithList(opt.clone(), args),
                Opt::Short(opt) => Opt::ShortWithList(opt, args),
                _ => unreachable!(),
              };
              opts.push(opt);
            }
            OptArg::None => {
              opts.push(opt.clone());
            }
          }
          break;
        }
      }
    }
  }
  Ok((non_opts, opts))
}

#[cfg(test)]
mod tests {
  use crate::eval::lex::{LexFlags, LexStream};
  use crate::var;

  use super::*;

  #[test]
  fn parse_short_single() {
    let opts = Opt::parse("-a");
    assert_eq!(opts, vec![Opt::Short('a')]);
  }

  #[test]
  fn parse_short_combined() {
    let opts = Opt::parse("-abc");
    assert_eq!(
      opts,
      vec![Opt::Short('a'), Opt::Short('b'), Opt::Short('c')]
    );
  }

  #[test]
  fn parse_long() {
    let opts = Opt::parse("--verbose");
    assert_eq!(opts, vec![Opt::Long("verbose".into())]);
  }

  #[test]
  fn parse_non_option() {
    let opts = Opt::parse("hello");
    assert!(opts.is_empty());
  }

  #[test]
  fn display_formatting() {
    assert_eq!(Opt::Short('v').to_string(), "-v");
    assert_eq!(Opt::Long("help".into()).to_string(), "--help");
    assert_eq!(Opt::ShortWithArg('o', "file".into()).to_string(), "-o file");
    assert_eq!(
      Opt::LongWithArg("output".into(), "file".into()).to_string(),
      "--output file"
    );
  }

  fn lex(input: &str) -> Vec<Tk> {
    LexStream::new(input.into(), LexFlags::empty())
      .collect::<ShResult<Vec<Tk>>>()
      .unwrap()
  }

  #[test]
  fn get_opts_from_tks() {
    let tokens = lex("file.txt --help -v arg");

    let opt_spec = vec![
      OptSpec {
        opt: Opt::Short('v'),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Long("help".into()),
        takes_arg: OptArg::None,
      },
    ];

    let (non_opts, opts) = get_opts_from_tokens(&tokens, &opt_spec).unwrap();

    let mut opts = opts.into_iter();
    assert!(opts.any(|o| o == Opt::Short('v') || o == Opt::Long("help".into())));
    assert!(opts.any(|o| o == Opt::Short('v') || o == Opt::Long("help".into())));

    let mut non_opts = non_opts.into_iter().map(|(s, _)| s);
    assert!(non_opts.any(|s| s == "file.txt" || s == "arg"));
    assert!(non_opts.any(|s| s == "file.txt" || s == "arg"));
  }

  #[test]
  fn tks_short_with_arg() {
    let tokens = lex("-o output.txt file.txt");

    let opt_spec = vec![OptSpec {
      opt: Opt::Short('o'),
      takes_arg: OptArg::Single,
    }];

    let (non_opts, opts) = get_opts_from_tokens(&tokens, &opt_spec).unwrap();

    assert_eq!(opts, vec![Opt::ShortWithArg('o', "output.txt".into())]);
    let non_opts: Vec<String> = non_opts.into_iter().map(|(s, _)| s).collect();
    assert!(non_opts.contains(&"file.txt".to_string()));
  }

  #[test]
  fn tks_long_with_arg() {
    let tokens = lex("--output result.txt input.txt");

    let opt_spec = vec![OptSpec {
      opt: Opt::Long("output".into()),
      takes_arg: OptArg::Single,
    }];

    let (non_opts, opts) = get_opts_from_tokens(&tokens, &opt_spec).unwrap();

    assert_eq!(
      opts,
      vec![Opt::LongWithArg("output".into(), "result.txt".into())]
    );
    let non_opts: Vec<String> = non_opts.into_iter().map(|(s, _)| s).collect();
    assert!(non_opts.contains(&"input.txt".to_string()));
  }

  #[test]
  fn tks_double_dash_stops() {
    let tokens = lex("-v -- -a --foo");

    let opt_spec = vec![
      OptSpec {
        opt: Opt::Short('v'),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('a'),
        takes_arg: OptArg::None,
      },
    ];

    let (non_opts, opts) = get_opts_from_tokens(&tokens, &opt_spec).unwrap();

    assert_eq!(opts, vec![Opt::Short('v')]);
    let non_opts: Vec<String> = non_opts.into_iter().map(|(s, _)| s).collect();
    assert!(non_opts.contains(&"-a".to_string()));
    assert!(non_opts.contains(&"--foo".to_string()));
  }

  #[test]
  fn tks_combined_short_with_spec() {
    let tokens = lex("-abc");

    let opt_spec = vec![
      OptSpec {
        opt: Opt::Short('a'),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('b'),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('c'),
        takes_arg: OptArg::None,
      },
    ];

    let (_non_opts, opts) = get_opts_from_tokens(&tokens, &opt_spec).unwrap();

    assert_eq!(
      opts,
      vec![Opt::Short('a'), Opt::Short('b'), Opt::Short('c')]
    );
  }

  #[test]
  fn tks_unknown_opt_becomes_non_opt() {
    let tokens = lex("-v -x file");

    let opt_spec = vec![OptSpec {
      opt: Opt::Short('v'),
      takes_arg: OptArg::None,
    }];

    let (non_opts, opts) = get_opts_from_tokens(&tokens, &opt_spec).unwrap();

    assert_eq!(opts, vec![Opt::Short('v')]);
    // -x is not in spec, so its token goes to non_opts
    assert!(
      non_opts
        .into_iter()
        .map(|(s, _)| s)
        .any(|s| s == "-x" || s == "file")
    );
  }

  #[test]
  fn tks_mixed_short_and_long_with_args() {
    let tokens = lex("-n 5 --output file.txt input");

    let opt_spec = vec![
      OptSpec {
        opt: Opt::Short('n'),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Long("output".into()),
        takes_arg: OptArg::Single,
      },
    ];

    let (non_opts, opts) = get_opts_from_tokens(&tokens, &opt_spec).unwrap();

    assert_eq!(
      opts,
      vec![
        Opt::ShortWithArg('n', "5".into()),
        Opt::LongWithArg("output".into(), "file.txt".into()),
      ]
    );
    let non_opts: Vec<String> = non_opts.into_iter().map(|(s, _)| s).collect();
    assert!(non_opts.contains(&"input".to_string()));
  }

  // ===================== Variable expansion through opts (TestGuard) =====================

  use crate::state::{Shed, vars::VarFlags, vars::VarKind};
  use crate::tests::testutil::{TestGuard, test_input};

  #[test]
  fn expanded_var_opts_echo() {
    let g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "ECHO_ARGS",
        VarKind::Str("-p \\s".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();
    test_input("echo $ECHO_ARGS").unwrap();
    let out = g.read_output();
    assert_eq!(out, "shed\n");
  }

  #[test]
  fn expanded_var_opts_read() {
    let g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "READ_ARGS",
        VarKind::Str("-r line".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();
    test_input("read $READ_ARGS <<< hello").unwrap();
    let line = var!("line");
    assert_eq!(line, "hello");
    drop(g);
  }

  #[test]
  fn expanded_var_multiple_opts() {
    let g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("ARGS", VarKind::Str("-e -n".into()), VarFlags::empty())).unwrap();
    test_input("echo $ARGS hello").unwrap();
    let out = g.read_output();
    // -e enables escapes, -n suppresses newline
    assert_eq!(out, "hello");
  }

  // ===================== sort_tks_raw =====================

  /// Lex without Soi/Eoi/Sep markers so `non_opts` holds only real args.
  fn lex_clean(input: &str) -> Vec<Tk> {
    use crate::eval::lex::TkRule;
    LexStream::new(input.into(), LexFlags::empty())
      .collect::<ShResult<Vec<Tk>>>()
      .unwrap()
      .into_iter()
      .filter(|t| {
        !matches!(
          t.class,
          TkRule::Soi | TkRule::Eoi | TkRule::Sep | TkRule::Null
        )
      })
      .collect()
  }

  fn raw_specs_v_flag_and_single() -> Vec<OptSpec> {
    vec![
      OptSpec {
        opt: Opt::Short('v'),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('o'),
        takes_arg: OptArg::Single,
      },
    ]
  }

  #[test]
  fn raw_parses_bare_flag() {
    let tokens = lex_clean("-v");
    let (non_opts, opts) =
      get_opts_from_tokens_raw(&tokens, &raw_specs_v_flag_and_single()).unwrap();
    assert!(non_opts.is_empty());
    assert_eq!(opts, vec![Opt::Short('v')]);
  }

  #[test]
  fn raw_parses_flag_with_single_arg() {
    let tokens = lex_clean("-o out.txt");
    let (non_opts, opts) =
      get_opts_from_tokens_raw(&tokens, &raw_specs_v_flag_and_single()).unwrap();
    assert!(non_opts.is_empty());
    assert_eq!(opts, vec![Opt::ShortWithArg('o', "out.txt".into())]);
  }

  #[test]
  fn raw_separates_double_dash_terminates_opts() {
    let tokens = lex_clean("-v -- -not-a-flag plain");
    let (non_opts, opts) =
      get_opts_from_tokens_raw(&tokens, &raw_specs_v_flag_and_single()).unwrap();
    assert_eq!(opts, vec![Opt::Short('v')]);
    let strs: Vec<String> = non_opts.iter().map(ToString::to_string).collect();
    assert_eq!(strs, vec!["-not-a-flag", "plain"]);
  }

  #[test]
  fn raw_bare_double_dash_at_end_kept_in_non_opts() {
    let tokens = lex_clean("-v --");
    let (non_opts, opts) =
      get_opts_from_tokens_raw(&tokens, &raw_specs_v_flag_and_single()).unwrap();
    assert_eq!(opts, vec![Opt::Short('v')]);
    assert_eq!(non_opts.len(), 1);
    assert_eq!(non_opts[0].to_string(), "--");
  }

  #[test]
  fn raw_unknown_opt_non_strict_goes_to_non_opts() {
    let tokens = lex_clean("-z");
    let (non_opts, opts) =
      get_opts_from_tokens_raw(&tokens, &raw_specs_v_flag_and_single()).unwrap();
    assert!(opts.is_empty());
    assert_eq!(non_opts.len(), 1);
  }

  #[test]
  fn raw_unknown_opt_strict_errors() {
    let tokens = lex_clean("-z");
    let strict_specs = raw_specs_v_flag_and_single();
    let result = super::sort_tks_raw(&tokens, &strict_specs, true);
    assert!(result.is_err());
  }

  #[test]
  fn raw_plain_arg_goes_to_non_opts() {
    let tokens = lex_clean("hello");
    let (non_opts, opts) =
      get_opts_from_tokens_raw(&tokens, &raw_specs_v_flag_and_single()).unwrap();
    assert!(opts.is_empty());
    assert_eq!(non_opts.len(), 1);
    assert_eq!(non_opts[0].to_string(), "hello");
  }

  #[test]
  fn raw_exact_arg_count_consumes_n_args() {
    let tokens = lex_clean("-s name buf cur extra");
    let specs = vec![OptSpec::exact_args('s', 3)];
    let (non_opts, opts) = get_opts_from_tokens_raw(&tokens, &specs).unwrap();
    match &opts[..] {
      [Opt::ShortWithList('s', args)] => {
        assert_eq!(args.len(), 3);
        assert_eq!(args[0], "name");
        assert_eq!(args[1], "buf");
        assert_eq!(args[2], "cur");
      }
      other => panic!("expected ShortWithList('s', 3 args), got {other:?}"),
    }
    assert_eq!(non_opts.len(), 1);
    assert_eq!(non_opts[0].to_string(), "extra");
  }

  #[test]
  fn raw_exact_arg_count_too_few_errors() {
    let tokens = lex_clean("-s only_one_arg");
    let specs = vec![OptSpec::exact_args('s', 3)];
    let result = get_opts_from_tokens_raw(&tokens, &specs);
    assert!(result.is_err());
  }

  // ===================== sort_tks (expansion-aware) =====================

  fn specs_v_and_o_single() -> Vec<OptSpec> {
    vec![
      OptSpec {
        opt: Opt::Short('v'),
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('o'),
        takes_arg: OptArg::Single,
      },
    ]
  }

  /// `sort_tks` expands variables before classifying; a single token
  /// holding `-v -o file` should split into three words via `get_words`.
  #[test]
  fn sort_tks_expands_var_holding_multiple_flags() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "GETOPT_ARGS",
        VarKind::Str("-v -o out.txt".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();
    let tokens = lex("$GETOPT_ARGS");
    let (non_opts, opts) = get_opts_from_tokens(&tokens, &specs_v_and_o_single()).unwrap();
    assert_eq!(
      opts,
      vec![Opt::Short('v'), Opt::ShortWithArg('o', "out.txt".into()),]
    );
    assert!(non_opts.is_empty());
  }

  /// Unrecognized options in strict mode must surface as `ShErr`,
  /// even when they're hidden inside an expansion.
  #[test]
  fn sort_tks_strict_rejects_unknown_from_expansion() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("BAD", VarKind::Str("-z".into()), VarFlags::empty())).unwrap();
    let tokens = lex("$BAD");
    let res = get_opts_from_tokens_strict(&tokens, &specs_v_and_o_single());
    assert!(res.is_err());
  }

  /// `--` arriving via expansion still terminates option parsing.
  #[test]
  fn sort_tks_double_dash_from_expansion_terminates_opts() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "SEP",
        VarKind::Str("-v -- -not-an-opt".into()),
        VarFlags::empty(),
      )
    })
    .unwrap();
    let tokens = lex("$SEP");
    let (non_opts, opts) = get_opts_from_tokens(&tokens, &specs_v_and_o_single()).unwrap();
    assert_eq!(opts, vec![Opt::Short('v')]);
    let strs: Vec<String> = non_opts.into_iter().map(|(s, _)| s).collect();
    assert_eq!(strs, vec!["-not-an-opt"]);
  }

  /// Empty bare `--` after the only flag is preserved as a `non_opt`
  /// (the `if rest.is_empty()` branch in `sort_tks`).
  #[test]
  fn sort_tks_bare_trailing_double_dash_kept_in_non_opts() {
    let tokens = lex("-v --");
    let (non_opts, opts) = get_opts_from_tokens(&tokens, &specs_v_and_o_single()).unwrap();
    assert_eq!(opts, vec![Opt::Short('v')]);
    let strs: Vec<String> = non_opts.into_iter().map(|(s, _)| s).collect();
    assert_eq!(strs, vec!["--"]);
  }

  /// Exact-arg option through `sort_tks` (this branch of the
  /// inner match — duplicated from raw — is still its own line).
  #[test]
  fn sort_tks_exact_arg_count_consumes_n_args() {
    let tokens = lex("-s a b c extra");
    let specs = vec![OptSpec::exact_args('s', 3)];
    let (non_opts, opts) = get_opts_from_tokens(&tokens, &specs).unwrap();
    match &opts[..] {
      [Opt::ShortWithList('s', args)] => {
        assert_eq!(
          args,
          &vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
      }
      other => panic!("expected ShortWithList('s'), got {other:?}"),
    }
    let strs: Vec<String> = non_opts.into_iter().map(|(s, _)| s).collect();
    assert_eq!(strs, vec!["extra"]);
  }

  /// Exact-arg with not enough following args → `ParseErr`.
  #[test]
  fn sort_tks_exact_arg_count_too_few_errors() {
    let tokens = lex("-s only");
    let specs = vec![OptSpec::exact_args('s', 3)];
    let res = get_opts_from_tokens(&tokens, &specs);
    assert!(res.is_err());
  }

  /// `OptArg::Single` with no following word — `.next()` returns
  /// None and the arg becomes empty (`unwrap_or_default`).
  #[test]
  fn sort_tks_single_arg_missing_uses_empty_string() {
    let tokens = lex("-o");
    let (_non_opts, opts) = get_opts_from_tokens(&tokens, &specs_v_and_o_single()).unwrap();
    assert_eq!(opts, vec![Opt::ShortWithArg('o', String::new())]);
  }
}
