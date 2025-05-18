use std::{arch::asm, os::fd::BorrowedFd};

use line::{strip_ansi_codes, LineBuf};
use nix::{libc::STDIN_FILENO, sys::termios::{self, Termios}, unistd::read};
use term::Terminal;
use unicode_width::UnicodeWidthStr;

use crate::{libsh::{error::ShResult, sys::sh_quit}, prelude::*};
pub mod term;
pub mod line;

#[derive(Clone,Copy,Debug)]
pub enum Key {
	Char(char),
	Enter,
	Backspace,
	Delete,
	Esc,
	Up,
	Down,
	Left,
	Right,
	Ctrl(char),
	Unknown,
}

#[derive(Clone,Debug)]
pub enum EditAction {
	Return,
	Exit(i32),
	ClearTerm,
	ClearLine,
	Signal(i32),
	MoveCursorStart,
	MoveCursorEnd,
	MoveCursorLeft, // Ctrl + B
	MoveCursorRight, // Ctrl + F
	DelWordBack,
	DelFromCursor,
	Backspace, // The Ctrl+H version
	RedrawScreen,
	HistNext,
	HistPrev,
	InsMode(InsAction),
	NormMode(NormAction),
}

#[derive(Clone,Debug)]
pub enum InsAction {
	InsChar(char),
	Backspace, // The backspace version
	Delete,
	Esc,
	MoveLeft, // Left Arrow
	MoveRight, // Right Arrow
	MoveUp,
	MoveDown
}

#[derive(Clone,Debug)]
pub enum NormAction {
	Count(usize),
	Motion(Motion),
}

#[derive(Clone,Debug)]
pub enum Motion {
}

impl EditAction {
	pub fn is_return(&self) -> bool {
		matches!(self, Self::Return)
	}
}


#[derive(Default,Debug)]
pub struct FernReader {
	pub term: Terminal,
	pub prompt: String,
	pub line: LineBuf,
	pub edit_mode: EditMode
}

