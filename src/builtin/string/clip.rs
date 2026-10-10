use crate::{
  procio, sherr,
  state::vars::VarStr,
  two_way_display,
  util::{
    strops::VarStrDisplay,
    ui::{self, Justify},
  },
};

use super::{Builtin, BuiltinArgs, ShResult, argv, mod_opt::OptSpec, opt, util};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
  Bytes,
  Chars,
  Width,
}

two_way_display! {Unit,
  Bytes <=> "bytes";
  Chars <=> "chars";
  Width <=> "width";
}

struct ClipSpec {
  limit  : usize,
  unit   : Unit,
  marker : Option<VarStr>,
  justify: Justify,
}

impl ClipSpec {
  fn get_unit(args: &BuiltinArgs) -> ShResult<Unit> {
    let mut chosen = None;
    for opt in args.options() {
      let Ok(unit) = opt.key().parse::<Unit>() else {
        continue;
      };
      match chosen {
        Some(prev) if prev != unit => {
          return Err(
            sherr!(ParseErr @ opt.span(), "conflicting unit options: '{prev}' and '{unit}'"),
          );
        }
        _ => chosen = Some(unit),
      }
    }

    Ok(chosen.unwrap_or(Unit::Chars))
  }
  fn parse_args(args: &mut BuiltinArgs) -> ShResult<Self> {
    let mut arguments = args
      .arguments()
      .map(|(a, s)| (a.clone(), s))
      .collect::<Vec<_>>()
      .into_iter();

    let unit = Self::get_unit(args)?;

    let limit = arguments
      .next()
      .map(|(a, s)| {
        a.parse::<usize>()
          .map_err(|v| sherr!(ParseErr @ s, "invalid limit '{v}'"))
      })
      .transpose()?;

    let Some(limit) = limit else {
      return Err(sherr!(ExecFail @ args.cmd_span(), "missing limit argument"));
    };

    let marker = args.opt_value("marker");
    let justify = args
      .opt_value("justify")
      .map(|j| {
        j.parse::<Justify>().map_err(|v| {
          sherr!(ParseErr @ args.opt_span("justify").unwrap(), "invalid justify value '{v}'")
        })
      })
      .transpose()?
      .unwrap_or(Justify::Left);

    Ok(Self {
      limit,
      unit,
      marker,
      justify,
    })
  }
}

pub(super) struct Clip;
impl Builtin for Clip {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("justify" | b'j', 1),
      opt!("marker" | b'm', 1),
      opt!("bytes" | b'b'),
      opt!("chars" | b'c'),
      opt!("width" | b'w'),
    ]
  }

  fn execute(&self, mut args: BuiltinArgs) -> ShResult<()> {
    let ClipSpec {
      limit,
      unit,
      marker,
      justify,
    } = ClipSpec::parse_args(&mut args)?;

    let string = self
      .get_input_with(&mut args, |a| a.arguments().count() <= 1)
      .map_or_else(
        || argv::join_raw_arg_iter(args.arguments().skip(1)).0,
        VarStr::from,
      );

    let out = match unit {
      Unit::Bytes => clip_bytes(
        string.as_bytes(),
        limit,
        justify,
        marker.as_ref().map(VarStr::as_bytes),
      ),
      Unit::Chars => clip_chars(&string.to_str_lossy(), limit, justify, marker),
      Unit::Width => ui::truncate_with_marker(
        &string.to_str_lossy(),
        limit,
        &marker.unwrap_or_default().to_str_lossy(),
        justify,
      )
      .to_var_str(),
    };

    procio::out_bytes(&out);

    util::with_status(0)
  }
}

fn clip_chars(s: &str, limit: usize, justify: Justify, marker: Option<VarStr>) -> VarStr {
  let marker     = marker.unwrap_or_default();
  let marker     = marker.to_str_lossy();

  let s_len      = s.chars().count();
  let marker_len = marker.chars().count();

  if limit >= s_len {
    return VarStr::from(s);
  }

  if limit <= marker_len {
    // If the limit is less than or equal to the marker length,
    // we can't fit any of the original string, so we just return an empty string.
    return VarStr::default();
  }

  let keep = limit - marker_len;
  let res = match justify {
    Justify::Left => {
      let mut result = String::with_capacity(keep);

      result.extend(s.chars().take(keep));
      result.push_str(&marker);

      result
    }
    Justify::Center => {
      let     head   = keep / 2;
      let     tail   = keep - head;
      let mut result = String::with_capacity(keep);

      result.extend(s.chars().take(head));
      result.push_str(&marker);
      result.extend(s.chars().skip(s_len - tail));

      result
    }
    Justify::Right => {
      let mut result = String::with_capacity(keep);

      result.push_str(&marker);
      result.extend(s.chars().skip(s_len - keep));

      result
    }
  };

  VarStr::from(res)
}

fn clip_bytes(s: &[u8], limit: usize, justify: Justify, marker: Option<&[u8]>) -> VarStr {
  if limit >= s.len() {
    return VarStr::from(s);
  }

  let marker_len = marker.map_or(0, <[u8]>::len);

  if limit <= marker_len {
    // If the limit is less than or equal to the marker length,
    // we can't fit any of the original string, so we just return an empty string.
    return VarStr::default();
  }

  let res = match justify {
    Justify::Left => {
      let     with_marker = limit - marker_len;
      let mut result      = Vec::with_capacity(with_marker);

      result.extend_from_slice(&s[..with_marker]);
      if let Some(marker) = marker {
        result.extend_from_slice(marker);
      }

      result
    }
    Justify::Center => {
      let     with_marker = limit - marker_len;
      let     half        = with_marker / 2;
      let mut result      = Vec::with_capacity(with_marker);

      result.extend_from_slice(&s[..half]);
      if let Some(marker) = marker {
        result.extend_from_slice(marker);
      }
      result.extend_from_slice(&s[s.len() - (with_marker - half)..]);

      result
    }
    Justify::Right => {
      let     with_marker = limit - marker_len;
      let mut result      = Vec::with_capacity(with_marker);

      if let Some(marker) = marker {
        result.extend_from_slice(marker);
      }
      result.extend_from_slice(&s[s.len() - with_marker..]);

      result
    }
  };

  VarStr::from(res)
}
