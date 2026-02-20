use std::iter::Peekable;
use std::str::Chars;

use nix::NixPath;
use unicode_segmentation::UnicodeSegmentation;

use super::keys::{KeyCode as K, KeyEvent as E, ModKeys as M};
use super::vicmd::{
  Anchor, Bound, CmdFlags, Dest, Direction, Motion, MotionCmd, RegisterName, TextObj, To, Verb,
  VerbCmd, ViCmd, Word,
};
use crate::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeReport {
  Insert,
  Normal,
  Visual,
  Replace,
  Unknown,
}

#[derive(Debug, Clone)]
pub enum CmdReplay {
  ModeReplay { cmds: Vec<ViCmd>, repeat: u16 },
  Single(ViCmd),
  Motion(Motion),
}

impl CmdReplay {
  pub fn mode(cmds: Vec<ViCmd>, repeat: u16) -> Self {
    Self::ModeReplay { cmds, repeat }
  }
  pub fn single(cmd: ViCmd) -> Self {
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

pub trait ViMode {
  fn handle_key(&mut self, key: E) -> Option<ViCmd>;
  fn is_repeatable(&self) -> bool;
  fn as_replay(&self) -> Option<CmdReplay>;
  fn cursor_style(&self) -> String;
  fn pending_seq(&self) -> Option<String>;
  fn move_cursor_on_undo(&self) -> bool;
  fn clamp_cursor(&self) -> bool;
  fn hist_scroll_start_pos(&self) -> Option<To>;
  fn report_mode(&self) -> ModeReport;
  fn cmds_from_raw(&mut self, raw: &str) -> Vec<ViCmd> {
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

#[derive(Default, Debug)]
pub struct ViInsert {
  cmds: Vec<ViCmd>,
  pending_cmd: ViCmd,
  repeat_count: u16,
}

impl ViInsert {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn with_count(mut self, repeat_count: u16) -> Self {
    self.repeat_count = repeat_count;
    self
  }
  pub fn register_and_return(&mut self) -> Option<ViCmd> {
    let mut cmd = self.take_cmd();
    cmd.normalize_counts();
    self.register_cmd(&cmd);
    Some(cmd)
  }
  pub fn ctrl_w_is_undo(&self) -> bool {
    let insert_count = self
      .cmds
      .iter()
      .filter(|cmd| matches!(cmd.verb(), Some(VerbCmd(1, Verb::InsertChar(_)))))
      .count();
    let backspace_count = self
      .cmds
      .iter()
      .filter(|cmd| matches!(cmd.verb(), Some(VerbCmd(1, Verb::Delete))))
      .count();
    insert_count > backspace_count
  }
  pub fn register_cmd(&mut self, cmd: &ViCmd) {
    self.cmds.push(cmd.clone())
  }
  pub fn take_cmd(&mut self) -> ViCmd {
    std::mem::take(&mut self.pending_cmd)
  }
}

impl ViMode for ViInsert {
  fn handle_key(&mut self, key: E) -> Option<ViCmd> {
    match key {
      E(K::Char(ch), M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::InsertChar(ch)));
        self
          .pending_cmd
          .set_motion(MotionCmd(1, Motion::ForwardChar));
        self.register_and_return()
      }
      E(K::Char('W'), M::CTRL) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::Delete));
        self.pending_cmd.set_motion(MotionCmd(
          1,
          Motion::WordMotion(To::Start, Word::Normal, Direction::Backward),
        ));
        self.register_and_return()
      }
      E(K::Char('H'), M::CTRL) | E(K::Backspace, M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::Delete));
        self
          .pending_cmd
          .set_motion(MotionCmd(1, Motion::BackwardCharForced));
        self.register_and_return()
      }

      E(K::BackTab, M::NONE) => {
        self
          .pending_cmd
          .set_verb(VerbCmd(1, Verb::CompleteBackward));
        self.register_and_return()
      }

      E(K::Char('I'), M::CTRL) | E(K::Tab, M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::Complete));
        self.register_and_return()
      }

      E(K::Esc, M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::NormalMode));
        self
          .pending_cmd
          .set_motion(MotionCmd(1, Motion::BackwardChar));
        self.register_and_return()
      }
      _ => common_cmds(key),
    }
  }

  fn is_repeatable(&self) -> bool {
    true
  }

  fn as_replay(&self) -> Option<CmdReplay> {
    Some(CmdReplay::mode(self.cmds.clone(), self.repeat_count))
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
    ModeReport::Insert
  }
}

#[derive(Default, Debug)]
pub struct ViReplace {
  cmds: Vec<ViCmd>,
  pending_cmd: ViCmd,
  repeat_count: u16,
}

