//! `timer` subcommand for `chrono`
//!
//! has its own subcommands: `start`, `stop`, `reset`, `status`, `resume`

use super::{super::opt::Parsed, Builtin, BuiltinArgs, BuiltinRouter, DurFmt};
use crate::{
  eval::lex::{Span, Tk},
  procio,
  state::{
    Shed,
    timers::{StopWatch, TimerStatus, WatchName},
    vars::VarStr,
  },
  util::{
    self,
    error::{ShResult, ShResultExt},
    strops,
  },
  varstr,
};

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
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut arguments = args.arguments();
    let mut fmt = None;

    if let Some((arg, span)) = arguments.next()
      && WatchName::new(arg.clone()).promote_err(span)?.is_none()
    {
      fmt = Some(arg.clone());
    }

    if fmt.is_none()
      && let Some((arg, _)) = arguments.next()
    {
      fmt = Some(arg.clone());
    }

    // snapshot under the borrow, render and print outside it
    let snapshot: Vec<(VarStr, TimerStatus)> = Shed::timers(|t| {
      std::iter::once((VarStr::from("default"), t.default_timer().status()))
        .chain(
          t.named()
            .map(|(name, timer)| (name.as_var_str().clone(), timer.status())),
        )
        .collect()
    });

    for (name, status) in snapshot {
      let rendered = if let Some(fmt) = &fmt {
        let mut buf = vec![];
        let dur_fmt = DurFmt {
          running: Some(status.is_running()),
        };
        strops::StrFormatter::parse(&dur_fmt, fmt)
          .and_then(|f| f.render(&mut strops::dur_delta(status.elapsed()), &mut buf))
          .promote_err(args.cmd_span())?;
        VarStr::from(buf)
      } else {
        super::fmt_timer_status(&status)
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
      .transpose()?
      .flatten();

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
  fn strict_opts(&self) -> bool {
    true
  }
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
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut arguments = args.arguments();

    let mut name = None;
    let mut fmt = None;

    if let Some((arg, span)) = arguments.next() {
      let watch_name = WatchName::new(arg.clone()).promote_err(span)?;
      match watch_name {
        Some(n) => name = Some(n),
        None => fmt = Some(arg.clone()),
      }
    }

    if fmt.is_none()
      && let Some((arg, _)) = arguments.next()
    {
      fmt = Some(arg.clone());
    }

    let status = if let Some(name) = name {
      if !Shed::timers(|t| t.has_timer(&name)) {
        return util::with_status(1);
      }
      Shed::timers_mut(|t| t.timer_mut(name).status())
    } else {
      Shed::timers(|t| t.default_timer().status())
    };

    let out = if let Some(fmt) = fmt {
      let mut buf = vec![];
      let dur_fmt = DurFmt {
        running: Some(status.is_running()),
      };
      strops::StrFormatter::parse(&dur_fmt, &fmt)
        .and_then(|f| f.render(&mut strops::dur_delta(status.elapsed()), &mut buf))
        .promote_err(args.cmd_span())?;

      VarStr::from(buf)
    } else {
      super::fmt_timer_status(&status)
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
  fn fmt_zero_span_prints_a_duration_not_nothing() {
    // `format_time` elides zero; `chrono fmt` must still print something.
    let (out, status) = run("chrono fmt -d 0s");
    assert_eq!(status, 0);
    assert_eq!(out.trim(), "0s", "got {out:?}");

    // a round trip telescopes to zero
    let (out, _) = run("chrono fmt -d '9:00am to 5:00pm to 9:00am'");
    assert_eq!(out.trim(), "0s", "got {out:?}");
  }

  #[test]
  fn fmt_chained_spans_telescope() {
    let (out, _) = run("chrono fmt -d '9:00am to 1:00pm to 5:00pm'");
    assert_eq!(out.trim(), "8h", "got {out:?}");

    let (out, _) = run("chrono fmt -d -f '%h' '9:00am to 11:00am to 2:00pm to 6:00pm'");
    assert_eq!(out.trim(), "9", "got {out:?}");

    // direction survives
    let (out, _) = run("chrono fmt -d '5:00pm to 9:00am'");
    assert_eq!(out.trim(), "-8h", "got {out:?}");
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
