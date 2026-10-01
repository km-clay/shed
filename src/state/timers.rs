//! Stopwatch state for the `chrono timer` builtin
//!
//! Lives on [`Shed`](super::Shed) rather than in the builtin so that it
//! crosses into worker threads with the rest of the shell's state. A bare
//! `thread_local!` is reconstructed empty on a threaded pipeline stage.

use std::time::{Duration, Instant};

use crate::{sherr, state::vars::VarStr, util::error::ShResult};

/// A timer's elapsed time plus whether the clock is still running.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TimerStatus {
  Running(Duration),
  Stopped(Duration),
}

impl TimerStatus {
  pub(crate) fn elapsed(&self) -> Duration {
    match self {
      Self::Running(dur) | Self::Stopped(dur) => *dur,
    }
  }
  pub(crate) fn is_running(&self) -> bool {
    matches!(self, Self::Running(_))
  }
}

/// A user-supplied timer name. `default` is reserved for the unnamed timer.
#[derive(Clone, Debug)]
pub(crate) struct WatchName(VarStr);

impl WatchName {
  pub(crate) fn new(name: VarStr) -> ShResult<Self> {
    if name == "default" {
      Err(sherr!(ParseErr, "timer name 'default' is reserved"))
    } else {
      Ok(Self(name))
    }
  }
}

impl WatchName {
  pub(crate) fn as_var_str(&self) -> &VarStr {
    &self.0
  }
}

impl std::ops::Deref for WatchName {
  type Target = [u8];
  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StopWatch {
  elapsed: Duration,
  since: Option<Instant>,
}

impl StopWatch {
  pub(crate) fn start_at(&mut self, now: Instant) {
    self.elapsed = Duration::ZERO;
    self.since = Some(now);
  }

  pub(crate) fn stop_at(&mut self, now: Instant) {
    if let Some(since) = self.since.take() {
      self.elapsed += now.duration_since(since);
    }
  }

  pub(crate) fn resume_at(&mut self, now: Instant) {
    if self.since.is_none() {
      self.since = Some(now);
    }
  }

  pub(crate) fn read_at(&self, now: Instant) -> Duration {
    self.since.map_or(self.elapsed, |since| {
      self.elapsed + now.duration_since(since)
    })
  }

  pub(crate) fn start(&mut self) {
    self.start_at(Instant::now());
  }

  pub(crate) fn stop(&mut self) {
    self.stop_at(Instant::now());
  }

  pub(crate) fn resume(&mut self) {
    self.resume_at(Instant::now());
  }

  pub(crate) fn reset(&mut self) {
    self.elapsed = Duration::ZERO;
    self.since = None;
  }

  pub(crate) fn status(&self) -> TimerStatus {
    let elapsed = self.read_at(Instant::now());
    if self.is_running() {
      TimerStatus::Running(elapsed)
    } else {
      TimerStatus::Stopped(elapsed)
    }
  }

  pub(crate) fn is_running(&self) -> bool {
    self.since.is_some()
  }
}

/// The unnamed timer plus every named one, in creation order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Timers {
  default: StopWatch,
  named: Vec<(WatchName, StopWatch)>,
}

impl Timers {
  pub(crate) fn new() -> Self {
    Self::default()
  }

  pub(crate) fn has_timer(&self, name: &WatchName) -> bool {
    self.named.iter().any(|(n, _)| **n == **name)
  }

  /// The named timer, created if absent.
  pub(crate) fn timer_mut(&mut self, name: WatchName) -> &mut StopWatch {
    if let Some(idx) = self.named.iter().position(|(n, _)| **n == *name) {
      &mut self.named[idx].1
    } else {
      self.named.push((name, StopWatch::default()));
      &mut self.named.last_mut().expect("just pushed").1
    }
  }

  pub(crate) fn default_mut(&mut self) -> &mut StopWatch {
    &mut self.default
  }

  pub(crate) fn default_timer(&self) -> &StopWatch {
    &self.default
  }

  /// Every named timer, in creation order.
  pub(crate) fn named(&self) -> impl Iterator<Item = (&WatchName, &StopWatch)> {
    self.named.iter().map(|(n, w)| (n, w))
  }
}
