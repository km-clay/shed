//! `timer` subcommand for `chrono`
//!
//! has its own subcommands: `start`, `stop`, `reset`, `status`, `resume`

use super::{
  super::opt::{OptSpec, Parsed},
  Builtin, BuiltinArgs, BuiltinRouter,
};
use crate::{
  eval::lex::{Span, Tk},
  opt, procio, sherr,
  state::{
    Shed,
    timers::{StopWatch, TimerStatus, WatchName},
    vars::VarStr,
  },
  util::{
    self,
    error::{ShResult, ShResultExt},
    strops::{self, ByteCursor, Field, FieldParams, SliceCursor, StrFmt, VarStrDisplay},
  },
  varstr,
};

struct DurFmt;

enum DurConv {
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
  type Source = TimerStatus;
  type Conv = DurConv;
  fn parse_conv(&self, cur: &mut SliceCursor) -> ShResult<Self::Conv> {
    let Some(b) = cur.next_byte() else {
      return Err(sherr!(ParseErr, "incomplete format specifier"));
    };
    Ok(match b {
      b'd' => DurConv::Days,
      b'h' => DurConv::Hours,
      b'H' => DurConv::TotalHours,
      b'm' => DurConv::Mins,
      b'M' => DurConv::TotalMins,
      b's' => DurConv::Secs,
      b'S' => DurConv::TotalSecs,
      b'l' => DurConv::Millis,
      b'L' => DurConv::TotalMillis,
      b'u' => DurConv::Micros,
      b'U' => DurConv::TotalMicros,
      b'n' => DurConv::Nanos,
      b'N' => DurConv::TotalNanos,
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
    _field: &FieldParams,
    src: &mut Self::Source,
  ) -> ShResult<Field> {
    const NANOS_PER_MICRO: u128 = 1_000;
    const NANOS_PER_MILLI: u128 = 1_000 * NANOS_PER_MICRO;
    const NANOS_PER_SEC  : u128 = 1_000 * NANOS_PER_MILLI;
    const NANOS_PER_MIN  : u128 = 60    * NANOS_PER_SEC;
    const NANOS_PER_HOUR : u128 = 60    * NANOS_PER_MIN;
    const NANOS_PER_DAY  : u128 = 24    * NANOS_PER_HOUR;

    let elapsed = src.elapsed();
    let nanos = elapsed.as_nanos();

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

      DurConv::Millis => u128::from(elapsed.subsec_millis()),
      DurConv::Micros => u128::from(elapsed.subsec_micros()),
      DurConv::Nanos  => u128::from(elapsed.subsec_nanos ()),

      DurConv::Status => {
        let status = if src.is_running() {
          "running"
        } else {
          "stopped"
        };

        return Ok(Field::string(status.into()));
      }
    };

    Ok(Field::numeric(varstr!("{n}").into_bytes(), None, None))
  }
}

fn fmt_timer_status(status: &TimerStatus) -> VarStr {
  let mut fmt = strops::format_time(status.elapsed()).to_var_str();
  if fmt.is_empty() {
    fmt.push_slice(b"0s");
  }

  fmt.push(b' ');

  if status.is_running() {
    fmt.push_slice(b"(running)");
  } else {
    fmt.push_slice(b"(stopped)");
  }

  fmt
}

pub(super) struct Timer;
impl BuiltinRouter for Timer {
  fn default_sub(&self) -> &'static dyn Builtin {
    &List
  }

  fn sub_for(&self, word: &[u8]) -> Option<&'static dyn Builtin> {
    match word {
      b"start" => Some(&Start),
      b"stop" => Some(&Stop),
      b"reset" => Some(&Reset),
      b"resume" => Some(&Resume),
      b"status" => Some(&Status),
      _ => None,
    }
  }
}

impl Builtin for Timer {
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

struct List;
impl Builtin for List {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("format" | b'F', 1)]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    // snapshot under the borrow, render and print outside it
    let snapshot: Vec<(VarStr, TimerStatus)> = Shed::timers(|t| {
      std::iter::once((VarStr::from("default"), t.default_timer().status()))
        .chain(
          t.named()
            .map(|(name, timer)| (name.as_var_str().clone(), timer.status())),
        )
        .collect()
    });

    let fmt = args.opt_value("format");
    for (name, mut status) in snapshot {
      let rendered = if let Some(fmt) = &fmt {
        let mut buf = vec![];
        strops::Formatter::parse(&DurFmt, fmt)
          .and_then(|f| f.render(&mut status, &mut buf))
          .promote_err(args.cmd_span())?;
        VarStr::from(buf)
      } else {
        fmt_timer_status(&status)
      };
      procio::outln_bytes(&varstr!("{name}: {rendered}"));
    }

    util::with_status(0)
  }
}

trait TimerCmd {
  fn timer_func(&self, timer: &mut StopWatch);
  fn create(&self) -> bool {
    false
  }
  fn fire(&self, args: BuiltinArgs) -> ShResult<()> {
    let name = args
      .arguments()
      .next()
      .map(|(name, _)| WatchName::new(name.clone()).promote_err(args.cmd_span()))
      .transpose()?;

    if let Some(name) = name {
      if !Shed::timers(|t| t.has_timer(&name)) && !self.create() {
        return util::with_status(1);
      }
      Shed::timers_mut(|t| self.timer_func(t.timer_mut(name)));
    } else {
      Shed::timers_mut(|t| self.timer_func(t.default_mut()));
    }

    util::with_status(0)
  }
}

impl<T: TimerCmd + Sync> Builtin for T {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    self.fire(args)
  }
}

struct Start;
impl TimerCmd for Start {
  fn create(&self) -> bool {
    true
  }
  fn timer_func(&self, timer: &mut StopWatch) {
    timer.start();
  }
}

