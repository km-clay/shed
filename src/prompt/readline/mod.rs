use std::{arch::asm, os::fd::BorrowedFd};

use keys::KeyEvent;
use line::{strip_ansi_codes, LineBuf};
use linecmd::{Anchor, At, InputMode, LineCmd, MoveCmd, Movement, Verb, VerbCmd, ViCmd, ViCmdBuilder, Word};
use nix::{libc::STDIN_FILENO, sys::termios::{self, Termios}, unistd::read};
use term::Terminal;
use unicode_width::UnicodeWidthStr;

use crate::{libsh::{error::{ShErr, ShErrKind, ShResult}, sys::sh_quit}, prelude::*};
pub mod term;
pub mod line;
pub mod keys;
pub mod linecmd;

#[derive(Default,Debug)]
pub struct FernReader {
	pub term: Terminal,
	pub prompt: String,
	pub line: LineBuf,
	pub edit_mode: InputMode,
	pub count_arg: u16,
	pub last_effect: Option<VerbCmd>,
	pub last_movement: Option<MoveCmd>
}

impl FernReader {
	pub fn new(prompt: String) -> Self {
		let line = LineBuf::new().with_initial("The quick brown fox jumped over the lazy dog.");
		Self {
			term: Terminal::new(),
			prompt,
			line,
			edit_mode: Default::default(),
			count_arg: Default::default(),
			last_effect: Default::default(),
			last_movement: Default::default(),
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
			let cmd = self.next_cmd()?;
			if cmd == LineCmd::AcceptLine {
				return Ok(self.pack_line())
			}
			self.execute_cmd(cmd)?;
			self.display_line(/* refresh: */ true);
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
	pub fn next_cmd(&mut self) -> ShResult<LineCmd> {
		let vi_cmd = ViCmdBuilder::new();
		match self.edit_mode {
			InputMode::Normal => self.get_normal_cmd(vi_cmd),
			InputMode::Insert => self.get_insert_cmd(vi_cmd),
			InputMode::Visual => todo!(),
			InputMode::Replace => todo!(),
		}
	}
	pub fn get_insert_cmd(&mut self, pending_cmd: ViCmdBuilder) -> ShResult<LineCmd> {
		use keys::{KeyEvent as E, KeyCode as K, ModKeys as M};
		let key = self.term.read_key();
		let cmd = match key {
			E(K::Char(ch), M::NONE) => {
				let cmd = pending_cmd
					.with_verb(Verb::InsertChar(ch))
					.build()?;
				LineCmd::ViCmd(cmd)
			}

			E(K::Char('H'), M::CTRL) |
			E(K::Backspace, M::NONE) => LineCmd::backspace(),

			E(K::BackTab, M::NONE) => LineCmd::CompleteBackward,

			E(K::Char('I'), M::CTRL) |
			E(K::Tab, M::NONE) => LineCmd::Complete,

			E(K::Esc, M::NONE) => {
				self.edit_mode = InputMode::Normal;
				let cmd = pending_cmd
					.with_movement(Movement::BackwardChar)
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			E(K::Char('D'), M::CTRL) => LineCmd::EndOfFile,
			_ => {
				flog!(INFO, "unhandled key in get_insert_cmd, trying common_cmd...");
				return self.common_cmd(key, pending_cmd)
			}
		};
		Ok(cmd)
	}

	pub fn get_normal_cmd(&mut self, mut pending_cmd: ViCmdBuilder) -> ShResult<LineCmd> {
		use keys::{KeyEvent as E, KeyCode as K, ModKeys as M};
		let key = self.term.read_key();
		if let E(K::Char(digit @ '0'..='9'), M::NONE) = key {
			pending_cmd.append_digit(digit);
			return self.get_normal_cmd(pending_cmd);
		}
		let cmd = match key {
			E(K::Char('h'), M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::BackwardChar)
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			E(K::Char('j'), M::NONE) => LineCmd::LineDownOrNextHistory,
			E(K::Char('k'), M::NONE) => LineCmd::LineUpOrPreviousHistory,
			E(K::Char('l'), M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::ForwardChar)
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			E(K::Char('w'), M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::ForwardWord(At::Start, Word::Normal))
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			E(K::Char('W'), M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::ForwardWord(At::Start, Word::Big))
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			E(K::Char('b'), M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::BackwardWord(Word::Normal))
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			E(K::Char('B'), M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::BackwardWord(Word::Big))
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			E(K::Char('x'), M::NONE) => {
				let cmd = pending_cmd
					.with_verb(Verb::DeleteOne(Anchor::After))
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			E(K::Char('i'), M::NONE) => {
				self.edit_mode = InputMode::Insert;
				let cmd = pending_cmd
					.with_movement(Movement::BackwardChar)
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			E(K::Char('I'), M::NONE) => {
				self.edit_mode = InputMode::Insert;
				let cmd = pending_cmd
					.with_movement(Movement::BeginningOfFirstWord)
					.build()?;
				LineCmd::ViCmd(cmd)
			}
			_ => {
				flog!(INFO, "unhandled key in get_normal_cmd, trying common_cmd...");
				return self.common_cmd(key, pending_cmd)
			}
		};
		Ok(cmd)
	}

	pub fn common_cmd(&mut self, key: KeyEvent, pending_cmd: ViCmdBuilder) -> ShResult<LineCmd> {
		use keys::{KeyEvent as E, KeyCode as K, ModKeys as M};
		match key {
			E(K::Home, M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::BeginningOfLine)
					.build()?;
				Ok(LineCmd::ViCmd(cmd))
			}
			E(K::End, M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::EndOfLine)
					.build()?;
				Ok(LineCmd::ViCmd(cmd))
			}
			E(K::Left, M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::BackwardChar)
					.build()?;
				Ok(LineCmd::ViCmd(cmd))
			}
			E(K::Right, M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::ForwardChar)
					.build()?;
				Ok(LineCmd::ViCmd(cmd))
			}
			E(K::Delete, M::NONE) => {
				let cmd = pending_cmd
					.with_movement(Movement::ForwardChar)
					.with_verb(Verb::Delete)
					.build()?;
				Ok(LineCmd::ViCmd(cmd))
			}
			E(K::Backspace, M::NONE) |
			E(K::Char('h'), M::CTRL) => {
				Ok(LineCmd::backspace())
			}
			E(K::Up, M::NONE) => Ok(LineCmd::LineUpOrPreviousHistory),
			E(K::Down, M::NONE) => Ok(LineCmd::LineDownOrNextHistory),
			E(K::Enter, M::NONE) => Ok(LineCmd::AcceptLine),
			_ => Err(ShErr::simple(ShErrKind::ReadlineErr,format!("Unhandled common key event: {key:?}")))
		}
	}
	pub fn exec_vi_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
		match cmd {
			ViCmd::MoveVerb(verb_cmd, move_cmd) => {
				self.last_effect = Some(verb_cmd.clone());
				self.last_movement = Some(move_cmd.clone());
				let VerbCmd { verb_count, verb } = verb_cmd;
				for _ in 0..verb_count {
					self.line.exec_vi_cmd(Some(verb.clone()), Some(move_cmd.clone()))?;
				}
			}
			ViCmd::Verb(verb_cmd) => {
				self.last_effect = Some(verb_cmd.clone());
				let VerbCmd { verb_count, verb } = verb_cmd;
				for _ in 0..verb_count {
					self.line.exec_vi_cmd(Some(verb.clone()), None)?;
				}
			}
			ViCmd::Move(move_cmd) => {
				self.last_movement = Some(move_cmd.clone());
				self.line.exec_vi_cmd(None, Some(move_cmd))?;
			}
		}
		Ok(())
	}
	pub fn execute_cmd(&mut self, cmd: LineCmd) -> ShResult<()> {
		match cmd {
			LineCmd::ViCmd(cmd) => self.exec_vi_cmd(cmd)?,
			LineCmd::Abort => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::BeginningOfHistory => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::CapitalizeWord => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::ClearScreen => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Complete => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::CompleteBackward => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::CompleteHint => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::DowncaseWord => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::EndOfFile => {
				if self.line.buffer.is_empty() {
					sh_quit(0);
				}  else {
					self.line.clear();
				}
			}
			LineCmd::EndOfHistory => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::ForwardSearchHistory => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::HistorySearchBackward => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::HistorySearchForward => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Insert(_) => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Interrupt => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Move(_) => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::NextHistory => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Noop => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Repaint => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Overwrite(ch) => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::PreviousHistory => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::QuotedInsert => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::ReverseSearchHistory => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Suspend => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::TransposeChars => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::TransposeWords => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Unknown => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::YankPop => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::LineUpOrPreviousHistory => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::LineDownOrNextHistory => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Newline => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::AcceptOrInsertLine { .. } => todo!("Unhandled cmd: {cmd:?}"),
			LineCmd::Null => { /* Pass */ }
			_ => todo!("Unhandled cmd: {cmd:?}"),
		}
		Ok(())
	}
}

