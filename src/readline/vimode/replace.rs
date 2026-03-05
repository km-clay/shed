use super::{CmdReplay, ModeReport, ViMode, common_cmds};
use crate::readline::keys::{KeyCode as K, KeyEvent as E, ModKeys as M};
use crate::readline::vicmd::{Direction, Motion, MotionCmd, To, Verb, VerbCmd, ViCmd, Word};

#[derive(Default, Debug)]
pub struct ViReplace {
  cmds: Vec<ViCmd>,
  pending_cmd: ViCmd,
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
  pub fn register_and_return(&mut self) -> Option<ViCmd> {
    let mut cmd = self.take_cmd();
    cmd.normalize_counts();
    self.register_cmd(&cmd);
    Some(cmd)
  }
  pub fn register_cmd(&mut self, cmd: &ViCmd) {
    self.cmds.push(cmd.clone())
  }
  pub fn take_cmd(&mut self) -> ViCmd {
    std::mem::take(&mut self.pending_cmd)
  }
}

impl ViMode for ViReplace {
  fn handle_key(&mut self, key: E) -> Option<ViCmd> {
    match key {
      E(K::Char(ch), M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::ReplaceChar(ch)));
        self
          .pending_cmd
          .set_motion(MotionCmd(1, Motion::ForwardChar));
        self.register_and_return()
      }
      E(K::Char('W'), M::CTRL) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::Delete));
        self.pending_cmd.set_motion(MotionCmd(
          1,
          Motion::WordMotion(To::Start, Word::Normal, Direction::Backward),
        ));
        self.register_and_return()
      }
      E(K::Char('H'), M::CTRL) | E(K::Backspace, M::NONE) => {
        self
          .pending_cmd
          .set_motion(MotionCmd(1, Motion::BackwardChar));
        self.register_and_return()
      }

      E(K::BackTab, M::NONE) => {
        self
          .pending_cmd
          .set_verb(VerbCmd(1, Verb::CompleteBackward));
        self.register_and_return()
      }

      E(K::Char('I'), M::CTRL) | E(K::Tab, M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::Complete));
        self.register_and_return()
      }

      E(K::Esc, M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::NormalMode));
        self
          .pending_cmd
          .set_motion(MotionCmd(1, Motion::BackwardChar));
        self.register_and_return()
      }
      _ => common_cmds(key),
    }
  }
  fn is_repeatable(&self) -> bool {
    true
  }
  fn cursor_style(&self) -> String {
    "\x1b[4 q".to_string()
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
