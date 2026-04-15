use crate::{
  readline::{editcmd::To, editmode::EditMode},
  state::write_meta,
};

pub struct RemoteMode;

impl EditMode for RemoteMode {
  fn handle_key(
    &mut self,
    key: crate::readline::keys::KeyEvent,
  ) -> Option<crate::readline::editcmd::EditCmd> {
    write_meta(|m| m.notify_key_event(key)).ok()?;
    None
  }

  fn is_repeatable(&self) -> bool {
    false
  }

  fn as_replay(&self) -> Option<super::CmdReplay> {
    None
  }

  fn cursor_style(&self) -> String {
    "\x1b[6 q".to_string()
  }

  fn pending_seq(&self) -> Option<String> {
    None
  }

  fn move_cursor_on_undo(&self) -> bool {
    true
  }

  fn clamp_cursor(&self) -> bool {
    false
  }

  fn hist_scroll_start_pos(&self) -> Option<crate::readline::editcmd::To> {
    Some(To::End)
  }

  fn report_mode(&self) -> super::ModeReport {
    super::ModeReport::Remote
  }
}
