use super::{CmdReplay, ModeReport, EditMode, common_cmds};
use crate::readline::keys::{KeyCode as K, KeyEvent as E, ModKeys as M};
use crate::readline::editcmd::{Direction, EditCmd, Motion, MotionCmd, To, Verb, VerbCmd, Word};

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
        self.set_verb(VerbCmd(1, Verb::InsertChar(ch)));
        self.set_motion(MotionCmd(1, Motion::ForwardChar));
        self.take_cmd()
      }
      E(K::ExMode, _) => {
				self.reset_cmd();
				self.set_verb(VerbCmd(1, Verb::ExMode));
				self.take_cmd()
			},
      E(K::Verbatim(seq), _) => {
				self.reset_cmd();
        self.set_verb(VerbCmd(1, Verb::Insert(seq.to_string())));
        self.take_cmd()
      }
      E(K::Backspace, M::NONE) => {
        self.set_verb(VerbCmd(1, Verb::Delete));
        self.set_motion(MotionCmd(1, Motion::BackwardCharForced));
        self.take_cmd()
      }
      E(K::BackTab, M::NONE) => {
        self.set_verb(VerbCmd(1, Verb::CompleteBackward));
        self.take_cmd()
      }
      E(K::Tab, M::NONE) | E(K::Char('I'), M::CTRL) => {
        self.set_verb(VerbCmd(1, Verb::Complete));
        self.take_cmd()
      }

			// Emacs keybinds
			E(K::Char('A'), M::CTRL) => {
				self.set_motion(MotionCmd(1, Motion::StartOfLine));
				self.take_cmd()
			}

			E(K::Char('E'), M::CTRL) => {
				self.set_motion(MotionCmd(1, Motion::EndOfLine));
				self.take_cmd()
			}

			E(k @ (K::Char('F') | K::Char('B')), M::CTRL) => {
				let motion = if k == K::Char('F') { Motion::ForwardCharForced } else { Motion::BackwardCharForced };
				self.set_motion(MotionCmd(1, motion));
				self.take_cmd()
			}

			E(k @ (K::Char('F') | K::Char('B')), M::ALT) => {
				let motion = if k == K::Char('F') {
					Motion::WordMotion(To::End, Word::Normal, Direction::Forward)
				} else {
					Motion::WordMotion(To::Start, Word::Normal, Direction::Backward)
				};
				self.set_motion(MotionCmd(1, motion));
				self.take_cmd()
			}

      E(K::Char('W'), M::CTRL) => {
        self.set_verb(VerbCmd(1, Verb::Delete));
        self.set_motion(MotionCmd(
          1,
          Motion::WordMotion(To::Start, Word::Normal, Direction::Backward),
        ));
        self.take_cmd()
      }

      _ => common_cmds(key),
    }
  }

  fn is_repeatable(&self) -> bool               { true }
  fn as_replay(&self) -> Option<CmdReplay>      { None }
	fn cursor_style(&self) -> String              { "\x1b[6 q".to_string() }
  fn pending_seq(&self) -> Option<String>       { None }
  fn move_cursor_on_undo(&self) -> bool         { true }
  fn clamp_cursor(&self) -> bool                { false }
  fn hist_scroll_start_pos(&self) -> Option<To> { Some(To::End) }
  fn report_mode(&self) -> ModeReport           { ModeReport::Emacs }
}
