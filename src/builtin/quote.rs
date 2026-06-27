use itertools::Itertools;

use crate::{
  ShResult, Shed,
  builtin::getopt::OptSpec,
  expand, match_loop, out, outln, procio,
  state::vars::{VarFlags, VarKind},
  util::{expand_ansi_c, with_status},
};

use super::getopt::Opt;

pub(super) struct Quote;
impl super::Builtin for Quote {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::single_arg('v'), OptSpec::single_arg("var")]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    if let Some(stdin) = self.get_input_str_with(&mut args, |_| procio::has_in_sink()) {
      let quoted = expand::shell_quote(&stdin);
      outln!("{quoted}");
      return with_status(0);
    }

    let mut parts: Vec<String> = args
      .argv
      .iter()
      .map(|(s, _)| expand::shell_quote(s))
      .collect();

    for opt in &args.opts {
      match opt {
        Opt::ShortWithArg('v', var) => {
          if let Some(quoted) = quote_var(var) {
            parts.push(quoted);
          }
        }
        Opt::LongWithArg(flag, var) if flag == "var" => {
          if let Some(quoted) = quote_var(var) {
            parts.push(quoted);
          }
        }
        _ => {}
      }
    }

    outln!("{}", parts.join(" "));
    with_status(0)
  }
}

fn quote_var(name: &str) -> Option<String> {
  let var = Shed::vars(|v| v.try_get_var_meta(name))?;
  match var.kind() {
    VarKind::Str(var_str) => Some(expand::shell_quote(var_str.as_str())),
    VarKind::Int(int) => {
      let mut buf = itoa::Buffer::new();
      let int_str = buf.format(*int);
      Some(expand::shell_quote(int_str))
    }
    VarKind::Arr(var_strs) => Some(
      var_strs
        .iter()
        .map(|v| expand::shell_quote(v.as_str()))
        .join(" "),
    ),
    VarKind::AssocArr(items) => Some(
      items
        .iter()
        .map(|(k, v)| [expand::shell_quote(k), expand::shell_quote(v)].join(" "))
        .join("\n"),
    ),
    VarKind::Magic(magic_var) => {
      let resolved = magic_var()?;
      Some(expand::shell_quote(resolved.as_str()))
    }
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
    let input = self
      .get_input_str(&mut args)
      .unwrap_or_else(|| super::join_raw_args(args.argv).0);

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
          if delim == "\n" {
            outln!();
          }
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
  let mut fields = vec![];
  let mut field = String::new();
  let mut chars = s.chars().peekable();
  let mut started = false;

  match_loop!(chars.next() => ch, {
    '\\' => {
      started = true;
      if let Some(next) = chars.next() {
        field.push(next);
      }
    }
    '\'' => {
      started = true;
      match_loop!(chars.next() => ch, {
        '\'' => break,
        _ => field.push(ch),
      })
    }
    '$' if chars.peek() == Some(&'\'') => {
      started = true;
      chars.next();
      let mut raw = String::new();
      match_loop!(chars.next() => ch, {
        '\'' => break,
        '\\' => {
          raw.push('\\');
          if let Some(next) = chars.next() {
            raw.push(next);
          }
        }
        _ => raw.push(ch),
      });

      field.push_str(&expand_ansi_c(&raw));
    }
    _ if ch.is_whitespace() => {
      if started {
        fields.push(std::mem::take(&mut field));
        started = false;
      }
    }
    _ => {
      started = true;
      field.push(ch);
    }
  });

  if started {
    fields.push(field);
  }

  Ok(fields)
}
