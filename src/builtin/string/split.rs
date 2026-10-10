use crate::{
  errln,
  eval::lex::Span,
  expand::escape,
  procio, sherr,
  state::{
    Shed, params,
    vars::{VarFlags, VarKind, VarStr},
  },
  util::{
    error::ShResultExt,
    strops::{self, ByteSet, QuotePolicy},
  },
};

use super::{Builtin, BuiltinArgs, ShResult, argv, mod_opt::OptSpec, opt, util};

enum SplitPolicy {
  Ifs(VarStr),
  Delimiter(VarStr),
  Set(Box<ByteSet>),
}

impl Default for SplitPolicy {
  fn default() -> Self {
    SplitPolicy::Ifs(params::get_separators())
  }
}

impl SplitPolicy {
  fn from_args(args: &BuiltinArgs) -> ShResult<Self> {
    let mut policy: Option<(SplitPolicy, Span)> = None;
    for opt in args.options() {
      if matches!(opt.key(), "delim" | "any" | "0in")
        && let Some((_, policy)) = policy
      {
        let span = opt.span();
        let prev = policy.slice();
        let this = span.slice();
        return Err(sherr!(ParseErr @ span, "conflicting input options: '{prev}' and '{this}'"));
      }
      match opt.key() {
        "delim" => {
          let value = opt.value()?;
          policy = Some((SplitPolicy::Delimiter(value.clone()), opt.span()));
        }
        "any" => {
          let value = opt.value()?;
          let set   = ByteSet::new(value.as_bytes());
          policy = Some((SplitPolicy::Set(Box::new(set)), opt.span()));
        }
        "0in" => {
          policy = Some((SplitPolicy::Set(Box::new(ByteSet::new(b"\0"))), opt.span()));
        }
        _ => (),
      }
    }

    Ok(policy.map_or(SplitPolicy::default(), |(policy, _)| policy))
  }
}

enum SplitKind {
  Separator(VarStr),
  Array(VarStr),
  Quoted,
  NullSep,
}

impl Default for SplitKind {
  fn default() -> Self {
    SplitKind::Separator(VarStr::from("\n"))
  }
}

impl SplitKind {
  fn from_args(args: &BuiltinArgs) -> ShResult<Self> {
    let mut kind: Option<(SplitKind, Span)> = None;
    for opt in args.options() {
      if matches!(opt.key(), "array" | "sep" | "quoted" | "0out")
        && let Some((_, kind)) = kind
      {
        let span = opt.span();
        let prev = kind.slice();
        let this = span.slice();
        return Err(sherr!(ParseErr @ span, "conflicting output options: '{prev}' and '{this}'"));
      }
      match opt.key() {
        "sep" => {
          let value = opt.value()?;
          kind = Some((SplitKind::Separator(value.clone()), opt.span()));
        }
        "0out" => {
          kind = Some((SplitKind::NullSep, opt.span()));
        }
        "array" => {
          let value = opt.value()?;
          kind = Some((SplitKind::Array(value.clone()), opt.span()));
        }
        "quoted" => kind = Some((SplitKind::Quoted, opt.span())),
        _        => (),
      }
    }

    Ok(kind.map_or(SplitKind::default(), |(kind, _)| kind))
  }
}

pub(super) struct Split;
impl Builtin for Split {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("any",               1),
      opt!("array"      | b'a', 1),
      opt!("sep"        | b's', 1),
      opt!("delim"      | b'd', 1),
      opt!("escaped"    | b'E'   ),
      opt!("quoted"     | b'q'   ),
      opt!("terminated" | b't'   ),
      opt!("0in"                 ),
      opt!("0out"                ),
    ]
  }
  fn execute(&self, mut args: BuiltinArgs) -> ShResult<()> {
    let s_kind      = SplitKind::from_args(&args)?;
    let s_policy    = SplitPolicy::from_args(&args)?;
    let use_escaped = args.has_opt("escaped");
    let terminated  = args.has_opt("terminated") || args.has_opt("0in");

    let q_policy = if use_escaped {
      QuotePolicy::ESCAPE
    } else {
      QuotePolicy::LITERAL
    };

    if matches!(s_policy, SplitPolicy::Ifs(_)) && use_escaped {
      errln!("str split: warning: IFS splitting does not honor escapes; ignoring -E");
    }

    let input = self
      .get_input(&mut args)
      .map_or_else(|| argv::join_raw_arg_iter(args.arguments()).0, VarStr::from);

    let mut parts: Vec<VarStr> = match s_policy {
      SplitPolicy::Delimiter(pat) => strops::split_all_with(
        &input,
        |s| strops::split_at_pat_with(s, &pat, q_policy),
        |start, end| VarStr::from(&input[start..end]),
      ),
      SplitPolicy::Set(set) => strops::split_all_with(
        &input,
        |s| strops::split_at_byteset_with(s, &set, q_policy),
        |start, end| VarStr::from(&input[start..end]),
      ),

      SplitPolicy::Ifs(ifs) => strops::ifs_split(&input, &ifs, None)
        .into_iter()
        .map(VarStr::from)
        .collect(),
    };

    if terminated && parts.last().is_some_and(|v| v.is_empty()) {
      parts.pop();
    }

    match s_kind {
      SplitKind::Separator(sep) => {
        let mut joined = vec![];

        for (i, part) in parts.iter().enumerate() {
          if i > 0 {
            joined.extend_from_slice(sep.as_bytes());
          }
          joined.extend_from_slice(part.as_bytes());
        }
        joined.push(b'\n');

        procio::out_bytes(&joined);
      }
      SplitKind::Array(arr_name) => {
        let arr = parts.into_iter();

        Shed::vars_mut(|v| {
          v.set_var(
            &arr_name.to_str_lossy(),
            VarKind::arr(arr),
            VarFlags::empty(),
          )
        })
        .promote_err(args.cmd_span())?;
      }
      SplitKind::Quoted => {
        let mut quoted = vec![];

        for (i, part) in parts.iter().enumerate() {
          if i > 0 {
            quoted.push(b' ');
          }
          quoted.extend_from_slice(&escape::shell_quote_bytes(part.as_bytes()));
        }
        quoted.push(b'\n');

        procio::out_bytes(&quoted);
      }

      SplitKind::NullSep => {
        let mut joined = vec![];

        for part in &parts {
          joined.extend_from_slice(part.as_bytes());
          joined.push(b'\0');
        }

        procio::out_bytes(&joined);
      }
    }

    util::with_status(0)
  }
}
