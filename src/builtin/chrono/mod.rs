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
      let zone = name.parse::<Tz>().map_err(|n| {
        sherr!(ExecFail, "unknown timezone '{n}'",)
          .with_note("for a list of timezone names, run `chrono zone`".into())
          .with_code(2)
      })?;
      Zone::Named(zone)
    } else if utc {
      Zone::Utc
    } else {
      // an inherited TZ we cannot parse is not this command's problem
      try_var!("TZ")
        .and_then(|v| v.parse::<Tz>().ok())
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

  TotalMillennia,
  Millennia,
  TotalCenturies,
  Centuries,
  TotalDecades,
  Decades,
  TotalYears,
  Years,
  TotalMonths,
  Months,
  TotalWeeks,
  Weeks,
  TotalDays,
  DaysModMonths,
  DaysModWeeks,
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
      b'E' => DurConv::Millennia,
      b'e' => DurConv::TotalMillennia,
      b'C' => DurConv::Centuries,
      b'c' => DurConv::TotalCenturies,
      b'T' => DurConv::Decades,
      b't' => DurConv::TotalDecades,
      b'Y' => DurConv::Years,
      b'y' => DurConv::TotalYears,
      b'O' => DurConv::Months,
      b'o' => DurConv::TotalMonths,
      b'W' => DurConv::Weeks,
      b'w' => DurConv::TotalWeeks,
      b'D' => DurConv::DaysModMonths,
      b'A' => DurConv::DaysModWeeks,
      b'd' => DurConv::TotalDays,
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
    const NANOS_PER_MICRO   : u128 = 1_000;
    const NANOS_PER_MILLI   : u128 = 1_000 * NANOS_PER_MICRO;
    const NANOS_PER_SEC     : u128 = 1_000 * NANOS_PER_MILLI;
    const NANOS_PER_MIN     : u128 = 60    * NANOS_PER_SEC;
    const NANOS_PER_HOUR    : u128 = 60    * NANOS_PER_MIN;
    const NANOS_PER_DAY     : u128 = 24    * NANOS_PER_HOUR;
    const NANOS_PER_WEEK    : u128 = 7     * NANOS_PER_DAY;
    const NANOS_PER_MONTH   : u128 = 30    * NANOS_PER_DAY; // roughly
    const NANOS_PER_YEAR    : u128 = 365   * NANOS_PER_DAY; // roughly
    const NANOS_PER_DECADE  : u128 = 10    * NANOS_PER_YEAR;
    const NANOS_PER_CENTURY : u128 = 10    * NANOS_PER_DECADE;
    const NANOS_PER_MILLENNIUM: u128 = 10  * NANOS_PER_CENTURY;

    let signed = i128::from(src.num_seconds()) * 1_000_000_000 + i128::from(src.subsec_nanos());
    let negative = signed < 0;
    let nanos = signed.unsigned_abs();

    let n = match conv {
      DurConv::TotalMillennia => nanos / NANOS_PER_MILLENNIUM,
      DurConv::TotalCenturies => nanos / NANOS_PER_CENTURY,
      DurConv::TotalDecades   => nanos / NANOS_PER_DECADE,
      DurConv::TotalYears     => nanos / NANOS_PER_YEAR,
      DurConv::TotalMonths    => nanos / NANOS_PER_MONTH,
      DurConv::TotalWeeks     => nanos / NANOS_PER_WEEK,
      DurConv::TotalDays      => nanos / NANOS_PER_DAY,
      DurConv::TotalHours  => nanos / NANOS_PER_HOUR,
      DurConv::TotalMins   => nanos / NANOS_PER_MIN,
      DurConv::TotalSecs   => nanos / NANOS_PER_SEC,
      DurConv::TotalMillis => nanos / NANOS_PER_MILLI,
      DurConv::TotalMicros => nanos / NANOS_PER_MICRO,
      DurConv::TotalNanos  => nanos,

      DurConv::Millennia =>  nanos / NANOS_PER_MILLENNIUM,
      DurConv::Centuries => (nanos % NANOS_PER_MILLENNIUM) / NANOS_PER_CENTURY,
      DurConv::Decades   => (nanos % NANOS_PER_CENTURY) / NANOS_PER_DECADE,
      DurConv::Years     => (nanos % NANOS_PER_DECADE ) / NANOS_PER_YEAR,
      // 365 is not a multiple of 30 and 30 is not a multiple of 7, so each of
      // these takes the remainder left by the unit above it rather than the
      // remainder of its own next-larger unit.
      DurConv::Months    =>   nanos % NANOS_PER_YEAR    / NANOS_PER_MONTH,
      DurConv::Weeks     =>  (nanos % NANOS_PER_YEAR)
                                   % NANOS_PER_MONTH    / NANOS_PER_WEEK,
      DurConv::DaysModWeeks  => ((nanos % NANOS_PER_YEAR)
                                   % NANOS_PER_MONTH)
                                   % NANOS_PER_WEEK     / NANOS_PER_DAY,
      DurConv::DaysModMonths => ((nanos % NANOS_PER_YEAR)
                                   % NANOS_PER_MONTH)
                                   / NANOS_PER_DAY,
      DurConv::Hours     => (nanos % NANOS_PER_DAY     ) / NANOS_PER_HOUR,
      DurConv::Mins      => (nanos % NANOS_PER_HOUR    ) / NANOS_PER_MIN,
      DurConv::Secs      => (nanos % NANOS_PER_MIN     ) / NANOS_PER_SEC,

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
      DurConv::TotalMillennia
      | DurConv::TotalCenturies
      | DurConv::TotalDecades
      | DurConv::TotalYears
      | DurConv::TotalMonths
      | DurConv::TotalWeeks
      | DurConv::TotalDays
      | DurConv::TotalHours
      | DurConv::TotalMins
      | DurConv::TotalSecs
      | DurConv::TotalMillis
      | DurConv::TotalMicros
      | DurConv::TotalNanos
        if negative && n != 0 =>
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

#[cfg(test)]
mod dur_fmt_tests {
  use crate::tests::testutil::{TestGuard, test_input};
  use pretty_assertions::assert_eq;

  fn out_of(cmd: &str) -> String {
    let g = TestGuard::new();
    test_input(cmd).unwrap();
    g.read_output()
  }

  /// `out_of` with the trailing newline the renderer appends removed.
  fn val_of(cmd: &str) -> String {
    out_of(cmd).trim_end().to_string()
  }

  /// Every rung takes the remainder left by the one above it, and 365 is not a
  /// multiple of 30 nor 30 of 7, so a rung that resets to its own next-larger
  /// unit drifts here and nowhere else.
  fn is(dur: &str, fmt: &str, expect: &str) {
    assert_eq!(
      val_of(&format!("chrono fmt -d '{dur}' -f '{fmt}'")),
      expect,
      "duration '{dur}' with format '{fmt}'"
    );
  }

  #[test]
  fn millennia_and_centuries_climb_the_ladder() {
    is(
      "2500 years",
      r"%{%1E millennia%}%{ %1C centuries%}",
      "2 millennia 5 centuries",
    );
  }

  #[test]
  fn decades_and_years_climb_the_ladder() {
    is(
      "37 years",
      r"%{%1T decades%}%{ %1Y years%}",
      "3 decades 7 years",
    );
  }

  #[test]
  fn years_months_and_days_climb_the_ladder() {
    is(
      "400 days",
      r"%{%1Y year%}%{ %1O month%}%{ %1D days%}",
      "1 year 1 month 5 days",
    );
  }

  /// `%A` is the week-relative rung; `%D` skips weeks and counts within the month.
  #[test]
  fn weeks_and_days_climb_the_ladder() {
    is("25 days", r"%{%1W weeks%}%{ %1A days%}", "3 weeks 4 days");
  }

  #[test]
  fn clock_units_climb_the_ladder() {
    is("3661 seconds", r"%{%1Hh%}%{ %1Mm%}%{ %1Ss%}", "1h 1m 1s");
  }

  /// The zero minute is dropped by the group.
  #[test]
  fn an_interior_zero_unit_is_omitted() {
    is("3605 seconds", r"%{%1Hh%}%{ %1Mm%}%{ %1Ss%}", "1h 5s");
  }

  #[test]
  fn the_default_format_pins_its_rungs() {
    assert_eq!(
      val_of("chrono fmt -d '400 days'"),
      "1 years 1 months 5 days, 00:00.000"
    );
    assert_eq!(val_of("chrono fmt -d '7 days'"), "7 days, 00:00.000");
    assert_eq!(val_of("chrono fmt -d '1 year'"), "1 years 00:00.000");
    assert_eq!(val_of("chrono fmt -d 0s"), "00:00.000");
    assert_eq!(val_of("chrono fmt -d '3661 seconds'"), "01:01:01.000");
  }

  #[test]
  fn components_climb_the_ladder() {
    assert_eq!(
      val_of("chrono fmt -d '2500 years' -f '%E/%C/%T/%Y'"),
      "2/5/0/0"
    );
    assert_eq!(val_of("chrono fmt -d '150 years' -f '%C/%T/%Y'"), "1/5/0");
    assert_eq!(val_of("chrono fmt -d '37 years' -f '%C/%T/%Y'"), "0/3/7");
    assert_eq!(
      val_of("chrono fmt -d '400 days' -f '%Y/%O/%W/%D'"),
      "1/1/0/5"
    );
    assert_eq!(val_of("chrono fmt -d '25 days' -f '%O/%W/%A'"), "0/3/4");
    assert_eq!(val_of("chrono fmt -d '25 days' -f '%O/%D'"), "0/25");
  }

  #[test]
  fn totals_are_independent_of_the_ladder() {
    assert_eq!(
      val_of("chrono fmt -d '400 days' -f '%y/%o/%w/%d'"),
      "1/13/57/400"
    );
  }

  #[test]
  fn totals_carry_the_sign_and_components_do_not() {
    assert!(val_of("chrono fmt -d 'now to 3 years ago' -f '%d'").starts_with('-'));
    assert!(!val_of("chrono fmt -d 'now to 3 years ago' -f '%D'").starts_with('-'));
  }

  #[test]
  fn a_zero_total_has_no_sign() {
    assert_eq!(val_of("chrono fmt -d 'now to 3 years ago' -f '%c'"), "0");
  }
}
