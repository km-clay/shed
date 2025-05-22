use std::{fmt::Display, ops::{Deref, DerefMut, Range}, sync::Arc};

use unicode_width::UnicodeWidthStr;

use crate::libsh::{error::ShResult, sys::sh_quit, term::{Style, Styled}};

use super::vicmd::{Anchor, Bound, Dest, Direction, Motion, RegisterName, TextObj, To, Verb, ViCmd, Word};

#[derive(Debug, PartialEq, Eq)]
pub enum CharClass {
	Alphanum,
	Symbol
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionKind {
	Forward(usize),
	To(usize),
	Backward(usize),
	Range(usize,usize),
	Null
}

#[derive(Clone,Default,Debug)]
pub struct TermCharBuf(pub Vec<TermChar>);

impl Deref for TermCharBuf {
	type Target = Vec<TermChar>;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

impl DerefMut for TermCharBuf {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.0
	}
}

impl Display for TermCharBuf {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		for ch in &self.0 {
			match ch {
				TermChar::Grapheme(str) => write!(f, "{str}")?,
				TermChar::Newline => write!(f, "\r\n")?,
			}
		}
		Ok(())
	}
}

impl FromIterator<TermChar> for TermCharBuf {
	fn from_iter<T: IntoIterator<Item = TermChar>>(iter: T) -> Self {
		let mut buf = vec![];
		for item in iter {
			buf.push(item)
		}
		Self(buf)
	}
}

impl From<TermCharBuf> for String {
	fn from(value: TermCharBuf) -> Self {
		let mut string = String::new();
		for char in value.0 {
			match char {
				TermChar::Grapheme(str) => string.push_str(&str),
				TermChar::Newline => {
					string.push('\r');
					string.push('\n');
				}
			}
		}
		string
	}
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TermChar {
	Grapheme(Arc<str>),
	// Treated as '\n' in the code, printed as '\r\n' to the terminal
	Newline 
}

impl TermChar {
	pub fn is_whitespace(&self) -> bool {
		match self {
			TermChar::Newline => true,
			TermChar::Grapheme(ch) => {
				ch.chars().next().is_some_and(|c| c.is_whitespace())
			}
		}
	}
	pub fn matches(&self, other: &str) -> bool {
		match self {
			TermChar::Grapheme(ch) => {
				ch.as_ref() == other
			}
			TermChar::Newline => other == "\n"
		}
	}
}

impl From<Arc<str>> for TermChar {
	fn from(value: Arc<str>) -> Self {
		Self::Grapheme(value)
	}
}

impl From<char> for TermChar {
	fn from(value: char) -> Self {
		match value {
			'\n' => Self::Newline,
			ch => Self::Grapheme(Arc::from(ch.to_string()))
		}
	}
}

impl Display for TermChar {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			TermChar::Grapheme(str) => {
				write!(f,"{str}")
			}
			TermChar::Newline => {
				write!(f,"\r\n")
			}
		}
	}
}

impl From<&TermChar> for CharClass {
	fn from(value: &TermChar) -> Self {
		match value {
			TermChar::Newline => Self::Symbol,
			TermChar::Grapheme(ch) => {
				if ch.chars().next().is_some_and(|c| c.is_alphanumeric()) {
					Self::Alphanum
				} else {
					Self::Symbol
				}
			}
		}
	}
}

impl From<char> for CharClass {
	fn from(value: char) -> Self {
		if value.is_alphanumeric() {
			Self::Alphanum
		} else {
			Self::Symbol
		}
	}
}

fn is_other_class_or_ws(a: &TermChar, b: &TermChar) -> bool {
	if a.is_whitespace() || b.is_whitespace() {
		return true;
	}

	CharClass::from(a) != CharClass::from(b)
}

#[derive(Default,Debug)]
pub struct LineBuf {
	buffer: TermCharBuf,
	cursor: usize,
}

