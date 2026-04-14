use std::fmt::{self, Display};
use std::str::FromStr;

use unicode_segmentation::UnicodeSegmentation;

use crate::libsh::error::{ShErr, ShResult};
use crate::readline::editcmd::{
  CmdFlags, Direction, EditCmd, Motion, MotionCmd, To, Verb, VerbCmd, Word,
};
use crate::readline::editmode::emacs::Emacs;
use crate::readline::history::History;
use crate::readline::keys::{KeyCode as K, KeyEvent as E, ModKeys as M};
use crate::readline::linebuf::LineBuf;
use crate::{ctrl, motion, verb};

pub mod emacs;
pub mod ex;
pub mod insert;
pub mod normal;
pub mod replace;
pub mod verbatim;
pub mod visual;

pub use ex::ViEx;
pub use insert::ViInsert;
pub use normal::ViNormal;
pub use replace::ViReplace;
pub use verbatim::ViVerbatim;
pub use visual::ViVisual;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeReport {
  Insert,
  Normal,
  Ex,
  Visual,
  Replace,
  Verbatim,
  Emacs,
  Unknown,
}

impl ModeReport {
	pub fn as_edit_mode(&self) -> Box<dyn EditMode> {
		match self {
			ModeReport::Insert => Box::new(ViInsert::new()) as Box<dyn EditMode>,
			ModeReport::Normal => Box::new(ViNormal::new()) as Box<dyn EditMode>,
			ModeReport::Ex => Box::new(ViEx::default()) as Box<dyn EditMode>,
			ModeReport::Visual => Box::new(ViVisual::new()) as Box<dyn EditMode>,
			ModeReport::Replace => Box::new(ViReplace::new()) as Box<dyn EditMode>,
			ModeReport::Verbatim => Box::new(ViVerbatim::new()) as Box<dyn EditMode>,
			ModeReport::Emacs => Box::new(Emacs::new()) as Box<dyn EditMode>,
			ModeReport::Unknown => unimplemented!(),
		}
	}
}

impl Display for ModeReport {
	fn fmt(&self,f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Insert => write!(f,"INSERT"),
			Self::Normal => write!(f,"NORMAL"),
			Self::Ex => write!(f,"COMMAND"),
			Self::Visual => write!(f,"VISUAL"),
			Self::Replace => write!(f,"REPLACE"),
			Self::Verbatim => write!(f,"VERBATIM"),
			Self::Emacs => write!(f,"EMACS"),
			Self::Unknown => write!(f,"UNKNOWN"),
		}
	}
}
impl FromStr for ModeReport {
	type Err = ShErr;
	fn from_str(s: &str) -> Result<Self,Self::Err>{
		match s {
			"INSERT" => Ok(Self::Insert),
			"NORMAL" => Ok(Self::Normal),
			"COMMAND" => Ok(Self::Ex),
			"VISUAL" => Ok(Self::Visual),
			"REPLACE" => Ok(Self::Replace),
			"VERBATIM" => Ok(Self::Verbatim),
			"EMACS" => Ok(Self::Emacs),
			_ => Err(crate::sherr!(ParseErr,"Invalid ModeReport kind: {s}")),

		}
	}
}


#[derive(Debug, Clone)]
pub enum CmdReplay {
  ModeReplay { cmds: Vec<EditCmd>, repeat: u16 },
  Single(EditCmd),
  Motion(Motion),
}

impl CmdReplay {
  pub fn mode(cmds: Vec<EditCmd>, repeat: u16) -> Self {
    Self::ModeReplay { cmds, repeat }
  }
  pub fn single(cmd: EditCmd) -> Self {
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

pub trait EditMode {
  fn handle_key_fallible(&mut self, key: E) -> ShResult<Option<EditCmd>> {
    Ok(self.handle_key(key))
  }
  fn handle_key(&mut self, key: E) -> Option<EditCmd>;
  fn is_repeatable(&self) -> bool;
  fn as_replay(&self) -> Option<CmdReplay>;
  fn cursor_style(&self) -> String;
  fn pending_seq(&self) -> Option<String>;
  fn pending_cursor(&self) -> Option<usize> {
    None
  }
  fn editor(&mut self) -> Option<&mut LineBuf> {
    None
  }
  fn history(&mut self) -> Option<&mut History> {
    None
  }
  fn move_cursor_on_undo(&self) -> bool;
  fn clamp_cursor(&self) -> bool;
  fn hist_scroll_start_pos(&self) -> Option<To>;
  fn report_mode(&self) -> ModeReport;
  fn cmds_from_raw(&mut self, raw: &str) -> Vec<EditCmd> {
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

pub fn common_cmds(key: E) -> Option<EditCmd> {
  let mut pending_cmd = EditCmd::new();
  match key {
    E(K::Home, M::NONE) => pending_cmd.set_motion(motion!(Motion::StartOfLine)),
    E(K::End, M::NONE) => pending_cmd.set_motion(motion!(Motion::EndOfLine)),
    E(K::Enter, M::SHIFT) => pending_cmd.set_verb(verb!(Verb::InsertChar('\n'))),
    E(K::Enter, M::NONE) => pending_cmd.set_verb(verb!(Verb::AcceptLineOrNewline)),
    E(K::Left, M::NONE) => pending_cmd.set_motion(motion!(Motion::BackwardChar)),
    E(K::Left, M::CTRL) => pending_cmd.set_motion(motion!(Motion::WordMotion(
      To::Start,
      Word::Normal,
      Direction::Backward
    ))),
    E(K::Right, M::NONE) => pending_cmd.set_motion(motion!(Motion::ForwardChar)),
    E(K::Right, M::CTRL) => pending_cmd.set_motion(motion!(Motion::WordMotion(
      To::Start,
      Word::Normal,
      Direction::Forward
    ))),
    E(K::Up, mods) => {
      pending_cmd.set_motion(motion!(Motion::LineUp));
      if mods.contains(M::SHIFT) {
        pending_cmd.flags |= CmdFlags::HAS_SHIFT;
      } else if mods.contains(M::CTRL) {
        pending_cmd.flags |= CmdFlags::HAS_CTRL;
      }
    }
    E(K::Down, mods) => {
      pending_cmd.set_motion(motion!(Motion::LineDown));
      if mods.contains(M::SHIFT) {
        pending_cmd.flags |= CmdFlags::HAS_SHIFT;
      } else if mods.contains(M::CTRL) {
        pending_cmd.flags |= CmdFlags::HAS_CTRL;
      }
    }
    E(K::Delete, M::NONE) => {
      pending_cmd.set_verb(verb!(Verb::Delete));
      pending_cmd.set_motion(motion!(Motion::ForwardCharForced));
    }
    E(K::Backspace, M::NONE) | ctrl!('H') => {
      pending_cmd.set_verb(verb!(Verb::Delete));
      pending_cmd.set_motion(motion!(Motion::BackwardCharForced));
    }
    ctrl!('D') => pending_cmd.set_verb(verb!(Verb::EndOfFile)),
    ctrl!('P') => pending_cmd.set_verb(verb!(Verb::HistoryUp)),
    ctrl!('N') => pending_cmd.set_verb(verb!(Verb::HistoryDown)),
    ctrl!('L') => pending_cmd.set_verb(verb!(Verb::ClearScreen)),
    _ => return None,
  }
  Some(pending_cmd)
}
