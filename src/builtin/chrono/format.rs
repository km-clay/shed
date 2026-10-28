use chrono::{Local, TimeDelta, Utc};

use crate::{
  builtin::opt::OptSpec,
  opt, procio, sherr,
  state::vars::ShStr,
  util::{self, error::ShResultExt, strops},
};

use super::{
  super::{Builtin, BuiltinArgs, ShResult, argv},
  DurFmt, Zone,
};

pub(super) struct Format;
impl Builtin for Format {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("timezone" | b'z', 1),
      opt!("format" | b'f', 1),
      opt!("duration" | b'd'),
      opt!("utc" | b'u'),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let arguments      = args.arguments();
    let parse_duration = args.has_opt("duration");
    let utc            = args.has_opt("utc");

    if parse_duration && args.has_opt("timezone") {
      let dur_flag = args.opt_span("duration").unwrap().slice();
      let tz_flag  = args.opt_span("timezone").unwrap().slice();
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "cannot use {dur_flag} and {tz_flag} together")
          .with_code(2),
      );
    }

    // check timezone option, then TZ var, then default to local or UTC based on `-u`
    let tz = Zone::parse(args.opt_value("timezone"), utc)?;

    let fmt_string = args
      .opt_value("format")
      .unwrap_or_else(|| match parse_duration {
        true  => ShStr::from("%[%y years %]%[%O months %]%[%D days, %]%[%H:%]%M:%S.%03L"),
        false => ShStr::from("%a %b %e %I:%M:%S %p %Z %Y"),
      });

    // Operands are joined, so a multi-word expression needs no quoting. A
    // span reads as `A to B`, which `parse_dur` splits.
    let time = argv::join_raw_arg_iter(arguments);
    if time.is_empty() {
      return Err(sherr!(ExecFail @ args.cmd_span(), "missing time argument").with_code(2));
    }

    let formatted = if parse_duration {
      let text = time.to_str_lossy();

      let mut elapsed = match strops::TimeReader::parse_dur(&text) {
        Ok(micros) => TimeDelta::microseconds(micros),
        Err(dur_err) => {
          let Ok(instant) = strops::TimeReader::interpret(&text) else {
            return Err(dur_err).option_promote(time.span()).with_code(2);
          };
          instant - Utc::now()
        }
      };

      let mut buf = vec![];

      strops::StrFormatter::parse(&DurFmt { running: None }, &fmt_string)
        .and_then(|f| f.render(&mut elapsed, &mut buf))
        .option_promote(args.opt_span("format"))
        .option_promote(time.span())
        .with_code(1)?;

      ShStr::from(buf)
    } else {
      let dt = strops::TimeReader::interpret(&time.to_str_lossy())
        .option_promote(time.span())
        .with_code(2)?;

      let fmt = fmt_string;

      match tz {
        Zone::Utc       => strops::strftime(&dt, &fmt)?,
        Zone::Local     => strops::strftime(&dt.with_timezone(&Local), &fmt)?,
        Zone::Named(tz) => strops::strftime(&dt.with_timezone(&tz), &fmt)?,
      }
    };

    procio::outln_bytes(&formatted);

    util::with_status(0)
  }
}
