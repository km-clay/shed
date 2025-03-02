use std::collections::VecDeque;

pub trait VecDequeAliases<T> {
	fn fpop(&mut self) -> Option<T>;
	fn fpush(&mut self, value: T);
	fn bpop(&mut self) -> Option<T>;
	fn bpush(&mut self, value: T);
	fn to_vec(self) -> Vec<T>;
}

impl<T> VecDequeAliases<T> for VecDeque<T> {
	/// Alias for pop_front()
	fn fpop(&mut self) -> Option<T> {
		self.pop_front()
	}
	/// Alias for push_front()
	fn fpush(&mut self, value: T) {
		self.push_front(value);
	}
	/// Alias for pop_back()
	fn bpop(&mut self) -> Option<T> {
		self.pop_back()
	}
	/// Alias for push_back()
	fn bpush(&mut self, value: T) {
		self.push_back(value);
	}
	/// Just turns the deque into a vector
	fn to_vec(mut self) -> Vec<T> {
		let mut vec = vec![];
		while let Some(item) = self.fpop() {
			vec.push(item)
		}
		vec
	}
}