impl FernReader {
	pub fn new(prompt: String) -> Self {
		Self {
			term: Terminal::new(),
			prompt,
			line: Default::default(),
			edit_mode: Default::default()
		}
	}
	fn pack_line(&mut self) -> String {
		self.line
			.buffer
			.iter()
			.collect::<String>()
	}
	pub fn readline(&mut self) -> ShResult<String> {
		self.display_line(/*refresh: */ false);
		loop {
			let cmds = self.get_cmds();
			for cmd in &cmds {
				if cmd.is_return() {
					self.term.write_bytes(b"\r\n");
					return Ok(self.pack_line())
				}
			}
			self.process_cmds(cmds)?;
			self.display_line(/* refresh: */ true);
		}
	}
	pub fn process_cmds(&mut self, cmds: Vec<EditAction>) -> ShResult<()> {
		for cmd in cmds {
			match cmd {
				EditAction::Exit(code) => {
					self.term.write_bytes(b"\r\n");
					sh_quit(code)
				}
				EditAction::ClearTerm => self.term.clear(),
				EditAction::ClearLine => self.line.clear(),
				EditAction::Signal(sig) => todo!(),
				EditAction::MoveCursorStart => self.line.move_cursor_start(),
				EditAction::MoveCursorEnd => self.line.move_cursor_end(),
				EditAction::MoveCursorLeft => self.line.move_cursor_left(),
				EditAction::MoveCursorRight => self.line.move_cursor_right(),
				EditAction::DelWordBack => self.line.del_word_back(),
				EditAction::DelFromCursor => self.line.del_from_cursor(),
				EditAction::Backspace => self.line.backspace_at_cursor(),
				EditAction::RedrawScreen => self.term.clear(),
				EditAction::HistNext => todo!(),
				EditAction::HistPrev => todo!(),
				EditAction::InsMode(ins_action) => self.process_ins_cmd(ins_action)?,
				EditAction::NormMode(norm_action) => self.process_norm_cmd(norm_action)?,
				EditAction::Return => unreachable!(), // handled earlier
			}
		}

		Ok(())
	}
	pub fn process_ins_cmd(&mut self, cmd: InsAction) -> ShResult<()> {
		match cmd {
			InsAction::InsChar(ch) => self.line.insert_at_cursor(ch),
			InsAction::Backspace => self.line.backspace_at_cursor(),
			InsAction::Delete => self.line.del_at_cursor(),
			InsAction::Esc => todo!(),
			InsAction::MoveLeft => self.line.move_cursor_left(),
			InsAction::MoveRight => self.line.move_cursor_right(),
			InsAction::MoveUp => todo!(),
			InsAction::MoveDown => todo!(),
		}
		Ok(())
	}
	pub fn process_norm_cmd(&mut self, cmd: NormAction) -> ShResult<()> {
		match cmd {
			NormAction::Count(num) => todo!(),
			NormAction::Motion(motion) => todo!(),
		}
		Ok(())
	}
	pub fn get_cmds(&mut self) -> Vec<EditAction> {
		match self.edit_mode {
			EditMode::Normal => {
				let keys = self.read_keys_normal_mode();
				self.process_keys_normal_mode(keys)
			}
			EditMode::Insert => {
				let key = self.read_key().unwrap();
				self.process_key_insert_mode(key)
			}
		}
	}
	pub fn read_keys_normal_mode(&mut self) -> Vec<Key> {
		todo!()
	}
	pub fn process_keys_normal_mode(&mut self, keys: Vec<Key>) -> Vec<EditAction> {
		todo!()
	}
	pub fn process_key_insert_mode(&mut self, key: Key) -> Vec<EditAction> {
		match key {
			Key::Char(ch) => {
				vec![EditAction::InsMode(InsAction::InsChar(ch))]
			}
			Key::Enter => {
				vec![EditAction::Return]
			}
			Key::Backspace => {
				vec![EditAction::InsMode(InsAction::Backspace)]
			}
			Key::Delete => {
				vec![EditAction::InsMode(InsAction::Delete)]
			}
			Key::Esc => {
				vec![EditAction::InsMode(InsAction::Esc)]
			}
			Key::Up => {
				vec![EditAction::InsMode(InsAction::MoveUp)]
			}
			Key::Down => {
				vec![EditAction::InsMode(InsAction::MoveDown)]
			}
			Key::Left => {
				vec![EditAction::InsMode(InsAction::MoveLeft)]
			}
			Key::Right => {
				vec![EditAction::InsMode(InsAction::MoveRight)]
			}
			Key::Ctrl(ctrl) => self.process_ctrl(ctrl),
			Key::Unknown => unimplemented!("Unknown key received: {key:?}")
		}
	}
	pub fn process_ctrl(&mut self, ctrl: char) -> Vec<EditAction> {
		match ctrl {
			'D' => {
				if self.line.buffer.is_empty() {
					vec![EditAction::Exit(0)]
				} else {
					vec![EditAction::Return]
				}
			}
			'C' => {
				vec![EditAction::ClearLine]
			}
			'Z' => {
				vec![EditAction::Signal(20)] // SIGTSTP
			}
			'A' => {
				vec![EditAction::MoveCursorStart]
			}
			'E' => {
				vec![EditAction::MoveCursorEnd]
			}
			'B' => {
				vec![EditAction::MoveCursorLeft]
			}
			'F' => {
				vec![EditAction::MoveCursorRight]
			}
			'U' => {
				vec![EditAction::ClearLine]
			}
			'W' => {
				vec![EditAction::DelWordBack]
			}
			'K' => {
				vec![EditAction::DelFromCursor]
			}
			'H' => {
				vec![EditAction::Backspace]
			}
			'L' => {
				vec![EditAction::RedrawScreen]
			}
			'N' => {
				vec![EditAction::HistNext]
			}
			'P' => {
				vec![EditAction::HistPrev]
			}
			_ => unimplemented!("Unhandled control character: {ctrl}")
		}
	}
	fn clear_line(&self) {
		let prompt_lines = self.prompt.lines().count();
		let buf_lines = self.line.count_lines().saturating_sub(1); // One of the buffer's lines will overlap with the prompt. probably.
		let total = prompt_lines + buf_lines;
		self.term.write_bytes(b"\r\n");
		for _ in 0..total {
			self.term.write_bytes(b"\r\x1b[2K\x1b[1A");
		}
		self.term.write_bytes(b"\r\x1b[2K");
	}
	fn display_line(&mut self, refresh: bool) {
		if refresh {
			self.clear_line();
		}
		let mut prompt_lines = self.prompt.lines().peekable();
		let mut last_line_len = 0;
		while let Some(line) = prompt_lines.next() {
			if prompt_lines.peek().is_none() {
				last_line_len = strip_ansi_codes(line).width();
				self.term.write(line);
			} else {
				self.term.writeln(line);
			}
		}
		let line = self.pack_line();
		self.term.write(&line);

		let cursor_offset = self.line.cursor() + last_line_len;
		self.term.write(&format!("\r\x1b[{}C", cursor_offset));
	}
	fn read_key(&mut self) -> Option<Key> {
		let mut buf = [0; 4];

		let n = self.term.read_byte(&mut buf);
		if n == 0 {
			return None;
		}
		match buf[0] {
			b'\x1b' => {
				if n == 3 {
					match (buf[1], buf[2]) {
						(b'[', b'A') => Some(Key::Up),
						(b'[', b'B') => Some(Key::Down),
						(b'[', b'C') => Some(Key::Right),
						(b'[', b'D') => Some(Key::Left),
						_ => {
							flog!(WARN, "unhandled control seq: {},{}", buf[1] as char, buf[2] as char);
							Some(Key::Esc)
						}
					}
				} else if n == 4 {
					match (buf[1], buf[2], buf[3]) {
						(b'[', b'3', b'~') => Some(Key::Delete),
						_ => {
							flog!(WARN, "unhandled control seq: {},{},{}", buf[1] as char, buf[2] as char, buf[3] as char);
							Some(Key::Esc)
						}
					}
				} else {
					Some(Key::Esc)
				}
			}
			b'\r' | b'\n' => Some(Key::Enter),
			0x7f => Some(Key::Backspace),
			c if (c as char).is_ascii_control() => {
				let ctrl = (c ^ 0x40) as char;
				Some(Key::Ctrl(ctrl))
			}
			c => Some(Key::Char(c as char))
		}
	}
}

#[derive(Default,Debug)]
pub enum EditMode {
	Normal,
	#[default]
	Insert,
}

