use super::{common_cmds, CmdReplay, ModeReport, ViMode};
use crate::readline::keys::{KeyCode as K, KeyEvent as E, ModKeys as M};
use crate::readline::register::Register;
use crate::readline::vicmd::{
  CmdFlags, Direction, Motion, MotionCmd, RegisterName, To, Verb, VerbCmd, ViCmd, Word
};

#[derive(Default, Clone, Debug)]
pub struct ViVerbatim {
	sent_cmd: Vec<ViCmd>,
	repeat_count: u16
}

impl ViVerbatim {
  pub fn new() -> Self {
    Self::default()
  }
	pub fn with_count(self, repeat_count: u16) -> Self {
		Self { repeat_count, ..self }
	}
}

impl ViMode for ViVerbatim {
  fn handle_key(&mut self, key: E) -> Option<ViCmd> {
    match key {
			E(K::Verbatim(seq),_mods) => {
				log::debug!("Received verbatim key sequence: {:?}", seq);
				let cmd = ViCmd { register: RegisterName::default(),
					verb: Some(VerbCmd(1,Verb::Insert(seq.to_string()))),
					motion: None,
					raw_seq: seq.to_string(),
					flags: CmdFlags::EXIT_CUR_MODE
				};
				self.sent_cmd.push(cmd.clone());
				Some(cmd)
			}
      _ => common_cmds(key),
    }
  }

  fn is_repeatable(&self) -> bool {
    true
  }

  fn as_replay(&self) -> Option<CmdReplay> {
    Some(CmdReplay::mode(self.sent_cmd.clone(), self.repeat_count))
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
  fn hist_scroll_start_pos(&self) -> Option<To> {
    Some(To::End)
  }
  fn report_mode(&self) -> ModeReport {
    ModeReport::Verbatim
  }
}
