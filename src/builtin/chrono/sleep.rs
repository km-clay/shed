use std::time::Duration;

use bstr::ByteSlice;
use nix::libc;

use crate::{
  builtin::opt::Parsed,
  eval::lex::{Span, Tk},
  sherr,
  util::{self, error::ShResultExt, strops},
};

use super::super::{Builtin, BuiltinArgs, BuiltinRouter, ShResult, argv};

pub(super) struct Sleep;
impl BuiltinRouter for Sleep {
  fn default_sub(&self) -> &'static dyn Builtin {
    &SleepDuration
  }

  fn sub_for(&self, word: &[u8]) -> Option<&'static dyn Builtin> {
    match word {
      b"until" => Some(&Until),
      _ => None,
    }
  }
}

impl Builtin for Sleep {
  fn as_router(&self) -> Option<&dyn BuiltinRouter> {
    Some(self)
  }
  fn get_argv_and_opts(&self, cmd_span: Span, argv: &[Tk], no_split: bool) -> ShResult<Parsed> {
    self.route_parse(cmd_span, argv, no_split)
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    self.dispatch_sub(args)
  }
}

struct SleepDuration;
impl Builtin for SleepDuration {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let (dur, span) = argv::join_raw_arg_iter(args.arguments());

    let dur = dur.trim().to_str_lossy();

    let dur = if dur == "inf" {
      Duration::MAX
    } else if let Ok(secs) = dur.parse::<f64>() {
      Duration::try_from_secs_f64(secs)
        .map_err(|e| sherr!(ParseErr @ span, "invalid duration: {e}").with_code(2))?
    } else {
      let micros = strops::TimeReader::parse_dur(&dur)
        .promote_err(span)
        .with_code(2)?;
      Duration::from_micros(micros.cast_unsigned())
    };

    let ts = super::now(libc::CLOCK_MONOTONIC);
    let deadline = super::deadline_after(ts, dur);

    super::sleep_until(libc::CLOCK_MONOTONIC, &deadline)
      .promote_err(span)
      .with_code(1)?;

    util::with_status(0)
  }
}

struct Until;
impl Builtin for Until {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let (until, span) = argv::join_raw_arg_iter(args.arguments());
    let inst = strops::TimeReader::interpret_upcoming(&until.to_str_lossy())
      .promote_err(span)
      .with_code(2)?;

    let deadline = super::timespec_for(inst);

    super::sleep_until(libc::CLOCK_REALTIME, &deadline)
      .promote_err(span)
      .with_code(1)?;

    util::with_status(0)
  }
}
