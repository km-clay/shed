use std::{fmt::Display, str::FromStr};

use unicode_width::UnicodeWidthStr;

use crate::{
  libsh::{
    error::{ShErr, ShErrKind, ShResult, ShResultExt},
    utils::VecDequeExt,
  },
  parse::{NdRule, Node, execute::prepare_argv},
  prelude::*,
  procio::borrow_fd,
  shopt::as_var_val_display,
  state::{self, VarKind, read_shopts, read_vars, write_shopts, write_vars},
  two_way_display,
};

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct SetFlags: u32 {
    const ALLEXPORT = 1 << 0;
    const ERREXIT = 1 << 1;
    const IGNORE_EOF = 1 << 2;
    const MONITOR = 1 << 3;
    const NO_CLOBBER = 1 << 4;
    const NO_GLOB = 1 << 5;
    const NO_EXEC = 1 << 6;
    const NO_LOG = 1 << 7;
    const NOTIFY = 1 << 8;
    const NO_UNSET = 1 << 9;
    const VERBOSE = 1 << 10;
    const VI_MODE = 1 << 11;
    const XTRACE = 1 << 12;
    const HASHALL = 1 << 13;
    const EMACS_MODE = 1 << 14;
  }
}

impl SetFlags {
  pub fn get_shopt_fields(&self) -> Vec<String> {
    let mut fields = vec![];
    for flag in *self {
      let opt = match flag {
        _ if flag == SetFlags::ERREXIT => "errexit",
        _ if flag == SetFlags::ALLEXPORT => "allexport",
        _ if flag == SetFlags::IGNORE_EOF => "ignoreeof",
        _ if flag == SetFlags::MONITOR => "monitor",
        _ if flag == SetFlags::NO_CLOBBER => "noclobber",
        _ if flag == SetFlags::NO_GLOB => "noglob",
        _ if flag == SetFlags::NO_EXEC => "noexec",
        _ if flag == SetFlags::NO_LOG => "nolog",
        _ if flag == SetFlags::NOTIFY => "notify",
        _ if flag == SetFlags::NO_UNSET => "nounset",
        _ if flag == SetFlags::VERBOSE => "verbose",
        _ if flag == SetFlags::VI_MODE => "vi",
        _ if flag == SetFlags::EMACS_MODE => "emacs",
        _ if flag == SetFlags::XTRACE => "xtrace",
        _ if flag == SetFlags::HASHALL => "hashall",
        _ => continue,
      };
      fields.push(opt.to_string());
    }
    fields
  }

  pub fn as_char(&self) -> Option<char> {
    match *self {
      _ if *self == Self::ALLEXPORT => Some('a'),
      _ if *self == Self::NOTIFY => Some('b'),
      _ if *self == Self::NO_CLOBBER => Some('C'),
      _ if *self == Self::ERREXIT => Some('e'),
      _ if *self == Self::NO_GLOB => Some('f'),
      _ if *self == Self::HASHALL => Some('h'),
      _ if *self == Self::MONITOR => Some('m'),
      _ if *self == Self::NO_EXEC => Some('n'),
      _ if *self == Self::NO_UNSET => Some('u'),
      _ if *self == Self::VERBOSE => Some('v'),
      _ if *self == Self::XTRACE => Some('x'),
      _ => None,
    }
  }
  pub fn as_str(&self) -> Option<&'static str> {
    match *self {
      _ if *self == Self::ALLEXPORT => Some("allexport"),
      _ if *self == Self::NOTIFY => Some("notify"),
      _ if *self == Self::NO_CLOBBER => Some("noclobber"),
      _ if *self == Self::ERREXIT => Some("errexit"),
      _ if *self == Self::NO_GLOB => Some("noglob"),
      _ if *self == Self::HASHALL => Some("hashall"),
      _ if *self == Self::MONITOR => Some("monitor"),
      _ if *self == Self::NO_EXEC => Some("noexec"),
      _ if *self == Self::NO_UNSET => Some("nounset"),
      _ if *self == Self::VERBOSE => Some("verbose"),
      _ if *self == Self::VI_MODE => Some("vi"),
      _ if *self == Self::EMACS_MODE => Some("emacs"),
      _ if *self == Self::XTRACE => Some("xtrace"),
      _ => None,
    }
  }
}

