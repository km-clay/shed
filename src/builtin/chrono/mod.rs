use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use chrono_tz::Tz;
use nix::libc;

use crate::{
  sherr, signal,
  state::{timers::TimerStatus, vars::VarStr},
  sub_command, try_var,
  util::{
    error::ShResult,
    strops::{self, ByteCursor, Field, FieldParams, Sign, SliceCursor, StrFmt, VarStrDisplay},
  },
  varstr,
};

use super::{Builtin, BuiltinArgs, BuiltinRouter, SubCommand};

mod every;
mod format;
mod sleep;
mod timer;
mod timezone;

const NANOS_PER_SEC: i128 = 1_000_000_000;

fn nanos_for(date: DateTime<Utc>) -> i128 {
  let seconds = i128::from(date.timestamp());
  let nanos = i128::from(date.timestamp_subsec_nanos());
  seconds * NANOS_PER_SEC + nanos
}

fn nanos_of(ts: libc::timespec) -> i128 {
  let seconds = i128::from(ts.tv_sec);
  let nanos = i128::from(ts.tv_nsec);
  seconds * NANOS_PER_SEC + nanos
}

fn nanos_now(clock: libc::clockid_t) -> i128 {
  nanos_of(now(clock))
}

fn now(clock: libc::clockid_t) -> libc::timespec {
  let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
  unsafe { libc::clock_gettime(clock, &raw mut ts) };
  ts
}

fn timespec_for(date: DateTime<Utc>) -> libc::timespec {
  timespec_for_nanos(nanos_for(date))
}

fn timespec_for_nanos(nanos: i128) -> libc::timespec {
  let tv_sec = i64::try_from(nanos.div_euclid(NANOS_PER_SEC)).unwrap_or(i64::MAX);
  let tv_nanos = nanos.rem_euclid(NANOS_PER_SEC) as i64;

  libc::timespec {
    tv_sec,
    tv_nsec: tv_nanos,
  }
}

/// `ts + dur`, saturating rather than wrapping so `Duration::MAX` gives a
/// deadline that will not arrive.
fn deadline_after(ts: libc::timespec, dur: Duration) -> libc::timespec {
  let secs = i64::try_from(dur.as_secs()).unwrap_or(i64::MAX);
  let nsec = ts.tv_nsec + i64::from(dur.subsec_nanos());

  let tv_sec = ts
    .tv_sec
    .saturating_add(secs)
    .saturating_add(nsec / NANOS_PER_SEC as i64);
  let tv_nanos = nsec % NANOS_PER_SEC as i64;

  libc::timespec {
    tv_sec,
    tv_nsec: tv_nanos,
  }
}

/// Sleep until `deadline` on the monotonic clock. Returns 0 when the deadline
/// arrived, or an errno. An absolute deadline means a signal-interrupted sleep
/// resumes with no drift, because there is no remaining time to recompute.
#[cfg(linux_like)]
fn nap(clock: libc::clockid_t, deadline: &libc::timespec) -> i32 {
  // clock_nanosleep reports failure by returning the error number directly.
  unsafe { libc::clock_nanosleep(clock, libc::TIMER_ABSTIME, deadline, std::ptr::null_mut()) }
}

/// No `clock_nanosleep` outside Linux, so convert the absolute deadline back
/// into a relative sleep each time round. A clock step during an
/// `CLOCK_REALTIME` wait is therefore only noticed on the next interruption.
#[cfg(not(linux_like))]
fn nap(clock: libc::clockid_t, deadline: &libc::timespec) -> i32 {
  // Work in nanoseconds and convert once: `tv_nsec` is `c_long`, whose width
  // is per-target, so arithmetic against it does not port.
  let remaining = nanos_of(*deadline) - nanos_now(clock);
  if remaining <= 0 {
    return 0;
  }
  let rem = timespec_for_nanos(remaining);
  if unsafe { libc::nanosleep(&rem, std::ptr::null_mut()) } == 0 {
    0
  } else {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
  }
}

fn sleep_until(clock: libc::clockid_t, deadline: &libc::timespec) -> ShResult<()> {
  loop {
    match nap(clock, deadline) {
      0 => break,
      libc::EINTR => signal::check_signals()?,
      e => {
        let err = std::io::Error::from_raw_os_error(e);
        return Err(sherr!(ExecFail, "sleep failed: {err}").with_code(1));
      }
    }
  }

  Ok(())
}

enum Zone {
  Utc,
  Local,
  Named(Tz),
}
impl Zone {
  fn parse(tz: Option<VarStr>, utc: bool) -> ShResult<Self> {
    let tz = if let Some(name) = tz {
      let zone = name.parse::<Tz>().ok_or_else(|| {
        sherr!(ExecFail, "unknown timezone '{name}'",)
          .with_note("for a list of timezone names, run `chrono zone`".into())
          .with_code(2)
      })?;
      Zone::Named(zone)
    } else if utc {
      Zone::Utc
    } else {
      // an inherited TZ we cannot parse is not this command's problem
      try_var!("TZ")
        .and_then(|v| v.parse::<Tz>())
        .map_or(Zone::Local, Zone::Named)
    };
    Ok(tz)
  }
}

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
          return Err(sherr!(ExecFail, "timer status can only be formatted with `chrono timer`"))
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
  let mut fmt = strops::format_time(strops::dur_delta(status.elapsed()), true)
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
  fn name(&self) -> &'static str {
    "chrono"
  }
  fn sub_commands(&self) -> &'static [SubCommand] {
    const SUB_COMMANDS: &[SubCommand] = &[
      sub_command!(
        &timer::Timer,
        "timer",
        "[subcommand]",
        "manage timers for measuring elapsed time"
      ),
      sub_command!(
        &sleep::Sleep,
        "sleep",
        "[<duration>|until <instant>]",
        "sleep for a specified duration"
      ),
      sub_command!(
        &format::Format,
        "fmt",
        "<format> [timestamp]",
        "format a timestamp or duration"
      ),
      sub_command!(
        &every::Every,
        "every",
        "<interval> <command>",
        "run a command repeatedly at a specified interval"
      ),
      sub_command!(
        &timezone::Timezone,
        "tz",
        "[timezone]",
        "print portable timezone names"
      ),
    ];
    SUB_COMMANDS
  }
}
