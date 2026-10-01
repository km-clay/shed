use chrono::TimeDelta;

use crate::{
  eval::lex::{Span, Tk},
  sherr,
  state::{timers::TimerStatus, vars::VarStr},
  util::{
    error::ShResult,
    strops::{self, ByteCursor, Field, FieldParams, Sign, SliceCursor, StrFmt, VarStrDisplay},
  },
  varstr,
};

use super::{Builtin, BuiltinArgs, BuiltinRouter, opt::Parsed};

mod format;
mod sleep;
mod timer;

/// Renders a [`TimeDelta`]. `running` is the timer state behind `%R`, absent
/// when the duration did not come from a timer. The delta is signed because a
/// span may run backwards from now; only totals carry the sign, since a
/// component is a magnitude within a larger unit.
pub(super) struct DurFmt {
  pub(super) running: Option<bool>,
}

pub(super) enum DurConv {
  Status,

  Days,
  Hours,
  TotalHours,
  Mins,
  TotalMins,
  Secs,
  TotalSecs,
  Millis,
  TotalMillis,
  Micros,
  TotalMicros,
  Nanos,
  TotalNanos,
}

impl StrFmt for DurFmt {
  type Source = TimeDelta;
  type Conv = DurConv;
  fn parse_conv(&self, cur: &mut SliceCursor) -> ShResult<Self::Conv> {
    let Some(b) = cur.next_byte() else {
      return Err(sherr!(ParseErr, "incomplete format specifier"));
    };
    Ok(match b {
      b'D' | b'd' => DurConv::Days,
      b'H' => DurConv::Hours,
      b'h' => DurConv::TotalHours,
      b'M' => DurConv::Mins,
      b'm' => DurConv::TotalMins,
      b'S' => DurConv::Secs,
      b's' => DurConv::TotalSecs,
      b'L' => DurConv::Millis,
      b'l' => DurConv::TotalMillis,
      b'U' => DurConv::Micros,
      b'u' => DurConv::TotalMicros,
      b'N' => DurConv::Nanos,
      b'n' => DurConv::TotalNanos,
      b'R' => DurConv::Status,
      other => {
        return Err(sherr!(
          ParseErr,
          "invalid format specifier: %{}",
          other as char
        ));
      }
    })
  }

  #[rustfmt::skip]
  fn render(
    &self,
    conv: &Self::Conv,
    field: &FieldParams,
    src: &mut Self::Source,
  ) -> ShResult<Field> {
    const NANOS_PER_MICRO: u128 = 1_000;
    const NANOS_PER_MILLI: u128 = 1_000 * NANOS_PER_MICRO;
    const NANOS_PER_SEC  : u128 = 1_000 * NANOS_PER_MILLI;
    const NANOS_PER_MIN  : u128 = 60    * NANOS_PER_SEC;
    const NANOS_PER_HOUR : u128 = 60    * NANOS_PER_MIN;
    const NANOS_PER_DAY  : u128 = 24    * NANOS_PER_HOUR;

    let signed = i128::from(src.num_seconds()) * 1_000_000_000 + i128::from(src.subsec_nanos());
    let negative = signed < 0;
    let nanos = signed.unsigned_abs();

    let n = match conv {
      DurConv::TotalHours  => nanos / NANOS_PER_HOUR,
      DurConv::TotalMins   => nanos / NANOS_PER_MIN,
      DurConv::TotalSecs   => nanos / NANOS_PER_SEC,
      DurConv::TotalMillis => nanos / NANOS_PER_MILLI,
      DurConv::TotalMicros => nanos / NANOS_PER_MICRO,
      DurConv::TotalNanos  => nanos,

      DurConv::Days   =>  nanos / NANOS_PER_DAY,
      DurConv::Hours  => (nanos % NANOS_PER_DAY  ) / NANOS_PER_HOUR,
      DurConv::Mins   => (nanos % NANOS_PER_HOUR ) / NANOS_PER_MIN,
      DurConv::Secs   => (nanos % NANOS_PER_MIN  ) / NANOS_PER_SEC,

      DurConv::Millis => (nanos % NANOS_PER_SEC) / NANOS_PER_MILLI,
      DurConv::Micros => (nanos % NANOS_PER_SEC) / NANOS_PER_MICRO,
      DurConv::Nanos  =>  nanos % NANOS_PER_SEC,

      DurConv::Status => {
        let Some(running) = self.running else {
          // FIXME: this is a hack.
          return Ok(Field::string("%R".into()));
        };
        let status = if running { "running" } else { "stopped" };

        return Ok(Field::string(status.into()));
      }
    };

    // pad with zeroes
    let pad: usize = match conv {
      DurConv::Hours | DurConv::Mins | DurConv::Secs => 2,
      DurConv::Millis => 3,
      DurConv::Micros => 6,
      DurConv::Nanos  => 9,
      _ => 1,
    };

    let body = if field.width().is_some() {
      varstr!("{n}").into_bytes()
    } else {
      varstr!("{n:0pad$}").into_bytes()
    };

    // A component is part of a whole, so the sign belongs to the totals --
    // the ones a script feeds back into arithmetic.
    let sign = match conv {
      DurConv::TotalHours
      | DurConv::TotalMins
      | DurConv::TotalSecs
      | DurConv::TotalMillis
      | DurConv::TotalMicros
      | DurConv::TotalNanos
      | DurConv::Days
        if negative =>
      {
        Some(Sign::Minus)
      }
      _ => None,
    };

    Ok(Field::numeric(body, sign, None))
  }
}

fn fmt_timer_status(status: &TimerStatus) -> VarStr {
  let mut fmt = strops::format_time(strops::dur_delta(status.elapsed()))
    .unwrap_or_else(|| String::from("0s"))
    .to_var_str();

  fmt.push(b' ');

  if status.is_running() {
    fmt.push_slice(b"(running)");
  } else {
    fmt.push_slice(b"(stopped)");
  }

  fmt
}

pub(super) struct Chrono;
impl BuiltinRouter for Chrono {
  fn default_sub(&self) -> &'static dyn Builtin {
    &ChronoError
  }

  #[rustfmt::skip]
  fn sub_for(&self, word: &[u8]) -> Option<&'static dyn Builtin> {
    match word {
      b"timer" => Some(&timer::Timer  ),
      b"sleep" => Some(&sleep::Sleep  ),
      b"fmt"   => Some(&format::Format),
      _ => None,
    }
  }
}

impl Builtin for Chrono {
  fn as_router(&self) -> Option<&dyn BuiltinRouter> {
    Some(self)
  }
  fn get_argv_and_opts(&self, cmd_span: Span, argv: &[Tk], no_split: bool) -> ShResult<Parsed> {
    self.route_parse(cmd_span, argv, no_split)
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.dispatch_sub(args)
  }
}

struct ChronoError;
impl Builtin for ChronoError {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    Err(sherr!(ExecFail @ args.cmd_span(), "no subcommand specified for `chrono`"))
  }
}
