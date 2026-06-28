use crate::{
  readline::{RegisterName, editcmd::CmdFlags},
  state::vars::VarStr,
};

use super::{
  CmdReplay, Direction, E, EditCmd, EditMode, K, M, ModeReport, Motion, To, Verb, ViNormal, Word,
  common_cmds,
  editcmd::Cmd,
  key, motion,
  state::Shed,
  state::{terminal::CursorStyle, vars::VarFlags, vars::VarKind},
  verb,
};

#[derive(Default, Debug)]
pub struct ViInsert {
  cmds: Vec<EditCmd>,
  normal: Option<ViNormal>,
  pending_cmd: EditCmd,
  repeat_count: u16,
}

impl ViInsert {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn record_cmd(mut self, cmd: EditCmd) -> Self {
    self.cmds.push(cmd);
    self
  }
  pub fn with_count(mut self, repeat_count: u16) -> Self {
    self.repeat_count = repeat_count;
    self
  }
  pub fn register_and_return(&mut self) -> EditCmd {
    let mut cmd = self.take_cmd();
    cmd.normalize_counts();
    self.register_cmd(&cmd);
    cmd
  }
  pub fn ctrl_w_is_undo(&self) -> bool {
    let insert_count = self
      .cmds
      .iter()
      .filter(|cmd: &&EditCmd| matches!(cmd.verb(), Some(Cmd(1, Verb::InsertChar(_)))))
      .count();
    let backspace_count = self
      .cmds
      .iter()
      .filter(|cmd: &&EditCmd| matches!(cmd.verb(), Some(Cmd(1, Verb::Delete))))
      .count();

    insert_count > backspace_count
  }
  pub fn register_cmd(&mut self, cmd: &EditCmd) {
    self.cmds.push(cmd.clone());
  }
  pub fn take_cmd(&mut self) -> EditCmd {
    std::mem::take(&mut self.pending_cmd)
  }
}

impl EditMode for ViInsert {
  #[expect(clippy::too_many_lines)]
  fn handle_key(&mut self, key: E) -> Option<EditCmd> {
    if let Some(mut normal) = self.normal.take() {
      if matches!(key, key!(Esc)) {
        Shed::vars_mut(|v| {
          v.set_var(
            "SHED_EDIT_MODE",
            VarKind::Str("INSERT".into()),
            VarFlags::empty(),
          )
        })
        .ok();
        return None;
      }

      let Some(cmd) = normal.handle_key(key) else {
        self.normal = Some(normal);
        return None;
      };

      Shed::vars_mut(|v| {
        v.set_var(
          "SHED_EDIT_MODE",
          VarKind::Str("INSERT".into()),
          VarFlags::empty(),
        )
      })
      .ok();

      if cmd.verb_is(&Verb::InsertMode) && cmd.motion_is(&Motion::BackwardChar) {
        // they pressed 'i', no op
        return None;
      }
      return Some(cmd);
    }
    match key {
      E(K::Char(ch), M::NONE) => {
        self.pending_cmd.set_verb(verb!(Verb::InsertChar(ch)));
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
      E(K::Verbatim(seq), _) => {
        self
          .pending_cmd
          .set_verb(verb!(Verb::Insert((*seq).into())));
        Some(self.register_and_return())
      }
      key!(Ctrl + 'o') => {
        let mode = ViNormal::new();
        self.normal = Some(mode);
        Shed::vars_mut(|v| {
          v.set_var(
            "SHED_EDIT_MODE",
            VarKind::Str("(insert)".into()),
            VarFlags::empty(),
          )
        })
        .ok();
        None
      }
      key!(Ctrl + 'w') => {
        let (verb, motion) = if self.ctrl_w_is_undo() {
          (verb!(Verb::Undo), None)
        } else {
          (
            verb!(Verb::Delete),
            Some(motion!(Motion::WordMotion(
              To::Start,
              Word::Normal,
              Direction::Backward
            ))),
          )
        };
        self.pending_cmd.set_verb(verb);
        if let Some(motion) = motion {
          self.pending_cmd.set_motion(motion);
        }
        Some(self.register_and_return())
      }
      key!(Ctrl + 'v') => {
        self.pending_cmd.set_verb(verb!(Verb::VerbatimMode));
        Some(self.register_and_return())
      }
      key!(Ctrl + 'h') | E(K::Backspace, _) => {
        self.pending_cmd.set_verb(verb!(Verb::Delete));
        self
          .pending_cmd
          .set_motion(motion!(Motion::BackwardCharForced));
        Some(self.register_and_return())
      }

      key!(Ctrl + 'i') | E(K::Tab, M::NONE) => {
        self.pending_cmd.set_verb(verb!(Verb::InsertChar('\t')));
        self.pending_cmd.set_motion(motion!(Motion::ForwardChar));
        Some(self.register_and_return())
      }

      E(K::Esc, M::NONE) => {
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

  fn as_replay(&self) -> Option<CmdReplay> {
    Some(CmdReplay::mode(self.cmds.clone(), self.repeat_count))
  }

  fn cursor_style(&self) -> CursorStyle {
    self
      .normal
      .as_ref()
      .map_or_else(|| CursorStyle::Beam(false), super::EditMode::cursor_style)
  }
  fn pending_seq(&self) -> Option<VarStr> {
    self.normal.as_ref().and_then(super::EditMode::pending_seq)
  }
  fn clamp_cursor(&self) -> bool {
    self.normal.is_some()
  }
  fn report_mode(&self) -> ModeReport {
    if self.normal.is_some() {
      ModeReport::Normal
    } else {
      ModeReport::Insert
    }
  }
}
