use std::{collections::HashMap, sync::Mutex};

use linebuf::{strip_ansi_codes_and_escapes, LineBuf, TermCharBuf};
use mode::{CmdReplay, ViInsert, ViMode, ViNormal};
use term::Terminal;
use unicode_width::UnicodeWidthStr;
use vicmd::{Verb, ViCmd};

use crate::libsh::{error::ShResult, term::{Style, Styled}};

pub mod keys;
pub mod term;
pub mod linebuf;
pub mod vicmd;
pub mod mode;
pub mod register;

pub struct FernVi {
	term: Terminal,
	line: LineBuf,
	prompt: String,
	mode: Box<dyn ViMode>,
	repeat_action: Option<CmdReplay>,
}

impl FernVi {
	pub fn new(prompt: Option<String>) -> Self {
		let prompt = prompt.unwrap_or("$ ".styled(Style::Green | Style::Bold));
		let line = LineBuf::new().with_initial("The quick brown fox jumps over the lazy dog");//\nThe quick brown fox jumps over the lazy dog\nThe quick brown fox jumps over the lazy dog\n");

		Self {
			term: Terminal::new(),
			line,
			prompt,
			mode: Box::new(ViInsert::new()),
			repeat_action: None,
		}
	}
	pub fn clear_line(&self) {
		let prompt_lines = self.prompt.lines().count();
		let buf_lines = if self.prompt.ends_with('\n') {
			self.line.count_lines()
		} else {
			// The prompt does not end with a newline, so one of the buffer's lines overlaps with it
			self.line.count_lines().saturating_sub(1) 
		};
		let total = prompt_lines + buf_lines;
		self.term.write_bytes(b"\r\n");
		for _ in 0..total {
			self.term.write_bytes(b"\r\x1b[2K\x1b[1A");
		}
		self.term.write_bytes(b"\r\x1b[2K");
	}
	pub fn print_buf(&self, refresh: bool) {
		if refresh {
			self.clear_line()
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

		let (x, y) = self.line.cursor_display_coords();
		let y = num_lines.saturating_sub(y + 1);

		if y > 0 {
			self.term.write(&format!("\r\x1b[{}A", y));
		}

		// Add prompt offset to X only if cursor is on the last line (y == 0)
		let cursor_x = if y == 0 { x + last_line_len } else { x };

		self.term.write(&format!("\r\x1b[{}C", cursor_x));
		self.term.write(&self.mode.cursor_style());
	}
	pub fn readline(&mut self) -> ShResult<String> {
		self.print_buf(false);
		loop {
			let key = self.term.read_key();
			let Some(cmd) = self.mode.handle_key(key) else {
				continue
			};

			if cmd.should_submit() {
				return Ok(self.line.to_string());
			}

			self.exec_cmd(cmd.clone())?;
			self.print_buf(true);
		}
	}
	pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
		if cmd.is_mode_transition() {
			let count = cmd.verb_count();
			let mut mode: Box<dyn ViMode> = match cmd.verb().unwrap() {
				Verb::InsertMode => Box::new(ViInsert::new().with_count(count)),
				Verb::NormalMode => Box::new(ViNormal::new()),
				Verb::VisualMode => todo!(),
				Verb::OverwriteMode => todo!(),
				_ => unreachable!()
			};

			std::mem::swap(&mut mode, &mut self.mode);
			self.term.write(&mode.cursor_style());

			if mode.is_repeatable() {
				self.repeat_action = mode.as_replay();
			}
		} 
		self.line.exec_cmd(cmd)?;
		Ok(())
	}
}
