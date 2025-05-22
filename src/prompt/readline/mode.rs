use super::keys::{KeyEvent as E, KeyCode as K, ModKeys as M};
use super::linebuf::TermChar;
use super::vicmd::{Anchor, Bound, Dest, Direction, Motion, MotionBuilder, TextObj, To, Verb, VerbBuilder, ViCmd, Word};

pub struct CmdReplay {
	cmds: Vec<ViCmd>,
	repeat: u16
}

impl CmdReplay {
	pub fn new(cmds: Vec<ViCmd>, repeat: u16) -> Self {
		Self { cmds, repeat }
	}
}

pub trait ViMode {
	fn handle_key(&mut self, key: E) -> Option<ViCmd>;
	fn is_repeatable(&self) -> bool;
	fn as_replay(&self) -> Option<CmdReplay>;
	fn cursor_style(&self) -> String;
}

#[derive(Default,Debug)]
pub struct ViInsert {
	cmds: Vec<ViCmd>,
	pending_cmd: ViCmd,
	repeat_count: u16
}

impl ViInsert {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn with_count(mut self, repeat_count: u16) -> Self {
		self.repeat_count = repeat_count;
		self
	}
	pub fn register_and_return(&mut self) -> Option<ViCmd> {
		let cmd = self.take_cmd();
		self.register_cmd(&cmd);
		return Some(cmd)
	}
	pub fn register_cmd(&mut self, cmd: &ViCmd) {
		self.cmds.push(cmd.clone())
	}
	pub fn take_cmd(&mut self) -> ViCmd {
		std::mem::take(&mut self.pending_cmd)
	}
}

impl ViMode for ViInsert {
	fn handle_key(&mut self, key: E) -> Option<ViCmd> {
		match key {
			E(K::Grapheme(ch), M::NONE) => {
				let ch = TermChar::from(ch);
				self.pending_cmd.set_verb(Verb::InsertChar(ch));
				self.pending_cmd.set_motion(Motion::ForwardChar);
				self.register_and_return()
			}
			E(K::Char(ch), M::NONE) => {
				self.pending_cmd.set_verb(Verb::InsertChar(TermChar::from(ch)));
				self.pending_cmd.set_motion(Motion::ForwardChar);
				self.register_and_return()
			}
			E(K::Char('H'), M::CTRL) |
			E(K::Backspace, M::NONE) => {
				self.pending_cmd.set_verb(Verb::Delete);
				self.pending_cmd.set_motion(Motion::BackwardChar);
				self.register_and_return()
			}

			E(K::BackTab, M::NONE) => {
				self.pending_cmd.set_verb(Verb::CompleteBackward);
				self.register_and_return()
			}

			E(K::Char('I'), M::CTRL) |
			E(K::Tab, M::NONE) => {
				self.pending_cmd.set_verb(Verb::Complete);
				self.register_and_return()
			}

			E(K::Esc, M::NONE) => {
				self.pending_cmd.set_verb(Verb::NormalMode);
				self.pending_cmd.set_motion(Motion::BackwardChar);
				self.register_and_return()
			}
			_ => common_cmds(key)
		}
	}

	fn is_repeatable(&self) -> bool {
		true
	}

	fn as_replay(&self) -> Option<CmdReplay> {
		Some(CmdReplay::new(self.cmds.clone(), self.repeat_count))
	}

	fn cursor_style(&self) -> String {
		"\x1b[6 q".to_string()
	}
}

#[derive(Default,Debug)]
pub struct ViNormal {
	pending_cmd: ViCmd,
}

