use crate::prelude::*;

#[derive(Clone,Debug,PartialEq)]
pub struct SavedSpan {
	pointer: Rc<RefCell<Span>>,
	start: usize,
	end: usize,
	expanded: bool
}

impl SavedSpan {
	pub fn from_span(pointer: Rc<RefCell<Span>>) -> Self {
		let expanded = pointer.borrow().expanded;
		let start = pointer.borrow().start();
		let end = pointer.borrow().end();
		Self { pointer, start, end, expanded }
	}
	pub fn restore(&self) {
		let mut deref = self.pointer.borrow_mut();
		deref.set_start(self.start);
		deref.set_end(self.end);
		deref.expanded = self.expanded
	}
	pub fn into_span(self) -> Rc<RefCell<Span>> {
		self.pointer
	}
}

#[derive(Clone,Debug,PartialEq)]
pub struct InputMan {
	input: Option<String>,
	spans: Vec<Rc<RefCell<Span>>>,
	saved_states: Vec<(String,Vec<SavedSpan>)>,
}

impl InputMan {
	pub fn new() -> Self {
		Self { input: None, spans: vec![], saved_states: vec![] }
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
	pub fn push_state(&mut self) {
		if let Some(input) = &self.input {
			let saved_input = input.clone();
			let mut saved_spans = vec![];
			for span in &self.spans {
				let saved_span = SavedSpan::from_span(span.clone());
				saved_spans.push(saved_span);
			}
			self.saved_states.push((saved_input,saved_spans));
		}
	}
	pub fn pop_state(&mut self) {
		if let Some((saved_input, saved_spans)) = self.saved_states.pop() {
			self.input = Some(saved_input);
			let mut restored_spans = vec![];
			for saved_span in saved_spans.into_iter() {
				saved_span.restore();
				let span = saved_span.into_span();
				restored_spans.push(span);
			}
			self.spans = restored_spans;
		}
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
	pub fn remove_span(&mut self, span: Rc<RefCell<Span>>) {
		if let Some(idx) = self.spans.iter().position(|iter_span| *iter_span == span) {
			self.spans.remove(idx);
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
