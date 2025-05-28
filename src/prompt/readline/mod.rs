use std::time::Duration;

use linebuf::{strip_ansi_codes_and_escapes, LineBuf};
use mode::{CmdReplay, ViInsert, ViMode, ViNormal, ViReplace};
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

/// Unified interface for different line editing methods
pub trait Readline {
	fn readline(&mut self) -> ShResult<String>;
}

pub struct FernVi {
	term: Terminal,
	line: LineBuf,
	prompt: String,
	mode: Box<dyn ViMode>,
	last_action: Option<CmdReplay>,
	last_movement: Option<MotionCmd>,
}

impl Readline for FernVi {
	fn readline(&mut self) -> ShResult<String> {
		/*
		self.term.writeln("This is a line!");
		self.term.writeln("This is a line!");
		self.term.writeln("This is a line!");
		let prompt_thing = "prompt thing -> ";
		self.term.write(prompt_thing);
		let line = "And another!";
		let mut iters: usize = 0;
		let mut newlines_written = 0;
		loop {
			iters += 1;
			for i in 0..iters {
				self.term.writeln(line);
			}
			std::thread::sleep(Duration::from_secs(1));
			self.clear_lines(iters,prompt_thing.len() + 1);
		}
		panic!()
		*/
		self.print_buf(false)?;
		loop {

			let key = self.term.read_key();
			let Some(cmd) = self.mode.handle_key(key) else {
				continue
			};

			if cmd.should_submit() {
				self.term.write("\n");
				return Ok(self.line.to_string());
			}

			self.exec_cmd(cmd.clone())?;
			self.print_buf(true)?;
		}
	}
}

impl FernVi {
	pub fn new(prompt: Option<String>) -> Self {
		let prompt = prompt.unwrap_or("$ ".styled(Style::Green | Style::Bold));
		let line = LineBuf::new().with_initial("The quick brown fox jumps over the lazy dog");//\nThe quick brown fox jumps over the lazy dog\nThe quick brown fox jumps over the lazy dog\n");
		let term = Terminal::new();
		Self {
			term,
			line,
			prompt,
			mode: Box::new(ViInsert::new()),
			last_action: None,
			last_movement: None,
		}
	}
	pub fn print_buf(&mut self, refresh: bool) -> ShResult<()> {
		let (height,width) = self.term.get_dimensions()?;
		if refresh {
			self.term.unwrite()?;
		}
		let offset = self.calculate_prompt_offset();
		self.line.set_first_line_offset(offset);
		self.line.update_term_dims((height,width));
		let mut line_buf = self.prompt.clone();
		line_buf.push_str(self.line.as_str());

		self.term.recorded_write(&line_buf, offset)?;
		self.term.position_cursor(self.line.cursor_display_coords(width))?;

		self.term.write(&self.mode.cursor_style());
		Ok(())
	}
	pub fn calculate_prompt_offset(&self) -> usize {
		if self.prompt.ends_with('\n') {
			return 0
		}
		strip_ansi_codes_and_escapes(self.prompt.lines().last().unwrap_or_default()).width() + 1 // 1 indexed
	}
	pub fn exec_cmd(&mut self, cmd: ViCmd) -> ShResult<()> {
		if cmd.is_mode_transition() {
			let count = cmd.verb_count();
			let mut mode: Box<dyn ViMode> = match cmd.verb().unwrap().1 {
				Verb::InsertModeLineBreak(_) |
				Verb::Change |
				Verb::InsertMode => {
					Box::new(ViInsert::new().with_count(count as u16))
				}
				Verb::NormalMode => {
					Box::new(ViNormal::new())
				}
				Verb::ReplaceMode => {
					Box::new(ViReplace::new().with_count(count as u16))
				}
				Verb::VisualMode => todo!(),
				_ => unreachable!()
			};

			std::mem::swap(&mut mode, &mut self.mode);
			self.line.set_cursor_clamp(self.mode.clamp_cursor());
			self.line.set_move_cursor_on_undo(self.mode.move_cursor_on_undo());
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
