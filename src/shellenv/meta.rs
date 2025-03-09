use std::time::{Duration, Instant};
use crate::prelude::*;

#[derive(Clone,Debug)]
pub struct MetaTab {
	timer_start: Instant,
	last_runtime: Option<Duration>,
}

impl MetaTab {
	pub fn new() -> Self {
		Self {
			timer_start: Instant::now(),
			last_runtime: None,
		}
	}
	pub fn start_timer(&mut self) {
		self.timer_start = Instant::now();
	}
	pub fn stop_timer(&mut self) {
		self.last_runtime = Some(self.timer_start.elapsed());
	}
	pub fn get_runtime(&self) -> Option<Duration> {
		self.last_runtime
	}
}