impl ViReplace {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn with_count(mut self, repeat_count: u16) -> Self {
    self.repeat_count = repeat_count;
    self
  }
  pub fn register_and_return(&mut self) -> Option<ViCmd> {
    let mut cmd = self.take_cmd();
    cmd.normalize_counts();
    self.register_cmd(&cmd);
    Some(cmd)
  }
  pub fn register_cmd(&mut self, cmd: &ViCmd) {
    self.cmds.push(cmd.clone())
  }
  pub fn take_cmd(&mut self) -> ViCmd {
    std::mem::take(&mut self.pending_cmd)
  }
}

impl ViMode for ViReplace {
  fn handle_key(&mut self, key: E) -> Option<ViCmd> {
    match key {
      E(K::Char(ch), M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::ReplaceChar(ch)));
        self
          .pending_cmd
          .set_motion(MotionCmd(1, Motion::ForwardChar));
        self.register_and_return()
      }
      E(K::Char('W'), M::CTRL) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::Delete));
        self.pending_cmd.set_motion(MotionCmd(
          1,
          Motion::WordMotion(To::Start, Word::Normal, Direction::Backward),
        ));
        self.register_and_return()
      }
      E(K::Char('H'), M::CTRL) | E(K::Backspace, M::NONE) => {
        self
          .pending_cmd
          .set_motion(MotionCmd(1, Motion::BackwardChar));
        self.register_and_return()
      }

      E(K::BackTab, M::NONE) => {
        self
          .pending_cmd
          .set_verb(VerbCmd(1, Verb::CompleteBackward));
        self.register_and_return()
      }

      E(K::Char('I'), M::CTRL) | E(K::Tab, M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::Complete));
        self.register_and_return()
      }

      E(K::Esc, M::NONE) => {
        self.pending_cmd.set_verb(VerbCmd(1, Verb::NormalMode));
        self
          .pending_cmd
          .set_motion(MotionCmd(1, Motion::BackwardChar));
        self.register_and_return()
      }
      _ => common_cmds(key),
    }
  }
  fn is_repeatable(&self) -> bool {
    true
  }
  fn cursor_style(&self) -> String {
    "\x1b[4 q".to_string()
  }
  fn pending_seq(&self) -> Option<String> {
    None
  }
  fn as_replay(&self) -> Option<CmdReplay> {
    Some(CmdReplay::mode(self.cmds.clone(), self.repeat_count))
  }
  fn move_cursor_on_undo(&self) -> bool {
    true
  }
  fn clamp_cursor(&self) -> bool {
    true
  }
  fn hist_scroll_start_pos(&self) -> Option<To> {
    Some(To::End)
  }
  fn report_mode(&self) -> ModeReport {
    ModeReport::Replace
  }
}
#[derive(Default, Debug)]
pub struct ViNormal {
  pending_seq: String,
  pending_flags: CmdFlags,
}

impl ViNormal {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn clear_cmd(&mut self) {
    self.pending_seq = String::new();
  }
  pub fn take_cmd(&mut self) -> String {
    std::mem::take(&mut self.pending_seq)
  }
  pub fn flags(&self) -> CmdFlags {
    self.pending_flags
  }
  #[allow(clippy::unnecessary_unwrap)]
  fn validate_combination(&self, verb: Option<&Verb>, motion: Option<&Motion>) -> CmdState {
    if verb.is_none() {
      match motion {
        Some(Motion::TextObj(obj)) => {
          return match obj {
            TextObj::Sentence(_) | TextObj::Paragraph(_) => CmdState::Complete,
            _ => CmdState::Invalid,
          };
        }
        Some(_) => return CmdState::Complete,
        None => return CmdState::Pending,
      }
    }
    if verb.is_some() && motion.is_none() {
      match verb.unwrap() {
        Verb::Put(_) => CmdState::Complete,
        _ => CmdState::Pending,
      }
    } else {
      CmdState::Complete
    }
  }
  pub fn parse_count(&self, chars: &mut Peekable<Chars<'_>>) -> Option<usize> {
    let mut count = String::new();
    let Some(_digit @ '1'..='9') = chars.peek() else {
      return None;
    };
    count.push(chars.next().unwrap());
    while let Some(_digit @ '0'..='9') = chars.peek() {
      count.push(chars.next().unwrap());
    }
    if !count.is_empty() {
      count.parse::<usize>().ok()
    } else {
      None
    }
  }
  /// End the parse and clear the pending sequence
  #[track_caller]
  pub fn quit_parse(&mut self) -> Option<ViCmd> {
    self.clear_cmd();
    None
  }
  pub fn try_parse(&mut self, ch: char) -> Option<ViCmd> {
    self.pending_seq.push(ch);
    let mut chars = self.pending_seq.chars().peekable();

    /*
     * Parse the register
     *
     * Registers can be any letter a-z or A-Z.
     * While uncommon, it is possible to give a count to a register name.
     */
    let register = 'reg_parse: {
      let mut chars_clone = chars.clone();
      let count = self.parse_count(&mut chars_clone);

      let Some('"') = chars_clone.next() else {
        break 'reg_parse RegisterName::default();
      };

      let Some(reg_name) = chars_clone.next() else {
        return None; // Pending register name
      };
      match reg_name {
        'a'..='z' | 'A'..='Z' => { /* proceed */ }
        _ => return self.quit_parse(),
      }

      chars = chars_clone;
      RegisterName::new(Some(reg_name), count)
    };

