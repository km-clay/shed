use crate::{
  eval::lex::{Span, Tk},
  sherr,
  util::error::ShResult,
};

use super::{Builtin, BuiltinArgs, BuiltinRouter, opt::Parsed};

pub(super) struct Chrono;
impl BuiltinRouter for Chrono {
  fn default_sub(&self) -> &'static dyn Builtin {
    &ChronoError
  }

  fn sub_for(&self, word: &[u8]) -> Option<&'static dyn Builtin> {
    match word {
      b"timer" => Some(&timer::Timer),
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

mod timer {
  //! `timer` subcommand for `chrono`
  //!
  //! has its own subcommands: `start`, `stop`, `reset`, `status`, `pause`, `resume`
  use std::{
    cell::RefCell,
    time::{Duration, Instant},
  };

  use super::{
    super::opt::{OptSpec, Parsed},
    Builtin, BuiltinArgs, BuiltinRouter,
  };
  use crate::{
    HashMap,
    eval::lex::{Span, Tk},
    opt, procio, sherr,
    state::vars::VarStr,
    util::{
      self,
      error::{ShResult, ShResultExt},
      strops::{self, ByteCursor, Field, FieldParams, SliceCursor, StrFmt, VarStrDisplay},
    },
    varstr,
  };

  enum TimerStatus {
    Running(Duration),
    Stopped(Duration),
  }

  impl TimerStatus {
    fn elapsed(&self) -> Duration {
      match self {
        TimerStatus::Running(dur) | TimerStatus::Stopped(dur) => *dur,
      }
    }
    fn is_running(&self) -> bool {
      matches!(self, TimerStatus::Running(_))
    }
  }

  #[derive(Debug, Default)]
  struct StopWatch {
    elapsed: Duration,
    since: Option<Instant>,
  }

  struct Timers {
    default: StopWatch,
    named: HashMap<String, StopWatch>,
  }

  impl Timers {
    fn new() -> Self {
      Self {
        default: StopWatch::default(),
        named: HashMap::default(),
      }
    }
  }

  impl Timers {
    fn has_timer(name: &str) -> bool {
      TIMERS.with(|t| {
        let timers = t.borrow();
        timers.named.contains_key(name)
      })
    }
    fn with_timer<F, R>(name: &str, f: F) -> R
    where
      F: FnOnce(&mut StopWatch) -> R,
    {
      TIMERS.with(|t| {
        let mut timers = t.borrow_mut();
        let timer = timers.named.entry(name.to_string()).or_default();
        f(timer)
      })
    }

    fn with_default_timer<F, R>(f: F) -> R
    where
      F: FnOnce(&mut StopWatch) -> R,
    {
      TIMERS.with(|t| {
        let mut timers = t.borrow_mut();
        f(&mut timers.default)
      })
    }
  }

  thread_local! {
    static TIMERS: RefCell<Timers> = RefCell::new(Timers::new());
  }

  impl StopWatch {
    fn start(&mut self) {
      self.elapsed = Duration::ZERO;
      self.since = Some(Instant::now());
    }

    fn stop(&mut self) {
      if let Some(since) = self.since.take() {
        self.elapsed += since.elapsed();
      }
    }

    fn resume(&mut self) {
      if self.since.is_none() {
        self.since = Some(Instant::now());
      }
    }

    fn reset(&mut self) {
      self.elapsed = Duration::ZERO;
      self.since = None;
    }

    fn status(&self) -> TimerStatus {
      let dur = self.since.as_ref().map_or(Duration::ZERO, Instant::elapsed);
      if self.is_running() {
        TimerStatus::Running(self.elapsed + dur)
      } else {
        TimerStatus::Stopped(self.elapsed + dur)
      }
    }

    fn is_running(&self) -> bool {
      self.since.is_some()
    }
  }

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

  pub(super) struct Timer;
  impl BuiltinRouter for Timer {
    fn default_sub(&self) -> &'static dyn Builtin {
      &TimerError
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

  struct TimerError;
  impl Builtin for TimerError {
    fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
      Err(sherr!(ExecFail @ args.cmd_span(), "no subcommand specified for `chrono timer`"))
    }
  }

  trait TimerCmd {
    fn timer_func(&self, timer: &mut StopWatch);
    fn create(&self) -> bool {
      false
    }
    fn fire(&self, args: BuiltinArgs) -> ShResult<()> {
      let name = args.arguments().next();

      if let Some((name, _)) = name {
        if !Timers::has_timer(&name.to_str_lossy()) && !self.create() {
          return util::with_status(1);
        }
        Timers::with_timer(&name.to_str_lossy(), |timer| self.timer_func(timer));
      } else {
        Timers::with_default_timer(|timer| self.timer_func(timer));
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
      let name = args.arguments().next();
      let fmt = args.opt_value("format");

      let mut status = if let Some((name, _)) = name {
        if !Timers::has_timer(&name.to_str_lossy()) {
          return util::with_status(1);
        }
        Timers::with_timer(&name.to_str_lossy(), |timer| timer.status())
      } else {
        Timers::with_default_timer(|timer| timer.status())
      };

      let out = if let Some(fmt) = fmt {
        let mut buf = vec![];
        strops::Formatter::parse(&DurFmt, &fmt)
          .and_then(|f| f.render(&mut status, &mut buf))
          .promote_err(args.cmd_span())?;

        VarStr::from(buf)
      } else {
        let mut fmt = strops::format_time(status.elapsed()).to_var_str();
        fmt.push(b' ');

        if status.is_running() {
          fmt.push_slice(b"(running)");
        } else {
          fmt.push_slice(b"(stopped)");
        }

        fmt
      };

      procio::outln_bytes(&out);

      util::with_status(0)
    }
  }
}