impl ViNormal {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn take_cmd(&mut self) -> ViCmd {
		std::mem::take(&mut self.pending_cmd)
	}
	pub fn clear_cmd(&mut self) {
		self.pending_cmd = ViCmd::new();
	}
	fn handle_pending_builder(&mut self, key: E) -> Option<ViCmd> {
		if self.pending_cmd.wants_register {
			if let E(K::Char(ch @ ('a'..='z' | 'A'..='Z')), M::NONE) = key {
				self.pending_cmd.set_register(ch);
				return None
			} else {
				self.clear_cmd();
				return None
			}
		} else if let Some(Verb::Builder(_)) = &self.pending_cmd.verb {
			todo!() // Don't have any verb builders yet, but might later
		} else if let Some(Motion::Builder(builder)) = self.pending_cmd.motion.clone() {
			match builder {
				MotionBuilder::CharSearch(direction, dest, _) => {
					if let E(K::Char(ch), M::NONE) = key {
						self.pending_cmd.set_motion(Motion::CharSearch(
								direction.unwrap(),
								dest.unwrap(),
								ch.into(),
						));
						return Some(self.take_cmd());
					} else {
						self.clear_cmd();
						return None;
					}
				}
				MotionBuilder::TextObj(_, bound) => {
					if let Some(bound) = bound {
						if let E(K::Char(ch), M::NONE) = key {
							let obj = match ch {
								'w' => TextObj::Word(Word::Normal),
								'W' => TextObj::Word(Word::Big),
								'(' | ')' => TextObj::Paren,
								'[' | ']' => TextObj::Bracket,
								'{' | '}' => TextObj::Brace,
								'<' | '>' => TextObj::Angle,
								'"'       => TextObj::DoubleQuote,
								'\''      => TextObj::SingleQuote,
								'`'       => TextObj::BacktickQuote,
								_         => TextObj::Custom(ch),
							};
							self.pending_cmd.set_motion(Motion::TextObj(obj, bound));
							return Some(self.take_cmd());
						} else {
							self.clear_cmd();
							return None;
						}
					} else if let E(K::Char(ch), M::NONE) = key {
						let bound = match ch {
							'i' => Bound::Inside,
							'a' => Bound::Around,
							_ => {
								self.clear_cmd();
								return None;
							}
						};
						self.pending_cmd.set_motion(Motion::Builder(MotionBuilder::TextObj(None, Some(bound))));
						return None;
					} else {
						self.clear_cmd();
						return None;
					}
				}
			}
		}
		None
	}
}

impl ViMode for ViNormal {
	fn handle_key(&mut self, key: E) -> Option<ViCmd> {
		if let E(K::Char(ch),M::NONE) = key {
			self.pending_cmd.append_seq_char(ch);
		}
		if self.pending_cmd.is_building() {
			return self.handle_pending_builder(key)
		}
		match key {
			E(K::Char(digit @ '0'..='9'), M::NONE) => self.pending_cmd.append_digit(digit),
			E(K::Char('"'),M::NONE)  => {
				if self.pending_cmd.is_empty() {
					if self.pending_cmd.register().name().is_none() {
						self.pending_cmd.wants_register = true;
					} else {
						self.clear_cmd();
					}
				} else {
					self.clear_cmd();
				}
				return None
			}
			E(K::Char('i'),M::NONE) if self.pending_cmd.verb().is_some() => {
				self.pending_cmd.set_motion(Motion::Builder(MotionBuilder::TextObj(None, Some(Bound::Inside))));
			}
			E(K::Char('a'),M::NONE) if self.pending_cmd.verb().is_some() => {
				self.pending_cmd.set_motion(Motion::Builder(MotionBuilder::TextObj(None, Some(Bound::Around))));
			}
			E(K::Char('h'),M::NONE) => self.pending_cmd.set_motion(Motion::BackwardChar),
			E(K::Char('j'),M::NONE) => self.pending_cmd.set_motion(Motion::LineDown),
			E(K::Char('k'),M::NONE) => self.pending_cmd.set_motion(Motion::LineUp),
			E(K::Char('l'),M::NONE) => self.pending_cmd.set_motion(Motion::ForwardChar),
			E(K::Char('w'),M::NONE) => self.pending_cmd.set_motion(Motion::ForwardWord(To::Start, Word::Normal)),
			E(K::Char('W'),M::NONE) => self.pending_cmd.set_motion(Motion::ForwardWord(To::Start, Word::Big)),
			E(K::Char('e'),M::NONE) => self.pending_cmd.set_motion(Motion::ForwardWord(To::End, Word::Normal)),
			E(K::Char('E'),M::NONE) => self.pending_cmd.set_motion(Motion::ForwardWord(To::End, Word::Big)),
			E(K::Char('b'),M::NONE) => self.pending_cmd.set_motion(Motion::BackwardWord(Word::Normal)),
			E(K::Char('B'),M::NONE) => self.pending_cmd.set_motion(Motion::BackwardWord(Word::Big)),
			E(K::Char('x'),M::NONE) => self.pending_cmd.set_verb(Verb::DeleteChar(Anchor::After)),
			E(K::Char('X'),M::NONE) => self.pending_cmd.set_verb(Verb::DeleteChar(Anchor::Before)),
			E(K::Char('d'),M::NONE) => {
				if self.pending_cmd.verb().is_none() {
					self.pending_cmd.set_verb(Verb::Delete)
				} else if let Some(verb) = self.pending_cmd.verb() {
					if verb == &Verb::Delete {
						self.pending_cmd.set_motion(Motion::WholeLine);
					} else {
						self.clear_cmd();
					}
				}
			}
			E(K::Char('c'),M::NONE) => {
				if self.pending_cmd.verb().is_none() {
					self.pending_cmd.set_verb(Verb::Change)
				} else if let Some(verb) = self.pending_cmd.verb() {
					if verb == &Verb::Change {
						self.pending_cmd.set_motion(Motion::WholeLine);
					} else {
						self.clear_cmd();
					}
				}
			}
			E(K::Char('y'),M::NONE) => {
				if self.pending_cmd.verb().is_none() {
					self.pending_cmd.set_verb(Verb::Yank)
				} else if let Some(verb) = self.pending_cmd.verb() {
					if verb == &Verb::Yank {
						self.pending_cmd.set_motion(Motion::WholeLine);
					} else {
						self.clear_cmd();
					}
				}
			}
			E(K::Char('p'),M::NONE) => self.pending_cmd.set_verb(Verb::Put(Anchor::After)),
			E(K::Char('P'),M::NONE) => self.pending_cmd.set_verb(Verb::Put(Anchor::Before)),
			E(K::Char('D'),M::NONE) => {
				self.pending_cmd.set_verb(Verb::Delete);
				self.pending_cmd.set_motion(Motion::EndOfLine);
			}
			E(K::Char('f'),M::NONE) => {
				let builder = MotionBuilder::CharSearch(
					Some(Direction::Forward),
					Some(Dest::On),
					None
				);
				self.pending_cmd.set_motion(Motion::Builder(builder));
			}
			E(K::Char('F'),M::NONE) => {
				let builder = MotionBuilder::CharSearch(
					Some(Direction::Backward),
					Some(Dest::On),
					None
				);
				self.pending_cmd.set_motion(Motion::Builder(builder));
			}
			E(K::Char('t'),M::NONE) => {
				let builder = MotionBuilder::CharSearch(
					Some(Direction::Forward),
					Some(Dest::Before),
					None
				);
				self.pending_cmd.set_motion(Motion::Builder(builder));
			}
			E(K::Char('T'),M::NONE) => {
				let builder = MotionBuilder::CharSearch(
					Some(Direction::Backward),
					Some(Dest::Before),
					None
				);
				self.pending_cmd.set_motion(Motion::Builder(builder));
			}
			E(K::Char('i'),M::NONE) => {
				self.pending_cmd.set_verb(Verb::InsertMode);
			}
			E(K::Char('I'),M::NONE) => {
				self.pending_cmd.set_verb(Verb::InsertMode);
				self.pending_cmd.set_motion(Motion::BeginningOfFirstWord);
			}
			E(K::Char('a'),M::NONE) => {
				self.pending_cmd.set_verb(Verb::InsertMode);
				self.pending_cmd.set_motion(Motion::ForwardChar);
			}
			E(K::Char('A'),M::NONE) => {
				self.pending_cmd.set_verb(Verb::InsertMode);
				self.pending_cmd.set_motion(Motion::EndOfLine);
			}
			_ => return common_cmds(key)
		}
		if self.pending_cmd.is_complete() {
			Some(self.take_cmd())
		} else {
			None
		}
	}