    /*
     * We will now parse the verb
     * If we hit an invalid sequence, we will call 'return self.quit_parse()'
     * self.quit_parse() will clear the pending command and return None
     *
     * If we hit an incomplete sequence, we will simply return None.
     * returning None leaves the pending sequence where it is
     *
     * Note that we do use a label here for the block and 'return' values from
     * this scope using "break 'verb_parse <value>"
     */
    let verb = 'verb_parse: {
      let mut chars_clone = chars.clone();
      let count = self.parse_count(&mut chars_clone).unwrap_or(1);

      let Some(ch) = chars_clone.next() else {
        break 'verb_parse None;
      };
      match ch {
        'g' => {
          if let Some(ch) = chars_clone.peek() {
            match ch {
              'v' => {
                return Some(ViCmd {
                  register,
                  verb: Some(VerbCmd(1, Verb::VisualModeSelectLast)),
                  motion: None,
                  raw_seq: self.take_cmd(),
                  flags: self.flags(),
                });
              }
              '~' => {
                chars_clone.next();
                chars = chars_clone;
                break 'verb_parse Some(VerbCmd(count, Verb::ToggleCaseRange));
              }
              'u' => {
                chars_clone.next();
                chars = chars_clone;
                break 'verb_parse Some(VerbCmd(count, Verb::ToLower));
              }
              'U' => {
                chars_clone.next();
                chars = chars_clone;
                break 'verb_parse Some(VerbCmd(count, Verb::ToUpper));
              }
              '?' => {
                chars_clone.next();
                chars = chars_clone;
                break 'verb_parse Some(VerbCmd(count, Verb::Rot13));
              }
              _ => break 'verb_parse None,
            }
          } else {
            break 'verb_parse None;
          }
        }
        '.' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::RepeatLast)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'x' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Delete)),
            motion: Some(MotionCmd(1, Motion::ForwardChar)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'X' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Delete)),
            motion: Some(MotionCmd(1, Motion::BackwardChar)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        's' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Change)),
            motion: Some(MotionCmd(1, Motion::ForwardChar)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'S' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Change)),
            motion: Some(MotionCmd(1, Motion::WholeLine)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'p' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Put(Anchor::After)));
        }
        'P' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Put(Anchor::Before)));
        }
        '>' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Indent));
        }
        '<' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Dedent));
        }
        'r' => {
          let ch = chars_clone.next()?;
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::ReplaceCharInplace(ch, count as u16))),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'R' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::ReplaceMode)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        '~' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::ToggleCaseInplace(count as u16))),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'u' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Undo)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'v' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::VisualMode)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'V' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::VisualModeLine)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'o' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::InsertModeLineBreak(Anchor::After))),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'O' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::InsertModeLineBreak(Anchor::Before))),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'a' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::InsertMode)),
            motion: Some(MotionCmd(1, Motion::ForwardChar)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'A' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::InsertMode)),
            motion: Some(MotionCmd(1, Motion::EndOfLine)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'i' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::InsertMode)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'I' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::InsertMode)),
            motion: Some(MotionCmd(1, Motion::BeginningOfFirstWord)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'J' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::JoinLines)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'y' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Yank));
        }
        'd' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Delete));
        }
        'c' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Change));
        }
        'Y' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Yank)),
            motion: Some(MotionCmd(1, Motion::EndOfLine)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'D' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Delete)),
            motion: Some(MotionCmd(1, Motion::EndOfLine)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        'C' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Change)),
            motion: Some(MotionCmd(1, Motion::EndOfLine)),
            raw_seq: self.take_cmd(),
            flags: self.flags(),
          });
        }
        '=' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Equalize));
        }
        _ => break 'verb_parse None,
      }
    };

    let motion = 'motion_parse: {
      let mut chars_clone = chars.clone();
      let count = self.parse_count(&mut chars_clone).unwrap_or(1);

      let Some(ch) = chars_clone.next() else {
        break 'motion_parse None;
      };
      // Double inputs like 'dd' and 'cc', and some special cases
      match (ch, &verb) {
        // Double inputs
        ('?', Some(VerbCmd(_, Verb::Rot13)))
        | ('d', Some(VerbCmd(_, Verb::Delete)))
        | ('c', Some(VerbCmd(_, Verb::Change)))
        | ('y', Some(VerbCmd(_, Verb::Yank)))
        | ('=', Some(VerbCmd(_, Verb::Equalize)))
        | ('u', Some(VerbCmd(_, Verb::ToLower)))
        | ('U', Some(VerbCmd(_, Verb::ToUpper)))
        | ('~', Some(VerbCmd(_, Verb::ToggleCaseRange)))
        | ('>', Some(VerbCmd(_, Verb::Indent)))
        | ('<', Some(VerbCmd(_, Verb::Dedent))) => {
          break 'motion_parse Some(MotionCmd(count, Motion::WholeLine));
        }
        ('W', Some(VerbCmd(_, Verb::Change))) => {
          // Same with 'W'
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::End, Word::Big, Direction::Forward),
          ));
        }
        _ => { /* Nothing weird, so let's continue */ }
      }
      match ch {
        'g' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };
          match ch {
            'g' => {
              chars_clone.next();
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::BeginningOfBuffer));
            }
            'e' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(
                count,
                Motion::WordMotion(To::End, Word::Normal, Direction::Backward),
              ));
            }
            'E' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(
                count,
                Motion::WordMotion(To::End, Word::Big, Direction::Backward),
              ));
            }
            'k' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ScreenLineUp));
            }
            'j' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ScreenLineDown));
            }
            '_' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::EndOfLastWord));
            }
            '0' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::BeginningOfScreenLine));
            }
            '^' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::FirstGraphicalOnScreenLine));
            }
            _ => return self.quit_parse(),
          }
        }
        ']' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };
          match ch {
            ')' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ToParen(Direction::Forward)));
            }
            '}' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ToBrace(Direction::Forward)));
            }
            _ => return self.quit_parse(),
          }
        }
        '[' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };
          match ch {
            '(' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ToParen(Direction::Backward)));
            }
            '{' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ToBrace(Direction::Backward)));
            }
            _ => return self.quit_parse(),
          }
        }
        '%' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::ToDelimMatch));
        }
        'v' => {
          // We got 'v' after a verb
          // Instead of normal operations, we will calculate the span based on how visual
          // mode would see it
          if self
            .flags()
            .intersects(CmdFlags::VISUAL | CmdFlags::VISUAL_LINE | CmdFlags::VISUAL_BLOCK)
          {
            // We can't have more than one of these
            return self.quit_parse();
          }
          self.pending_flags |= CmdFlags::VISUAL;
          break 'motion_parse None;
        }
        'V' => {
          // We got 'V' after a verb
          // Instead of normal operations, we will calculate the span based on how visual
          // line mode would see it
          if self
            .flags()
            .intersects(CmdFlags::VISUAL | CmdFlags::VISUAL_LINE | CmdFlags::VISUAL_BLOCK)
          {
            // We can't have more than one of these
            // I know vim can technically do this, but it doesn't really make sense to allow
            // it since even in vim only the first one given is used
            return self.quit_parse();
          }
          self.pending_flags |= CmdFlags::VISUAL;
          break 'motion_parse None;
        }
        // TODO: figure out how to include 'Ctrl+V' here, might need a refactor
        'G' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::EndOfBuffer));
        }
        'f' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Forward, Dest::On, *ch),
          ));
        }
        'F' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Backward, Dest::On, *ch),
          ));
        }
        't' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Forward, Dest::Before, *ch),
          ));
        }
        'T' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Backward, Dest::Before, *ch),
          ));
        }
        ';' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::RepeatMotion));
        }
        ',' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::RepeatMotionRev));
        }
        '|' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::ToColumn));
        }
        '^' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::BeginningOfFirstWord));
        }
        '0' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::BeginningOfLine));
        }
        '$' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::EndOfLine));
        }
        'k' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::LineUp));
        }
        'j' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::LineDown));
        }
        'h' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::BackwardChar));
        }
        'l' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::ForwardChar));
        }
        'w' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::Start, Word::Normal, Direction::Forward),
          ));
        }
        'W' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::Start, Word::Big, Direction::Forward),
          ));
        }
        'e' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::End, Word::Normal, Direction::Forward),
          ));
        }
        'E' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::End, Word::Big, Direction::Forward),
          ));
        }
        'b' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::Start, Word::Normal, Direction::Backward),
          ));
        }
        'B' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::Start, Word::Big, Direction::Backward),
          ));
        }
        ')' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::TextObj(TextObj::Sentence(Direction::Forward)),
          ));
        }
        '(' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::TextObj(TextObj::Sentence(Direction::Backward)),
          ));
        }
        '}' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::TextObj(TextObj::Paragraph(Direction::Forward)),
          ));
        }
        '{' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::TextObj(TextObj::Paragraph(Direction::Backward)),
          ));
        }
        ch if ch == 'i' || ch == 'a' => {
          let bound = match ch {
            'i' => Bound::Inside,
            'a' => Bound::Around,
            _ => unreachable!(),
          };
          if chars_clone.peek().is_none() {
            break 'motion_parse None;
          }
          let obj = match chars_clone.next().unwrap() {
            'w' => TextObj::Word(Word::Normal, bound),
            'W' => TextObj::Word(Word::Big, bound),
            's' => TextObj::WholeSentence(bound),
            'p' => TextObj::WholeParagraph(bound),
            '"' => TextObj::DoubleQuote(bound),
            '\'' => TextObj::SingleQuote(bound),
            '`' => TextObj::BacktickQuote(bound),
            '(' | ')' | 'b' => TextObj::Paren(bound),
            '{' | '}' | 'B' => TextObj::Brace(bound),
            '[' | ']' => TextObj::Bracket(bound),
            '<' | '>' => TextObj::Angle(bound),
            _ => return self.quit_parse(),
          };
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::TextObj(obj)));
        }
        _ => return self.quit_parse(),
      }
    };

    if chars.peek().is_some() {}

    let verb_ref = verb.as_ref().map(|v| &v.1);
    let motion_ref = motion.as_ref().map(|m| &m.1);

    match self.validate_combination(verb_ref, motion_ref) {
      CmdState::Complete => Some(ViCmd {
        register,
        verb,
        motion,
        raw_seq: std::mem::take(&mut self.pending_seq),
        flags: self.flags(),
      }),
      CmdState::Pending => None,
      CmdState::Invalid => {
        self.pending_seq.clear();
        None
      }
    }
  }
}

