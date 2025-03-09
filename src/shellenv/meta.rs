use std::time::{Duration, Instant};
use crate::prelude::*;

#[derive(Clone,Debug)]
pub struct MetaTab {
	timer_start: Instant,
	last_runtime: Option<Duration>,
	last_status: i32
}

impl MetaTab {
	pub fn new() -> Self {
		Self {
			timer_start: Instant::now(),
			last_runtime: None,
			last_status: 0
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
	pub fn set_status(&mut self, code: i32) {
		self.last_status = code
	}
	pub fn last_status(&self) -> i32 {
		self.last_status
	}
}