	fn is_repeatable(&self) -> bool {
		false
	}

	fn as_replay(&self) -> Option<CmdReplay> {
		None
	}

	fn cursor_style(&self) -> String {
		"\x1b[2 q".to_string()
	}
}

pub fn common_cmds(key: E) -> Option<ViCmd> {
	let mut pending_cmd = ViCmd::new();
	match key {
		E(K::Home, M::NONE) => pending_cmd.set_motion(Motion::BeginningOfLine),
		E(K::End, M::NONE) => pending_cmd.set_motion(Motion::EndOfLine),
		E(K::Left, M::NONE) => pending_cmd.set_motion(Motion::BackwardChar),
		E(K::Right, M::NONE) => pending_cmd.set_motion(Motion::ForwardChar),
		E(K::Up, M::NONE) => pending_cmd.set_motion(Motion::LineUp),
		E(K::Down, M::NONE) => pending_cmd.set_motion(Motion::LineDown),
		E(K::Enter, M::NONE) => pending_cmd.set_verb(Verb::AcceptLine),
		E(K::Char('D'), M::CTRL) => pending_cmd.set_verb(Verb::EndOfFile),
		E(K::Backspace, M::NONE) |
		E(K::Char('H'), M::CTRL) => {
			pending_cmd.set_verb(Verb::Delete);
			pending_cmd.set_motion(Motion::BackwardChar);
		}
		E(K::Delete, M::NONE) => {
			pending_cmd.set_verb(Verb::Delete);
			pending_cmd.set_motion(Motion::ForwardChar);
		}
		_ => return None
	}
	Some(pending_cmd)
}
