use keys::{KeyCode, KeyEvent, ModKeys};
use linebuf::{LineBuf, SelectAnchor, SelectMode};
use nix::libc::STDOUT_FILENO;
use term::{Layout, LineWriter, TermReader};
use vicmd::{Motion, MotionCmd, RegisterName, Verb, VerbCmd, ViCmd};
use vimode::{CmdReplay, ModeReport, ViInsert, ViMode, ViNormal, ViReplace, ViVisual};

use crate::libsh::{error::ShResult, sys::sh_quit, term::{Style, Styled}};
use crate::prelude::*;

pub mod term;
pub mod linebuf;
pub mod layout;
pub mod keys;
pub mod vicmd;
pub mod register;
pub mod vimode;

pub trait Readline {
	fn readline(&mut self) -> ShResult<String>;
}

pub struct FernVi {
	reader: TermReader,
	writer: LineWriter,
	prompt: String,
	mode: Box<dyn ViMode>,
	old_layout: Option<Layout>,
	repeat_action: Option<CmdReplay>,
	repeat_motion: Option<MotionCmd>,
	editor: LineBuf
}

impl Readline for FernVi {
	fn readline(&mut self) -> ShResult<String> {
		self.editor = LineBuf::new().with_initial("\nThe quick brown fox jumps over\n the lazy dogThe quick\nbrown fox jumps over the a", 1004);
		let raw_mode_guard = self.reader.raw_mode(); // Restores termios state on drop

		loop {
			let new_layout = self.get_layout();
			if let Some(layout) = self.old_layout.as_ref() {
				self.writer.clear_rows(layout)?;
			}
			raw_mode_guard.disable_for(|| self.print_line(new_layout))?;
			let key = self.reader.read_key()?;
			flog!(DEBUG, key);

			let Some(mut cmd) = self.mode.handle_key(key) else {
				continue
			};
			cmd.alter_line_motion_if_no_verb();

			if cmd.should_submit() {
				raw_mode_guard.disable_for(|| self.writer.flush_write("\n"))?;
				return Ok(std::mem::take(&mut self.editor.buffer))
			}

			if cmd.verb().is_some_and(|v| v.1 == Verb::EndOfFile) {
				if self.editor.buffer.is_empty() {
					std::mem::drop(raw_mode_guard);
					sh_quit(0);
				} else {
					self.editor.buffer.clear();
					continue
				}
			}
			flog!(DEBUG,cmd);

			self.exec_cmd(cmd)?;

		}
	}
}

impl Default for FernVi {
	fn default() -> Self {
	  Self::new(None)
	}
}

impl FernVi {
	pub fn new(prompt: Option<String>) -> Self {
		Self {
			reader: TermReader::new(),
			writer: LineWriter::new(STDOUT_FILENO),
			prompt: prompt.unwrap_or("$ ".styled(Style::Green)),
			mode: Box::new(ViInsert::new()),
			old_layout: None,
			repeat_action: None,
			repeat_motion: None,
			editor: LineBuf::new()
		}
	}

	pub fn get_layout(&mut self) -> Layout {
		let line = self.editor.as_str().to_string();
		let to_cursor = self.editor.slice_to_cursor().unwrap();
		self.writer.get_layout_from_parts(&self.prompt, to_cursor, &line)
	}

	pub fn print_line(&mut self, new_layout: Layout) -> ShResult<()> {

		self.writer.redraw(
			&self.prompt,
			&self.editor,
			&new_layout
		)?;

		self.writer.flush_write(&self.mode.cursor_style())?;

		self.old_layout = Some(new_layout);
		Ok(())
	}

	pub fn exec_cmd(&mut self, mut cmd: ViCmd) -> ShResult<()> {
		let mut selecting = false;
		if cmd.is_mode_transition() {
			let count = cmd.verb_count();
			let mut mode: Box<dyn ViMode> = match cmd.verb().unwrap().1 {
				Verb::Change |
				Verb::InsertModeLineBreak(_) |
				Verb::InsertMode => Box::new(ViInsert::new().with_count(count as u16)),

				Verb::NormalMode => Box::new(ViNormal::new()),

				Verb::ReplaceMode => Box::new(ViReplace::new()),

				Verb::VisualModeSelectLast => {
					if self.mode.report_mode() != ModeReport::Visual {
						self.editor.start_selecting(SelectMode::Char(SelectAnchor::End));
					}
					let mut mode: Box<dyn ViMode> = Box::new(ViVisual::new());
					std::mem::swap(&mut mode, &mut self.mode);
					self.editor.set_cursor_clamp(self.mode.clamp_cursor());

					return self.editor.exec_cmd(cmd)
				}
				Verb::VisualMode => {
					selecting = true;
					Box::new(ViVisual::new())
				}

				_ => unreachable!()
			};

			std::mem::swap(&mut mode, &mut self.mode);

			self.editor.set_cursor_clamp(self.mode.clamp_cursor());
			if mode.is_repeatable() {
				self.repeat_action = mode.as_replay();
			}

			self.editor.exec_cmd(cmd)?;

			if selecting {
				self.editor.start_selecting(SelectMode::Char(SelectAnchor::End));
			} else {
				self.editor.stop_selecting();
			}
			return Ok(())
		} else if cmd.is_cmd_repeat() {
			let Some(replay) = self.repeat_action.clone() else {
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
							self.editor.exec_cmd(cmd)?
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
								m_mut.0 = 1
							}
						} else {
							return Ok(()) // it has to have a verb to be repeatable, something weird happened
						}
					}
					self.editor.exec_cmd(cmd)?;
				}
				_ => unreachable!("motions should be handled in the other branch")
			}
			return Ok(())
		} else if cmd.is_motion_repeat() {
			match cmd.motion.as_ref().unwrap() {
				MotionCmd(count,Motion::RepeatMotion) => {
					let Some(motion) = self.repeat_motion.clone() else {
						return Ok(())
					};
					let repeat_cmd = ViCmd {
						register: RegisterName::default(),
						verb: None,
						motion: Some(motion),
						raw_seq: format!("{count};")
					};
					return self.editor.exec_cmd(repeat_cmd);
				}
				MotionCmd(count,Motion::RepeatMotionRev) => {
					let Some(motion) = self.repeat_motion.clone() else {
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
					return self.editor.exec_cmd(repeat_cmd);
				}
				_ => unreachable!()
			}
		}

		if cmd.is_repeatable() {
			if self.mode.report_mode() == ModeReport::Visual {
				// The motion is assigned in the line buffer execution, so we also have to assign it here
				// in order to be able to repeat it
				let range = self.editor.select_range().unwrap();
				cmd.motion = Some(MotionCmd(1,Motion::Range(range.0, range.1)))
			}
			self.repeat_action = Some(CmdReplay::Single(cmd.clone()));
		} 

		if cmd.is_char_search() {
			self.repeat_motion = cmd.motion.clone()
		}

		self.editor.exec_cmd(cmd.clone())?;

		if self.mode.report_mode() == ModeReport::Visual && cmd.verb().is_some_and(|v| v.1.is_edit()) {
			self.editor.stop_selecting();
			let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
			std::mem::swap(&mut mode, &mut self.mode);
		}
		Ok(())
	}
}

