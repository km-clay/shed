use std::sync::Arc;

use ariadne::Fmt;
use fmt::Display;

use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult, next_color},
  parse::lex::{Span, Tk},
  prelude::*,
  sherr,
  shopt::xtrace_print,
};

pub type OptSet = Arc<[Opt]>;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Opt {
  Long(String),
  LongWithArg(String, String),
  LongWithList(String, Vec<String>),
  Short(char),
  ShortWithArg(char, String),
  ShortWithList(char, Vec<String>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OptArg {
  None,
  Single,
  List,
}

pub struct OptSpec {
  pub opt: Opt,
  pub takes_arg: OptArg,
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
      Self::LongWithList(opt, args) => write!(f, "--{} {}", opt, args.join(" ")),
      Self::ShortWithList(opt, args) => write!(f, "-{} {}", opt, args.join(" ")),
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

type GetOptResult = ShResult<(Vec<(String, Span)>, Vec<Opt>)>;
pub fn get_opts_from_tokens_strict(tokens: Vec<Tk>, opt_specs: &[OptSpec]) -> GetOptResult {
  sort_tks(tokens, opt_specs, true)
}

pub fn get_opts_from_tokens(tokens: Vec<Tk>, opt_specs: &[OptSpec]) -> GetOptResult {
  sort_tks(tokens, opt_specs, false)
}

/// Variant that returns raw Tk values for callsites that need
/// pre-expansion token operations (e.g. split_tk_at).
pub fn get_opts_from_tokens_raw(
  tokens: Vec<Tk>,
  opt_specs: &[OptSpec],
) -> ShResult<(Vec<Tk>, Vec<Opt>)> {
  sort_tks_raw(tokens, opt_specs, false)
}

pub fn sort_tks(tokens: Vec<Tk>, opt_specs: &[OptSpec], strict: bool) -> GetOptResult {
  // Expand tokens and flatten via get_words, preserving spans
  let mut words: Vec<(String, Span)> = vec![];
  for tk in tokens {
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
      non_opts.push((word, span));
      non_opts.extend(words_iter);
      break;
    }
    let parsed_opts = Opt::parse(&word);

    if parsed_opts.is_empty() {
      non_opts.push((word, span))
    } else {
      for opt in parsed_opts {
        let mut pushed = false;
        for opt_spec in opt_specs {
          if opt_spec.opt == opt {
            match &opt_spec.takes_arg {
              OptArg::Single => {
                let arg = words_iter.next().map(|(w, _)| w).unwrap_or_default();
                let opt = match opt {
                  Opt::Long(ref opt) => Opt::LongWithArg(opt.to_string(), arg),
                  Opt::Short(opt) => Opt::ShortWithArg(opt, arg),
                  _ => unreachable!(),
                };
                opts.push(opt);
              }
              OptArg::List => {
                let mut args = vec![];
                while let Some((w, _)) = words_iter.peek() {
                  if w.starts_with('-') {
                    break;
                  }
                  args.push(words_iter.next().unwrap().0);
                }
                let opt = match opt {
                  Opt::Long(ref opt) => Opt::LongWithList(opt.to_string(), args),
                  Opt::Short(opt) => Opt::ShortWithList(opt, args),
                  _ => unreachable!(),
                };
                opts.push(opt);
              }
              OptArg::None => {
                opts.push(opt.clone());
              }
            }
            pushed = true;
          }
        }
        if !pushed {
          if strict {
            return Err(sherr!(
              ParseErr,
              "Unknown option: {}",
              opt.to_string().fg(next_color()),
            ));
          } else {
            non_opts.push((word.clone(), span.clone()));
          }
        }
      }
    }
  }
  Ok((non_opts, opts))
}

fn sort_tks_raw(
  tokens: Vec<Tk>,
  opt_specs: &[OptSpec],
  strict: bool,
) -> ShResult<(Vec<Tk>, Vec<Opt>)> {
  let mut tokens_iter = tokens
    .into_iter()
    .map(|t| t.expand())
    .collect::<ShResult<Vec<_>>>()?
    .into_iter()
    .peekable();
  let mut opts = vec![];
  let mut non_opts = vec![];

  while let Some(token) = tokens_iter.next() {
    if &token.to_string() == "--" {
      non_opts.push(token);
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
            match &opt_spec.takes_arg {
              OptArg::Single => {
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
              }
              OptArg::List => {
                let mut args = vec![];
                while let Some(t) = tokens_iter.peek() {
                  if t.to_string().starts_with('-') {
                    break;
                  }
                  args.push(tokens_iter.next().unwrap().to_string());
                }
                let opt = match opt {
                  Opt::Long(ref opt) => Opt::LongWithList(opt.to_string(), args),
                  Opt::Short(opt) => Opt::ShortWithList(opt, args),
                  _ => unreachable!(),
                };
                opts.push(opt);
              }
              OptArg::None => {
                opts.push(opt.clone());
              }
            }
            pushed = true;
          }
        }
        if !pushed {
          if strict {
            return Err(sherr!(
              ParseErr,
              "Unknown option: {}",
              opt.to_string().fg(next_color()),
            ));
          } else {
            non_opts.push(token.clone());
          }
        }
      }
    }
  }
  Ok((non_opts, opts))
}

