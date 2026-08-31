use std::fmt::{self, Display};
use std::str::FromStr;

use crate::state::terminal::CursorStyle;
use crate::state::vars::VarStr;
use crate::util::error::{ShErr, ShResult};

use super::{
  KeyCode as K, KeyEvent as E, ModKeys as M, Shed, SimpleEditor,
  editcmd::{self, CmdFlags, Direction, EditCmd, Motion, To, Verb, Word},
  eval, history,
  history::History,
  key, keys,
  linebuf::{self, LineBuf},
  match_loop, motion, register, state, status_msg, verb,
};

mod emacs;
mod ex;
mod insert;
mod normal;
mod parse;
mod remote;
mod replace;
mod search;
mod verbatim;
mod visual;

pub(crate) use self::{
  emacs::Emacs,
  ex::ViEx,
  insert::ViInsert,
  normal::ViNormal,
  parse::{ParseResult, ViParser},
  remote::RemoteMode,
  replace::ViReplace,
  search::{ViSearch, ViSearchRev},
  verbatim::ViVerbatim,
  visual::ViVisual,
};

pub(crate) use ex::{
  AddressRange, COMMANDS, ExCommand, ExLexer, ExLineAddr, ExNdRule, ExNode, ExTk, ExTkRule,
  SubFlags,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeReport {
  Insert,
  Normal,
  Ex,
  Visual,
  Replace,
  Verbatim,
  Emacs,
  Remote,
  Search,
  RevSearch,
}

impl ModeReport {
  pub(crate) fn as_edit_mode(self) -> Box<dyn EditMode> {
    match self {
      ModeReport::Insert => Box::new(ViInsert::new()) as Box<dyn EditMode>,
      ModeReport::Normal => Box::new(ViNormal::new()) as Box<dyn EditMode>,
      ModeReport::Ex => Box::new(ViEx::default()) as Box<dyn EditMode>,
      ModeReport::Visual => Box::new(ViVisual::new()) as Box<dyn EditMode>,
      ModeReport::Replace => Box::new(ViReplace::new()) as Box<dyn EditMode>,
      ModeReport::Verbatim => Box::new(ViVerbatim::new()) as Box<dyn EditMode>,
      ModeReport::Emacs => Box::new(Emacs::new()) as Box<dyn EditMode>,
      ModeReport::Remote => Box::new(RemoteMode) as Box<dyn EditMode>,
      ModeReport::Search => Box::new(ViSearch::new(1)) as Box<dyn EditMode>,
      ModeReport::RevSearch => Box::new(ViSearchRev::new(1)) as Box<dyn EditMode>,
    }
  }
}

impl Display for ModeReport {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Insert => write!(f, "INSERT"),
      Self::Normal => write!(f, "NORMAL"),
      Self::Ex => write!(f, "COMMAND"),
      Self::Visual => write!(f, "VISUAL"),
      Self::Replace => write!(f, "REPLACE"),
      Self::Verbatim => write!(f, "VERBATIM"),
      Self::Emacs => write!(f, "EMACS"),
      Self::Remote => write!(f, "REMOTE"),
      Self::Search | Self::RevSearch => write!(f, "SEARCH"),
    }
  }
}
impl FromStr for ModeReport {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s.to_uppercase().as_str() {
      "INSERT" => Ok(Self::Insert),
      "NORMAL" => Ok(Self::Normal),
      "COMMAND" => Ok(Self::Ex),
      "VISUAL" => Ok(Self::Visual),
      "REPLACE" => Ok(Self::Replace),
      "VERBATIM" => Ok(Self::Verbatim),
      "REMOTE" => Ok(Self::Remote),
      "EMACS" => Ok(Self::Emacs),
      "SEARCH" => Ok(Self::Search),
      "REVSEARCH" => Ok(Self::RevSearch),
      _ => Err(crate::sherr!(ParseErr, "Invalid ModeReport kind: {s}")),
    }
  }
}

#[derive(Debug, Clone)]
pub(crate) enum CmdReplay {
  ModeReplay { cmds: Vec<EditCmd>, repeat: u16 },
  Single(Box<EditCmd>),
}