impl ViMode for ViNormal {
  fn handle_key(&mut self, key: E) -> Option<ViCmd> {
    let mut cmd = match key {
      E(K::Char(ch), M::NONE) => self.try_parse(ch),
      E(K::Backspace, M::NONE) => Some(ViCmd {
        register: Default::default(),
        verb: None,
        motion: Some(MotionCmd(1, Motion::BackwardChar)),
        raw_seq: "".into(),
        flags: self.flags(),
      }),
      E(K::Char('R'), M::CTRL) => {
        let mut chars = self.pending_seq.chars().peekable();
        let count = self.parse_count(&mut chars).unwrap_or(1);
        Some(ViCmd {
          register: RegisterName::default(),
          verb: Some(VerbCmd(count, Verb::Redo)),
          motion: None,
          raw_seq: self.take_cmd(),
          flags: self.flags(),
        })
      }
      E(K::Esc, M::NONE) => {
        self.clear_cmd();
        None
      }
      _ => {
        if let Some(cmd) = common_cmds(key) {
          self.clear_cmd();
          Some(cmd)
        } else {
          None
        }
      }
    };

    if let Some(cmd) = cmd.as_mut() {
      cmd.normalize_counts();
    };
    cmd
  }

  fn is_repeatable(&self) -> bool {
    false
  }

