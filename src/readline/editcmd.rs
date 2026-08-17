use std::path::PathBuf;

use bitflags::bitflags;

use crate::{state::vars::VarStr, verb};

use super::{
  editmode::ExNode,
  linebuf::{Grapheme, Pos, SelectShape},
  try_var,
};

pub(crate) use super::util::Direction;

use super::{editmode::ExNdRule, register::RegisterName};

bitflags! {
  #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct CmdFlags: u32 {
    const VISUAL = 1<<0;
    const VISUAL_LINE = 1<<1;
    const VISUAL_BLOCK = 1<<2;
    const EXIT_CUR_MODE = 1<<3;
    const IS_EX_CMD = 1<<4;
    const HAS_SHIFT = 1<<5;
    const HAS_CTRL = 1<<6;
    const IS_SUBMIT = 1<<7;
    const IS_CANCEL = 1<<8;
  }
}

#[derive(Clone, Default, Debug)]
pub(crate) struct EditCmd {
  pub register: RegisterName,
  pub verb: Option<Cmd<Verb>>,
  pub motion: Option<Cmd<Motion>>,
  pub raw_seq: String,
  pub flags: CmdFlags,
}

impl EditCmd {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn set_motion(&mut self, motion: Cmd<Motion>) {
    self.motion = Some(motion);
  }
  pub fn set_verb(&mut self, verb: Cmd<Verb>) {
    self.verb = Some(verb);
  }
  pub fn new_with_verb(&self, verb: Option<Cmd<Verb>>) -> Self {
    Self {
      verb,
      ..self.clone()
    }
  }
  pub fn verb_is(&self, verb: &Verb) -> bool {
    self.verb().is_some_and(|v| v.1 == *verb)
  }
  pub fn motion_is(&self, motion: &Motion) -> bool {
    self.motion().is_some_and(|m| m.1 == *motion)
  }
  pub fn new_with_motion(&self, motion: Option<Cmd<Motion>>) -> Self {
    Self {
      motion,
      ..self.clone()
    }
  }
  pub fn history_scroll_offset(&self) -> Option<isize> {
    if matches!(
      self.verb().map(|v| &v.1),
      Some(Verb::HistoryUp | Verb::HistoryDown)
    ) {
      let count = self.verb().map_or(1, |v| v.0);
      let offset = match self.verb().map(|v| &v.1) {
        Some(Verb::HistoryUp) => -(count as isize),
        Some(Verb::HistoryDown) => count as isize,
        _ => 0,
      };
      Some(offset)
    } else if matches!(
      self.motion().map(|m| &m.1),
      Some(Motion::LineUp | Motion::LineDown)
    ) {
      let count = self.motion().map_or(1, |m| m.0);
      let offset = match self.motion().map(|m| &m.1) {
        Some(Motion::LineUp) => -(count as isize),
        Some(Motion::LineDown) => count as isize,
        _ => 0,
      };
      Some(offset)
    } else {
      None
    }
  }
  pub fn verb(&self) -> Option<&Cmd<Verb>> {
    self.verb.as_ref()
  }
  pub fn verb_mut(&mut self) -> Option<&mut Cmd<Verb>> {
    self.verb.as_mut()
  }
  pub fn motion(&self) -> Option<&Cmd<Motion>> {
    self.motion.as_ref()
  }
  pub fn verb_count(&self) -> usize {
    self.verb.as_ref().map_or(1, |v| v.0)
  }
  pub fn normalize_counts(&mut self) {
    let Some(verb) = self.verb.as_mut() else {
      return;
    };
    let Some(motion) = self.motion.as_mut() else {
      return;
    };
    let Cmd(v_count, _) = verb;
    let Cmd(m_count, _) = motion;
    let product = *v_count * *m_count;
    verb.0 = 1;
    motion.0 = product;
  }
  pub fn is_repeatable(&self) -> bool {
    self.verb.as_ref().is_some_and(|v| v.1.is_repeatable())
  }
  pub fn is_edit(&self) -> bool {
    self.verb.as_ref().is_some_and(|v| v.1.is_edit())
  }
  pub fn is_cmd_repeat(&self) -> bool {
    self
      .verb
      .as_ref()
      .is_some_and(|v| matches!(v.1, Verb::RepeatLast))
  }
  pub fn is_virtual_scroll(&self) -> bool {
    self.verb.as_ref().is_none()
      && self
        .motion
        .as_ref()
        .is_some_and(|v| matches!(v.1, Motion::LineUp | Motion::LineDown))
      && self
        .flags
        .intersects(CmdFlags::HAS_SHIFT | CmdFlags::HAS_CTRL)
  }
  pub fn is_motion_repeat(&self) -> bool {
    self
      .motion
      .as_ref()
      .is_some_and(|m| matches!(m.1, Motion::RepeatMotion | Motion::RepeatMotionRev))
  }
  pub fn is_char_search(&self) -> bool {
    self
      .motion
      .as_ref()
      .is_some_and(|m| matches!(m.1, Motion::CharSearch(..)))
  }
  pub fn is_separator_insert(&self) -> bool {
    self.verb.as_ref().is_some_and(|v| {
      let mut ifs = try_var!("IFS").unwrap_or(" \t\n".into()).to_string();
      ifs.push(';');
      match &v.1 {
        Verb::AcceptLineOrNewline => true,
        Verb::InsertChar(ch) => ifs.contains(*ch),
        Verb::Insert(s) => s.len() == 1 && ifs.contains(s.to_str_lossy().chars().next().unwrap()),
        _ => false,
      }
    })
  }
  pub fn try_get_normal_seq(&self) -> Option<(&str, bool)> {
    let Some(Cmd(_, Verb::ExCmd(node))) = self.verb.as_ref() else {
      return None;
    };
    find_normal_seq(node)
  }
  /// Constructs a plain `:w` command, which is used as the default write command for `:x` and `:wq`
  pub fn plain_write() -> Self {
    EditCmd {
      register: RegisterName::default(),
      verb: Some(verb!(Verb::ExCmd(ExNode {
        address: None,
        bang: false,
        kind: ExNdRule::Write(WriteDest::File(None)),
      }))),
      motion: None,
      raw_seq: String::new(),
      flags: CmdFlags::empty(),
    }
  }
}

