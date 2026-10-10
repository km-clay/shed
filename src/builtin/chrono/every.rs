use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use nix::libc;

use crate::{
  eval::execute,
  opt, sherr, signal,
  state::{Shed, vars::VarStr},
  util::{
    self,
    error::{ShResult, ShResultExt},
    strops,
  },
};

use super::super::{Builtin, BuiltinArgs, argv, opt::OptSpec};

fn get_k(anchor: i128, every: i128, clock: libc::clockid_t) -> i128 {
  (super::nanos_now(clock) - anchor).div_euclid(every)
}

/// Owed executions and the first tick to schedule
fn initial_state(k_now: i128, catch_up: bool, queue: Option<Count>) -> (i128, i128) {
  // With ticks numbered from zero, the count already due and the index of the
  // next one are the same number.
  let pending = (k_now + 1).max(0);
  let debt = if let (true, Some(q)) = (catch_up, queue) {
    q.get().map_or(pending, |cap| pending.min(i128::from(cap)))
  } else {
    0
  };
  (debt, pending)
}

/// Fold the boundaries crossed during a run into the owed count.
fn advance(next_k: i128, k_due: i128, debt: i128, queue: Option<Count>) -> (i128, i128) {
  if k_due < next_k {
    return (debt, next_k);
  }
  let missed = k_due - next_k + 1;
  let debt = match queue {
    None => 0,
    Some(q) => q
      .get()
      .map_or(debt + missed, |cap| (debt + missed).min(i128::from(cap))),
  };
  (debt, k_due + 1)
}

#[derive(Debug, Clone, Copy)]
enum Count {
  Infinite,
  Exact(u32),
}

impl Count {
  const fn zero() -> Self {
    Self::Exact(0)
  }
  fn get(self) -> Option<u32> {
    match self {
      Self::Infinite => None,
      Self::Exact(n) => Some(n),
    }
  }
  fn parse_queue(s: &VarStr) -> ShResult<Self> {
    Self::parse(s, "queue")
  }
  fn parse_retry(s: &VarStr) -> ShResult<Self> {
    Self::parse(s, "retry")
  }
  fn parse(s: &VarStr, opt: &str) -> ShResult<Self> {
    let n = s
      .parse::<i32>()
      .map_err(|v| sherr!(ParseErr, "invalid {opt} value: '{v}'"))?;
    match n.cmp(&-1) {
      Ordering::Less    => Err(sherr!(ParseErr, "{opt} value must be -1 or greater")),
      Ordering::Equal   => Ok(Self::Infinite),
      Ordering::Greater => Ok(Self::Exact(n.unsigned_abs())),
    }
  }
  fn dec(&mut self) -> bool {
    match self {
      Self::Infinite => true,
      Self::Exact(n) => {
        if *n == 0 {
          false
        } else {
          *self = Self::Exact(*n - 1);
          true
        }
      }
    }
  }
}

struct EverySpec {
  starting: Option<DateTime<Utc>>,
  until   : Option<DateTime<Utc>>,
  queue   : Option<Count>,
  retry   : Option<Count>,
  times   : Option<u32>,
  now     : bool,
  catch_up: bool,
}

impl EverySpec {
  fn from_args(args: &BuiltinArgs) -> ShResult<Self> {
    let starting = args
      .opt_value("starting")
      .map(|v| {
        strops::TimeReader::interpret(&v.to_str_lossy())
          .promote_err(args.opt_span("starting").unwrap())
          .with_code(2)
      })
      .transpose()?;

    let until = args
      .opt_value("until")
      .map(|v| {
        strops::TimeReader::interpret(&v.to_str_lossy())
          .promote_err(args.opt_span("until").unwrap())
          .with_code(2)
      })
      .transpose()?;

    let queue = args
      .opt_value("queue")
      .map(|v| {
        Count::parse_queue(&v)
          .promote_err(args.opt_span("queue").unwrap())
          .with_code(2)
      })
      .transpose()?;

    let retry = args
      .opt_value("retry")
      .map(|v| {
        Count::parse_retry(&v)
          .promote_err(args.opt_span("retry").unwrap())
          .with_code(2)
      })
      .transpose()?;

    let times = args
      .opt_value("times")
      .map(|v| {
        v.parse::<u32>()
          .map_err(|v| {
            sherr!(ParseErr @ args.opt_span("times").unwrap(), "invalid times value: '{v}'")
          })
          .with_code(2)
      })
      .transpose()?;

    let catch_up = args.has_opt("catch-up");
    let now      = args.has_opt("now");

    Ok(Self {
      starting,
      until,
      queue,
      retry,
      times,
      now,
      catch_up,
    })
  }
}

pub(super) struct Every;
impl Builtin for Every {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("starting",        1),
      opt!("queue",           1),
      opt!("retry",           1),
      opt!("until",           1),
      opt!("times"    | b'T', 1),
      opt!("catch-up" | b'C'   ),
      opt!("now"      | b'N'   ),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let EverySpec {
      starting,
      until,
      queue,
      retry,
      times,
      now,
      catch_up,
    } = EverySpec::from_args(&args)?;
    let mut arguments = args.arguments();

    let Some((dur, dur_span)) = arguments.next() else {
      return Err(sherr!(ExecFail @ args.cmd_span(), "missing interval argument").with_code(2));
    };

    let (cmd, cmd_span) = argv::join_raw_arg_iter(arguments);
    let interval = strops::TimeReader::parse_dur(&dur.to_str_lossy())
      .promote_err(dur_span)
      .with_code(2)?;
    let every = i128::from(interval) * 1_000; // micros to nanos
    let every = every.unsigned_abs() as i128; // convert negative to positive
    if every == 0 {
      return Err(
        sherr!(ExecFail @ dur_span, "interval must be greater than 0 nanoseconds, got '{every}us'")
          .with_code(2),
      );
    }

