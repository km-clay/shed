use std::iter::Peekable;
use std::str::Chars;

use nix::NixPath;

use super::keys::{KeyEvent as E, KeyCode as K, ModKeys as M};
use super::linebuf::TermChar;
use super::vicmd::{Anchor, Bound, Dest, Direction, Motion, MotionBuilder, MotionCmd, RegisterName, TextObj, To, Verb, VerbBuilder, VerbCmd, ViCmd, Word};
use crate::prelude::*;

#[derive(Debug,Clone)]
pub enum CmdReplay {
	ModeReplay { cmds: Vec<ViCmd>, repeat: u16 },
	Single(ViCmd),
	Motion(Motion)
}

impl CmdReplay {
	pub fn mode(cmds: Vec<ViCmd>, repeat: u16) -> Self {
		Self::ModeReplay { cmds, repeat }
	}
	pub fn single(cmd: ViCmd) -> Self {
		Self::Single(cmd)
	}
	pub fn motion(motion: Motion) -> Self {
		Self::Motion(motion)
	}
}

pub enum CmdState {
	Pending,
	Complete,
	Invalid
}

pub trait ViMode {
	fn handle_key(&mut self, key: E) -> Option<ViCmd>;
	fn is_repeatable(&self) -> bool;
	fn as_replay(&self) -> Option<CmdReplay>;
	fn cursor_style(&self) -> String;
	fn pending_seq(&self) -> Option<String>;
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
				self.pending_cmd.set_verb(VerbCmd(1,Verb::InsertChar(ch)));
				self.pending_cmd.set_motion(MotionCmd(1,Motion::ForwardChar));
				self.register_and_return()
			}
			E(K::Char(ch), M::NONE) => {
				self.pending_cmd.set_verb(VerbCmd(1,Verb::InsertChar(TermChar::from(ch))));
				self.pending_cmd.set_motion(MotionCmd(1,Motion::ForwardChar));
				self.register_and_return()
			}
			E(K::Char('H'), M::CTRL) |
			E(K::Backspace, M::NONE) => {
				self.pending_cmd.set_verb(VerbCmd(1,Verb::Delete));
				self.pending_cmd.set_motion(MotionCmd(1,Motion::BackwardChar));
				self.register_and_return()
			}

			E(K::BackTab, M::NONE) => {
				self.pending_cmd.set_verb(VerbCmd(1,Verb::CompleteBackward));
				self.register_and_return()
			}

			E(K::Char('I'), M::CTRL) |
			E(K::Tab, M::NONE) => {
				self.pending_cmd.set_verb(VerbCmd(1,Verb::Complete));
				self.register_and_return()
			}

			E(K::Esc, M::NONE) => {
				self.pending_cmd.set_verb(VerbCmd(1,Verb::NormalMode));
				self.pending_cmd.set_motion(MotionCmd(1,Motion::BackwardChar));
				self.register_and_return()
			}
			_ => common_cmds(key)
		}
	}

	fn is_repeatable(&self) -> bool {
		true
	}

	fn as_replay(&self) -> Option<CmdReplay> {
		Some(CmdReplay::mode(self.cmds.clone(), self.repeat_count))
	}

	fn cursor_style(&self) -> String {
		"\x1b[6 q".to_string()
	}
	fn pending_seq(&self) -> Option<String> {
		None
	}
}

#[derive(Default,Debug)]
pub struct ViNormal {
	pending_seq: String,
}

