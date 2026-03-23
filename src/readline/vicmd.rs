use std::path::PathBuf;

use bitflags::bitflags;

use crate::{readline::{
  linebuf::{Grapheme, Pos},
  vimode::ex::SubFlags,
}, state::read_shopts};

use super::register::{RegisterContent, append_register, read_register, write_register};

//TODO: write tests that take edit results and cursor positions from actual
// neovim edits and test them against the behavior of this editor

#[derive(Clone, Copy, Debug)]
pub struct RegisterName {
  name: Option<char>,
  count: usize,
  append: bool,
}

impl RegisterName {
  pub fn new(name: Option<char>, count: Option<usize>) -> Self {
    let Some(ch) = name else {
      return Self::default();
    };

    let append = ch.is_uppercase();
    let name = ch.to_ascii_lowercase();
    Self {
      name: Some(name),
      count: count.unwrap_or(1),
      append,
    }
  }
  pub fn name(&self) -> Option<char> {
    self.name
  }
  pub fn is_append(&self) -> bool {
    self.append
  }
  pub fn count(&self) -> usize {
    self.count
  }
  pub fn write_to_register(&self, buf: RegisterContent) {
    if self.append {
      append_register(self.name, buf);
    } else {
      write_register(self.name, buf);
    }
  }
  pub fn read_from_register(&self) -> Option<RegisterContent> {
    read_register(self.name)
  }
}

impl Default for RegisterName {
  fn default() -> Self {
    Self {
      name: None,
      count: 1,
      append: false,
    }
  }
}

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
  }
}

#[derive(Clone, Default, Debug)]
pub struct ViCmd {
  pub register: RegisterName,
  pub verb: Option<VerbCmd>,
  pub motion: Option<MotionCmd>,
  pub raw_seq: String,
  pub flags: CmdFlags,
}