  fn as_replay(&self) -> Option<CmdReplay> {
    None
  }

  fn cursor_style(&self) -> String {
    "\x1b[2 q".to_string()
  }

  fn pending_seq(&self) -> Option<String> {
    Some(self.pending_seq.clone())
  }

  fn move_cursor_on_undo(&self) -> bool {
    false
  }
  fn clamp_cursor(&self) -> bool {
    true
  }
  fn hist_scroll_start_pos(&self) -> Option<To> {
    None
  }
  fn report_mode(&self) -> ModeReport {
    ModeReport::Normal
  }
}

#[derive(Default, Debug)]
pub struct ViVisual {
  pending_seq: String,
}

impl ViVisual {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn clear_cmd(&mut self) {
    self.pending_seq = String::new();
  }
  pub fn take_cmd(&mut self) -> String {
    std::mem::take(&mut self.pending_seq)
  }

  #[allow(clippy::unnecessary_unwrap)]
  fn validate_combination(&self, verb: Option<&Verb>, motion: Option<&Motion>) -> CmdState {
    if verb.is_none() {
      match motion {
        Some(_) => return CmdState::Complete,
        None => return CmdState::Pending,
      }
    }
    if motion.is_none() && verb.is_some() {
      match verb.unwrap() {
        Verb::Put(_) => CmdState::Complete,
        _ => CmdState::Pending,
      }
    } else {
      CmdState::Complete
    }
  }
  pub fn parse_count(&self, chars: &mut Peekable<Chars<'_>>) -> Option<usize> {
    let mut count = String::new();
    let Some(_digit @ '1'..='9') = chars.peek() else {
      return None;
    };
    count.push(chars.next().unwrap());
    while let Some(_digit @ '0'..='9') = chars.peek() {
      count.push(chars.next().unwrap());
    }
    if !count.is_empty() {
      count.parse::<usize>().ok()
    } else {
      None
    }
  }
  /// End the parse and clear the pending sequence
  #[track_caller]
  pub fn quit_parse(&mut self) -> Option<ViCmd> {
    self.clear_cmd();
    None
  }
  pub fn try_parse(&mut self, ch: char) -> Option<ViCmd> {
    self.pending_seq.push(ch);
    let mut chars = self.pending_seq.chars().peekable();

    let register = 'reg_parse: {
      let mut chars_clone = chars.clone();
      let count = self.parse_count(&mut chars_clone);

      let Some('"') = chars_clone.next() else {
        break 'reg_parse RegisterName::default();
      };

      let Some(reg_name) = chars_clone.next() else {
        return None; // Pending register name
      };
      match reg_name {
        'a'..='z' | 'A'..='Z' => { /* proceed */ }
        _ => return self.quit_parse(),
      }

      chars = chars_clone;
      RegisterName::new(Some(reg_name), count)
    };