impl ViNormal {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn clear_cmd(&mut self) {
		self.pending_seq = String::new();
	}
	pub fn take_cmd(&mut self) -> String {
		std::mem::take(&mut self.pending_seq)
	}
	fn validate_combination(&self, verb: Option<&Verb>, motion: Option<&Motion>) -> CmdState {
		if verb.is_none() {
			match motion {
				Some(Motion::TextObj(_,_)) => return CmdState::Invalid,
				Some(_) => return CmdState::Complete,
				None => return CmdState::Pending
			}
		}
		if verb.is_some() && motion.is_none() {
			match verb.unwrap() {
				Verb::Put(_) |
				Verb::DeleteChar(_) => CmdState::Complete,
				_ => CmdState::Pending
			}
		} else {
			CmdState::Complete
		}
	} 
	pub fn parse_count(&self, chars: &mut Peekable<Chars<'_>>) -> Option<usize> {
		let mut count = String::new();
		let Some(_digit @ '1'..='9') = chars.peek() else {
			return None
		};
		count.push(chars.next().unwrap());
		while let Some(_digit @ '0'..='9') = chars.peek() {
			count.push(chars.next().unwrap());
		}
		if !count.is_empty() {
			count.parse::<usize>().ok()
		} else {
			None
		}
	}
	/// End the parse and clear the pending sequence
	pub fn quit_parse(&mut self) -> Option<ViCmd> {
		flog!(WARN, "exiting parse early with sequence: {}",self.pending_seq);
		self.clear_cmd();
		None
	}
	pub fn try_parse(&mut self, ch: char) -> Option<ViCmd> {
		self.pending_seq.push(ch);
		flog!(DEBUG, self.pending_seq);
		let mut chars = self.pending_seq.chars().peekable();

		let register = 'reg_parse: {
			let mut chars_clone = chars.clone();
			let count = self.parse_count(&mut chars_clone);

			let Some('"') = chars_clone.next() else {
				break 'reg_parse RegisterName::default()
			};

			let Some(reg_name)  = chars_clone.next() else {
				return None // Pending register name
			};
			match reg_name {
				'a'..='z' |
				'A'..='Z' => { /* proceed */ }
				_ => return self.quit_parse()
			}

			chars = chars_clone;
			RegisterName::new(Some(reg_name), count)
		};

		let verb = 'verb_parse: {
			let mut chars_clone = chars.clone();
			let count = self.parse_count(&mut chars_clone).unwrap_or(1);