    let (clock, anchor) = if let Some(anchor) = starting {
      (libc::CLOCK_REALTIME, super::nanos_for(anchor))
    } else {
      (
        libc::CLOCK_MONOTONIC,
        super::nanos_now(libc::CLOCK_MONOTONIC),
      )
    };

    let mut status                 = 0;
    let mut runs                   = 0;
    let     (mut debt, mut next_k) = initial_state(get_k(anchor, every, clock), catch_up, queue);
    if now {
      debt += 1;
    }

    'every: loop {
      if debt > 0 {
        // we owe some executions, so skip the sleep
        signal::check_signals()?;
        debt -= 1;
      } else {
        // all caught up, sleep until the next tick's instant
        let due = anchor + (next_k * every);
        super::sleep_until(clock, &super::timespec_for_nanos(due))?;
        next_k += 1;
      }

      if let Some(until) = until
        && Utc::now() > until
      {
        break 'every;
      }

      let mut retry = retry.unwrap_or(Count::zero());
      loop {
        execute::exec_nonint(cmd.clone(), Some("every".into()))
          .promote_err(cmd_span)
          .with_code(1)?; // propagate shell-level errors and interrupts
        status = Shed::get_status();

        if status != 0 && retry.dec() {
          // command failed, and we have retries left over
          continue;
        }
        runs += 1; // retries are done, run is complete

        if let Some(times) = times
          && runs >= times
        {
          break 'every;
        }

        break;
      }

      // calculate owed runs from how many intervals we missed during the command
      (debt, next_k) = advance(next_k, get_k(anchor, every, clock), debt, queue);
    }

    util::with_status(status)
  }
}

#[cfg(test)]
mod tests {
  use super::{Count, advance, initial_state};

  const CAPPED: Option<Count> = Some(Count::Exact(3));
  const UNCAPPED: Option<Count> = Some(Count::Infinite);
  const ZERO: Option<Count> = Some(Count::Exact(0));
  const OFF: Option<Count> = None;

  // ─── initial_state ───────────────────────────────────────────────

  #[test]
  fn future_anchor_owes_nothing_and_starts_at_tick_zero() {
    // An anchor five intervals ahead puts the clock in tick -5. Ticks before
    // the anchor do not exist, so the first one to run is 0, not -4.
    for queue in [OFF, ZERO, CAPPED, UNCAPPED] {
      assert_eq!(initial_state(-5, true, queue), (0, 0), "{queue:?}");
      assert_eq!(initial_state(-5, false, queue), (0, 0), "{queue:?}");
    }
  }

  #[test]
  fn past_anchor_backlog_is_every_tick_through_now() {
    // Ten intervals elapsed means ticks 0..=10 came due -- eleven of them --
    // and the next to schedule is 11.
    assert_eq!(initial_state(10, true, UNCAPPED), (11, 11));
  }

  #[test]
  fn backlog_respects_the_cap() {
    assert_eq!(initial_state(10, true, CAPPED), (3, 11));
    assert_eq!(initial_state(10, true, ZERO), (0, 11));
  }

  #[test]
  fn backlog_needs_both_catch_up_and_queue() {
    assert_eq!(initial_state(10, false, UNCAPPED), (0, 11));
    assert_eq!(initial_state(10, true, OFF), (0, 11));
  }

  #[test]
  fn anchor_exactly_now_skips_the_tick_it_sits_on() {
    // k_now == 0 means tick 0's instant has arrived, so the next scheduled is
    // 1; with catch-up it is still owed.
    assert_eq!(initial_state(0, false, OFF), (0, 1));
    assert_eq!(initial_state(0, true, UNCAPPED), (1, 1));
  }

  // ─── advance ─────────────────────────────────────────────────────

  #[test]
  fn a_run_inside_its_interval_owes_nothing() {
    // Ran tick 4, so next_k is 5 and the clock is still in tick 4. Counting
    // this as a missed tick is what doubled the execution rate.
    assert_eq!(advance(5, 4, 0, UNCAPPED), (0, 5));
    assert_eq!(advance(5, 4, 0, CAPPED), (0, 5));
    assert_eq!(
      advance(5, 4, 2, UNCAPPED),
      (2, 5),
      "existing debt is untouched"
    );
  }

  #[test]
  fn overrun_owes_each_crossed_boundary() {
    // next_k 5 with the clock in tick 7: boundaries 5, 6 and 7 went by.
    assert_eq!(advance(5, 7, 0, UNCAPPED), (3, 8));
  }

  #[test]
  fn overrun_respects_the_cap() {
    assert_eq!(advance(5, 7, 0, CAPPED), (3, 8));
    assert_eq!(advance(5, 20, 0, CAPPED), (3, 21), "16 missed, capped to 3");
    assert_eq!(
      advance(5, 7, 2, CAPPED),
      (3, 8),
      "debt plus missed, then capped"
    );
  }

  #[test]
  fn without_queue_missed_ticks_are_dropped() {
    assert_eq!(advance(5, 20, 0, OFF), (0, 21));
    assert_eq!(
      advance(5, 20, 4, OFF),
      (0, 21),
      "a queue turned off clears debt"
    );
  }

  #[test]
  fn uncapped_debt_accumulates() {
    assert_eq!(advance(5, 7, 5, UNCAPPED), (8, 8));
  }

  #[test]
  fn advance_is_idempotent_once_caught_up() {
    // Calling it again with no time passed must not keep advancing.
    let (debt, next_k) = advance(5, 7, 0, UNCAPPED);
    assert_eq!(advance(next_k, 7, debt, UNCAPPED), (debt, next_k));
  }
}
