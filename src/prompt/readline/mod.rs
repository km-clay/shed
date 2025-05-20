use std::{arch::asm, os::fd::BorrowedFd};

use keys::KeyEvent;
use line::{strip_ansi_codes_and_escapes, LineBuf};
use linecmd::{Anchor, At, CharSearch, InputMode, LineCmd, MoveCmd, Movement, Verb, VerbCmd, ViCmd, ViCmdBuilder, Word};
use nix::{libc::STDIN_FILENO, sys::termios::{self, Termios}, unistd::read};
use term::Terminal;
use unicode_width::UnicodeWidthStr;

use crate::{libsh::{error::{ShErr, ShErrKind, ShResult}, sys::sh_quit}, prelude::*};
use linecmd::Repeat;
pub mod term;
pub mod line;
pub mod keys;
pub mod linecmd;

/// Add a verb to a specified ViCmdBuilder, then build it
///
/// Returns the built value as a LineCmd::ViCmd
macro_rules! build_verb {
	($cmd:expr,$verb:expr) => {{
		$cmd.with_verb($verb).build().map(|cmd| LineCmd::ViCmd(cmd))
	}}
}

/// Add a movement to a specified ViCmdBuilder, then build it
///
/// Returns the built value as a LineCmd::ViCmd
macro_rules! build_movement {
	($cmd:expr,$move:expr) => {{ 
		$cmd.with_movement($move).build().map(|cmd| LineCmd::ViCmd(cmd))
	}}
}

/// Add both a movement and a verb to a specified ViCmdBuilder, then build it
///
/// Returns the built value as a LineCmd::ViCmd
macro_rules! build_moveverb {
	($cmd:expr,$verb:expr,$move:expr) => {{
		$cmd.with_movement($move).with_verb($verb).build().map(|cmd| LineCmd::ViCmd(cmd))
	}}
}

#[derive(Default,Debug)]
pub struct FernReader {
	pub term: Terminal,
	pub prompt: String,
	pub line: LineBuf,
	pub edit_mode: InputMode,
	pub last_vicmd: Option<Repeat>,
}