/// Walks an `ExNode` tree, descending through any nesting Global wrappers,
/// looking for a Normal leaf. Returns the seq if found.
fn find_normal_seq(node: &ExNode) -> Option<(&str, bool)> {
  match &node.kind {
    ExNdRule::Normal { seq, bang } => Some((seq, *bang)),
    ExNdRule::Global { nested, .. } => find_normal_seq(nested),
    _ => None,
  }
}

impl EditCmd {
  pub fn is_quit(&self) -> bool {
    matches!(
      self.verb.as_ref(),
      Some(Cmd(
        _,
        Verb::ExCmd(ExNode {
          address: _,
          bang: _,
          kind: ExNdRule::Quit
        })
      ))
    )
  }
  pub fn is_write_quit(&self) -> bool {
    matches!(
      self.verb.as_ref(),
      Some(Cmd(
        _,
        Verb::ExCmd(ExNode {
          address: _,
          bang: _,
          kind: ExNdRule::WriteQuit
        })
      ))
    )
  }
  pub fn is_shell_cmd(&self) -> bool {
    matches!(
      self.verb.as_ref(),
      Some(Cmd(
        _,
        Verb::ExCmd(ExNode {
          address: _,
          bang: _,
          kind: ExNdRule::Shell(_)
        })
      ))
    )
  }
  pub fn is_submit_action(&self) -> bool {
    self
      .verb
      .as_ref()
      .is_some_and(|v| matches!(v.1, Verb::AcceptLineOrNewline))
      || self.flags.contains(CmdFlags::IS_SUBMIT)
  }
  pub fn is_undo_op(&self) -> bool {
    self
      .verb
      .as_ref()
      .is_some_and(|v| matches!(v.1, Verb::Undo | Verb::Redo))
  }
  pub fn is_line_motion(&self) -> bool {
    self
      .motion
      .as_ref()
      .is_some_and(|m| matches!(m.1, Motion::LineUp | Motion::LineDown))
  }
  /// If a `EditCmd` has a linewise motion, but no verb, we change it to charwise
  pub fn is_mode_transition(&self) -> bool {
    self.verb.as_ref().is_some_and(|v| {
      matches!(
        v.1,
        Verb::Change
          | Verb::VerbatimMode
          | Verb::ExMode
          | Verb::InsertMode
          | Verb::SearchMode
          | Verb::RevSearchMode
          | Verb::InsertModeLineBreak(_)
          | Verb::NormalMode
          | Verb::VisualModeSelectLast
          | Verb::VisualMode
          | Verb::VisualModeLine
          | Verb::ReplaceMode
      ) || self.flags.contains(CmdFlags::EXIT_CUR_MODE)
    })
  }
}

