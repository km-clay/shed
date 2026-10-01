use std::rc::Rc;

use bstr::ByteSlice;
use regex::Regex;

use crate::{builtin::opt::OptSpec, opt, out, sherr, state::Shed, util};

use super::super::{Builtin, BuiltinArgs, ShResult, argv};

enum Target {
  Regex(Rc<Regex>),
  Substring(String),
}

impl Target {
  fn matches(&self, name: &str) -> bool {
    match self {
      Target::Regex(re) => re.is_match(name),
      Target::Substring(sub) => name.to_ascii_lowercase().contains(sub),
    }
  }
}

pub(super) struct Timezone;
impl Builtin for Timezone {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("regex" | b'E')]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let (arg, arg_span) = argv::join_raw_arg_iter(args.arguments());
    let use_re = args.has_opt("regex");

    let raw = arg.trim().to_str_lossy().to_string();

    let target = if raw.is_empty() {
      None
    } else if use_re {
      let re = Shed::meta_mut(|m| m.get_regex(&raw))
        .map_err(|_| sherr!(ParseErr @ arg_span, "invalid regex pattern: {raw}").with_code(2))?;
      Some(Target::Regex(re))
    } else {
      Some(Target::Substring(raw.to_ascii_lowercase()))
    };

    let mut out = String::new();
    for tz in chrono_tz::TZ_VARIANTS {
      let name = tz.name();

      if let Some(target) = target.as_ref()
        && !target.matches(name)
      {
        continue;
      }

      out.push_str(name);
      out.push('\n');
    }

    if out.is_empty() {
      return util::with_status(1);
    }

    out!("{out}");

    util::with_status(0)
  }
}
