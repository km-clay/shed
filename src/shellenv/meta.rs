#[derive(Clone,Debug)]
pub struct MetaTab {
	last_status: i32
}

impl MetaTab {
	pub fn new() -> Self {
		Self {
			last_status: 0
		}
	}
	pub fn set_status(&mut self, code: i32) {
		self.last_status = code
	}
	pub fn last_status(&self) -> i32 {
		self.last_status
	}
}