impl FernReader {
	pub fn new(prompt: String) -> Self {
		let line = LineBuf::new().with_initial("The quick brown fox jumped over the lazy dog.");
		Self {
			term: Terminal::new(),
			prompt,
			line,
			edit_mode: Default::default(),
			last_vicmd: Default::default()
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
		let lines = self.line.display_lines();
		while let Some(line) = prompt_lines.next() {
			if prompt_lines.peek().is_none() {
				last_line_len = strip_ansi_codes_and_escapes(line).width();
				self.term.write(line);
			} else {
				self.term.writeln(line);
			}
		}
		let num_lines = lines.len();
		let mut lines_iter = lines.into_iter().peekable();

		while let Some(line) = lines_iter.next() {
			if lines_iter.peek().is_some() {
				self.term.writeln(&line);
			} else {
				self.term.write(&line);
			}
		}

		if num_lines == 1 {
			let cursor_offset = self.line.cursor() + last_line_len;
			self.term.write(&format!("\r\x1b[{}C", cursor_offset));
		} else {
			let (x, y) = self.line.cursor_display_coords();
			// Y-axis movements are 1-indexed and must move up from the bottom
			// Therefore, add 1 to Y and subtract that number from the number of lines
			// to find the number of times we have to push the cursor upward
			let y = num_lines.saturating_sub(y+1);
			if y > 0 {
				self.term.write(&format!("\r\x1b[{}A", y))
			}
			self.term.write(&format!("\r\x1b[{}C", x+2)); // Factor in the line bullet thing
		}
		match self.edit_mode {
			InputMode::Replace |
			InputMode::Insert => {
				self.term.write("\x1b[6 q")
			}
			InputMode::Normal |
			InputMode::Visual => {
				self.term.write("\x1b[2 q")
			}
		}
	}
	pub fn set_normal_mode(&mut self) {
		self.edit_mode = InputMode::Normal;
		self.line.finish_insert();
		let ins_text = self.line.take_ins_text();
		self.last_vicmd.as_mut().map(|cmd| cmd.set_ins_text(ins_text));
	}
	pub fn set_insert_mode(&mut self) {
		self.edit_mode = InputMode::Insert;
		self.line.begin_insert();
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
			E(K::Char(ch), M::NONE) => build_verb!(pending_cmd, Verb::InsertChar(ch))?,

			E(K::Char('H'), M::CTRL) |
			E(K::Backspace, M::NONE) => LineCmd::backspace(),

			E(K::BackTab, M::NONE) => LineCmd::CompleteBackward,

			E(K::Char('I'), M::CTRL) |
			E(K::Tab, M::NONE) => LineCmd::Complete,

			E(K::Esc, M::NONE) => {
				build_movement!(pending_cmd, Movement::BackwardChar)?
			}
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

		if let E(K::Char(ch), M::NONE) = key {
			if pending_cmd.movement().is_some_and(|m| matches!(m, Movement::CharSearch(_))) {
				let Movement::CharSearch(charsearch) = pending_cmd.movement().unwrap() else {unreachable!()};
				match charsearch {
					CharSearch::FindFwd(_) => {
						let finalized = CharSearch::FindFwd(Some(ch));
						return build_movement!(pending_cmd, Movement::CharSearch(finalized))
					}
					CharSearch::FwdTo(_) => {
						let finalized = CharSearch::FwdTo(Some(ch));
						return build_movement!(pending_cmd, Movement::CharSearch(finalized))
					}
					CharSearch::FindBkwd(_) => {
						let finalized = CharSearch::FindBkwd(Some(ch));
						return build_movement!(pending_cmd, Movement::CharSearch(finalized))
					}
					CharSearch::BkwdTo(_) => {
						let finalized = CharSearch::BkwdTo(Some(ch));
						return build_movement!(pending_cmd, Movement::CharSearch(finalized))
					}
				}
			}
		}

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
			E(K::Char('.'), M::NONE) => {
				match &self.last_vicmd {
					None => LineCmd::Null,
					Some(cmd) => {
						build_verb!(pending_cmd, Verb::Repeat(cmd.clone()))?
					}
				}
			}
			E(K::Char('j'), M::NONE) => LineCmd::LineDownOrNextHistory,
			E(K::Char('k'), M::NONE) => LineCmd::LineUpOrPreviousHistory,
			E(K::Char('D'), M::NONE) => build_moveverb!(pending_cmd,Verb::Delete,Movement::EndOfLine)?,
			E(K::Char('C'), M::NONE) => build_moveverb!(pending_cmd,Verb::Change,Movement::EndOfLine)?,
			E(K::Char('Y'), M::NONE) => build_moveverb!(pending_cmd,Verb::Yank,Movement::EndOfLine)?,
			E(K::Char('l'), M::NONE) => build_movement!(pending_cmd,Movement::ForwardChar)?,
			E(K::Char('w'), M::NONE) => build_movement!(pending_cmd,Movement::ForwardWord(At::Start, Word::Normal))?,
			E(K::Char('W'), M::NONE) => build_movement!(pending_cmd,Movement::ForwardWord(At::Start, Word::Big))?,
			E(K::Char('b'), M::NONE) => build_movement!(pending_cmd,Movement::BackwardWord(Word::Normal))?,
			E(K::Char('B'), M::NONE) => build_movement!(pending_cmd,Movement::BackwardWord(Word::Big))?,
			E(K::Char('e'), M::NONE) => build_movement!(pending_cmd,Movement::ForwardWord(At::BeforeEnd, Word::Normal))?,
			E(K::Char('E'), M::NONE) => build_movement!(pending_cmd,Movement::ForwardWord(At::BeforeEnd, Word::Big))?,
			E(K::Char('^'), M::NONE) => build_movement!(pending_cmd,Movement::BeginningOfFirstWord)?,
			E(K::Char('0'), M::NONE) => build_movement!(pending_cmd,Movement::BeginningOfLine)?,
			E(K::Char('$'), M::NONE) => build_movement!(pending_cmd,Movement::EndOfLine)?,
			E(K::Char('x'), M::NONE) => build_verb!(pending_cmd,Verb::DeleteOne(Anchor::After))?,
			E(K::Char('o'), M::NONE) => {
				self.set_insert_mode();
				build_verb!(pending_cmd,Verb::Breakline(Anchor::After))?
			}
			E(K::Char('O'), M::NONE) => {
				self.set_insert_mode();
				build_verb!(pending_cmd,Verb::Breakline(Anchor::Before))?
			}
			E(K::Char('i'), M::NONE) => {
				self.set_insert_mode();
				LineCmd::Null
			}
			E(K::Char('I'), M::NONE) => {
				self.set_insert_mode();
				build_movement!(pending_cmd,Movement::BeginningOfFirstWord)?
			}
			E(K::Char('a'), M::NONE) => {
				self.set_insert_mode();
				build_movement!(pending_cmd,Movement::ForwardChar)?
			}
			E(K::Char('A'), M::NONE) => {
				self.set_insert_mode();
				build_movement!(pending_cmd,Movement::EndOfLine)?
			}
			E(K::Char('c'), M::NONE) => {
				if pending_cmd.verb() == Some(&Verb::Change) {
					build_moveverb!(pending_cmd,Verb::Change,Movement::WholeLine)?
				} else {
					pending_cmd = pending_cmd.with_verb(Verb::Change);
					self.get_normal_cmd(pending_cmd)?
				}
			}
			E(K::Char('>'), M::NONE) => {
				if pending_cmd.verb() == Some(&Verb::Indent) {
					build_verb!(pending_cmd,Verb::Indent)?
				} else {
					pending_cmd = pending_cmd.with_verb(Verb::Indent);
					self.get_normal_cmd(pending_cmd)?
				}
			}
			E(K::Char('<'), M::NONE) => {
				if pending_cmd.verb() == Some(&Verb::Dedent) {
					build_verb!(pending_cmd,Verb::Dedent)?
				} else {
					pending_cmd = pending_cmd.with_verb(Verb::Dedent);
					self.get_normal_cmd(pending_cmd)?
				}
			}
			E(K::Char('d'), M::NONE) => {
				if pending_cmd.verb() == Some(&Verb::Delete) {
					LineCmd::ViCmd(pending_cmd.with_movement(Movement::WholeLine).build()?)
				} else {
					pending_cmd = pending_cmd.with_verb(Verb::Delete);
					self.get_normal_cmd(pending_cmd)?
				}
			}
			E(K::Char('f'), M::NONE) => {
				pending_cmd = pending_cmd.with_movement(Movement::CharSearch(CharSearch::FindFwd(None)));
				self.get_normal_cmd(pending_cmd)?
			}
			E(K::Char('F'), M::NONE) => {
				pending_cmd = pending_cmd.with_movement(Movement::CharSearch(CharSearch::FindBkwd(None)));
				self.get_normal_cmd(pending_cmd)?
			}
			E(K::Char('t'), M::NONE) => {
				pending_cmd = pending_cmd.with_movement(Movement::CharSearch(CharSearch::FwdTo(None)));
				self.get_normal_cmd(pending_cmd)?
			}
			E(K::Char('T'), M::NONE) => {
				pending_cmd = pending_cmd.with_movement(Movement::CharSearch(CharSearch::BkwdTo(None)));
				self.get_normal_cmd(pending_cmd)?
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
			E(K::Home, M::NONE) => build_movement!(pending_cmd,Movement::BeginningOfLine),
			E(K::End, M::NONE) => build_movement!(pending_cmd,Movement::EndOfLine),
			E(K::Left, M::NONE) => build_movement!(pending_cmd,Movement::BackwardChar),
			E(K::Right, M::NONE) => build_movement!(pending_cmd,Movement::ForwardChar),
			E(K::Delete, M::NONE) => build_moveverb!(pending_cmd,Verb::Delete,Movement::ForwardChar),
			E(K::Up, M::NONE) => Ok(LineCmd::LineUpOrPreviousHistory),
			E(K::Down, M::NONE) => Ok(LineCmd::LineDownOrNextHistory),
			E(K::Enter, M::NONE) => Ok(LineCmd::AcceptLine),
			E(K::Char('D'), M::CTRL) => Ok(LineCmd::EndOfFile),
			E(K::Backspace, M::NONE) |
			E(K::Char('h'), M::CTRL) => {
				Ok(LineCmd::backspace())
			}
			_ => Err(ShErr::simple(ShErrKind::ReadlineErr,format!("Unhandled common key event: {key:?}")))
		}
	}
	pub fn handle_repeat(&mut self, cmd: &ViCmd) -> ShResult<()> {
		Ok(())
	}
	pub fn exec_vi_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
		self.last_vicmd = Some(Repeat::from_cmd(cmd.clone()));
		match cmd {
			ViCmd::MoveVerb(verb_cmd, move_cmd) => {
				let VerbCmd { verb_count, verb } = verb_cmd;
				for _ in 0..verb_count {
					self.line.exec_vi_cmd(Some(verb.clone()), Some(move_cmd.clone()))?;
				}
				if verb == Verb::Change {
					self.set_insert_mode();
				}
			}
			ViCmd::Verb(verb_cmd) => {
				let VerbCmd { verb_count, verb } = verb_cmd;
				for _ in 0..verb_count {
					self.line.exec_vi_cmd(Some(verb.clone()), None)?;
				}
			}
			ViCmd::Move(move_cmd) => {
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

impl Drop for FernReader {
	fn drop(&mut self) {
		self.term.write("\x1b[2 q");
	}
}