struct Stop;
impl TimerCmd for Stop {
  fn timer_func(&self, timer: &mut StopWatch) {
    timer.stop();
  }
}

struct Resume;
impl TimerCmd for Resume {
  fn timer_func(&self, timer: &mut StopWatch) {
    timer.resume();
  }
}

struct Reset;
impl TimerCmd for Reset {
  fn timer_func(&self, timer: &mut StopWatch) {
    timer.reset();
  }
}

struct Status;
impl Builtin for Status {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("format" | b'F', 1)]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let name = args
      .arguments()
      .next()
      .map(|(name, _)| WatchName::new(name.clone()).promote_err(args.cmd_span()))
      .transpose()?;
    let fmt = args.opt_value("format");

    let mut status = if let Some(name) = name {
      if !Shed::timers(|t| t.has_timer(&name)) {
        return util::with_status(1);
      }
      Shed::timers_mut(|t| t.timer_mut(name).status())
    } else {
      Shed::timers(|t| t.default_timer().status())
    };

    let out = if let Some(fmt) = fmt {
      let mut buf = vec![];
      strops::Formatter::parse(&DurFmt, &fmt)
        .and_then(|f| f.render(&mut status, &mut buf))
        .promote_err(args.cmd_span())?;

      VarStr::from(buf)
    } else {
      fmt_timer_status(&status)
    };

    procio::outln_bytes(&out);

    util::with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant};

  use super::*;
  use crate::state::Shed;
  use crate::tests::testutil::{TestGuard, test_input};

  fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
  }

  #[test]
  fn accumulates_across_stop_resume_cycles() {
    let t = Instant::now();
    let mut w = StopWatch::default();

    w.start_at(t);
    w.stop_at(t + ms(100));
    w.resume_at(t + ms(500));
    w.stop_at(t + ms(700));

    assert_eq!(
      w.read_at(t + ms(900)),
      ms(300),
      "paused span must not count"
    );
  }

  #[test]
  fn running_timer_includes_time_since_resume() {
    let t = Instant::now();
    let mut w = StopWatch::default();

    w.start_at(t);
    w.stop_at(t + ms(100));
    w.resume_at(t + ms(200));

    assert_eq!(w.read_at(t + ms(350)), ms(250));
  }

  #[test]
  fn stopped_timer_does_not_advance() {
    let t = Instant::now();
    let mut w = StopWatch::default();

    w.start_at(t);
    w.stop_at(t + ms(100));

    assert_eq!(w.read_at(t + ms(100)), ms(100));
    assert_eq!(w.read_at(t + ms(9000)), ms(100));
  }

  #[test]
  fn stop_and_resume_are_idempotent() {
    let t = Instant::now();
    let mut w = StopWatch::default();

    w.start_at(t);
    w.stop_at(t + ms(100));
    w.stop_at(t + ms(800));
    assert_eq!(
      w.read_at(t + ms(900)),
      ms(100),
      "second stop must be a no-op"
    );

    w.resume_at(t + ms(900));
    w.resume_at(t + ms(950));
    assert_eq!(
      w.read_at(t + ms(1000)),
      ms(200),
      "second resume must not rebase"
    );
  }

  #[test]
  fn start_clears_previous_elapsed() {
    let t = Instant::now();
    let mut w = StopWatch::default();

    w.start_at(t);
    w.stop_at(t + ms(500));
    w.start_at(t + ms(600));

    assert_eq!(w.read_at(t + ms(700)), ms(100));
  }

  #[test]
  fn reset_zeroes_and_stops() {
    let t = Instant::now();
    let mut w = StopWatch::default();

    w.start_at(t);
    w.reset();

    assert!(!w.is_running());
    assert_eq!(w.read_at(t + ms(500)), Duration::ZERO);
  }

  fn run(input: &str) -> (String, i32) {
    Shed::timers_mut(|t| *t = crate::state::timers::Timers::new());
    let guard = TestGuard::new();
    test_input(input).ok();
    (guard.read_output(), Shed::get_status())
  }

  #[test]
  fn missing_timer_exits_nonzero_without_creating_it() {
    let (_, status) = run("chrono timer status nosuch");
    assert_eq!(status, 1);

    let (out, _) = run("chrono timer stop nosuch; chrono timer");
    assert!(
      !out.contains("nosuch"),
      "a failed stop must not create the timer: {out:?}"
    );
  }

  #[test]
  fn default_name_is_reserved() {
    let (_, status) = run("chrono timer start default");
    assert_eq!(status, 1);
  }

  #[test]
  fn list_includes_the_default_timer() {
    let (out, _) = run("chrono timer start; chrono timer start build; chrono timer");
    assert!(
      out.contains("default:"),
      "default missing from list: {out:?}"
    );
    assert!(
      out.contains("build:"),
      "named timer missing from list: {out:?}"
    );
  }

  #[test]
  fn reset_timer_renders_a_zero_duration() {
    let (out, _) = run("chrono timer start z; chrono timer reset z; chrono timer status z");
    assert_eq!(out.trim(), "0s (stopped)");
  }

  #[test]
  fn status_format_reaches_the_subcommand_options() {
    let (out, _) = run("chrono timer start q; chrono timer status q -F '%R'");
    assert_eq!(out.trim(), "running");

    let (out, _) = run("chrono timer start q; chrono timer stop q; chrono timer status q -F '%R'");
    assert_eq!(out.trim(), "stopped");
  }

  #[test]
  fn bad_format_specifier_is_an_error() {
    let (_, status) = run("chrono timer start q; chrono timer status q -F '%zzz'");
    assert_ne!(status, 0, "an unknown specifier should not succeed");
  }
}
