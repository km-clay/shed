
#[derive(Default,Debug)]
pub struct LineBuf {
	pub buffer: Vec<char>,
	cursor: usize
}

impl LineBuf {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn count_lines(&self) -> usize {
		self.buffer.iter().filter(|&&c| c == '\n').count()
	}
	pub fn cursor(&self) -> usize {
		self.cursor
	}
	pub fn clear(&mut self) {
		self.buffer.clear();
		self.cursor = 0;
	}
	pub fn insert_at_cursor(&mut self, ch: char) {
		self.buffer.insert(self.cursor, ch);
		self.move_cursor_right();
	}
	pub fn backspace_at_cursor(&mut self) {
		assert!(self.cursor <= self.buffer.len());
		if self.buffer.is_empty() {
			return
		}
		self.buffer.remove(self.cursor.saturating_sub(1));
		self.move_cursor_left();
	}
	pub fn del_at_cursor(&mut self) {
		assert!(self.cursor <= self.buffer.len());
		if self.buffer.is_empty() || self.cursor == self.buffer.len() {
			return
		}
		self.buffer.remove(self.cursor);
	}
	pub fn move_cursor_left(&mut self) {
		self.cursor = self.cursor.saturating_sub(1);
	}
	pub fn move_cursor_start(&mut self) {
		self.cursor = 0;
	}
	pub fn move_cursor_end(&mut self) {
		self.cursor = self.buffer.len();
	}
	pub fn move_cursor_right(&mut self) {
		if self.cursor == self.buffer.len() {
			return
		}
		self.cursor = self.cursor.saturating_add(1);
	}
	pub fn del_from_cursor(&mut self) {
		self.buffer.truncate(self.cursor);
	}
	pub fn del_word_back(&mut self) {
		if self.cursor == 0 {
			return 
		}
		let end = self.cursor;
		let mut start = self.cursor;

		while start > 0 && self.buffer[start - 1].is_whitespace() {
			start -= 1;
		}

		while start > 0 && !self.buffer[start - 1].is_whitespace() {
			start -= 1;
		}

		self.buffer.drain(start..end);
		self.cursor = start;
	}
}

pub fn strip_ansi_codes(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	let mut chars = s.chars().peekable();

	while let Some(c) = chars.next() {
		if c == '\x1b' && chars.peek() == Some(&'[') {
			// Skip over the escape sequence
			chars.next(); // consume '['
			while let Some(&ch) = chars.peek() {
				if ch.is_ascii_lowercase() || ch.is_ascii_uppercase() {
					chars.next(); // consume final letter
					break;
				}
				chars.next(); // consume intermediate characters
			}
		} else {
			out.push(c);
		}
	}
	out
}
