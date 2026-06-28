use crate::state::vars::VarStr;

use super::{
  CmdReplay, E as KeyEvent, EditCmd, EditMode, ModeReport, Shed, state::terminal::CursorStyle,
};

pub(crate) struct RemoteMode;

impl EditMode for RemoteMode {
  fn handle_key(&mut self, key: KeyEvent) -> Option<EditCmd> {
    Shed::notify_key_event(&key);
    None
  }

  fn is_repeatable(&self) -> bool {
    false
  }

  fn as_replay(&self) -> Option<CmdReplay> {
    None
  }

  fn cursor_style(&self) -> CursorStyle {
    CursorStyle::Beam(false)
  }

  fn pending_seq(&self) -> Option<VarStr> {
    None
  }

  fn clamp_cursor(&self) -> bool {
    false
  }

  fn report_mode(&self) -> ModeReport {
    super::ModeReport::Remote
  }
}