impl CmdReplay {
  pub fn mode(cmds: Vec<EditCmd>, repeat: u16) -> Self {
    Self::ModeReplay { cmds, repeat }
  }
}

pub(crate) enum CmdState {
  Pending,
  Complete,
  Invalid,
}

pub(crate) trait EditMode {
  fn handle_key_fallible(&mut self, key: E) -> ShResult<Option<EditCmd>> {
    Ok(self.handle_key(key))
  }
  fn handle_key(&mut self, key: E) -> Option<EditCmd>;
  fn is_repeatable(&self) -> bool;
  fn as_replay(&self) -> Option<CmdReplay>;
  fn cursor_style(&self) -> CursorStyle;
  fn pending_seq(&self) -> Option<VarStr>;
  fn pending_cursor(&self) -> Option<usize> {
    None
  }
  fn editor(&mut self) -> Option<&mut LineBuf> {
    None
  }
  fn history(&mut self) -> Option<&mut History> {
    None
  }
  fn is_input_mode(&self) -> bool {
    false
  }
  fn clamp_cursor(&self) -> bool;
  fn report_mode(&self) -> ModeReport;
}

pub fn common_cmds(key: &E) -> Option<EditCmd> {
  let mut pending_cmd = EditCmd::new();
  match key {
    key!(Home) => pending_cmd.set_motion(motion!(Motion::StartOfLine)),
    key!(End) => pending_cmd.set_motion(motion!(Motion::EndOfLine)),
    key!(Tab) => pending_cmd.set_verb(verb!(Verb::InsertChar('\t'))),
    key!(Shift + Enter) => pending_cmd.set_verb(verb!(Verb::InsertChar('\n'))),
    key!(Enter) => pending_cmd.set_verb(verb!(Verb::AcceptLineOrNewline)),
    key!(Left) => pending_cmd.set_motion(motion!(Motion::BackwardChar)),
    key!(Ctrl + 'd') => pending_cmd.set_verb(verb!(Verb::EndOfFile)),
    key!(Ctrl + 'c') => pending_cmd.set_verb(verb!(Verb::Interrupt)),
    key!(Ctrl + 'p') => pending_cmd.set_verb(verb!(Verb::HistoryUp)),
    key!(Ctrl + 'n') => pending_cmd.set_verb(verb!(Verb::HistoryDown)),
    key!(Ctrl + 'l') => pending_cmd.set_verb(verb!(Verb::ClearScreen)),
    key!(Ctrl + 's') => pending_cmd.set_verb(verb!(Verb::AcceptHint)),
    key!(Right) => pending_cmd.set_motion(motion!(Motion::ForwardChar)),
    key!(Ctrl + Left) => pending_cmd.set_motion(motion!(Motion::WordMotion(
      To::Start,
      Word::Normal,
      Direction::Backward
    ))),
    key!(Ctrl + Right) => pending_cmd.set_motion(motion!(Motion::WordMotion(
      To::Start,
      Word::Normal,
      Direction::Forward
    ))),
    key!(Delete) => {
      pending_cmd.set_verb(verb!(Verb::Delete));
      pending_cmd.set_motion(motion!(Motion::ForwardCharForced));
    }
    key!(Backspace) | key!(Ctrl + 'h') => {
      pending_cmd.set_verb(verb!(Verb::Delete));
      pending_cmd.set_motion(motion!(Motion::BackwardCharForced));
    }
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
    _ => return None,
  }
  Some(pending_cmd)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn key(code: K, mods: M) -> E {
    E(code, mods)
  }

  // ─── Motion-only mappings ────────────────────────────────────────────

  #[test]
  fn common_home_maps_to_start_of_line() {
    let cmd = common_cmds(&key(K::Home, M::NONE)).unwrap();
    assert!(cmd.motion_is(&Motion::StartOfLine));
    assert!(cmd.verb().is_none());
  }

  #[test]
  fn common_end_maps_to_end_of_line() {
    let cmd = common_cmds(&key(K::End, M::NONE)).unwrap();
    assert!(cmd.motion_is(&Motion::EndOfLine));
  }

  #[test]
  fn common_left_maps_to_backward_char() {
    let cmd = common_cmds(&key(K::Left, M::NONE)).unwrap();
    assert!(cmd.motion_is(&Motion::BackwardChar));
  }

  #[test]
  fn common_right_maps_to_forward_char() {
    let cmd = common_cmds(&key(K::Right, M::NONE)).unwrap();
    assert!(cmd.motion_is(&Motion::ForwardChar));
  }

  #[test]
  fn common_ctrl_left_word_backward() {
    let cmd = common_cmds(&key(K::Left, M::CTRL)).unwrap();
    assert!(cmd.motion_is(&Motion::WordMotion(
      To::Start,
      Word::Normal,
      Direction::Backward,
    )));
  }

  #[test]
  fn common_ctrl_right_word_forward() {
    let cmd = common_cmds(&key(K::Right, M::CTRL)).unwrap();
    assert!(cmd.motion_is(&Motion::WordMotion(
      To::Start,
      Word::Normal,
      Direction::Forward,
    )));
  }

  // ─── Verb-only mappings ──────────────────────────────────────────────

  #[test]
  fn common_tab_inserts_tab_char() {
    let cmd = common_cmds(&key(K::Tab, M::NONE)).unwrap();
    assert!(cmd.verb_is(&Verb::InsertChar('\t')));
  }

  #[test]
  fn common_enter_accepts_line() {
    let cmd = common_cmds(&key(K::Enter, M::NONE)).unwrap();
    assert!(cmd.verb_is(&Verb::AcceptLineOrNewline));
  }

  #[test]
  fn common_shift_enter_inserts_newline() {
    let cmd = common_cmds(&key(K::Enter, M::SHIFT)).unwrap();
    assert!(cmd.verb_is(&Verb::InsertChar('\n')));
  }

  #[test]
  fn common_ctrl_d_end_of_file() {
    let cmd = common_cmds(&key(K::Char('d'), M::CTRL)).unwrap();
    assert!(cmd.verb_is(&Verb::EndOfFile));
  }

  #[test]
  fn common_ctrl_c_interrupt() {
    let cmd = common_cmds(&key(K::Char('c'), M::CTRL)).unwrap();
    assert!(cmd.verb_is(&Verb::Interrupt));
  }

  #[test]
  fn common_ctrl_p_history_up() {
    let cmd = common_cmds(&key(K::Char('p'), M::CTRL)).unwrap();
    assert!(cmd.verb_is(&Verb::HistoryUp));
  }

  #[test]
  fn common_ctrl_n_history_down() {
    let cmd = common_cmds(&key(K::Char('n'), M::CTRL)).unwrap();
    assert!(cmd.verb_is(&Verb::HistoryDown));
  }

  #[test]
  fn common_ctrl_l_clear_screen() {
    let cmd = common_cmds(&key(K::Char('l'), M::CTRL)).unwrap();
    assert!(cmd.verb_is(&Verb::ClearScreen));
  }

  #[test]
  fn common_ctrl_s_accept_hint() {
    let cmd = common_cmds(&key(K::Char('s'), M::CTRL)).unwrap();
    assert!(cmd.verb_is(&Verb::AcceptHint));
  }

  // ─── Verb + Motion combos ────────────────────────────────────────────

  #[test]
  fn common_delete_deletes_forward_char() {
    let cmd = common_cmds(&key(K::Delete, M::NONE)).unwrap();
    assert!(cmd.verb_is(&Verb::Delete));
    assert!(cmd.motion_is(&Motion::ForwardCharForced));
  }

  #[test]
  fn common_backspace_deletes_backward_char() {
    let cmd = common_cmds(&key(K::Backspace, M::NONE)).unwrap();
    assert!(cmd.verb_is(&Verb::Delete));
    assert!(cmd.motion_is(&Motion::BackwardCharForced));
  }

  #[test]
  fn common_ctrl_h_is_backspace_alias() {
    let cmd = common_cmds(&key(K::Char('h'), M::CTRL)).unwrap();
    assert!(cmd.verb_is(&Verb::Delete));
    assert!(cmd.motion_is(&Motion::BackwardCharForced));
  }

  // ─── Up/Down with modifier flags ─────────────────────────────────────

  #[test]
  fn common_up_plain() {
    let cmd = common_cmds(&key(K::Up, M::NONE)).unwrap();
    assert!(cmd.motion_is(&Motion::LineUp));
    assert!(!cmd.flags.contains(CmdFlags::HAS_SHIFT));
    assert!(!cmd.flags.contains(CmdFlags::HAS_CTRL));
  }

  #[test]
  fn common_shift_up_sets_has_shift_flag() {
    let cmd = common_cmds(&key(K::Up, M::SHIFT)).unwrap();
    assert!(cmd.motion_is(&Motion::LineUp));
    assert!(cmd.flags.contains(CmdFlags::HAS_SHIFT));
  }

  #[test]
  fn common_ctrl_up_sets_has_ctrl_flag() {
    let cmd = common_cmds(&key(K::Up, M::CTRL)).unwrap();
    assert!(cmd.motion_is(&Motion::LineUp));
    assert!(cmd.flags.contains(CmdFlags::HAS_CTRL));
  }

  #[test]
  fn common_down_plain() {
    let cmd = common_cmds(&key(K::Down, M::NONE)).unwrap();
    assert!(cmd.motion_is(&Motion::LineDown));
    assert!(!cmd.flags.contains(CmdFlags::HAS_SHIFT));
    assert!(!cmd.flags.contains(CmdFlags::HAS_CTRL));
  }

  #[test]
  fn common_shift_down_sets_has_shift_flag() {
    let cmd = common_cmds(&key(K::Down, M::SHIFT)).unwrap();
    assert!(cmd.motion_is(&Motion::LineDown));
    assert!(cmd.flags.contains(CmdFlags::HAS_SHIFT));
  }

  #[test]
  fn common_ctrl_down_sets_has_ctrl_flag() {
    let cmd = common_cmds(&key(K::Down, M::CTRL)).unwrap();
    assert!(cmd.motion_is(&Motion::LineDown));
    assert!(cmd.flags.contains(CmdFlags::HAS_CTRL));
  }

  #[test]
  fn common_up_shift_takes_precedence_over_ctrl() {
    // The code is `if SHIFT { ... } else if CTRL { ... }` — so when
    // both are set, SHIFT wins and HAS_CTRL stays unset.
    let cmd = common_cmds(&key(K::Up, M::SHIFT | M::CTRL)).unwrap();
    assert!(cmd.flags.contains(CmdFlags::HAS_SHIFT));
    assert!(!cmd.flags.contains(CmdFlags::HAS_CTRL));
  }

  // ─── Unmapped keys → None ────────────────────────────────────────────

  #[test]
  fn common_unmapped_key_returns_none() {
    // A plain letter with no modifiers isn't in the common table.
    assert!(common_cmds(&key(K::Char('a'), M::NONE)).is_none());
    // Random control key not in the list.
    assert!(common_cmds(&key(K::Char('q'), M::CTRL)).is_none());
    // Function key — not handled here.
    assert!(common_cmds(&key(K::F(5), M::NONE)).is_none());
  }

  // ─── ModeReport::as_edit_mode round-trip ────────────────────────

  #[test]
  fn as_edit_mode_round_trips_through_report_mode() {
    // Each variant should construct an EditMode whose `report_mode()`
    // returns the original variant. Acts as a sanity check that the
    // 1:1 mapping isn't drifted (e.g., Insert → ViNormal by accident).
    let cases = [
      ModeReport::Insert,
      ModeReport::Normal,
      ModeReport::Ex,
      ModeReport::Visual,
      ModeReport::Replace,
      ModeReport::Verbatim,
      ModeReport::Emacs,
      ModeReport::Remote,
      ModeReport::Search,
      ModeReport::RevSearch,
    ];
    for variant in cases {
      let mode = variant.as_edit_mode();
      assert_eq!(
        mode.report_mode(),
        variant,
        "{variant:?} round-tripped to {:?}",
        mode.report_mode()
      );
    }
  }
}
