use std::time::{Duration, Instant};

#[derive(Clone,Debug)]
pub struct MetaTab {
	timer_start: Option<Instant>,
	last_runtime: Option<Duration>,
	last_status: i32
}

impl MetaTab {
	pub fn new() -> Self {
		Self {
			timer_start: None,
			last_runtime: None,
			last_status: 0
		}
	}
	pub fn start_timer(&mut self) {
		self.timer_start = Some(Instant::now())
	}
	pub fn stop_timer(&mut self) {
		let timer_start = self.timer_start.take();
		if let Some(instant) = timer_start {
			self.last_runtime = Some(instant.elapsed())
		}
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
