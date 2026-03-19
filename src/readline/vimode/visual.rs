use std::iter::Peekable;
use std::str::Chars;

use super::{CmdReplay, CmdState, ModeReport, ViMode, common_cmds};
use crate::readline::keys::{KeyCode as K, KeyEvent as E, ModKeys as M};
use crate::readline::linebuf::Grapheme;
use crate::readline::vicmd::{
  Anchor, Bound, CmdFlags, Dest, Direction, Motion, MotionCmd, RegisterName, TextObj, To, Verb,
  VerbCmd, ViCmd, Word,
};

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
        ':' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::ExMode)),
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
            verb: Some(VerbCmd(1, Verb::ReplaceCharInplace(ch, 1))),
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
        's' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Delete)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
        }
        'S' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Change)),
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
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::Yank)),
            motion: None,
            raw_seq: self.take_cmd(),
            flags: CmdFlags::empty(),
          });
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
        | ('y', Some(VerbCmd(_, Verb::Yank)))
        | ('=', Some(VerbCmd(_, Verb::Equalize)))
        | ('>', Some(VerbCmd(_, Verb::Indent)))
        | ('<', Some(VerbCmd(_, Verb::Dedent))) => {
          break 'motion_parse Some(MotionCmd(count, Motion::WholeLine));
        }
        ('c', Some(VerbCmd(_, Verb::Change))) => {
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
                break 'motion_parse Some(MotionCmd(count, Motion::StartOfBuffer));
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
            Motion::CharSearch(Direction::Forward, Dest::On, Grapheme::from(*ch)),
          ));
        }
        'F' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Backward, Dest::On, Grapheme::from(*ch)),
          ));
        }
        't' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Forward, Dest::Before, Grapheme::from(*ch)),
          ));
        }
        'T' => {
          let Some(ch) = chars_clone.peek() else {
            break 'motion_parse None;
          };

          break 'motion_parse Some(MotionCmd(
            count,
            Motion::CharSearch(Direction::Backward, Dest::Before, Grapheme::from(*ch)),
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

    let _ = chars; // suppresses unused warnings, creates an error if we decide to use chars later

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
    let mut cmd: Option<ViCmd> = match key {
      E(K::Char(ch), M::NONE) => self.try_parse(ch),
      E(K::Backspace, M::NONE) => Some(ViCmd {
        register: Default::default(),
        verb: None,
        motion: Some(MotionCmd(1, Motion::BackwardChar)),
        raw_seq: "".into(),
        flags: CmdFlags::empty(),
      }),
      E(K::ExMode, _) => {
        return Some(ViCmd {
          register: Default::default(),
          verb: Some(VerbCmd(1, Verb::ExMode)),
          motion: None,
          raw_seq: String::new(),
          flags: Default::default(),
        });
      }
      E(K::Char('A'), M::CTRL) => {
        let count = self
          .parse_count(&mut self.pending_seq.chars().peekable())
          .unwrap_or(1) as u16;
        self.pending_seq.clear();
        Some(ViCmd {
          register: Default::default(),
          verb: Some(VerbCmd(1, Verb::IncrementNumber(count))),
          motion: None,
          raw_seq: "".into(),
          flags: CmdFlags::empty(),
        })
      }
      E(K::Char('X'), M::CTRL) => {
        let count = self
          .parse_count(&mut self.pending_seq.chars().peekable())
          .unwrap_or(1) as u16;
        self.pending_seq.clear();
        Some(ViCmd {
          register: Default::default(),
          verb: Some(VerbCmd(1, Verb::DecrementNumber(count))),
          motion: None,
          raw_seq: "".into(),
          flags: CmdFlags::empty(),
        })
      }
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
