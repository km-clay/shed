use history::History;
use keys::{KeyCode, KeyEvent, ModKeys};
use linebuf::{LineBuf, SelectAnchor, SelectMode};
use nix::libc::STDOUT_FILENO;
use term::{get_win_size, KeyReader, Layout, LineWriter, PollReader, TermWriter};
use vicmd::{CmdFlags, Motion, MotionCmd, RegisterName, To, Verb, VerbCmd, ViCmd};
use vimode::{CmdReplay, ModeReport, ViInsert, ViMode, ViNormal, ViReplace, ViVisual};

use crate::libsh::{
  error::{ShErrKind, ShResult},
  term::{Style, Styled},
};
use crate::prelude::*;

pub mod history;
pub mod keys;
pub mod layout;
pub mod linebuf;
pub mod register;
pub mod term;
pub mod vicmd;
pub mod vimode;

/// Non-blocking readline result
pub enum ReadlineEvent {
  /// A complete line was entered
  Line(String),
  /// Ctrl+D on empty line - request to exit
  Eof,
  /// No complete input yet, need more bytes
  Pending,
}

pub struct FernVi {
  pub reader: PollReader,
  pub writer: Box<dyn LineWriter>,
  pub prompt: String,
  pub mode: Box<dyn ViMode>,
  pub old_layout: Option<Layout>,
  pub repeat_action: Option<CmdReplay>,
  pub repeat_motion: Option<MotionCmd>,
  pub editor: LineBuf,
  pub history: History,
  needs_redraw: bool,
}

impl FernVi {
  pub fn new(prompt: Option<String>) -> ShResult<Self> {
		let mut new = Self {
				reader: PollReader::new(),
				writer: Box::new(TermWriter::new(STDOUT_FILENO)),
				prompt: prompt.unwrap_or("$ ".styled(Style::Green)),
				mode: Box::new(ViInsert::new()),
				old_layout: None,
				repeat_action: None,
				repeat_motion: None,
				editor: LineBuf::new(),
				history: History::new()?,
				needs_redraw: true,
			};
		new.print_line()?;
		Ok(new)
  }

  pub fn with_initial(mut self, initial: &str) -> Self {
    self.editor = LineBuf::new().with_initial(initial, 0);
    self.history.update_pending_cmd(self.editor.as_str());
    self
  }

  /// Feed raw bytes from stdin into the reader's buffer
  pub fn feed_bytes(&mut self, bytes: &[u8]) {
		log::info!("Feeding bytes: {:?}", bytes.iter().map(|b| *b as char).collect::<String>());
    self.reader.feed_bytes(bytes);
  }

  /// Mark that the display needs to be redrawn (e.g., after SIGWINCH)
  pub fn mark_dirty(&mut self) {
    self.needs_redraw = true;
  }

  /// Reset readline state for a new prompt
  pub fn reset(&mut self, prompt: Option<String>) {
    if let Some(p) = prompt {
      self.prompt = p;
    }
    self.editor.buffer.clear();
    self.editor.cursor = Default::default();
    self.old_layout = None;
    self.needs_redraw = true;
  }

  /// Process any available input and return readline event
  /// This is non-blocking - returns Pending if no complete line yet
  pub fn process_input(&mut self) -> ShResult<ReadlineEvent> {
    // Redraw if needed
    if self.needs_redraw {
      self.print_line()?;
      self.needs_redraw = false;
    }

    // Process all available keys
    while let Some(key) = self.reader.read_key()? {
      flog!(DEBUG, key);

      if self.should_accept_hint(&key) {
        self.editor.accept_hint();
        self.history.update_pending_cmd(self.editor.as_str());
        self.needs_redraw = true;
        continue;
      }

      let Some(mut cmd) = self.mode.handle_key(key) else {
        continue;
      };
      flog!(DEBUG, cmd);
      cmd.alter_line_motion_if_no_verb();

      if self.should_grab_history(&cmd) {
        self.scroll_history(cmd);
        self.needs_redraw = true;
        continue;
      }

      if cmd.should_submit() {
        self.writer.flush_write("\n")?;
        let buf = self.editor.take_buf();
        // Save command to history
        self.history.push(buf.clone());
        if let Err(e) = self.history.save() {
          eprintln!("Failed to save history: {e}");
        }
        return Ok(ReadlineEvent::Line(buf));
      }

      if cmd.verb().is_some_and(|v| v.1 == Verb::EndOfFile) {
        if self.editor.buffer.is_empty() {
          return Ok(ReadlineEvent::Eof);
        } else {
          self.editor.buffer.clear();
          self.needs_redraw = true;
          continue;
        }
      }

      let before = self.editor.buffer.clone();
      self.exec_cmd(cmd)?;
      let after = self.editor.as_str();

      if before != after {
        self.history.update_pending_cmd(self.editor.as_str());
      }

      let hint = self.history.get_hint();
      self.editor.set_hint(hint);
      self.needs_redraw = true;
    }

    // Redraw if we processed any input
    if self.needs_redraw {
      self.print_line()?;
      self.needs_redraw = false;
    }

    Ok(ReadlineEvent::Pending)
  }