impl ViCmd {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn set_motion(&mut self, motion: MotionCmd) {
    self.motion = Some(motion)
  }
  pub fn set_verb(&mut self, verb: VerbCmd) {
    self.verb = Some(verb)
  }
  pub fn verb(&self) -> Option<&VerbCmd> {
    self.verb.as_ref()
  }
  pub fn motion(&self) -> Option<&MotionCmd> {
    self.motion.as_ref()
  }
  pub fn verb_count(&self) -> usize {
    self.verb.as_ref().map(|v| v.0).unwrap_or(1)
  }
  pub fn motion_count(&self) -> usize {
    self.motion.as_ref().map(|m| m.0).unwrap_or(1)
  }
  pub fn normalize_counts(&mut self) {
    let Some(verb) = self.verb.as_mut() else {
      return;
    };
    let Some(motion) = self.motion.as_mut() else {
      return;
    };
    let VerbCmd(v_count, _) = verb;
    let MotionCmd(m_count, _) = motion;
    let product = *v_count * *m_count;
    verb.0 = 1;
    motion.0 = product;
  }
  pub fn is_repeatable(&self) -> bool {
    self.verb.as_ref().is_some_and(|v| v.1.is_repeatable())
  }
  pub fn is_cmd_repeat(&self) -> bool {
    self
      .verb
      .as_ref()
      .is_some_and(|v| matches!(v.1, Verb::RepeatLast))
  }
	pub fn is_virtual_scroll(&self) -> bool {
		read_shopts(|o| o.prompt.hist_cat)
		&& self.verb.as_ref().is_none()
		&& self.motion.as_ref().is_some_and(|v| matches!(v.1, Motion::LineUp | Motion::LineDown))
		&& self.flags.intersects(CmdFlags::HAS_SHIFT | CmdFlags::HAS_CTRL)
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
  pub fn is_submit_action(&self) -> bool {
    self
      .verb
      .as_ref()
      .is_some_and(|v| matches!(v.1, Verb::AcceptLineOrNewline))
  }
  pub fn is_undo_op(&self) -> bool {
    self
      .verb
      .as_ref()
      .is_some_and(|v| matches!(v.1, Verb::Undo | Verb::Redo))
  }
  pub fn is_inplace_edit(&self) -> bool {
    self.verb.as_ref().is_some_and(|v| {
      matches!(
        v.1,
        Verb::ReplaceCharInplace(_, _) | Verb::ToggleCaseInplace(_)
      )
    }) && self.motion.is_none()
  }
  pub fn is_line_motion(&self) -> bool {
    self
      .motion
      .as_ref()
      .is_some_and(|m| matches!(m.1, Motion::LineUp | Motion::LineDown))
  }
  /// If a ViCmd has a linewise motion, but no verb, we change it to charwise
  pub fn is_mode_transition(&self) -> bool {
    self.verb.as_ref().is_some_and(|v| {
      matches!(
        v.1,
        Verb::Change
          | Verb::VerbatimMode
          | Verb::ExMode
          | Verb::InsertMode
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
pub struct VerbCmd(pub usize, pub Verb);
#[derive(Clone, Debug)]
pub struct MotionCmd(pub usize, pub Motion);

impl MotionCmd {
  pub fn invert_char_motion(self) -> Self {
    let MotionCmd(count, Motion::CharSearch(dir, dest, ch)) = self else {
      unreachable!()
    };
    let new_dir = match dir {
      Direction::Forward => Direction::Backward,
      Direction::Backward => Direction::Forward,
    };
    MotionCmd(count, Motion::CharSearch(new_dir, dest, ch))
  }
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Verb {
  Delete,
  Change,
  Yank,
  Rot13,                         // lol
  ReplaceChar(char),             // char to replace with, number of chars to replace
  ReplaceCharInplace(char, u16), // char to replace with, number of chars to replace
  ToggleCaseInplace(u16),        // Number of chars to toggle
  ToggleCaseRange,
  IncrementNumber(u16),
  DecrementNumber(u16),
  ToLower,
  ToUpper,
  Complete,
  CompleteBackward,
  Undo,
  Redo,
  RepeatLast,
  Put(Anchor),
  ReplaceMode,
  VerbatimMode,
  InsertMode,
  InsertModeLineBreak(Anchor),
  NormalMode,
  VisualMode,
  VisualModeLine,
  VisualModeBlock, // dont even know if im going to implement this
  VisualModeSelectLast,
  SwapVisualAnchor,
  JoinLines,
  InsertChar(char),
  Insert(String),
  Indent,
  Dedent,
  Equalize,
  AcceptLineOrNewline,
  EndOfFile,
	PrintPosition,
  // Ex-mode verbs
  ExMode,
  ShellCmd(String),
  Normal(String),
  Read(ReadSrc),
  Write(WriteDest),
  Edit(PathBuf),
  Quit,
  Substitute(String, String, SubFlags),
  RepeatSubstitute,
  RepeatGlobal,
}

impl Verb {
  pub fn is_repeatable(&self) -> bool {
    matches!(
      self,
      Self::Delete
        | Self::Change
        | Self::ReplaceChar(_)
        | Self::ReplaceCharInplace(_, _)
        | Self::ToLower
        | Self::ToUpper
        | Self::ToggleCaseRange
        | Self::ToggleCaseInplace(_)
        | Self::Put(_)
        | Self::ReplaceMode
        | Self::InsertModeLineBreak(_)
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
    matches!(
      self,
      Self::InsertChar(_) | Self::ReplaceChar(_)
    )
  }
}

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
  WholeBuffer,
  StartOfBuffer,
  EndOfBuffer,
  ToColumn,
  ToDelimMatch,
	HalfScreenDown,
	HalfScreenUp,
  ToBrace(Direction),
  ToBracket(Direction),
  ToParen(Direction),
  CharRange(Pos, Pos),
  LineRange(usize, usize),
  BlockRange(Pos, Pos),
  RepeatMotion,
  RepeatMotionRev,
  Null,
  // Ex-mode motions
  Global(Val),
  NotGlobal(Val),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotionBehavior {
  Exclusive,
  Inclusive,
  Linewise,
}

impl Motion {
  pub fn behavior(&self) -> MotionBehavior {
    if self.is_linewise() {
      MotionBehavior::Linewise
    } else if self.is_exclusive() {
      MotionBehavior::Exclusive
    } else {
      MotionBehavior::Inclusive
    }
  }
  pub fn is_exclusive(&self) -> bool {
    matches!(
      &self,
      Self::StartOfLine
        | Self::StartOfFirstWord
        | Self::ToColumn
        | Self::TextObj(TextObj::Sentence(_))
        | Self::TextObj(TextObj::Paragraph(_))
        | Self::CharSearch(Direction::Backward, _, _)
        | Self::WordMotion(To::Start, _, _)
        | Self::ToBrace(_)
        | Self::ToBracket(_)
        | Self::ToParen(_)
        | Self::CharRange(_, _)
    )
  }
  pub fn is_linewise(&self) -> bool {
    matches!(self, Self::WholeLine | Self::LineUp | Self::LineDown)
  }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Anchor {
  After,
  Before,
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TextObj {
  /// `iw`, `aw` — inner word, around word
  Word(Word, Bound),

  /// `)`, `(` — forward, backward
  Sentence(Direction),

  /// `}`, `{` — forward, backward
  Paragraph(Direction),

  WholeSentence(Bound),
  WholeParagraph(Bound),

  /// `i"`, `a"` — inner/around double quotes
  DoubleQuote(Bound),
  /// `i'`, `a'`
  SingleQuote(Bound),
  /// `i\``, `a\``
  BacktickQuote(Bound),

  /// `i)`, `a)` — round parens
  Paren(Bound),
  /// `i]`, `a]`
  Bracket(Bound),
  /// `i}`, `a}`
  Brace(Bound),
  /// `i<`, `a<`
  Angle(Bound),

  /// `it`, `at` — HTML/XML tags
  Tag(Bound),

  /// Custom user-defined objects maybe?
  Custom(char),
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

#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub enum Direction {
  #[default]
  Forward,
  Backward,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Dest {
  On,
  Before,
  After,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum To {
  Start,
  End,
}

// Ex-mode types

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReadSrc {
  File(std::path::PathBuf),
  Cmd(String),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum WriteDest {
  File(std::path::PathBuf),
  FileAppend(std::path::PathBuf),
  Cmd(String),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Val {
  Str(String),
  Regex(String),
}

impl Val {
  pub fn new_str(s: String) -> Self {
    Self::Str(s)
  }
}