    let verb = 'verb_parse: {
      let mut chars_clone = chars.clone();
      let count = self.parse_count(&mut chars_clone).unwrap_or(1);

      let Some(ch) = chars_clone.next() else {
        break 'verb_parse None;
      };
      match ch {
        'g' => {
          if let Some(ch) = chars_clone.peek() {
            match ch {
              'v' => {
                return Some(ViCmd {
                  register,
                  verb: Some(VerbCmd(1, Verb::VisualModeSelectLast)),
                  motion: None,
                  raw_seq: self.take_cmd(),
                  flags: CmdFlags::empty(),
                });
              }
              '?' => {
                return Some(ViCmd {
                  register,
                  verb: Some(VerbCmd(1, Verb::Rot13)),
                  motion: None,
                  raw_seq: self.take_cmd(),
                  flags: CmdFlags::empty(),
                });
              }
              _ => break 'verb_parse None,
            }
          } else {
            break 'verb_parse None;
          }
        }
        '.' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::RepeatLast)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'x' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Delete));
        }
        'X' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::Delete)),
            motion: Some(MotionCmd(1, Motion::WholeLine)),
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'Y' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::Yank)),
            motion: Some(MotionCmd(1, Motion::WholeLine)),
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'D' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::Delete)),
            motion: Some(MotionCmd(1, Motion::WholeLine)),
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'R' | 'C' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::Change)),
            motion: Some(MotionCmd(1, Motion::WholeLine)),
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        '>' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::Indent)),
            motion: Some(MotionCmd(1, Motion::WholeLine)),
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        '<' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::Dedent)),
            motion: Some(MotionCmd(1, Motion::WholeLine)),
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        '=' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::Equalize)),
            motion: Some(MotionCmd(1, Motion::WholeLine)),
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'p' | 'P' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Put(Anchor::Before)));
        }
        'r' => {
          let ch = chars_clone.next()?;
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::ReplaceChar(ch))),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        '~' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(1, Verb::ToggleCaseRange)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'u' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::ToLower)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'U' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::ToUpper)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'O' | 'o' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::SwapVisualAnchor)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'A' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::InsertMode)),
            motion: Some(MotionCmd(1, Motion::ForwardChar)),
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'I' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::InsertMode)),
            motion: Some(MotionCmd(1, Motion::BeginningOfLine)),
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'J' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::JoinLines)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'y' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Yank));
        }
        'd' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Delete));
        }
        'c' => {
          chars = chars_clone;
          break 'verb_parse Some(VerbCmd(count, Verb::Change));
        }
        _ => break 'verb_parse None,
      }
    };

    if let Some(verb) = verb {
      return Some(ViCmd {
        register,
        verb: Some(verb),
        motion: None,
        raw_seq: self.take_cmd(),
        flags: CmdFlags::empty(),
      });
    }

    let motion = 'motion_parse: {
      let mut chars_clone = chars.clone();
      let count = self.parse_count(&mut chars_clone).unwrap_or(1);

      let Some(ch) = chars_clone.next() else {
        break 'motion_parse None;
      };
      match (ch, &verb) {
        ('d', Some(VerbCmd(_, Verb::Delete)))
        | ('c', Some(VerbCmd(_, Verb::Change)))
        | ('y', Some(VerbCmd(_, Verb::Yank)))
        | ('=', Some(VerbCmd(_, Verb::Equalize)))
        | ('>', Some(VerbCmd(_, Verb::Indent)))
        | ('<', Some(VerbCmd(_, Verb::Dedent))) => {
          break 'motion_parse Some(MotionCmd(count, Motion::WholeLine));
        }
        _ => {}
      }
      match ch {
        'g' => {
          if let Some(ch) = chars_clone.peek() {
            match ch {
              'g' => {
                chars_clone.next();
                chars = chars_clone;
                break 'motion_parse Some(MotionCmd(count, Motion::BeginningOfBuffer));
              }
              'e' => {
                chars_clone.next();
                chars = chars_clone;
                break 'motion_parse Some(MotionCmd(
                  count,
                  Motion::WordMotion(To::End, Word::Normal, Direction::Backward),
                ));
              }
              'E' => {
                chars_clone.next();
                chars = chars_clone;
                break 'motion_parse Some(MotionCmd(
                  count,
                  Motion::WordMotion(To::End, Word::Big, Direction::Backward),
                ));
              }
              'k' => {
                chars_clone.next();
                chars = chars_clone;
                break 'motion_parse Some(MotionCmd(count, Motion::ScreenLineUp));
              }
              'j' => {
                chars_clone.next();
                chars = chars_clone;
                break 'motion_parse Some(MotionCmd(count, Motion::ScreenLineDown));
              }
              _ => return self.quit_parse(),
            }
          } else {
            break 'motion_parse None;
          }
        }
        ']' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };
          match ch {
            ')' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ToParen(Direction::Forward)));
            }
            '}' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ToBrace(Direction::Forward)));
            }
            _ => return self.quit_parse(),
          }
        }
        '[' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };
          match ch {
            '(' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ToParen(Direction::Backward)));
            }
            '{' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::ToBrace(Direction::Backward)));
            }
            _ => return self.quit_parse(),
          }
        }
        '%' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::ToDelimMatch));
        }
        'f' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Forward, Dest::On, *ch),
          ));
        }
        'F' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Backward, Dest::On, *ch),
          ));
        }
        't' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Forward, Dest::Before, *ch),
          ));
        }
        'T' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Backward, Dest::Before, *ch),
          ));
        }
        ';' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::RepeatMotion));
        }
        ',' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::RepeatMotionRev));
        }
        '|' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::ToColumn));
        }
        '0' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::BeginningOfLine));
        }
        '$' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::EndOfLine));
        }
        'k' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::LineUp));
        }
        'j' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::LineDown));
        }
        'h' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::BackwardChar));
        }
        'l' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::ForwardChar));
        }
        'w' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::Start, Word::Normal, Direction::Forward),
          ));
        }
        'W' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::Start, Word::Big, Direction::Forward),
          ));
        }
        'e' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::End, Word::Normal, Direction::Forward),
          ));
        }
        'E' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::End, Word::Big, Direction::Forward),
          ));
        }
        'b' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::Start, Word::Normal, Direction::Backward),
          ));
        }
        'B' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::WordMotion(To::Start, Word::Big, Direction::Backward),
          ));
        }
        ')' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::TextObj(TextObj::Sentence(Direction::Forward)),
          ));
        }
        '(' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::TextObj(TextObj::Sentence(Direction::Backward)),
          ));
        }
        '}' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::TextObj(TextObj::Paragraph(Direction::Forward)),
          ));
        }
        '{' => {
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(
            count,
            Motion::TextObj(TextObj::Paragraph(Direction::Backward)),
          ));
        }
        ch if ch == 'i' || ch == 'a' => {
          let bound = match ch {
            'i' => Bound::Inside,
            'a' => Bound::Around,
            _ => unreachable!(),
          };
          if chars_clone.peek().is_none() {
            break 'motion_parse None;
          }
          let obj = match chars_clone.next().unwrap() {
            'w' => TextObj::Word(Word::Normal, bound),
            'W' => TextObj::Word(Word::Big, bound),
            's' => TextObj::WholeSentence(bound),
            'p' => TextObj::WholeParagraph(bound),
            '"' => TextObj::DoubleQuote(bound),
            '\'' => TextObj::SingleQuote(bound),
            '`' => TextObj::BacktickQuote(bound),
            '(' | ')' | 'b' => TextObj::Paren(bound),
            '{' | '}' | 'B' => TextObj::Brace(bound),
            '[' | ']' => TextObj::Bracket(bound),
            '<' | '>' => TextObj::Angle(bound),
            _ => return self.quit_parse(),
          };
          chars = chars_clone;
          break 'motion_parse Some(MotionCmd(count, Motion::TextObj(obj)));
        }
        _ => return self.quit_parse(),
      }
    };

    if chars.peek().is_some() {}

    let verb_ref = verb.as_ref().map(|v| &v.1);
    let motion_ref = motion.as_ref().map(|m| &m.1);

    match self.validate_combination(verb_ref, motion_ref) {
      CmdState::Complete => Some(ViCmd {
        register,
        verb,
        motion,
        raw_seq: std::mem::take(&mut self.pending_seq),
        flags: CmdFlags::empty(),
      }),
      CmdState::Pending => None,
      CmdState::Invalid => {
        self.pending_seq.clear();
        None
      }
    }
  }
}