impl TryFrom<char> for SetFlags {
  type Error = ShErr;
  fn try_from(value: char) -> Result<Self, Self::Error> {
    // set flags:
    // -abCefhmnuvx
    match value {
      'a' => Ok(Self::ALLEXPORT),
      'b' => Ok(Self::NOTIFY),
      'C' => Ok(Self::NO_CLOBBER),
      'e' => Ok(Self::ERREXIT),
      'f' => Ok(Self::NO_GLOB),
      'h' => Ok(Self::HASHALL),
      'm' => Ok(Self::MONITOR),
      'n' => Ok(Self::NO_EXEC),
      'u' => Ok(Self::NO_UNSET),
      'v' => Ok(Self::VERBOSE),
      'x' => Ok(Self::XTRACE),
      _ => Err(ShErr::simple(
        ShErrKind::ParseErr,
        format!("invalid option: {}", value),
      )),
    }
  }
}

impl FromStr for SetFlags {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "ignoreeof" => Ok(Self::IGNORE_EOF),
      "vi" => Ok(Self::VI_MODE),
      "emacs" => Ok(Self::EMACS_MODE),
      "allexport" => Ok(Self::ALLEXPORT),
      "notify" => Ok(Self::NOTIFY),
      "noclobber" => Ok(Self::NO_CLOBBER),
      "errexit" => Ok(Self::ERREXIT),
      "noglob" => Ok(Self::NO_GLOB),
      "hashall" => Ok(Self::HASHALL),
      "monitor" => Ok(Self::MONITOR),
      "noexec" => Ok(Self::NO_EXEC),
      "nounset" => Ok(Self::NO_UNSET),
      "nolog" => Ok(Self::NO_LOG),
      "verbose" => Ok(Self::VERBOSE),
      "xtrace" => Ok(Self::XTRACE),
      _ => Err(ShErr::simple(
        ShErrKind::ParseErr,
        format!("invalid option: {}", s),
      )),
    }
  }
}

pub fn build_set_call(readable: bool) -> String {
  // I hope you like iterators :)

  let opts = write_shopts(|o| o.query("set").unwrap().unwrap());
  if !readable {
    let mut call = String::from("set ");

    let lines: Vec<_> = opts
      .lines()
      .map(|l| l.strip_prefix("set.").unwrap().split_once('=').unwrap())
      .collect();

    let on: Vec<&str> = lines
      .iter()
      .filter(|(_, r)| *r == "true")
      .map(|(l, _)| *l)
      .collect();

    let on_chars: String = on
      .iter()
      .filter_map(|opt| SetFlags::from_str(opt).unwrap().as_char())
      .collect();

    let on_strs: String = on
      .into_iter()
      .filter(|opt| SetFlags::from_str(opt).unwrap().as_char().is_none())
      .map(|o| format!("-o {o}"))
      .collect::<Vec<_>>()
      .join(" ");

    let off: Vec<_> = lines
      .iter()
      .filter(|(_, r)| *r != "true")
      .map(|(l, _)| *l)
      .collect();

    let off_chars: String = off
      .iter()
      .filter_map(|opt| SetFlags::from_str(opt).unwrap().as_char())
      .collect();

    let off_strs: String = off
      .into_iter()
      .filter(|opt| SetFlags::from_str(opt).unwrap().as_char().is_none())
      .map(|o| format!("+o {o}"))
      .collect::<Vec<_>>()
      .join(" ");

    let pos_args = read_vars(|v| {
      v.sh_argv()
        .clone()
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ")
    });

    if !on_chars.is_empty() {
      call.push_str(&format!("-{on_chars} "));
    }
    if !off_chars.is_empty() {
      call.push_str(&format!("+{off_chars} "));
    }
    if !on_strs.is_empty() {
      call.push_str(&format!("{on_strs} "));
    }
    if !off_strs.is_empty() {
      call.push_str(&format!("{off_strs} "));
    }
    if !pos_args.is_empty() {
      call.push_str(&format!("-- {pos_args}"));
    }

    call.trim_end().to_string()
  } else {
    let mut longest_width: usize = 0;
    let lines = opts
      .lines()
      .map(|l| {
        l.split_once('=')
          .map(|(l, r)| {
            (
              l.strip_prefix("set.").unwrap().to_string(),
              if r.parse::<bool>().unwrap() {
                "on".to_string()
              } else {
                "off".to_string()
              },
            )
          })
          .unwrap()
      })
      .collect::<Vec<_>>();

    for (opt, _) in lines.iter() {
      if opt.width() > longest_width {
        longest_width = opt.width();
      }
    }

    lines
      .into_iter()
      .map(|(l, r)| format!("{l:<width$} {r}", width = longest_width))
      .collect::<Vec<_>>()
      .join("\n")
  }
}

