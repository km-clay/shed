use super::{CmdReplay, EditMode, ModeReport, common_cmds};
use crate::readline::editcmd::{Direction, EditCmd, Motion, MotionCmd, To, Verb, VerbCmd, Word};
use crate::readline::keys::{KeyCode as K, KeyEvent as E, ModKeys as M};
use crate::state::CursorStyle;
use crate::{key, motion, verb};

#[derive(Default, Debug)]
pub struct ViReplace {
  cmds: Vec<EditCmd>,
  pending_cmd: EditCmd,
  repeat_count: u16,
}

impl ViReplace {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn with_count(mut self, repeat_count: u16) -> Self {
    self.repeat_count = repeat_count;
    self
  }
  pub fn register_and_return(&mut self) -> Option<EditCmd> {
    let mut cmd = self.take_cmd();
    cmd.normalize_counts();
    self.register_cmd(&cmd);
    Some(cmd)
  }
  pub fn register_cmd(&mut self, cmd: &EditCmd) {
    self.cmds.push(cmd.clone())
  }
  pub fn take_cmd(&mut self) -> EditCmd {
    std::mem::take(&mut self.pending_cmd)
  }
}

impl EditMode for ViReplace {
  fn handle_key(&mut self, key: E) -> Option<EditCmd> {
    match key {
      E(K::Char(ch), M::NONE) => {
        self.pending_cmd.set_verb(verb!(Verb::ReplaceChar(ch)));
        self.pending_cmd.set_motion(motion!(Motion::ForwardChar));
        self.register_and_return()
      }
      E(K::ExMode, _) => Some(EditCmd {
        register: Default::default(),
        verb: Some(verb!(Verb::ExMode)),
        motion: None,
        raw_seq: String::new(),
        flags: Default::default(),
      }),
      key!(Ctrl + 'w') => {
        self.pending_cmd.set_verb(verb!(Verb::Delete));
        self.pending_cmd.set_motion(motion!(Motion::WordMotion(
          To::Start,
          Word::Normal,
          Direction::Backward
        )));
        self.register_and_return()
      }
      key!(Ctrl + 'h') | key!(Backspace) => {
        self.pending_cmd.set_motion(motion!(Motion::BackwardChar));
        self.register_and_return()
      }

      key!(BackTab) => {
        self.pending_cmd.set_verb(verb!(Verb::CompleteBackward));
        self.register_and_return()
      }

      key!(Ctrl + 'i') | key!(Tab) => {
        self.pending_cmd.set_verb(verb!(Verb::Complete));
        self.register_and_return()
      }

      key!(Esc) => {
        self.pending_cmd.set_verb(verb!(Verb::NormalMode));
        self.pending_cmd.set_motion(motion!(Motion::BackwardChar));
        self.register_and_return()
      }
      _ => common_cmds(key),
    }
  }
  fn is_repeatable(&self) -> bool {
    true
  }
  fn cursor_style(&self) -> String {
    CursorStyle::Underline(false).to_string()
  }
  fn pending_seq(&self) -> Option<String> {
    None
  }
  fn as_replay(&self) -> Option<CmdReplay> {
    Some(CmdReplay::mode(self.cmds.clone(), self.repeat_count))
  }
  fn move_cursor_on_undo(&self) -> bool {
    true
  }
  fn clamp_cursor(&self) -> bool {
    true
  }
  fn hist_scroll_start_pos(&self) -> Option<To> {
    Some(To::End)
  }
  fn report_mode(&self) -> ModeReport {
    ModeReport::Replace
  }
}
