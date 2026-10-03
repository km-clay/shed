use chrono::{Local, TimeDelta, Utc};

use crate::{
  builtin::opt::OptSpec,
  opt, procio, sherr,
  state::vars::VarStr,
  util::{
    self,
    error::ShResultExt,
    strops::{self, VarStrDisplay},
  },
};

use super::{
  super::{Builtin, BuiltinArgs, ShResult, argv},
  DurFmt, Zone,
};

pub(super) struct Format;
impl Builtin for Format {
  #[rustfmt::skip]
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("timezone" | b'z', 1),
      opt!("format"   | b'f', 1),
      opt!("duration" | b'd'   ),
      opt!("utc"      | b'u'   ),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let arguments = args.arguments();
    let parse_duration = args.has_opt("duration");
    let utc = args.has_opt("utc");

    if parse_duration && args.has_opt("timezone") {
      let dur_flag = args.opt_span("duration").unwrap().slice();
      let tz_flag = args.opt_span("timezone").unwrap().slice();
      return Err(
        sherr!(ExecFail @ args.cmd_span(), "cannot use {dur_flag} and {tz_flag} together")
          .with_code(2),
      );
    }

    // check timezone option, then TZ var, then default to local or UTC based on `-u`
    let tz = Zone::parse(args.opt_value("timezone"), utc)?;

    let fmt_string = args
      .opt_value("format")
      .or_else(|| (!parse_duration).then_some(VarStr::from("%a %b %e %I:%M:%S %p %Z %Y")));

    // Operands are joined, so a multi-word expression needs no quoting. A
    // span reads as `A to B`, which `parse_dur` splits.
    let (time, span) = argv::join_raw_arg_iter(arguments);
    if time.is_empty() {
      return Err(sherr!(ExecFail @ args.cmd_span(), "missing time argument").with_code(2));
    }

    let formatted = if parse_duration {
      let text = time.to_str_lossy();
      let mut elapsed = match strops::TimeReader::parse_dur(&text) {
        Ok(micros) => TimeDelta::microseconds(micros),
        Err(dur_err) => {
          let Ok(instant) = strops::TimeReader::interpret(&text) else {
            return Err(dur_err).promote_err(span).with_code(2);
          };
          instant - Utc::now()
        }
      };

      if let Some(fmt_string) = fmt_string {
        let mut buf = vec![];

        strops::StrFormatter::parse(&DurFmt { running: None }, &fmt_string)
          .and_then(|f| f.render(&mut elapsed, &mut buf))
          .promote_err(span)
          .with_code(1)?;

        VarStr::from(buf)
      } else {
        strops::format_time(elapsed, true)
          .unwrap_or_else(|| String::from("0s"))
          .to_var_str()
      }
    } else {
      let dt = strops::TimeReader::interpret(&time.to_str_lossy())
        .promote_err(span)
        .with_code(2)?;

      let Some(fmt_string) = fmt_string else {
        return Err(sherr!(ExecFail @ args.cmd_span(), "missing format string").with_code(2));
      };
      let fmt = fmt_string.to_str_lossy();

      match tz {
        Zone::Utc => strops::strftime(&dt, &fmt)?.to_var_str(),
        Zone::Local => strops::strftime(&dt.with_timezone(&Local), &fmt)?.to_var_str(),
        Zone::Named(tz) => strops::strftime(&dt.with_timezone(&tz), &fmt)?.to_var_str(),
      }
    };

    procio::outln_bytes(&formatted);

    util::with_status(0)
  }
}