pub fn set_builtin(node: Node) -> ShResult<()> {
  let nd_span = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }

  if argv.is_empty() {
    // print values of all variables
    let out = borrow_fd(STDOUT_FILENO);
    let all_vars = read_vars(|v| v.all_vars());
    for (k, v) in all_vars {
      match v.kind() {
        VarKind::Arr(items) => {
          let items = items
            .clone()
            .into_iter()
            .map(|v| as_var_val_display(&v.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
          write(out, format!("{k}=( {items} )\n").as_bytes())?;
        }
        _ => {
          let v = as_var_val_display(&v.to_string());
          write(out, format!("{k}={v}\n").as_bytes())?;
        }
      }
    }
  }

  let mut args = argv.into_iter().peekable();
  let mut clear_if_empty = false;

  let mut pos_args = vec![];

  'outer: while let Some((arg, span)) = args.next() {
    let mut flags = SetFlags::empty();
    let mut chars = arg.chars().peekable();

    match chars.peek() {
      Some(polarity @ ('+' | '-')) => {
        let mut chars = arg[1..].chars().peekable();
        let should_set = *polarity == '-';
        match chars.next() {
          Some('-') => {
            clear_if_empty = true;
            break 'outer;
          }
          Some('o') => {
            let mut found = false;
            while let Some((arg, _)) = args.peek() {
              found = true;
              if arg.starts_with('-') || arg.starts_with('+') {
                break;
              }
              let (arg, span) = args.next().unwrap();
              match SetFlags::from_str(&arg) {
                Ok(f) => flags |= f,
                Err(e) => return Err(e).promote_err(span),
              }
            }
            if !found {
              let output = build_set_call(should_set);
              let out = borrow_fd(STDOUT_FILENO);
              write(out, format!("{output}\n").as_bytes())?;
            }
          }
          Some(c) => {
            match SetFlags::try_from(c) {
              Ok(f) => flags |= f,
              Err(e) => return Err(e).promote_err(span),
            }
            while let Some(ch) = chars.next() {
              match SetFlags::try_from(ch) {
                Ok(f) => flags |= f,
                Err(e) => return Err(e).promote_err(span),
              }
            }
          }
          None => {
            if should_set && flags.is_empty() {
              // the arg is a single literal '-'
              // the behavior for this is actually unspecified by POSIX
              // so let's just make it reset all of the set opts to their defaults
              write_shopts(|o| o.set = Default::default());
              continue;
            }
          }
        }
        for opt in flags.get_shopt_fields() {
          let opt_val = if should_set { "true" } else { "false" };
          if &opt == "emacs" {
            let opt_val = if should_set { "false" } else { "true" };
            write_shopts(|o| o.query(&format!("set.vi={opt_val}"))).promote_err(nd_span.clone())?;
            continue;
          }

          write_shopts(|o| o.query(&format!("set.{opt}={opt_val}")))
            .promote_err(nd_span.clone())?;
        }
      }
      Some(_) => pos_args.push(arg),
      None => {}
    }
  }

  while let Some((arg, _)) = args.next() {
    pos_args.push(arg);
  }

  if !pos_args.is_empty() || clear_if_empty {
    write_vars(|v| {
      let cur_scope = v.cur_scope_mut();
      cur_scope.clear_args();
      for arg in pos_args {
        cur_scope.bpush_arg(arg);
      }
    })
  }

  state::set_status(0);
  Ok(())
}