			let Some(ch) = chars_clone.next() else {
				break 'verb_parse None
			};
			flog!(DEBUG, "parsing verb char '{}'",ch);
			match ch {
				'.' => {
					return Some(
						ViCmd {
							register,
							verb: Some(VerbCmd(count, Verb::RepeatLast)),
							motion: None,
							raw_seq: self.take_cmd(),
						}
					)
				}
				'x' => {
					chars = chars_clone;
					break 'verb_parse Some(VerbCmd(count, Verb::DeleteChar(Anchor::After)));
				}
				'X' => {
					chars = chars_clone;
					break 'verb_parse Some(VerbCmd(count, Verb::DeleteChar(Anchor::Before)));
				}
				'p' => {
					chars = chars_clone;
					break 'verb_parse Some(VerbCmd(count, Verb::Put(Anchor::After)));
				}
				'P' => {
					chars = chars_clone;
					break 'verb_parse Some(VerbCmd(count, Verb::Put(Anchor::Before)));
				}
				'r' => {
					let Some(ch) = chars_clone.next() else {
						return None
					};
					return Some(
						ViCmd {
							register,
							verb: Some(VerbCmd(1, Verb::ReplaceChar(ch))),
							motion: Some(MotionCmd(count, Motion::ForwardChar)),
							raw_seq: self.take_cmd()
						}
					)
				}
				'u' => {
					return Some(
						ViCmd { 
							register,
							verb: Some(VerbCmd(count, Verb::Undo)),
							motion: None,
							raw_seq: self.take_cmd() 
						}
					)
				}
				'o' => {
					return Some(
						ViCmd { 
							register,
							verb: Some(VerbCmd(count, Verb::InsertModeLineBreak(Anchor::After))),
							motion: None,
							raw_seq: self.take_cmd() 
						}
					)
				}
				'O' => {
					return Some(
						ViCmd { 
							register,
							verb: Some(VerbCmd(count, Verb::InsertModeLineBreak(Anchor::Before))),
							motion: None,
							raw_seq: self.take_cmd() 
						}
					)
				}
				'a' => {
					return Some(
						ViCmd { 
							register,
							verb: Some(VerbCmd(count, Verb::InsertMode)),
							motion: Some(MotionCmd(1, Motion::ForwardChar)),
							raw_seq: self.take_cmd() 
						}
					)
				}
				'A' => {
					return Some(
						ViCmd { 
							register,
							verb: Some(VerbCmd(count, Verb::InsertMode)),
							motion: Some(MotionCmd(1, Motion::EndOfLine)),
							raw_seq: self.take_cmd() 
						}
					)
				}
				'i' => {
					return Some(
						ViCmd { 
							register,
							verb: Some(VerbCmd(count, Verb::InsertMode)),
							motion: None,
							raw_seq: self.take_cmd() 
						}
					)
				}
				'I' => {
					return Some(
						ViCmd { 
							register,
							verb: Some(VerbCmd(count, Verb::InsertMode)),
							motion: Some(MotionCmd(1, Motion::BeginningOfFirstWord)),
							raw_seq: self.take_cmd() 
						}
					)
				}
				'y' => {
					chars = chars_clone;
					break 'verb_parse Some(VerbCmd(count, Verb::Yank))
				}
				'd' => {
					chars = chars_clone;
					break 'verb_parse Some(VerbCmd(count, Verb::Delete))
				}
				'c' => {
					chars = chars_clone;
					break 'verb_parse Some(VerbCmd(count, Verb::Change))
				}
				'=' => {
					chars = chars_clone;
					break 'verb_parse Some(VerbCmd(count, Verb::Equalize))
				}
				_ => break 'verb_parse None
			}
		};

		let motion = 'motion_parse: {
			let mut chars_clone = chars.clone();
			let count = self.parse_count(&mut chars_clone).unwrap_or(1);

			let Some(ch) = chars_clone.next() else {
				break 'motion_parse None
			};
			flog!(DEBUG, "parsing motion char '{}'",ch);
			match (ch, &verb) {
				('d', Some(VerbCmd(_,Verb::Delete))) |
				('c', Some(VerbCmd(_,Verb::Change))) |
				('y', Some(VerbCmd(_,Verb::Yank))) |
				('>', Some(VerbCmd(_,Verb::Indent))) |
				('<', Some(VerbCmd(_,Verb::Dedent))) => break 'motion_parse Some(MotionCmd(count, Motion::WholeLine)),
				_ => {}
			}
			match ch {
				'g' => {
					if let Some(ch) = chars_clone.peek() {
						match ch {
							'g' => {
								chars_clone.next();
								chars = chars_clone;
								break 'motion_parse Some(MotionCmd(count, Motion::BeginningOfBuffer))
							}
							'e' => {
								chars = chars_clone;
								break 'motion_parse Some(MotionCmd(count, Motion::BackwardWord(To::End, Word::Normal)));
							}
							'E' => {
								chars = chars_clone;
								break 'motion_parse Some(MotionCmd(count, Motion::BackwardWord(To::End, Word::Big)));
							}
							_ => return self.quit_parse()
						}
					} else {
						break 'motion_parse None
					}
				}
				'f' => {
					let Some(ch) = chars_clone.peek() else {
						break 'motion_parse None
					};

					break 'motion_parse Some(MotionCmd(count, Motion::CharSearch(Direction::Forward, Dest::On, (*ch).into())))
				}
				'F' => {
					let Some(ch) = chars_clone.peek() else {
						break 'motion_parse None
					};

					break 'motion_parse Some(MotionCmd(count, Motion::CharSearch(Direction::Backward, Dest::On, (*ch).into())))
				}
				't' => {
					let Some(ch) = chars_clone.peek() else {
						break 'motion_parse None
					};

					break 'motion_parse Some(MotionCmd(count, Motion::CharSearch(Direction::Forward, Dest::Before, (*ch).into())))
				}
				'T' => {
					let Some(ch) = chars_clone.peek() else {
						break 'motion_parse None
					};

					break 'motion_parse Some(MotionCmd(count, Motion::CharSearch(Direction::Backward, Dest::Before, (*ch).into())))
				}
				';' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::RepeatMotion));
				}
				',' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::RepeatMotionRev));
				}
				'|' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(1, Motion::ToColumn(count)));
				}
				'0' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::BeginningOfLine));
				}
				'$' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::EndOfLine));
				}
				'k' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::LineUp));
				}
				'j' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::LineDown));
				}
				'h' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::BackwardChar));
				}
				'l' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::ForwardChar));
				}
				'w' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::ForwardWord(To::Start, Word::Normal)));
				}
				'W' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::ForwardWord(To::Start, Word::Big)));
				}
				'e' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::ForwardWord(To::End, Word::Normal)));
				}
				'E' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::ForwardWord(To::End, Word::Big)));
				}
				'b' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::BackwardWord(To::Start, Word::Normal)));
				}
				'B' => {
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::BackwardWord(To::Start, Word::Big)));
				}
				ch if ch == 'i' || ch == 'a' => {
					let bound = match ch {
						'i' => Bound::Inside,
						'a' => Bound::Around,
						_ => unreachable!()
					};
					if chars_clone.peek().is_none() {
						break 'motion_parse None
					}
					let obj = match chars_clone.next().unwrap() {
						'w' => TextObj::Word(Word::Normal),
						'W' => TextObj::Word(Word::Big),
						'"' => TextObj::DoubleQuote,
						'\'' => TextObj::SingleQuote,
						'(' | ')' | 'b' => TextObj::Paren,
						'{' | '}' | 'B' => TextObj::Brace,
						'[' | ']' => TextObj::Bracket,
						'<' | '>' => TextObj::Angle,
						_ => return self.quit_parse()
					};
					chars = chars_clone;
					break 'motion_parse Some(MotionCmd(count, Motion::TextObj(obj, bound)))
				}
				_ => return self.quit_parse(),
			}
		};

		if chars.peek().is_some() {
			flog!(WARN, "Unused characters in Vi command parse!");
			flog!(WARN, "{:?}",chars)
		}

		let verb_ref = verb.as_ref().map(|v| &v.1);
		let motion_ref = motion.as_ref().map(|m| &m.1);

		match self.validate_combination(verb_ref, motion_ref) {
			CmdState::Complete => {
				let cmd = Some(
					ViCmd {
						register,
						verb,
						motion,
						raw_seq: std::mem::take(&mut self.pending_seq)
					}
				);
				flog!(DEBUG, cmd);
				cmd
			}
			CmdState::Pending => {
				flog!(DEBUG, "pending sequence: {}", self.pending_seq);
				None
			}
			CmdState::Invalid => {
				flog!(DEBUG, "invalid sequence: {}",self.pending_seq);
				self.pending_seq.clear();
				None
			}
		}
	}
}

