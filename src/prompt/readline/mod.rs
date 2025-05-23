use std::{collections::HashMap, sync::Mutex};

use linebuf::{strip_ansi_codes_and_escapes, LineBuf, TermCharBuf};
use mode::{CmdReplay, ViInsert, ViMode, ViNormal};
use term::Terminal;
use unicode_width::UnicodeWidthStr;
use vicmd::{Motion, MotionCmd, RegisterName, Verb, VerbCmd, ViCmd};

use crate::libsh::{error::{ShErr, ShErrKind, ShResult}, term::{Style, Styled}};
use crate::prelude::*;

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
	last_action: Option<CmdReplay>,
	last_movement: Option<MotionCmd>,
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
			last_action: None,
			last_movement: None,
		}
	}
	pub fn clear_line(&self) {
		let prompt_lines = self.prompt.lines().count();
		let last_line_len = strip_ansi_codes_and_escapes(self.prompt.lines().last().unwrap_or_default()).width();
		let buf_lines = if self.prompt.ends_with('\n') {
			self.line.count_lines(last_line_len)
		} else {
			// The prompt does not end with a newline, so one of the buffer's lines overlaps with it
			self.line.count_lines(last_line_len).saturating_sub(1) 
		};
		let total = prompt_lines + buf_lines;
		self.term.write_bytes(b"\r\n");
		self.term.write_bytes(format!("\r\x1b[{total}B").as_bytes());
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
		let lines = self.line.split_lines();
		while let Some(line) = prompt_lines.next() {
			if prompt_lines.peek().is_none() {
				last_line_len = strip_ansi_codes_and_escapes(line).width();
				self.term.write(line);
			} else {
				self.term.writeln(line);
			}
		}
		let mut lines_iter = lines.into_iter().peekable();

		let pos = self.term.cursor_pos();
		while let Some(line) = lines_iter.next() {
			if lines_iter.peek().is_some() {
				self.term.writeln(&line);
			} else {
				self.term.write(&line);
			}
		}
		self.term.move_cursor_to(pos);

		let (x, y) = self.line.cursor_display_coords(Some(last_line_len));

		if y > 0 {
			self.term.write(&format!("\r\x1b[{}B", y));
		}


		let cursor_x = if y == 0 { x + last_line_len } else { x };

		if cursor_x > 0 {
			self.term.write(&format!("\r\x1b[{}C", cursor_x));
		}
		self.term.write(&self.mode.cursor_style());
	}
	pub fn readline(&mut self) -> ShResult<String> {
		let dims = self.term.get_dimensions()?;
		self.line.update_term_dims(dims.0, dims.1);
		self.print_buf(false);
		loop {
			let dims = self.term.get_dimensions()?;
			self.line.update_term_dims(dims.0, dims.1);

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
			let mut mode: Box<dyn ViMode> = match cmd.verb().unwrap().1 {
				Verb::InsertModeLineBreak(_) |
				Verb::InsertMode => {
					self.line.set_cursor_clamp(false);
					Box::new(ViInsert::new().with_count(count as u16))
				}
				Verb::NormalMode => {
					self.line.set_cursor_clamp(true);
					Box::new(ViNormal::new())
				}
				Verb::VisualMode => todo!(),
				Verb::OverwriteMode => todo!(),
				_ => unreachable!()
			};

			std::mem::swap(&mut mode, &mut self.mode);
			self.term.write(&mode.cursor_style());

			if mode.is_repeatable() {
				self.last_action = mode.as_replay();
			}
			return self.line.exec_cmd(cmd);
		} else if cmd.is_cmd_repeat() {
			let Some(replay) = self.last_action.clone() else {
				return Ok(())
			};
			let ViCmd { verb, .. } = cmd;
			let VerbCmd(count,_) = verb.unwrap();
			match replay {
				CmdReplay::ModeReplay { cmds, mut repeat } => {
					if count > 1 {
						repeat = count as u16;
					}
					for _ in 0..repeat {
						let cmds = cmds.clone();
						for cmd in cmds {
							self.line.exec_cmd(cmd)?
						}
					}
				}
				CmdReplay::Single(mut cmd) => {
					if count > 1 {
						// Override the counts with the one passed to the '.' command
						if cmd.verb.is_some() {
							if let Some(v_mut) = cmd.verb.as_mut() {
								v_mut.0 = count
							}
							if let Some(m_mut) = cmd.motion.as_mut() {
								m_mut.0 = 0
							}
						} else {
							return Ok(()) // it has to have a verb to be repeatable, something weird happened
						}
					}
					self.line.exec_cmd(cmd)?;
				}
				_ => unreachable!("motions should be handled in the other branch")
			}
			return Ok(())
		} else if cmd.is_motion_repeat() {
			match cmd.motion.as_ref().unwrap() {
				MotionCmd(count,Motion::RepeatMotion) => {
					let Some(motion) = self.last_movement.clone() else {
						return Ok(())
					};
					let repeat_cmd = ViCmd {
						register: RegisterName::default(),
						verb: None,
						motion: Some(motion),
						raw_seq: format!("{count};")
					};
					return self.line.exec_cmd(repeat_cmd);
				}
				MotionCmd(count,Motion::RepeatMotionRev) => {
					let Some(motion) = self.last_movement.clone() else {
						return Ok(())
					};
					let mut new_motion = motion.invert_char_motion();
					new_motion.0 = *count;
					let repeat_cmd = ViCmd {
						register: RegisterName::default(),
						verb: None,
						motion: Some(new_motion),
						raw_seq: format!("{count},")
					};
					return self.line.exec_cmd(repeat_cmd);
				}
				_ => unreachable!()
			}
		}

		if cmd.is_repeatable() {
			self.last_action = Some(CmdReplay::Single(cmd.clone()));
		}
		if cmd.is_char_search() {
			self.last_movement = cmd.motion.clone()
		}

		self.line.exec_cmd(cmd.clone())
	}
}
