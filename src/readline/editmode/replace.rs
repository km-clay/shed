use crate::{
  readline::{RegisterName, editcmd::CmdFlags},
  state::vars::VarStr,
};

use super::{
  CmdReplay, E, EditMode, K, M, ModeReport, common_cmds,
  editcmd::{Direction, EditCmd, Motion, To, Verb, Word},
  key, motion,
  state::terminal::CursorStyle,
  verb,
};

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
  pub fn register_and_return(&mut self) -> EditCmd {
    let mut cmd = self.take_cmd();
    cmd.normalize_counts();
    self.register_cmd(&cmd);
    cmd
  }
  pub fn register_cmd(&mut self, cmd: &EditCmd) {
    self.cmds.push(cmd.clone());
  }
  pub fn take_cmd(&mut self) -> EditCmd {
    std::mem::take(&mut self.pending_cmd)
  }
}

impl EditMode for ViReplace {
  fn handle_key(&mut self, key: E) -> Option<EditCmd> {
    match key {
      E(K::Char(ch), M::NONE) => {
        self
          .pending_cmd
          .set_verb(verb!(Verb::ReplaceCharInplace(ch, 1)));
        self.pending_cmd.set_motion(motion!(Motion::ForwardChar));
        Some(self.register_and_return())
      }
      E(K::ExMode, _) => Some(EditCmd {
        register: RegisterName::default(),
        verb: Some(verb!(Verb::ExMode)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::default(),
      }),
      key!(Ctrl + 'w') => {
        self.pending_cmd.set_verb(verb!(Verb::Delete));
        self.pending_cmd.set_motion(motion!(Motion::WordMotion(
          To::Start,
          Word::Normal,
          Direction::Backward
        )));
        Some(self.register_and_return())
      }
      key!(Ctrl + 'h') | key!(Backspace) => {
        self.pending_cmd.set_motion(motion!(Motion::BackwardChar));
        Some(self.register_and_return())
      }

      key!(Ctrl + 'i') | key!(Tab) => {
        self.pending_cmd.set_verb(verb!(Verb::Complete));
        Some(self.register_and_return())
      }

      key!(Esc) => {
        self.pending_cmd.set_verb(verb!(Verb::NormalMode));
        self.pending_cmd.set_motion(motion!(Motion::BackwardChar));
        Some(self.register_and_return())
      }
      _ => common_cmds(&key),
    }
  }
  fn is_repeatable(&self) -> bool {
    true
  }
  fn cursor_style(&self) -> CursorStyle {
    CursorStyle::Underline(false)
  }
  fn pending_seq(&self) -> Option<VarStr> {
    None
  }
  fn as_replay(&self) -> Option<CmdReplay> {
    Some(CmdReplay::mode(self.cmds.clone(), self.repeat_count))
  }
  fn clamp_cursor(&self) -> bool {
    true
  }
  fn report_mode(&self) -> ModeReport {
    ModeReport::Replace
  }
}