impl ViMode for ViVisual {
  fn handle_key(&mut self, key: E) -> Option<ViCmd> {
    let mut cmd = match key {
      E(K::Char(ch), M::NONE) => self.try_parse(ch),
      E(K::Backspace, M::NONE) => Some(ViCmd {
        register: Default::default(),
        verb: None,
        motion: Some(MotionCmd(1, Motion::BackwardChar)),
        raw_seq: "".into(),
        flags: CmdFlags::empty(),
      }),
      E(K::Char('R'), M::CTRL) => {
        let mut chars = self.pending_seq.chars().peekable();
        let count = self.parse_count(&mut chars).unwrap_or(1);
        Some(ViCmd {
          register: RegisterName::default(),
          verb: Some(VerbCmd(count, Verb::Redo)),
          motion: None,
          raw_seq: self.take_cmd(),
          flags: CmdFlags::empty(),
        })
      }
      E(K::Esc, M::NONE) => Some(ViCmd {
        register: Default::default(),
        verb: Some(VerbCmd(1, Verb::NormalMode)),
        motion: Some(MotionCmd(1, Motion::Null)),
        raw_seq: self.take_cmd(),
        flags: CmdFlags::empty(),
      }),
      _ => {
        if let Some(cmd) = common_cmds(key) {
          self.clear_cmd();
          Some(cmd)
        } else {
          None
        }
      }
    };

    if let Some(cmd) = cmd.as_mut() {
      cmd.normalize_counts();
    };
    cmd
  }