impl ViMode for ViNormal {
	fn handle_key(&mut self, key: E) -> Option<ViCmd> {
		flog!(DEBUG, key);
		match key {
			E(K::Char(ch), M::NONE) => self.try_parse(ch),
			E(K::Char('R'), M::CTRL) => {
				let mut chars = self.pending_seq.chars().peekable();
				let count = self.parse_count(&mut chars).unwrap_or(1);
				Some(
					ViCmd {
						register: RegisterName::default(),
						verb: Some(VerbCmd(count,Verb::Redo)),
						motion: None,
						raw_seq: self.take_cmd()
					}
				)
			}
			E(K::Esc, M::NONE) => {
				self.clear_cmd();
				None
			}
			_ => {
				if let Some(cmd) = common_cmds(key) {
					self.clear_cmd();
					Some(cmd)
				} else {
					None
				}
			}
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
	
	fn pending_seq(&self) -> Option<String> {
		Some(self.pending_seq.clone())
	}
}

pub fn common_cmds(key: E) -> Option<ViCmd> {
	let mut pending_cmd = ViCmd::new();
	match key {
		E(K::Home, M::NONE) => pending_cmd.set_motion(MotionCmd(1,Motion::BeginningOfLine)),
		E(K::End, M::NONE) => pending_cmd.set_motion(MotionCmd(1,Motion::EndOfLine)),
		E(K::Left, M::NONE) => pending_cmd.set_motion(MotionCmd(1,Motion::BackwardChar)),
		E(K::Right, M::NONE) => pending_cmd.set_motion(MotionCmd(1,Motion::ForwardChar)),
		E(K::Up, M::NONE) => pending_cmd.set_motion(MotionCmd(1,Motion::LineUp)),
		E(K::Down, M::NONE) => pending_cmd.set_motion(MotionCmd(1,Motion::LineDown)),
		E(K::Enter, M::NONE) => pending_cmd.set_verb(VerbCmd(1,Verb::AcceptLine)),
		E(K::Char('D'), M::CTRL) => pending_cmd.set_verb(VerbCmd(1,Verb::EndOfFile)),
		E(K::Delete, M::NONE) => pending_cmd.set_verb(VerbCmd(1,Verb::DeleteChar(Anchor::After))),
		E(K::Backspace, M::NONE) |
		E(K::Char('H'), M::CTRL) => pending_cmd.set_verb(VerbCmd(1,Verb::DeleteChar(Anchor::Before))),
		_ => return None
	}
	Some(pending_cmd)
}
