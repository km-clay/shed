use unicode_segmentation::UnicodeSegmentation;

use crate::libsh::error::ShResult;
use crate::readline::keys::{KeyCode as K, KeyEvent as E, ModKeys as M};
use crate::readline::vicmd::{
  Motion, MotionCmd, To, Verb, VerbCmd, ViCmd,
};

pub mod insert;
pub mod normal;
pub mod replace;
pub mod visual;
pub mod ex;

pub use ex::ViEx;
pub use insert::ViInsert;
pub use normal::ViNormal;
pub use replace::ViReplace;
pub use visual::ViVisual;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeReport {
  Insert,
  Normal,
	Ex,
  Visual,
  Replace,
  Unknown,
}

#[derive(Debug, Clone)]
pub enum CmdReplay {
  ModeReplay { cmds: Vec<ViCmd>, repeat: u16 },
  Single(ViCmd),
  Motion(Motion),
}

impl CmdReplay {
  pub fn mode(cmds: Vec<ViCmd>, repeat: u16) -> Self {
    Self::ModeReplay { cmds, repeat }
  }
  pub fn single(cmd: ViCmd) -> Self {
    Self::Single(cmd)
  }
  pub fn motion(motion: Motion) -> Self {
    Self::Motion(motion)
  }
}

pub enum CmdState {
  Pending,
  Complete,
  Invalid,
}

pub trait ViMode {
	fn handle_key_fallible(&mut self, key: E) -> ShResult<Option<ViCmd>> { Ok(self.handle_key(key)) }
  fn handle_key(&mut self, key: E) -> Option<ViCmd>;
  fn is_repeatable(&self) -> bool;
  fn as_replay(&self) -> Option<CmdReplay>;
  fn cursor_style(&self) -> String;
  fn pending_seq(&self) -> Option<String>;
	fn pending_cursor(&self) -> Option<usize> { None }
  fn move_cursor_on_undo(&self) -> bool;
  fn clamp_cursor(&self) -> bool;
  fn hist_scroll_start_pos(&self) -> Option<To>;
  fn report_mode(&self) -> ModeReport;
  fn cmds_from_raw(&mut self, raw: &str) -> Vec<ViCmd> {
    let mut cmds = vec![];
    for ch in raw.graphemes(true) {
      let key = E::new(ch, M::NONE);
      let Some(cmd) = self.handle_key(key) else {
        continue;
      };
      cmds.push(cmd)
    }
    cmds
  }
}

pub fn common_cmds(key: E) -> Option<ViCmd> {
  let mut pending_cmd = ViCmd::new();
  match key {
    E(K::Home, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::BeginningOfLine)),
    E(K::End, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::EndOfLine)),
    E(K::Left, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::BackwardChar)),
    E(K::Right, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::ForwardChar)),
    E(K::Up, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::LineUp)),
    E(K::Down, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::LineDown)),
    E(K::Enter, M::NONE) => pending_cmd.set_verb(VerbCmd(1, Verb::AcceptLineOrNewline)),
    E(K::Char('D'), M::CTRL) => pending_cmd.set_verb(VerbCmd(1, Verb::EndOfFile)),
    E(K::Delete, M::NONE) => {
      pending_cmd.set_verb(VerbCmd(1, Verb::Delete));
      pending_cmd.set_motion(MotionCmd(1, Motion::ForwardChar));
    }
    E(K::Backspace, M::NONE) | E(K::Char('H'), M::CTRL) => {
      pending_cmd.set_verb(VerbCmd(1, Verb::Delete));
      pending_cmd.set_motion(MotionCmd(1, Motion::BackwardChar));
    }
    _ => return None,
  }
  Some(pending_cmd)
}
