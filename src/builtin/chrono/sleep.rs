use std::time::Duration;

use nix::libc;

use crate::{
  sherr, sub_command,
  util::{self, error::ShResultExt, strops},
};

use super::{
  super::{Builtin, BuiltinArgs, BuiltinRouter, ShResult, argv},
  SubCommand,
};

pub(super) struct Sleep;
impl BuiltinRouter for Sleep {
  fn default_sub(&self) -> Option<&'static dyn Builtin> {
    Some(&SleepDuration)
  }
  fn name(&self) -> &'static str {
    "sleep"
  }
  fn sub_commands(&self) -> &'static [SubCommand] {
    const SUB_COMMANDS: &[SubCommand] =
      &[sub_command!(&Until, "until", "<instant>", "sleep until the given instant")];
    SUB_COMMANDS
  }
}

struct SleepDuration;
impl Builtin for SleepDuration {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let dur_raw = argv::join_raw_arg_iter(args.arguments());

    let dur = if **dur_raw == *b"inf" {
      Duration::MAX
    } else if let Ok(secs) = dur_raw.parse::<f64>() {
      Duration::try_from_secs_f64(secs)
        .map_err(|e| sherr!(ParseErr ? dur_raw.span(), "invalid duration: {e}").with_code(2))?
    } else {
      let micros = strops::TimeReader::parse_dur(&dur_raw.to_str_lossy())
        .option_promote(dur_raw.span())
        .with_code(2)?;
      Duration::from_micros(micros.cast_unsigned())
    };

    let ts       = super::now(libc::CLOCK_MONOTONIC);
    let deadline = super::deadline_after(ts, dur);

    super::sleep_until(libc::CLOCK_MONOTONIC, &deadline)
      .option_promote(dur_raw.span())
      .with_code(1)?;

    util::with_status(0)
  }
}

struct Until;
impl Builtin for Until {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let until = argv::join_raw_arg_iter(args.arguments());
    let inst = strops::TimeReader::interpret_upcoming(&until.to_str_lossy())
      .option_promote(until.span())
      .with_code(2)?;

    let deadline = super::timespec_for(inst);

    super::sleep_until(libc::CLOCK_REALTIME, &deadline)
      .option_promote(until.span())
      .with_code(1)?;

    util::with_status(0)
  }
}