  pub fn get_layout(&mut self) -> Layout {
    let line = self.editor.to_string();
    flog!(DEBUG, line);
    let to_cursor = self.editor.slice_to_cursor().unwrap_or_default();
    let (cols, _) = get_win_size(STDIN_FILENO);
    Layout::from_parts(/* tab_stop: */ 8, cols, &self.prompt, to_cursor, &line)
  }
  pub fn scroll_history(&mut self, cmd: ViCmd) {
    flog!(DEBUG, "scrolling");
    /*
    if self.history.cursor_entry().is_some_and(|ent| ent.is_new()) {
      let constraint = SearchConstraint::new(SearchKind::Prefix, self.editor.to_string());
      self.history.constrain_entries(constraint);
    }
    */
    let count = &cmd.motion().unwrap().0;
    let motion = &cmd.motion().unwrap().1;
    flog!(DEBUG, count, motion);
    flog!(DEBUG, self.history.masked_entries());
    let entry = match motion {
      Motion::LineUpCharwise => {
        let Some(hist_entry) = self.history.scroll(-(*count as isize)) else {
          return;
        };
        flog!(DEBUG, "found entry");
        flog!(DEBUG, hist_entry.command());
        hist_entry
      }
      Motion::LineDownCharwise => {
        let Some(hist_entry) = self.history.scroll(*count as isize) else {
          return;
        };
        flog!(DEBUG, "found entry");
        flog!(DEBUG, hist_entry.command());
        hist_entry
      }
      _ => unreachable!(),
    };
    let col = self.editor.saved_col.unwrap_or(self.editor.cursor_col());
    let mut buf = LineBuf::new().with_initial(entry.command(), 0);
    let line_end = buf.end_of_line();
    if let Some(dest) = self.mode.hist_scroll_start_pos() {
      match dest {
        To::Start => { /* Already at 0 */ }
        To::End => {
          // History entries cannot be empty
          // So this subtraction is safe (maybe)
          buf.cursor.add(line_end);
        }
      }
    } else {
      let target = (col).min(line_end);
      buf.cursor.add(target);
    }

    self.editor = buf
  }
  pub fn should_accept_hint(&self, event: &KeyEvent) -> bool {
    flog!(DEBUG, self.editor.cursor_at_max());
    flog!(DEBUG, self.editor.cursor);
    if self.editor.cursor_at_max() && self.editor.has_hint() {
      match self.mode.report_mode() {
        ModeReport::Replace | ModeReport::Insert => {
          matches!(event, KeyEvent(KeyCode::Right, ModKeys::NONE))
        }
        ModeReport::Visual | ModeReport::Normal => {
          matches!(event, KeyEvent(KeyCode::Right, ModKeys::NONE))
            || (self.mode.pending_seq().unwrap(/* always Some on normal mode */).is_empty()
              && matches!(event, KeyEvent(KeyCode::Char('l'), ModKeys::NONE)))
        }
        _ => unimplemented!(),
      }
    } else {
      false
    }
  }