#[derive(Clone, Debug)]
pub struct Cmd<T>(pub usize, pub T);

pub fn invert_char_motion(motion: Cmd<Motion>) -> Cmd<Motion> {
  let Cmd(count, Motion::CharSearch(dir, dest, ch)) = motion else {
    return motion;
  };
  let new_dir = match dir {
    Direction::Forward => Direction::Backward,
    Direction::Backward => Direction::Forward,
  };
  Cmd(count, Motion::CharSearch(new_dir, dest, ch))
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum StashListArg {
  Stack,
  Named,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum StashArgs {
  Push(Option<VarStr>),
  Pop(Option<VarStr>),
  Drop(Option<VarStr>),
  Apply(Option<VarStr>),
  Insert(Option<VarStr>),
  Swap(Option<VarStr>),
  List(Option<StashListArg>),
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Verb {
  // misc stuff
  HistoryUp,
  HistoryDown,
  ClearScreen,
  AcceptHint,
  Expand,
  ExpandAll,

  // emacs stuff
  Kill,
  KillPut,
  KillCycle,
  TransposeChar,
  TransposeWord,
  Capitalize,

  // vi stuff
  DeleteOrEof,
  Delete,
  Change,
  Yank,
  Rot13,                         // lol
  ReplaceChar(char),             // char to replace with, number of chars to replace
  ReplaceCharInplace(char, u16), // char to replace with, number of chars to replace
  ReplaceOrInsertChar(char),
  ToggleCaseInplace(u16), // Number of chars to toggle
  ToggleCaseRange,
  IncrementNumber(u16),
  DecrementNumber(u16),
  ToLower,
  ToUpper,
  Complete,
  RecordMacro,
  PlayMacro,
  Undo,
  Redo,
  RepeatLast,
  Put(Anchor),
  ReplaceMode,
  VerbatimMode,
  InsertMode,
  InsertModeLineBreak(Anchor),
  NormalMode,
  SearchMode,
  RevSearchMode,
  ExMode,
  VisualMode,
  VisualModeLine,
  VisualModeSelectLast,
  SwapVisualAnchor,
  JoinLines,
  InsertChar(char),
  Insert(VarStr),
  Indent,
  Dedent,
  Equalize,
  AcceptLineOrNewline,
  EndOfFile,
  Interrupt,
  PrintPosition,

  ExCmd(ExNode),
}

impl Verb {
  pub fn is_repeatable(&self) -> bool {
    matches!(
      self,
      Self::Delete
        | Self::Change
        | Self::ReplaceChar(_)
        | Self::ReplaceCharInplace(_, _)
        | Self::ReplaceOrInsertChar(_)
        | Self::ToLower
        | Self::ToUpper
        | Self::ToggleCaseRange
        | Self::ToggleCaseInplace(_)
        | Self::Put(_)
        | Self::ReplaceMode
        | Self::InsertMode
        | Self::VisualMode
        | Self::VisualModeLine
        | Self::InsertModeLineBreak(_)
        | Self::Rot13
        | Self::IncrementNumber(_)
        | Self::DecrementNumber(_)
        | Self::JoinLines
        | Self::InsertChar(_)
        | Self::Insert(_)
        | Self::Indent
        | Self::Dedent
        | Self::Equalize
    )
  }
  pub fn is_edit(&self) -> bool {
    matches!(
      self,
      Self::Delete
        | Self::Change
        | Self::ReplaceChar(_)
        | Self::ReplaceCharInplace(_, _)
        | Self::ReplaceOrInsertChar(_)
        | Self::ToggleCaseRange
        | Self::ToggleCaseInplace(_)
        | Self::ToLower
        | Self::ToUpper
        | Self::RepeatLast
        | Self::Put(_)
        | Self::ReplaceMode
        | Self::InsertModeLineBreak(_)
        | Self::JoinLines
        | Self::InsertChar(_)
        | Self::Insert(_)
        | Self::AcceptLineOrNewline
        | Self::Dedent
        | Self::Indent
        | Self::Equalize
        | Self::Rot13
        | Self::EndOfFile
        | Self::IncrementNumber(_)
        | Self::DecrementNumber(_)
    )
  }
  pub fn is_char_insert(&self) -> bool {
    matches!(self, Self::InsertChar(_) | Self::ReplaceChar(_))
  }
}

#[derive(Debug, Clone, Eq, PartialEq, PartialOrd, Ord)]
pub enum LineAddr {
  Number(usize),
  Current,
  Last,
  Offset(isize),
  Pattern(VarStr),
  PatternRev(VarStr),
  Mark(char),
}

#[expect(clippy::enum_variant_names)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Motion {
  WholeLine,
  TextObj(TextObj),
  EndOfLastWord,
  StartOfFirstWord,
  StartOfLine,
  EndOfLine,
  WordMotion(To, Word, Direction),
  CharSearch(Direction, Dest, Grapheme),
  BackwardChar,
  ForwardChar,
  BackwardCharForced, // These two variants can cross line boundaries
  ForwardCharForced,
  LineUp,
  LineDown,
  StartOfBuffer,
  EndOfBuffer,
  ToColumn,
  ToDelimMatch,
  HalfScreenDown,
  HalfScreenUp,
  ToBrace(Direction),
  ToParen(Direction),
  CharRange(Pos, Pos),
  LineRange(LineAddr, LineAddr),
  Line(LineAddr),
  Search(VarStr, Direction),
  RepeatSearch,
  RepeatSearchRev,
  BlockRange(Pos, Pos),
  Selection(SelectShape), // used in dot-repeats of visual mode
  RepeatMotion,
  RepeatMotionRev,
  Null,
}

impl Motion {
  /// Builds a `Motion::WordMotion` from the given characters
  /// takes a slice because of the 'ge' case.
  /// Only works for w, W, b, B, e, and E, and 'ge'
  pub fn word_motion(chars: &[char]) -> Option<Self> {
    match chars.first()? {
      'w' => Some(Motion::WordMotion(
        To::Start,
        Word::Normal,
        Direction::Forward,
      )),
      'W' => Some(Motion::WordMotion(To::Start, Word::Big, Direction::Forward)),
      'b' => Some(Motion::WordMotion(
        To::Start,
        Word::Normal,
        Direction::Backward,
      )),
      'B' => Some(Motion::WordMotion(
        To::Start,
        Word::Big,
        Direction::Backward,
      )),
      'e' => Some(Motion::WordMotion(
        To::End,
        Word::Normal,
        Direction::Forward,
      )),
      'E' => Some(Motion::WordMotion(To::End, Word::Big, Direction::Forward)),
      'g' => {
        let next = chars.get(1)?;
        match next {
          'e' => Some(Motion::WordMotion(
            To::End,
            Word::Normal,
            Direction::Backward,
          )),
          'E' => Some(Motion::WordMotion(To::End, Word::Big, Direction::Backward)),
          _ => None,
        }
      }
      _ => None,
    }
  }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Anchor {
  After,
  Before,
}
#[derive(Copy, Debug, Clone, Eq, PartialEq)]
pub enum TextObj {
  /// `iw`, `aw` - inner word, around word
  Word(Word, Bound),

  /// `)`, `(` - forward, backward
  Sentence(Direction),

  /// `}`, `{` - forward, backward
  Paragraph(Direction),

  WholeSentence(Bound),
  WholeParagraph(Bound),

  /// `i"`, `a"` - inner/around double quotes
  DoubleQuote(Bound),
  /// `i'`, `a'`
  SingleQuote(Bound),
  BacktickQuote(Bound),

  /// `i)`, `a)` - round parens
  Paren(Bound),
  /// `i]`, `a]`
  Bracket(Bound),
  /// `i}`, `a}`
  Brace(Bound),
  /// `i<`, `a<`
  Angle(Bound),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Word {
  Big,
  Normal,
}
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Bound {
  Inside,
  Around,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Dest {
  On,
  Before,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum To {
  Start,
  End,
}

// Ex-mode types

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReadSrc {
  File(PathBuf),
  Cmd(VarStr),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum WriteDest {
  File(Option<PathBuf>),
  FileAppend(Option<PathBuf>),
  Cmd(VarStr),
}

#[cfg(test)]
mod history_scroll_offset_tests {
  use super::*;

  fn cmd_with_verb(verb: Verb, count: usize) -> EditCmd {
    let mut c = EditCmd::new();
    c.set_verb(Cmd(count, verb));
    c
  }

  fn cmd_with_motion(motion: Motion, count: usize) -> EditCmd {
    let mut c = EditCmd::new();
    c.set_motion(Cmd(count, motion));
    c
  }

  // ─── Verb path ────────────────────────────────────────────────────

  #[test]
  fn history_up_returns_negative_count() {
    assert_eq!(
      cmd_with_verb(Verb::HistoryUp, 1).history_scroll_offset(),
      Some(-1)
    );
  }

  #[test]
  fn history_down_returns_positive_count() {
    assert_eq!(
      cmd_with_verb(Verb::HistoryDown, 1).history_scroll_offset(),
      Some(1)
    );
  }

  #[test]
  fn history_up_with_count_n() {
    assert_eq!(
      cmd_with_verb(Verb::HistoryUp, 5).history_scroll_offset(),
      Some(-5)
    );
  }

  #[test]
  fn history_down_with_count_n() {
    assert_eq!(
      cmd_with_verb(Verb::HistoryDown, 5).history_scroll_offset(),
      Some(5)
    );
  }

  // ─── Motion path (LineUp / LineDown) ─────────────────────────────

  #[test]
  fn line_up_motion_returns_negative_count() {
    assert_eq!(
      cmd_with_motion(Motion::LineUp, 1).history_scroll_offset(),
      Some(-1)
    );
  }

  #[test]
  fn line_down_motion_returns_positive_count() {
    assert_eq!(
      cmd_with_motion(Motion::LineDown, 1).history_scroll_offset(),
      Some(1)
    );
  }

  #[test]
  fn line_up_motion_with_count_n() {
    assert_eq!(
      cmd_with_motion(Motion::LineUp, 3).history_scroll_offset(),
      Some(-3)
    );
  }

  // ─── No matching verb/motion → None ──────────────────────────────

  #[test]
  fn unrelated_verb_returns_none() {
    assert_eq!(cmd_with_verb(Verb::Delete, 1).history_scroll_offset(), None);
  }

  #[test]
  fn unrelated_motion_returns_none() {
    assert_eq!(
      cmd_with_motion(Motion::ForwardChar, 1).history_scroll_offset(),
      None
    );
  }

  #[test]
  fn empty_cmd_returns_none() {
    assert_eq!(EditCmd::new().history_scroll_offset(), None);
  }
}
