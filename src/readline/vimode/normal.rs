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
            motion: Some(MotionCmd(1, Motion::ForwardCharForced)),
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
        ':' => {
          return Some(ViCmd {
            register,
            verb: Some(VerbCmd(count, Verb::ExMode)),
            motion: None,
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
        | ('y', Some(VerbCmd(_, Verb::Yank)))
        | ('=', Some(VerbCmd(_, Verb::Equalize)))
        | ('u', Some(VerbCmd(_, Verb::ToLower)))
        | ('U', Some(VerbCmd(_, Verb::ToUpper)))
        | ('~', Some(VerbCmd(_, Verb::ToggleCaseRange)))
        | ('>', Some(VerbCmd(_, Verb::Indent)))
        | ('<', Some(VerbCmd(_, Verb::Dedent))) => {
          break 'motion_parse Some(MotionCmd(count, Motion::WholeLine));
        }
        ('c', Some(VerbCmd(_, Verb::Change))) => {
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
              break 'motion_parse Some(MotionCmd(count, Motion::StartOfBuffer));
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
            '_' => {
              chars = chars_clone;
              break 'motion_parse Some(MotionCmd(count, Motion::EndOfLastWord));
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

    let _ = chars; // suppresses unused warnings, creates an error if we decide to use chars later

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
    let mut cmd: Option<ViCmd> = match key {
      E(K::Char('V'), M::NONE) => Some(ViCmd {
        register: Default::default(),
        verb: Some(VerbCmd(1, Verb::VisualModeLine)),
        motion: None,
        raw_seq: "".into(),
        flags: self.flags(),
      }),
      E(K::ExMode, _) => {
        return Some(ViCmd {
          register: Default::default(),
          verb: Some(VerbCmd(1, Verb::ExMode)),
          motion: None,
          raw_seq: self.take_cmd(),
          flags: self.flags(),
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
          flags: self.flags(),
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
          flags: self.flags(),
        })
      }

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