impl LineBuf {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn with_initial(mut self, initial: &str) -> Self {
		let chars = initial.chars();
		for char in chars {
			self.buffer.push(char.into())
		}
		self
	}
	pub fn buffer(&self) -> &TermCharBuf {
		&self.buffer
	}
	pub fn cursor(&self) -> usize {
		self.cursor
	}
	pub fn cursor_char(&self) -> Option<&TermChar> {
		let tc = self.buffer.get(self.cursor())?;
		Some(tc)
	}
	pub fn get_char(&self, pos: usize) -> Option<&TermChar> {
		let tc = self.buffer.get(pos)?;
		Some(tc)
	}
	pub fn insert_at_cursor(&mut self, tc: TermChar) {
		let cursor = self.cursor();
		self.buffer.insert(cursor,tc)
	}
	pub fn count_lines(&self) -> usize {
		self.buffer.iter().filter(|&c| c == &TermChar::Newline).count()
	}
	pub fn cursor_back(&mut self, count: usize) {
		self.cursor = self.cursor.saturating_sub(count)
	}
	pub fn cursor_fwd(&mut self, count: usize) {
		self.cursor = self.num_or_len(self.cursor + count)
	}
	pub fn cursor_to(&mut self, pos: usize) {
		self.cursor = self.num_or_len(pos)
	}
	pub fn prepare_line(&self) -> String {
		self.buffer.to_string()
	}
	pub fn clamp_cursor(&mut self) {
		if self.cursor_char().is_none() && !self.buffer.is_empty() {
			self.cursor = self.cursor.saturating_sub(1)
		}
	}
	pub fn cursor_display_coords(&self) -> (usize, usize) {
		let mut x = 0;
		let mut y = 0;
		for i in 0..self.cursor() {
			let ch = self.get_char(i).unwrap();
			match ch {
				TermChar::Grapheme(str) => x += str.width().max(1),
				TermChar::Newline => {
					y += 1;
					x = 0;
				}
			}
		}

		(x, y)
	}
	pub fn split_lines(&self) -> Vec<String> {
		let line = self.prepare_line();
		let mut lines = vec![];
		let mut cur_line = String::new();
		for ch in line.chars() {
			match ch {
				'\n' => lines.push(std::mem::take(&mut cur_line)),
				_ => cur_line.push(ch)
			}
		}
		lines.push(cur_line);
		lines
	}
	pub fn display_lines(&self) -> Vec<String> {
		let line_bullet = "∙ ".styled(Style::Dim);
		self.split_lines()
			.into_iter()
			.enumerate()
			.map(|(i, line)| {
				if i == 0 {
					line.to_string()
				} else {
					format!("{line_bullet}{line}")
				}
			})
		.collect()
	}
	pub fn on_word_bound(&self, word: Word, pos: usize, dir: Direction) -> bool {
		let check_pos = match dir {
			Direction::Forward => self.num_or_len(pos + 1),
			Direction::Backward => pos.saturating_sub(1)
		};
		let Some(curr_char) = self.cursor_char() else {
			return false
		};
		self.get_char(check_pos).is_some_and(|c| {
			match word {
				Word::Big => c.is_whitespace(),
				Word::Normal => is_other_class_or_ws(curr_char, c)
			}
		})
	}
	fn backward_until<F: Fn(&TermChar) -> bool>(&self, mut start: usize, cond: F, inclusive: bool) -> usize {
		while start > 0 && !cond(&self.buffer[start]) {
			start -= 1;
		}
		if !inclusive {
			if start > 0 {
				start.saturating_add(1)
			} else {
				start
			}
		} else {
			start
		}
	}
	fn forward_until<F: Fn(&TermChar) -> bool>(&self, mut start: usize, cond: F, inclusive: bool) -> usize {
		while start < self.buffer.len() && !cond(&self.buffer[start]) {
			start += 1;
		}
		if !inclusive {
			if start < self.buffer.len() {
				start.saturating_sub(1)
			} else {
				start
			}
		} else {
			start
		}
	}
	pub fn find_word_pos(&self, word: Word, dest: To, dir: Direction) -> usize {
		let mut pos = self.cursor();
		match dir {
			Direction::Forward => {
				match word {
					Word::Big => {
						match dest {
							To::Start => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off of the word
									pos = self.num_or_len(pos + 1);
								}
								// Pass the current word if any
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									pos = self.forward_until(pos, |c| c.is_whitespace(), true);
								}
								// Land on the start of the next word
								pos = self.forward_until(pos, |c| !c.is_whitespace(), true)
							}
							To::End => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off of the word
									pos = self.num_or_len(pos + 1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									// We are in a word
									// Go to the end of the current word
									pos = self.forward_until(pos, |c| c.is_whitespace(), false)
								} else {
									// We are outside of a word
									// Find the next word, then go to the end of it
									pos = self.forward_until(pos, |c| !c.is_whitespace(), true);
									pos = self.forward_until(pos, |c| c.is_whitespace(), false)
								}
							}
						}
					}
					Word::Normal => {
						match dest {
							To::Start => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off of the word
									pos = self.num_or_len(pos + 1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									// We are inside of a word
									// Find the next instance of whitespace or a different char class
									let this_char = self.get_char(pos).unwrap();
									pos = self.forward_until(pos, |c| is_other_class_or_ws(this_char, c), true);

									// If we found whitespace, continue until we find non-whitespace
									if self.get_char(pos).is_some_and(|c| c.is_whitespace()) {
										pos = self.forward_until(pos, |c| !c.is_whitespace(), true)
									}
								} else {
									// We are in whitespace, proceed to the next word
									pos = self.forward_until(pos, |c| !c.is_whitespace(), true)
								}
							}
							To::End => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off of the word
									pos = self.num_or_len(pos + 1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									// Proceed up until the next differing char class
									let this_char = self.get_char(pos).unwrap();
									pos = self.forward_until(pos, |c| is_other_class_or_ws(this_char, c), false);
								} else {
									// Find the next non-whitespace character
									pos = self.forward_until(pos, |c| !c.is_whitespace(), true);
									// Then proceed until a differing char class is found
									let this_char = self.get_char(pos).unwrap();
									pos = self.forward_until(pos, |c|is_other_class_or_ws(this_char, c), false);
								}
							}
						}
					}
				}
			}
			Direction::Backward => {
				match word {
					Word::Big => {
						match dest {
							To::Start => {
								if self.on_word_bound(word, pos, dir) {
									// Push the cursor off
									pos = pos.saturating_sub(1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									// We are in a word, go to the start of it
									pos = self.backward_until(pos, |c| c.is_whitespace(), false);
								} else {
									// We are not in a word, find one and go to the start of it
									pos = self.backward_until(pos, |c| !c.is_whitespace(), true);
									pos = self.backward_until(pos, |c| c.is_whitespace(), false);
								}
							}
							To::End => unreachable!()
						}
					}
					Word::Normal => {
						match dest {
							To::Start => {
								if self.on_word_bound(word, pos, dir) {
									pos = pos.saturating_sub(1);
								}
								if self.get_char(pos).is_some_and(|c| !c.is_whitespace()) {
									let this_char = self.get_char(pos).unwrap();
									pos = self.backward_until(pos, |c| is_other_class_or_ws(this_char, c), false)
								} else {
									pos = self.backward_until(pos, |c| !c.is_whitespace(), true);
									let this_char = self.get_char(pos).unwrap();
									pos = self.backward_until(pos, |c| is_other_class_or_ws(this_char, c), false);
								}
							}
							To::End => unreachable!()
						}
					}
				}
			}
		}
		pos
	}
	pub fn eval_text_obj(&self, obj: TextObj, bound: Bound) -> Range<usize> {
		let mut start = self.cursor();
		let mut end = self.cursor();

		match obj {
			TextObj::Word(word) => {
				start = match self.on_word_bound(word, self.cursor(), Direction::Backward) {
					true => self.cursor(),
					false => self.find_word_pos(word, To::Start, Direction::Backward),
				};
				end = match self.on_word_bound(word, self.cursor(), Direction::Forward) {
					true => self.cursor(),
					false => self.find_word_pos(word, To::End, Direction::Forward),
				};
				end = self.num_or_len(end + 1);
				if bound == Bound::Around {
					end = self.forward_until(end, |c| c.is_whitespace(), true);
					end = self.forward_until(end, |c| !c.is_whitespace(), true);
				}
				return start..end
			}
			TextObj::Line => {
				let cursor = self.cursor();
				start = self.backward_until(cursor, |c| c == &TermChar::Newline, false);
				end = self.forward_until(cursor, |c| c == &TermChar::Newline, true);
			}
			TextObj::Sentence => todo!(),
			TextObj::Paragraph => todo!(),
			TextObj::DoubleQuote => {
				let cursor = self.cursor();
				let ln_start = self.backward_until(cursor, |c| c == &TermChar::Newline, false);
				let mut line_chars = self.buffer[ln_start..cursor].iter();
				let mut in_quote = false;
				while let Some(ch) = line_chars.next() {
					let TermChar::Grapheme(ch) = ch else { unreachable!() };
					match ch.as_ref() {
						"\\" => {
							line_chars.next();
						}
						"\"" => in_quote = !in_quote,
						_ => { /* continue */ }
					}
				}
				let mut start_pos = cursor;
				let end_pos;
				if !in_quote {
					start_pos = self.forward_until(start_pos, |c| c.matches("\n") || c.matches("\""), true);
					if !self.get_char(start_pos).is_some_and(|c| c.matches("\"")) {
						return cursor..cursor
					}
					end_pos = self.forward_until(start_pos, |c| c.matches("\n") || c.matches("\""), true);
					if !self.get_char(end_pos).is_some_and(|c| c.matches("\"")) {
						return cursor..cursor
					}
					start = start_pos;
					end = end_pos;
				} else {
					start_pos = self.backward_until(start_pos, |c| c.matches("\n") || c.matches("\""), true);
					if !self.get_char(start_pos).is_some_and(|c| c.matches("\"")) {
						return cursor..cursor
					}
					end_pos = self.forward_until(self.num_or_len(start_pos + 1), |c| c.matches("\n") || c.matches("\""), true);
					if !self.get_char(end_pos).is_some_and(|c| c.matches("\"")) {
						return cursor..cursor
					}
					start = start_pos;
					end = self.num_or_len(end_pos + 1);

					if bound == Bound::Around && self.get_char(end).is_some_and(|c| c.is_whitespace()) {
						end += 1;
						end = self.forward_until(end, |c| !c.is_whitespace(), true);
					}
				}
			}
			TextObj::SingleQuote => todo!(),
			TextObj::BacktickQuote => todo!(),
			TextObj::Paren => todo!(),
			TextObj::Bracket => todo!(),
			TextObj::Brace => todo!(),
			TextObj::Angle => todo!(),
			TextObj::Tag => todo!(),
			TextObj::Custom(_) => todo!(),
		}

		if bound == Bound::Inside {
			start = self.num_or_len(start + 1);
			end = end.saturating_sub(1);
		}
		start..end
	}
	/// Clamp a number to the length of the buffer
	pub fn num_or_len(&self, num: usize) -> usize {
		num.min(self.buffer.len().saturating_sub(1))
	}
	pub fn eval_motion(&self, motion: Motion) -> MotionKind {
		match motion {
			Motion::WholeLine => {
				let cursor = self.cursor();
				let start = self.backward_until(cursor, |c| c == &TermChar::Newline, false);
				let end = self.forward_until(cursor, |c| c == &TermChar::Newline, true);
				MotionKind::Range(start,end)
			}
			Motion::TextObj(text_obj, bound) => {
				let range = self.eval_text_obj(text_obj, bound);
				let range = mk_range(range.start, range.end);
				MotionKind::Range(range.start,range.end)
			}
			Motion::BeginningOfFirstWord => {
				let cursor = self.cursor();
				let line_start = self.backward_until(cursor, |c| c == &TermChar::Newline, true);
				let first_print = self.forward_until(line_start, |c| !c.is_whitespace(), true);
				MotionKind::To(first_print)
			}
			Motion::BeginningOfLine => {
				let cursor = self.cursor();
				let line_start = self.backward_until(cursor, |c| c == &TermChar::Newline, false);
				MotionKind::To(line_start)
			}
			Motion::EndOfLine => {
				let cursor = self.cursor();
				let line_end = self.forward_until(cursor, |c| c == &TermChar::Newline, false);
				MotionKind::To(line_end)
			}
			Motion::BackwardWord(word) => MotionKind::To(self.find_word_pos(word, To::Start, Direction::Backward)),
			Motion::ForwardWord(dest, word) => MotionKind::To(self.find_word_pos(word, dest, Direction::Forward)),
			Motion::CharSearch(direction, dest, ch) => {
				let mut cursor = self.cursor();
				let inclusive = matches!(dest, Dest::On);

				let stop_condition = |c: &TermChar| {
					c == &TermChar::Newline ||
					c == &ch
				};
				if self.cursor_char().is_some_and(|c| c == &ch) {
					// We are already on the character we are looking for
					// Let's nudge the cursor
					match direction {
						Direction::Backward => cursor = self.cursor().saturating_sub(1),
						Direction::Forward => cursor = self.num_or_len(self.cursor() + 1),
					}
				}

				let stop_pos = match direction {
					Direction::Forward => self.forward_until(cursor, stop_condition, inclusive),
					Direction::Backward => self.backward_until(cursor, stop_condition, inclusive),
				};

				let found_char = match dest {
					Dest::On => self.get_char(stop_pos).is_some_and(|c| c == &ch),
					_ => {
						match direction {
							Direction::Forward => self.get_char(stop_pos + 1).is_some_and(|c| c == &ch),
							Direction::Backward => self.get_char(stop_pos.saturating_sub(1)).is_some_and(|c| c == &ch),
						}
					}
				};

				if found_char {
					MotionKind::To(stop_pos)
				} else {
					MotionKind::Null
				}
			}
			Motion::BackwardChar => MotionKind::Backward(1),
			Motion::ForwardChar => MotionKind::Forward(1),
			Motion::LineUp => todo!(),
			Motion::LineDown => todo!(),
			Motion::WholeBuffer => MotionKind::Range(0,self.buffer.len().saturating_sub(1)),
			Motion::BeginningOfBuffer => MotionKind::To(0),
			Motion::EndOfBuffer => MotionKind::To(self.buffer.len().saturating_sub(1)),
			Motion::Null => MotionKind::Null,
			Motion::Builder(_) => unreachable!(),
		}
	}
	pub fn exec_verb(&mut self, verb: Verb, motion: MotionKind, register: RegisterName) -> ShResult<()> {
		match verb {
			Verb::Change |
			Verb::Delete => {
				let deleted;
				match motion {
					MotionKind::Forward(n) => {
						let fwd = self.num_or_len(self.cursor() + n);
						let cursor = self.cursor();
						deleted = self.buffer.drain(cursor..=fwd).collect::<TermCharBuf>();
					}
					MotionKind::To(pos) => {
						let range = mk_range(self.cursor(), pos);
						deleted = self.buffer.drain(range.clone()).collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(range.start));
					}
					MotionKind::Backward(n) => {
						let back = self.cursor.saturating_sub(n);
						let cursor = self.cursor();
						deleted = self.buffer.drain(back..cursor).collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(back));
					}
					MotionKind::Range(s, e) => {
						deleted = self.buffer.drain(s..e).collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(s));
					}
					MotionKind::Null => return Ok(())
				}
				register.write_to_register(deleted);
			}
			Verb::DeleteChar(anchor) => {
				match anchor {
					Anchor::After => {
						let pos = self.num_or_len(self.cursor() + 1);
						self.buffer.remove(pos);
					}
					Anchor::Before => {
						let pos = self.cursor.saturating_sub(1);
						self.buffer.remove(pos);
						self.cursor = self.cursor.saturating_sub(1);
					}
				}
			}
			Verb::Yank => {
				let yanked;
				match motion {
					MotionKind::Forward(n) => {
						let fwd = self.num_or_len(self.cursor() + n);
						let cursor = self.cursor();
						yanked = self.buffer[cursor..=fwd]
							.iter()
							.cloned()
							.collect::<TermCharBuf>();
					}
					MotionKind::To(pos) => {
						let range = mk_range(self.cursor(), pos);
						yanked = self.buffer[range.clone()]
							.iter()
							.cloned()
							.collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(range.start));
					}
					MotionKind::Backward(n) => {
						let back = self.cursor.saturating_sub(n);
						let cursor = self.cursor();
						yanked = self.buffer[back..cursor]
							.iter()
							.cloned()
							.collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(back));
					}
					MotionKind::Range(s, e) => {
						yanked = self.buffer[s..e]
							.iter()
							.cloned()
							.collect::<TermCharBuf>();
						self.apply_motion(MotionKind::To(s));
					}
					MotionKind::Null => return Ok(())
				}
				register.write_to_register(yanked);
			}
			Verb::ReplaceChar(_) => todo!(),
			Verb::Substitute => todo!(),
			Verb::ToggleCase => todo!(),
			Verb::Complete => todo!(),
			Verb::CompleteBackward => todo!(),
			Verb::Undo => todo!(),
			Verb::RepeatLast => todo!(),
			Verb::Put(anchor) => {
				if let Some(charbuf) = register.read_from_register() {
					let chars = charbuf.0.into_iter();
					if anchor == Anchor::Before {
						self.cursor_back(1);
					}
					for char in chars {
						self.insert_at_cursor(char);
						self.cursor_fwd(1);
					}
				}
			}
			Verb::JoinLines => todo!(),
			Verb::InsertChar(ch) => {
				self.insert_at_cursor(ch);
				self.apply_motion(motion);
			}
			Verb::Insert(_) => todo!(),
			Verb::Breakline(anchor) => todo!(),
			Verb::Indent => todo!(),
			Verb::Dedent => todo!(),
			Verb::AcceptLine => todo!(),
			Verb::EndOfFile => {
				if self.buffer.is_empty() {
					sh_quit(0)
				} else {
					self.buffer.clear();
					self.cursor = 0;
				}
			}
			Verb::InsertMode |
			Verb::NormalMode |
			Verb::VisualMode |
			Verb::OverwriteMode => {
				self.apply_motion(motion);
			}
		}
		Ok(())
	}
	pub fn apply_motion(&mut self, motion: MotionKind) {
		match motion {
			MotionKind::Forward(n) => self.cursor_fwd(n),
			MotionKind::To(pos) => self.cursor_to(pos),
			MotionKind::Backward(n) => self.cursor_back(n),
			MotionKind::Range(s, _) => self.cursor_to(s), // TODO: not sure if this is correct in every case
			MotionKind::Null => { /* Pass */ }
		}
	}
	pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
		let ViCmd { register, verb_count, verb, motion_count, motion, .. } = cmd;

		for _ in 0..verb_count.unwrap_or(1) {
			for _ in 0..motion_count.unwrap_or(1) {
				let motion = motion
					.clone()
					.map(|m| self.eval_motion(m))
					.unwrap_or(MotionKind::Null);

				if let Some(verb) = verb.clone() {
					self.exec_verb(verb, motion, register)?;
				} else {
					self.apply_motion(motion);
				}
			}
		}

		self.clamp_cursor();
		Ok(())
	}
}

impl Display for LineBuf {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f,"{}",self.buffer)
	}
}

pub fn strip_ansi_codes_and_escapes(s: &str) -> String {
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
			match c {
				'\n' |
				'\r' => { /* Continue */ }
				_ => out.push(c)
			}
		}
	}
	out
}

fn mk_range(a: usize, b: usize) -> Range<usize> {
    std::cmp::min(a, b)..std::cmp::max(a, b)
}
