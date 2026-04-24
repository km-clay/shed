use super::{CmdReplay, EditMode, ModeReport, common_cmds};
use crate::readline::editcmd::{Direction, EditCmd, Motion, MotionCmd, To, Verb, VerbCmd, Word};
use crate::readline::keys::{KeyCode as K, KeyEvent as E, ModKeys as M};
use crate::{key, motion, verb};

#[derive(Default, Clone, Debug)]
pub struct Emacs {
  pending_cmd: Option<EditCmd>,
}

impl Emacs {
  pub fn new() -> Self {
    Self::default()
  }
  fn reset_cmd(&mut self) {
    self.pending_cmd = None;
  }
  fn set_verb(&mut self, verb: VerbCmd) {
    if let Some(cmd) = &mut self.pending_cmd {
      cmd.verb = Some(verb);
    } else {
      self.pending_cmd = Some(EditCmd {
        register: Default::default(),
        verb: Some(verb),
        motion: None,
        raw_seq: String::new(),
        flags: Default::default(),
      });
    }
  }
  fn set_motion(&mut self, motion: MotionCmd) {
    if let Some(cmd) = &mut self.pending_cmd {
      cmd.motion = Some(motion);
    } else {
      self.pending_cmd = Some(EditCmd {
        register: Default::default(),
        verb: None,
        motion: Some(motion),
        raw_seq: String::new(),
        flags: Default::default(),
      });
    }
  }
  pub fn take_cmd(&mut self) -> Option<EditCmd> {
    self.pending_cmd.take()
  }
}

impl EditMode for Emacs {
  fn handle_key(&mut self, key: E) -> Option<EditCmd> {
    match key {
      E(K::Char(ch), M::NONE) => {
        self.set_verb(verb!(Verb::InsertChar(ch)));
        self.set_motion(motion!(Motion::ForwardChar));
        self.take_cmd()
      }
      E(K::ExMode, _) => {
        self.reset_cmd();
        self.set_verb(verb!(Verb::ExMode));
        self.take_cmd()
      }
      E(K::Verbatim(seq), _) => {
        self.reset_cmd();
        self.set_verb(verb!(Verb::Insert(seq.to_string())));
        self.take_cmd()
      }
      key!(Backspace) => {
        self.set_verb(verb!(Verb::Delete));
        self.set_motion(motion!(Motion::BackwardCharForced));
        self.take_cmd()
      }
      key!(BackTab) => {
        self.set_verb(verb!(Verb::CompleteBackward));
        self.take_cmd()
      }
      key!(Tab) | key!(Ctrl + 'i') => {
        self.set_verb(verb!(Verb::Complete));
        self.take_cmd()
      }

      // Emacs keybinds
      key!(Ctrl + 'a') => {
        self.set_motion(motion!(Motion::StartOfLine));
        self.take_cmd()
      }

      key!(Ctrl + 'e') => {
        self.set_motion(motion!(Motion::EndOfLine));
        self.take_cmd()
      }

      key!(Ctrl + 'f') | key!(Ctrl + 'b') => {
        let motion = if matches!(key, key!(Ctrl + 'f')) {
          Motion::ForwardCharForced
        } else {
          Motion::BackwardCharForced
        };
        self.set_motion(motion!(motion));
        self.take_cmd()
      }

      key!(Alt + 'f') | key!(Alt + 'b') => {
        let motion = if matches!(key, key!(Alt + 'f')) {
          Motion::WordMotion(To::End, Word::Normal, Direction::Forward)
        } else {
          Motion::WordMotion(To::Start, Word::Normal, Direction::Backward)
        };
        self.set_motion(motion!(motion));
        self.take_cmd()
      }

      key!(Alt + ';') => {
        self.set_verb(verb!(Verb::ExMode));
        self.take_cmd()
      }

      key!(Ctrl + 'w') | key!(Alt + Backspace) => {
        self.set_verb(verb!(Verb::Kill));
        self.set_motion(motion!(Motion::WordMotion(
          To::Start,
          Word::Normal,
          Direction::Backward
        )));
        self.take_cmd()
      }

      key!(Alt + 'd') => {
        self.set_verb(verb!(Verb::Kill));
        self.set_motion(motion!(Motion::WordMotion(
          To::End,
          Word::Normal,
          Direction::Forward
        )));
        self.take_cmd()
      }

      key!(Ctrl + 'd') => {
        self.set_verb(verb!(Verb::DeleteOrEof));
        self.set_motion(motion!(Motion::ForwardCharForced));
        self.take_cmd()
      }

      key!(Ctrl + 'k') => {
        self.set_verb(verb!(Verb::Kill));
        self.set_motion(motion!(Motion::EndOfLine));
        self.take_cmd()
      }

      key!(Ctrl + 'u') => {
        self.set_verb(verb!(Verb::Kill));
        self.set_motion(motion!(Motion::StartOfLine));
        self.take_cmd()
      }

      key!(Ctrl + 'y') => {
        self.set_verb(verb!(Verb::KillPut));
        self.take_cmd()
      }

      key!(Alt + 'y') => {
        self.set_verb(verb!(Verb::KillCycle));
        self.take_cmd()
      }

      key!(Ctrl + 't') => {
        self.set_verb(verb!(Verb::TransposeChar));
        self.take_cmd()
      }

      key!(Alt + 't') => {
        self.set_verb(verb!(Verb::TransposeWord));
        self.take_cmd()
      }

      key!(Alt + 'u') => {
        self.set_motion(motion!(Motion::WordMotion(
          To::End,
          Word::Normal,
          Direction::Forward
        )));
        self.set_verb(verb!(Verb::ToUpper));
        self.take_cmd()
      }

      key!(Alt + 'l') => {
        self.set_motion(motion!(Motion::WordMotion(
          To::End,
          Word::Normal,
          Direction::Forward
        )));
        self.set_verb(verb!(Verb::ToLower));
        self.take_cmd()
      }

      key!(Ctrl + '/') => {
        self.set_verb(verb!(Verb::Undo));
        self.take_cmd()
      }

      key!(Alt + '/') => {
        self.set_verb(verb!(Verb::Redo));
        self.take_cmd()
      }

      key!(Alt + 'c') => {
        self.set_verb(verb!(Verb::Capitalize));
        self.set_motion(motion!(Motion::WordMotion(
          To::End,
          Word::Normal,
          Direction::Forward
        )));
        self.take_cmd()
      }

      _ => common_cmds(key),
    }
  }

  fn is_repeatable(&self) -> bool {
    true
  }
  fn as_replay(&self) -> Option<CmdReplay> {
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
  fn hist_scroll_start_pos(&self) -> Option<To> {
    Some(To::End)
  }
  fn report_mode(&self) -> ModeReport {
    ModeReport::Emacs
  }
}
