use itertools::Itertools;

use crate::{
  ShResult, Shed,
  builtin::getopt::OptSpec,
  eval::lex::{LexFlags, LexStream},
  expand, out, outln, procio,
  state::vars::{VarFlags, VarKind},
  util::with_status,
};

use super::getopt::Opt;

pub(super) struct Quote;
impl super::Builtin for Quote {
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    if let Some(stdin) = args.take_stdin() {
      let quoted = expand::shell_quote(&stdin);
      outln!("{quoted}");
      return with_status(0);
    }

    let parts: Vec<String> = args
      .argv
      .iter()
      .map(|(s, _)| expand::shell_quote(s))
      .collect();
    outln!("{}", parts.join(" "));
    with_status(0)
  }
}

enum UnquoteTarget {
  Array(String),
  Var(String),
}

pub(super) struct Unquote;
impl super::Builtin for Unquote {
  fn opts(&self) -> Vec<super::getopt::OptSpec> {
    vec![
      OptSpec::single_arg('a'),
      OptSpec::single_arg("array"),
      OptSpec::single_arg('v'),
      OptSpec::single_arg("var"),
      OptSpec::single_arg('s'),
      OptSpec::single_arg("sep"),
      OptSpec::flag('0'),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    log::debug!("entered unquote execute()");
    let input = if args.argv.is_empty() || args.has_stdin() {
      if args.has_stdin() {
        args.take_stdin().unwrap()
      } else {
        procio::read_input()?
      }
    } else {
      super::join_raw_args(args.argv).0
    };
    let mut target = None;
    let mut delim = "\n";

    for opt in &args.opts {
      match opt {
        Opt::LongWithArg(flag, arg) => match flag.as_str() {
          "array" => target = Some(UnquoteTarget::Array(arg.clone())),
          "var" => target = Some(UnquoteTarget::Var(arg.clone())),
          "sep" => delim = arg,
          _ => {}
        },
        Opt::Short('0') => delim = "\0",
        Opt::ShortWithArg('s', arg) => delim = arg,
        Opt::ShortWithArg('a', arg) => target = Some(UnquoteTarget::Array(arg.clone())),
        Opt::ShortWithArg('v', arg) => target = Some(UnquoteTarget::Var(arg.clone())),
        _ => {}
      }
    }

    let mut fields = unquote_raw(&input)?.into_iter();

    match target {
      None => {
        if let Some(first) = fields.next() {
          out!("{first}");
          for fields in fields {
            out!("{delim}{fields}");
          }
          outln!();
        }
      }
      Some(UnquoteTarget::Array(name)) => {
        let var = VarKind::arr(fields);
        Shed::vars_mut(|v| v.set_var(&name, var, VarFlags::empty()))?;
      }
      Some(UnquoteTarget::Var(name)) => {
        let var = VarKind::string(fields.join(" "));
        Shed::vars_mut(|v| v.set_var(&name, var, VarFlags::empty()))?;
      }
    }

    with_status(0)
  }
}

pub(crate) fn unquote_raw(s: &str) -> ShResult<Vec<String>> {
  let tokens = LexStream::new(s.into(), LexFlags::empty());
  let mut fields = vec![];

  for tk in tokens {
    let tk = tk?;

    fields.extend(tk.expand_to_words()?);
  }

  Ok(fields)
}
