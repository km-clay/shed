use bstr::ByteSlice;

use crate::{HashSet, out, procio, state::vars::VarStr, util::strops::ByteSet};

use super::{Builtin, BuiltinArgs, ShResult, argv, mod_opt::OptSpec, opt, util};

enum CharSet {
  Small(String),
  Big(HashSet<char>),
}

impl CharSet {
  fn new(s: String) -> Self {
    if s.chars().count() > 128 {
      let set = s.chars().collect::<HashSet<char>>();
      Self::Big(set)
    } else {
      Self::Small(s)
    }
  }

  fn contains(&self, c: char) -> bool {
    match self {
      CharSet::Small(s) => s.contains(c),
      CharSet::Big(set) => set.contains(&c),
    }
  }
}

pub(super) struct Trim;
impl Builtin for Trim {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("end"           | b'e'   ),
      opt!("start"         | b's'   ),
      opt!("end-matches"   | b'E', 1),
      opt!("start-matches" | b'S', 1),
      opt!("matches"       | b'm', 1),
    ]
  }
  fn execute(&self, mut args: BuiltinArgs) -> ShResult<()> {
    let string = self
      .get_input_str(&mut args)
      .unwrap_or_else(|| argv::join_raw_arg_iter(args.arguments()).0);

    // if neither `--start` nor `--end` is specified, default to trimming both ends
    let want_end = args.has_opt("end") || args.has_opt("end-matches") || args.has_opt("matches");
    let want_start =
      args.has_opt("start") || args.has_opt("start-matches") || args.has_opt("matches");

    let (start, end) = if want_start || want_end {
      (want_start, want_end)
    } else {
      (true, true)
    };

    let shared = args.opt_value("matches");
    let e_spec = args.opt_value("end-matches");
    let s_spec = args.opt_value("start-matches");

    let is_ascii = e_spec
      .iter()
      .chain(s_spec.iter())
      .chain(shared.iter())
      .all(|s| s.is_ascii());

    let join = |spec: Option<VarStr>| -> VarStr {
      match (spec, shared.as_ref()) {
        (None, None)         => VarStr::from(" \t\n\r"),
        (Some(spec), None)   => spec,
        (None, Some(shared)) => shared.clone(),
        (Some(mut spec), Some(shared)) => {
          spec.push_slice(shared.as_bytes());
          spec
        }
      }
    };

    let end_matches   = end.then(|| join(e_spec));
    let start_matches = start.then(|| join(s_spec));

    if is_ascii {
      Self::trim_raw(&string, end_matches, start_matches)
    } else {
      Self::trim(&string, end_matches, start_matches)
    }
  }
}

impl Trim {
  fn trim(
    string: &VarStr,
    end_matches: Option<VarStr>,
    start_matches: Option<VarStr>,
  ) -> ShResult<()> {
    let     input         = string.to_string();
    let     end_matches   = end_matches.map(|s| s.to_string());
    let     start_matches = start_matches.map(|s| s.to_string());

    let mut out           = input.as_str();
    if let Some(end) = end_matches {
      let set = CharSet::new(end);

      out = out.trim_end_matches(|c| set.contains(c));
    }
    if let Some(start) = start_matches {
      let set = CharSet::new(start);

      out = out.trim_start_matches(|c| set.contains(c));
    }

    out!("{out}");

    util::with_status(0)
  }
  fn trim_raw(
    string: &VarStr,
    end_matches: Option<VarStr>,
    start_matches: Option<VarStr>,
  ) -> ShResult<()> {
    let mut out = string.as_bytes();

    if let Some(end) = end_matches {
      let set = ByteSet::new(end.as_bytes()); // all possible byte values

      let end = out
        .bytes()
        .rposition(|b| !set.contains(b)) // find values we have not seen
        .map_or(0, |i| i + 1);

      out = &out[..end];
    }
    if let Some(start) = start_matches {
      let set = ByteSet::new(start.as_bytes()); // all possible byte values

      let start = out
        .bytes()
        .position(|b| !set.contains(b)) // find values we have not seen
        .unwrap_or(out.len());

      out = &out[start..];
    }

    procio::out_bytes(out);

    util::with_status(0)
  }
}