#[cfg(test)]
mod tests {
  use crate::parse::lex::{LexFlags, LexStream};

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
  fn get_opts_basic() {
    let words = vec![
      "file.txt".into(),
      "-v".into(),
      "--help".into(),
      "arg".into(),
    ];
    let (non_opts, opts) = get_opts(words);
    assert_eq!(non_opts, vec!["file.txt", "arg"]);
    assert_eq!(opts, vec![Opt::Short('v'), Opt::Long("help".into())]);
  }

  #[test]
  fn get_opts_double_dash_stops_parsing() {
    let words = vec!["-a".into(), "--".into(), "-b".into(), "--foo".into()];
    let (non_opts, opts) = get_opts(words);
    assert_eq!(opts, vec![Opt::Short('a')]);
    assert_eq!(non_opts, vec!["-b", "--foo"]);
  }

  #[test]
  fn get_opts_combined_short() {
    let words = vec!["-abc".into(), "file".into()];
    let (non_opts, opts) = get_opts(words);
    assert_eq!(
      opts,
      vec![Opt::Short('a'), Opt::Short('b'), Opt::Short('c')]
    );
    assert_eq!(non_opts, vec!["file"]);
  }

  #[test]
  fn get_opts_no_flags() {
    let words = vec!["foo".into(), "bar".into()];
    let (non_opts, opts) = get_opts(words);
    assert!(opts.is_empty());
    assert_eq!(non_opts, vec!["foo", "bar"]);
  }

  #[test]
  fn get_opts_empty_input() {
    let (non_opts, opts) = get_opts(vec![]);
    assert!(opts.is_empty());
    assert!(non_opts.is_empty());
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
    LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
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

    let (non_opts, opts) = get_opts_from_tokens(tokens, &opt_spec).unwrap();

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

    let (non_opts, opts) = get_opts_from_tokens(tokens, &opt_spec).unwrap();

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

    let (non_opts, opts) = get_opts_from_tokens(tokens, &opt_spec).unwrap();

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

    let (non_opts, opts) = get_opts_from_tokens(tokens, &opt_spec).unwrap();

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

    let (_non_opts, opts) = get_opts_from_tokens(tokens, &opt_spec).unwrap();

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

    let (non_opts, opts) = get_opts_from_tokens(tokens, &opt_spec).unwrap();

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

    let (non_opts, opts) = get_opts_from_tokens(tokens, &opt_spec).unwrap();

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

  use crate::state::{self, VarFlags, VarKind, write_vars};
  use crate::testutil::{TestGuard, test_input};

  #[test]
  fn expanded_var_opts_echo() {
    let g = TestGuard::new();
    write_vars(|v| v.set_var("ECHO_ARGS", VarKind::Str("-p \\s".into()), VarFlags::NONE)).unwrap();
    test_input("echo $ECHO_ARGS").unwrap();
    let out = g.read_output();
    assert_eq!(out, "shed\n");
  }

  #[test]
  fn expanded_var_opts_read() {
    let g = TestGuard::new();
    write_vars(|v| v.set_var("READ_ARGS", VarKind::Str("-r line".into()), VarFlags::NONE)).unwrap();
    test_input("read $READ_ARGS <<< hello").unwrap();
    let line = state::read_vars(|v| v.get_var("line"));
    assert_eq!(line, "hello");
    drop(g);
  }

  #[test]
  fn expanded_var_multiple_opts() {
    let g = TestGuard::new();
    write_vars(|v| v.set_var("ARGS", VarKind::Str("-e -n".into()), VarFlags::NONE)).unwrap();
    test_input("echo $ARGS hello").unwrap();
    let out = g.read_output();
    // -e enables escapes, -n suppresses newline
    assert_eq!(out, "hello");
  }
}