  pub fn should_grab_history(&mut self, cmd: &ViCmd) -> bool {
    cmd.verb().is_none()
      && (cmd
        .motion()
        .is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineUpCharwise)))
        && self.editor.start_of_line() == 0)
      || (cmd
        .motion()
        .is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineDownCharwise)))
        && self.editor.end_of_line() == self.editor.cursor_max()
        && !self.history.cursor_entry().is_some_and(|ent| ent.is_new()))
  }

  pub fn print_line(&mut self) -> ShResult<()> {
    let new_layout = self.get_layout();
    if let Some(layout) = self.old_layout.as_ref() {
      self.writer.clear_rows(layout)?;
    }

    self
      .writer
      .redraw(&self.prompt, &self.editor, &new_layout)?;

    self.writer.flush_write(&self.mode.cursor_style())?;

    self.old_layout = Some(new_layout);
    Ok(())
  }

  pub fn exec_cmd(&mut self, mut cmd: ViCmd) -> ShResult<()> {
    let mut selecting = false;
    let mut is_insert_mode = false;
    if cmd.is_mode_transition() {
      let count = cmd.verb_count();
      let mut mode: Box<dyn ViMode> = match cmd.verb().unwrap().1 {
        Verb::Change | Verb::InsertModeLineBreak(_) | Verb::InsertMode => {
          is_insert_mode = true;
          Box::new(ViInsert::new().with_count(count as u16))
        }

        Verb::NormalMode => Box::new(ViNormal::new()),

        Verb::ReplaceMode => Box::new(ViReplace::new()),

        Verb::VisualModeSelectLast => {
          if self.mode.report_mode() != ModeReport::Visual {
            self
              .editor
              .start_selecting(SelectMode::Char(SelectAnchor::End));
          }
          let mut mode: Box<dyn ViMode> = Box::new(ViVisual::new());
          std::mem::swap(&mut mode, &mut self.mode);
          self.editor.set_cursor_clamp(self.mode.clamp_cursor());

          return self.editor.exec_cmd(cmd);
        }
        Verb::VisualMode => {
          selecting = true;
          Box::new(ViVisual::new())
        }

        _ => unreachable!(),
      };

      std::mem::swap(&mut mode, &mut self.mode);

      if mode.is_repeatable() {
        self.repeat_action = mode.as_replay();
      }

      // Set cursor clamp BEFORE executing the command so that motions
      // (like EndOfLine for 'A') can reach positions valid in the new mode
      self.editor.set_cursor_clamp(self.mode.clamp_cursor());
      self.editor.exec_cmd(cmd)?;

      if selecting {
        self
          .editor
          .start_selecting(SelectMode::Char(SelectAnchor::End));
      } else {
        self.editor.stop_selecting();
      }
      if is_insert_mode {
        self.editor.mark_insert_mode_start_pos();
      } else {
        self.editor.clear_insert_mode_start_pos();
      }
      return Ok(());
    } else if cmd.is_cmd_repeat() {
      let Some(replay) = self.repeat_action.clone() else {
        return Ok(());
      };
      let ViCmd { verb, .. } = cmd;
      let VerbCmd(count, _) = verb.unwrap();
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
              return Ok(()); // it has to have a verb to be repeatable,
                             // something weird happened
            }
          }
          self.editor.exec_cmd(cmd)?;
        }
        _ => unreachable!("motions should be handled in the other branch"),
      }
      return Ok(());
    } else if cmd.is_motion_repeat() {
      match cmd.motion.as_ref().unwrap() {
        MotionCmd(count, Motion::RepeatMotion) => {
          let Some(motion) = self.repeat_motion.clone() else {
            return Ok(());
          };
          let repeat_cmd = ViCmd {
            register: RegisterName::default(),
            verb: None,
            motion: Some(motion),
            raw_seq: format!("{count};"),
            flags: CmdFlags::empty(),
          };
          return self.editor.exec_cmd(repeat_cmd);
        }
        MotionCmd(count, Motion::RepeatMotionRev) => {
          let Some(motion) = self.repeat_motion.clone() else {
            return Ok(());
          };
          let mut new_motion = motion.invert_char_motion();
          new_motion.0 = *count;
          let repeat_cmd = ViCmd {
            register: RegisterName::default(),
            verb: None,
            motion: Some(new_motion),
            raw_seq: format!("{count},"),
            flags: CmdFlags::empty(),
          };
          return self.editor.exec_cmd(repeat_cmd);
        }
        _ => unreachable!(),
      }
    }

    if cmd.is_repeatable() {
      if self.mode.report_mode() == ModeReport::Visual {
        // The motion is assigned in the line buffer execution, so we also have to
        // assign it here in order to be able to repeat it
        let range = self.editor.select_range().unwrap();
        cmd.motion = Some(MotionCmd(1, Motion::Range(range.0, range.1)))
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
