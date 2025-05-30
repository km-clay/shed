use std::time::Duration;

use history::{History, SearchConstraint, SearchKind};
use keys::{KeyCode, KeyEvent, ModKeys};
use linebuf::{strip_ansi_codes_and_escapes, LineBuf};
use mode::{CmdReplay, ViInsert, ViMode, ViNormal, ViReplace};
use term::Terminal;
use unicode_width::UnicodeWidthStr;
use vicmd::{Motion, MotionCmd, RegisterName, To, Verb, VerbCmd, ViCmd};

use crate::libsh::{error::{ShErr, ShErrKind, ShResult}, term::{Style, Styled}};
use crate::prelude::*;

pub mod keys;
pub mod term;
pub mod linebuf;
pub mod vicmd;
pub mod mode;
pub mod register;
pub mod history;

const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore\nmagna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo\nconsequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

/// Unified interface for different line editing methods
pub trait Readline {
	fn readline(&mut self) -> ShResult<String>;
}

pub struct FernVi {
	term: Terminal,
	line: LineBuf,
	history: History,
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

			if let KeyEvent(KeyCode::Char('V'), ModKeys::CTRL) = key {
				self.handle_verbatim()?;
				continue
			}
			if self.should_accept_hint(&key) {
				self.line.accept_hint();
				self.history.update_pending_cmd(self.line.as_str());
				self.print_buf(true)?;
				continue
			}

			let Some(cmd) = self.mode.handle_key(key) else {
				continue
			};

			if self.should_grab_history(&cmd) {
				flog!(DEBUG, "scrolling");
				self.scroll_history(cmd);
				self.print_buf(true)?;
				continue
			}



			if cmd.should_submit() {
				self.term.unposition_cursor()?;
				self.term.write("\n");
				let command = self.line.to_string();
				if !command.is_empty() {
					// We're just going to trim the command
					// reduces clutter in the case of two history commands whose only difference is insignificant whitespace
					self.history.push(command.trim().to_string());
					self.history.save()?;
				}
				return Ok(command);
			}
			let line = self.line.to_string();
			self.exec_cmd(cmd.clone())?;
			let new_line = self.line.as_str();
			let has_changes = line != new_line;
			flog!(DEBUG, has_changes);

			if has_changes {
				self.history.update_pending_cmd(self.line.as_str());
			}

			self.print_buf(true)?;
		}
	}
}

impl FernVi {
	pub fn new(prompt: Option<String>) -> ShResult<Self> {
		let prompt = prompt.unwrap_or("$ ".styled(Style::Green | Style::Bold));
		let line = LineBuf::new();//.with_initial(LOREM_IPSUM);
		let term = Terminal::new();
		let history = History::new()?;
		Ok(Self {
			term,
			line,
			history,
			prompt,
			mode: Box::new(ViInsert::new()),
			last_action: None,
			last_movement: None,
		})
	}
	pub fn should_accept_hint(&self, event: &KeyEvent) -> bool {
		if self.line.at_end_of_buffer() && self.line.has_hint() {
			matches!(
				event,
				KeyEvent(KeyCode::Right, ModKeys::NONE)
			)
		} else {
			false
		}
	}
	/// Ctrl+V handler
	pub fn handle_verbatim(&mut self) -> ShResult<()> {
		let mut buf = [0u8; 8];
		let mut collected = Vec::new();

		loop {
			let n = self.term.read_byte(&mut buf[..1]);
			if n == 0 {
				continue;
			}
			collected.push(buf[0]);

			// If it starts with ESC, treat as escape sequence
			if collected[0] == 0x1b {
				loop {
					let n = self.term.peek_byte(&mut buf[..1]);
					if n == 0 {
						break
					}
					collected.push(buf[0]);
					// Ends a CSI sequence
					if (0x40..=0x7e).contains(&buf[0]) {
						break;
					}
				}
				let Ok(seq) = std::str::from_utf8(&collected) else {
					return Ok(())
				};
				let cmd = ViCmd {
					register: Default::default(),
					verb: Some(VerbCmd(1, Verb::Insert(seq.to_string()))),
					motion: None,
					raw_seq: seq.to_string(),
				};
				self.line.exec_cmd(cmd)?;
			}

			// Optional: handle other edge cases, e.g., raw control codes
			if collected[0] < 0x20 || collected[0] == 0x7F {
				let ctrl_seq = std::str::from_utf8(&collected).unwrap();
				let cmd = ViCmd {
					register: Default::default(),
					verb: Some(VerbCmd(1, Verb::Insert(ctrl_seq.to_string()))),
					motion: None,
					raw_seq: ctrl_seq.to_string(),
				};
				self.line.exec_cmd(cmd)?;
				break;
			}

			// Try to parse as UTF-8 if it's a valid Unicode sequence
			if let Ok(s) = std::str::from_utf8(&collected) {
				if s.chars().count() == 1 {
					let ch = s.chars().next().unwrap();
					// You got a literal Unicode char
					eprintln!("Got char: {:?}", ch);
					break;
				}
			}

		}
		Ok(())
	}
	pub fn scroll_history(&mut self, cmd: ViCmd) {
		if self.history.cursor_entry().is_some_and(|ent| ent.is_new()) {
			let constraint = SearchConstraint::new(SearchKind::Prefix, self.line.to_string());
			self.history.constrain_entries(constraint);
		}
		let count = &cmd.motion().unwrap().0;
		let motion = &cmd.motion().unwrap().1;
		flog!(DEBUG,count,motion);
		let entry = match motion {
			Motion::LineUp => {
				let Some(hist_entry) = self.history.scroll(-(*count as isize)) else {
					return
				};
				flog!(DEBUG,"found entry");
				flog!(DEBUG,hist_entry.command());
				hist_entry
			}
			Motion::LineDown => {
				let Some(hist_entry) = self.history.scroll(*count as isize) else {
					return
				};
				flog!(DEBUG,"found entry");
				flog!(DEBUG,hist_entry.command());
				hist_entry
			}
			_ => unreachable!()
		};
		let col = self.line.saved_col().unwrap_or(self.line.cursor_column());
		let mut buf = LineBuf::new().with_initial(entry.command());
		let line_end = buf.end_of_line();
		if let Some(dest) = self.mode.hist_scroll_start_pos() {
			match dest {
				To::Start => {
					/* Already at 0 */
				}
				To::End => {
					// History entries cannot be empty
					// So this subtraction is safe (maybe)
					buf.cursor_fwd_to(line_end + 1);
				}
			}
		} else {
			let target = (col + 1).min(line_end + 1);
			buf.cursor_fwd_to(target);
		}

		self.line = buf
	}

	pub fn should_grab_history(&self, cmd: &ViCmd) -> bool {
		cmd.verb().is_none() &&
		(
			cmd.motion().is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineUp))) &&
			self.line.start_of_line() == 0
		) ||
		(
			cmd.motion().is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineDown))) &&
			self.line.end_of_line() == self.line.byte_len()
		)
	}
	pub fn print_buf(&mut self, refresh: bool) -> ShResult<()> {
		let (height,width) = self.term.get_dimensions()?;
		if refresh {
			self.term.unwrite()?;
		}
		let hint = self.history.get_hint();
		self.line.set_hint(hint);

		let offset = self.calculate_prompt_offset();
		self.line.set_first_line_offset(offset);
		self.line.update_term_dims((height,width));
		let mut line_buf = self.prompt.clone();
		line_buf.push_str(&self.line.to_string());

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
								m_mut.0 = 1
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
