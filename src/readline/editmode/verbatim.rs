use crate::state::vars::VarStr;

use super::{
  CmdReplay, E, EditMode, K, ModeReport, common_cmds,
  editcmd::{CmdFlags, EditCmd, Verb},
  register::RegisterName,
  state::terminal::CursorStyle,
  verb,
};

#[derive(Default, Clone, Debug)]
pub(crate) struct ViVerbatim {
  sent_cmd: Vec<EditCmd>,
  repeat_count: u16,
}

impl ViVerbatim {
  pub(crate) fn new() -> Self {
    Self::default()
  }
  pub(crate) fn with_count(self, repeat_count: u16) -> Self {
    Self {
      repeat_count,
      ..self
    }
  }
}

impl EditMode for ViVerbatim {
  fn handle_key(&mut self, key: E) -> Option<EditCmd> {
    match key {
      E(K::Verbatim(seq), _mods) => {
        log::debug!("Received verbatim key sequence: {seq:?}");
        let cmd = EditCmd {
          register: RegisterName::default(),
          verb: Some(verb!(Verb::Insert((*seq).into()))),
          motion: None,
          raw_seq: seq.to_string(),
          flags: CmdFlags::EXIT_CUR_MODE,
        };
        self.sent_cmd.push(cmd.clone());
        Some(cmd)
      }
      _ => common_cmds(&key),
    }
  }

  fn is_repeatable(&self) -> bool {
    true
  }

  fn as_replay(&self) -> Option<CmdReplay> {
    Some(CmdReplay::mode(self.sent_cmd.clone(), self.repeat_count))
  }

  fn cursor_style(&self) -> CursorStyle {
    CursorStyle::Underline(true)
  }
  fn pending_seq(&self) -> Option<VarStr> {
    None
  }
  fn clamp_cursor(&self) -> bool {
    false
  }
  fn report_mode(&self) -> ModeReport {
    ModeReport::Verbatim
  }
}