  fn is_repeatable(&self) -> bool {
    true
  }

  fn as_replay(&self) -> Option<CmdReplay> {
    None
  }

  fn cursor_style(&self) -> String {
    "\x1b[2 q".to_string()
  }

  fn pending_seq(&self) -> Option<String> {
    Some(self.pending_seq.clone())
  }

  fn move_cursor_on_undo(&self) -> bool {
    true
  }

  fn clamp_cursor(&self) -> bool {
    true
  }

  fn hist_scroll_start_pos(&self) -> Option<To> {
    None
  }

  fn report_mode(&self) -> ModeReport {
    ModeReport::Visual
  }
}

pub fn common_cmds(key: E) -> Option<ViCmd> {
  let mut pending_cmd = ViCmd::new();
  match key {
    E(K::Home, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::BeginningOfLine)),
    E(K::End, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::EndOfLine)),
    E(K::Left, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::BackwardChar)),
    E(K::Right, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::ForwardChar)),
    E(K::Up, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::LineUp)),
    E(K::Down, M::NONE) => pending_cmd.set_motion(MotionCmd(1, Motion::LineDown)),
    E(K::Enter, M::NONE) => pending_cmd.set_verb(VerbCmd(1, Verb::AcceptLineOrNewline)),
    E(K::Char('D'), M::CTRL) => pending_cmd.set_verb(VerbCmd(1, Verb::EndOfFile)),
    E(K::Delete, M::NONE) => {
      pending_cmd.set_verb(VerbCmd(1, Verb::Delete));
      pending_cmd.set_motion(MotionCmd(1, Motion::ForwardChar));
    }
    E(K::Backspace, M::NONE) | E(K::Char('H'), M::CTRL) => {
      pending_cmd.set_verb(VerbCmd(1, Verb::Delete));
      pending_cmd.set_motion(MotionCmd(1, Motion::BackwardChar));
    }
    _ => return None,
  }
  Some(pending_cmd)
}
