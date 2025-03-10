use std::time::{Duration, Instant};
use crate::prelude::*;

#[derive(Clone,Debug)]
pub struct MetaTab {
	timer_start: Instant,
	last_runtime: Option<Duration>,
	path_cmds: Vec<String> // Used for command completion
}

impl MetaTab {
	pub fn new() -> Self {
		let path_cmds = get_path_cmds().unwrap_or_default();
		Self {
			timer_start: Instant::now(),
			last_runtime: None,
			path_cmds
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
	pub fn path_cmds(&self) -> &[String] {
		&self.path_cmds
	}
}
