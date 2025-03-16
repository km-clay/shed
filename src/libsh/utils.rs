use std::collections::VecDeque;

pub trait VecDequeExt<T> {
	fn to_vec(self) -> Vec<T>;
}

impl<T> VecDequeExt<T> for VecDeque<T> {
	fn to_vec(self) -> Vec<T> {
		self.into_iter().collect::<Vec<T>>()
	}
}
