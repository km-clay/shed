
use crate::prelude::*;

#[derive(Clone,Debug)]
pub struct InputMan {
	input: Option<String>,
	spans: Vec<Rc<RefCell<Span>>>,
}

impl InputMan {
	pub fn new() -> Self {
		Self { input: None, spans: vec![] }
	}
	pub fn clear(&mut self) {
		*self = Self::new();
	}
	pub fn new_input(&mut self, input: &str) {
		self.input = Some(input.to_string())
	}
	pub fn get_input(&self) -> Option<&String> {
		self.input.as_ref()
	}
	pub fn get_input_mut(&mut self) -> Option<&mut String> {
		self.input.as_mut()
	}
	pub fn new_span(&mut self, start: usize, end: usize) -> Rc<RefCell<Span>> {
		if let Some(_input) = &self.input {
			let span = Rc::new(RefCell::new(Span::new(start, end)));
			self.spans.push(span.clone());
			span
		} else {
			Rc::new(RefCell::new(Span::new(0,0)))
		}
	}
	pub fn spans_mut(&mut self) -> &mut Vec<Rc<RefCell<Span>>> {
		&mut self.spans
	}
	pub fn clamp(&self, span: Rc<RefCell<Span>>) {
		let mut span = span.borrow_mut();
		if let Some(input) = &self.input {
			span.clamp_start(input.len());
			span.clamp_end(input.len());
		}
	}
	pub fn clamp_all(&self) {
		for span in &self.spans {
			self.clamp(span.clone());
		}
	}
	pub fn get_slice(&self, span: Rc<RefCell<Span>>) -> Option<&str> {
		let span = span.borrow();
		let mut start = span.start();
		let end = span.end();
		if start > end {
			start = end;
		}

		self.input.as_ref().map(|s| &s[start..end])
	}
}
