use std::time::Duration;

use bstr::ByteSlice;
use nix::libc;

use crate::{
  builtin::opt::Parsed,
  eval::lex::{Span, Tk},
  sherr, signal,
  util::{self, error::ShResultExt, strops},
};

use super::super::{Builtin, BuiltinArgs, BuiltinRouter, ShResult, argv};

const NANOS_PER_SEC: i64 = 1_000_000_000;

fn now(clock: libc::clockid_t) -> libc::timespec {
  let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
  unsafe { libc::clock_gettime(clock, &raw mut ts) };
  ts
}

/// `ts + dur`, saturating rather than wrapping so `Duration::MAX` gives a
/// deadline that will not arrive.
fn deadline_after(ts: libc::timespec, dur: Duration) -> libc::timespec {
  let secs = i64::try_from(dur.as_secs()).unwrap_or(i64::MAX);
  let nsec = ts.tv_nsec + i64::from(dur.subsec_nanos());
  libc::timespec {
    tv_sec: ts
      .tv_sec
      .saturating_add(secs)
      .saturating_add(nsec / NANOS_PER_SEC),
    tv_nsec: nsec % NANOS_PER_SEC,
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

/// No `clock_nanosleep` outside Linux, so convert the absolute deadline back
/// into a relative sleep each time round. A clock step during an
/// `CLOCK_REALTIME` wait is therefore only noticed on the next interruption.
#[cfg(not(linux_like))]
fn nap(clock: libc::clockid_t, deadline: &libc::timespec) -> i32 {
  let ts = now(clock);
  let mut rem = libc::timespec {
    tv_sec: deadline.tv_sec - ts.tv_sec,
    tv_nsec: deadline.tv_nsec - ts.tv_nsec,
  };
  if rem.tv_nsec < 0 {
    rem.tv_sec -= 1;
    rem.tv_nsec += NANOS_PER_SEC;
  }
  if rem.tv_sec < 0 {
    return 0;
  }
  if unsafe { libc::nanosleep(&rem, std::ptr::null_mut()) } == 0 {
    0
  } else {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
  }
}

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

    let ts = now(libc::CLOCK_MONOTONIC);
    let deadline = deadline_after(ts, dur);

    sleep_until(libc::CLOCK_MONOTONIC, &deadline)
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

    let mut sec = inst.timestamp();
    let mut nsec = inst.timestamp_subsec_nanos();

    // timestamp_subsec_nanos doc comment:
    // "in the event of a leap second, this may exceed 999,999,999"
    // this handles that case
    let (overflow, ns) = (nsec / 1_000_000_000, nsec % 1_000_000_000);
    sec = sec.saturating_add(i64::from(overflow));
    nsec = ns;

    let deadline = libc::timespec {
      tv_sec: sec,
      tv_nsec: i64::from(nsec),
    };

    sleep_until(libc::CLOCK_REALTIME, &deadline)
      .promote_err(span)
      .with_code(1)?;

    util::with_status(0)
  }
}
