use std::iter::Peekable;
use std::str::Chars;

use crate::state::vars::VarStr;

use super::{
  CmdReplay, CmdState, E, EditMode, K, M, ModeReport, ParseResult, ViParser, common_cmds,
  editcmd::{CmdFlags, EditCmd, Motion, TextObj, Verb},
  key, motion,
  register::RegisterName,
  state::terminal::CursorStyle,
  verb,
};

#[derive(Debug)]
pub struct ViNormal {
  parser: ViParser,
  pending_seq: String,
  pending_flags: CmdFlags,
}

impl ViNormal {
  pub fn new() -> Self {
    Self {
      parser: ViParser::new(None, None, Self::validate_combination),
      pending_seq: String::new(),
      pending_flags: CmdFlags::empty(),
    }
  }
  pub fn take_cmd(&mut self) -> String {
    std::mem::take(&mut self.pending_seq)
  }
  pub fn flags(&self) -> CmdFlags {
    self.pending_flags
  }
  fn validate_combination(verb: Option<&Verb>, motion: Option<&Motion>) -> CmdState {
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
    if let Some(verb) = verb
      && motion.is_none()
    {
      match verb {
        Verb::Put(_) => CmdState::Complete,
        _ => CmdState::Pending,
      }
    } else {
      CmdState::Complete
    }
  }
  pub fn parse_count(chars: &mut Peekable<Chars<'_>>) -> Option<usize> {
    let mut count = String::new();
    let Some(_digit @ '1'..='9') = chars.peek() else {
      return None;
    };
    count.push(chars.next().unwrap());
    while let Some(_digit @ '0'..='9') = chars.peek() {
      count.push(chars.next().unwrap());
    }
    if count.is_empty() {
      None
    } else {
      count.parse::<usize>().ok()
    }
  }
}

impl Default for ViNormal {
  fn default() -> Self {
    Self::new()
  }
}

impl EditMode for ViNormal {
  #[expect(clippy::too_many_lines)]
  fn handle_key(&mut self, key: E) -> Option<EditCmd> {
    let mut cmd: Option<EditCmd> = match key {
      key!('V') => Some(EditCmd {
        register: RegisterName::default(),
        verb: Some(verb!(Verb::VisualModeLine)),
        motion: None,
        raw_seq: String::new(),
        flags: self.flags(),
      }),
      E(K::ExMode, _) => {
        return Some(EditCmd {
          register: RegisterName::default(),
          verb: Some(verb!(Verb::ExMode)),
          motion: None,
          raw_seq: self.take_cmd(),
          flags: self.flags(),
        });
      }
      key!(Ctrl + 'a') => {
        let count = Self::parse_count(&mut self.pending_seq.chars().peekable()).unwrap_or(1) as u16;
        self.pending_seq.clear();
        Some(EditCmd {
          register: RegisterName::default(),
          verb: Some(verb!(Verb::IncrementNumber(count))),
          motion: None,
          raw_seq: String::new(),
          flags: self.flags(),
        })
      }
      key!(Ctrl + 'x') => {
        let count = Self::parse_count(&mut self.pending_seq.chars().peekable()).unwrap_or(1) as u16;
        self.pending_seq.clear();
        Some(EditCmd {
          register: RegisterName::default(),
          verb: Some(verb!(Verb::DecrementNumber(count))),
          motion: None,
          raw_seq: String::new(),
          flags: self.flags(),
        })
      }
      key!(Ctrl + 'g') => {
        self.pending_seq.clear();
        Some(EditCmd {
          register: RegisterName::default(),
          verb: Some(verb!(Verb::PrintPosition)),
          motion: None,
          raw_seq: String::new(),
          flags: self.flags(),
        })
      }
      key!(Ctrl + 'd') => {
        self.pending_seq.clear();
        Some(EditCmd {
          register: RegisterName::default(),
          verb: None,
          motion: Some(motion!(Motion::HalfScreenDown)),
          raw_seq: String::new(),
          flags: self.flags(),
        })
      }
      key!(Ctrl + 'u') => {
        self.pending_seq.clear();
        Some(EditCmd {
          register: RegisterName::default(),
          verb: None,
          motion: Some(motion!(Motion::HalfScreenUp)),
          raw_seq: String::new(),
          flags: self.flags(),
        })
      }
      key!(Enter) => {
        self.pending_seq.clear();
        Some(EditCmd {
          register: RegisterName::default(),
          verb: None,
          motion: Some(motion!(Motion::LineDown)),
          raw_seq: String::new(),
          flags: self.flags() | CmdFlags::IS_SUBMIT,
        })
      }
      E(K::Char(ch), M::NONE) => {
        self.pending_seq.push(ch);
        match self.parser.try_parse(&self.pending_seq) {
          ParseResult::Complete(cmd) => {
            self.pending_seq.clear();
            Some(*cmd)
          }
          ParseResult::Invalid => {
            self.pending_seq.clear();
            None
          }
          ParseResult::Pending => None,
        }
      }
      key!(Backspace) => Some(EditCmd {
        register: RegisterName::default(),
        verb: None,
        motion: Some(motion!(Motion::BackwardChar)),
        raw_seq: String::new(),
        flags: self.flags(),
      }),
      key!(Ctrl + 'r') => {
        let mut chars = self.pending_seq.chars().peekable();
        let count = Self::parse_count(&mut chars).unwrap_or(1);
        Some(EditCmd {
          register: RegisterName::default(),
          verb: Some(verb!(count, Verb::Redo)),
          motion: None,
          raw_seq: self.take_cmd(),
          flags: self.flags(),
        })
      }
      E(K::Esc, _) => {
        self.pending_seq.clear();
        None
      }
      _ => {
        if let Some(cmd) = common_cmds(&key) {
          self.pending_seq.clear();
          Some(cmd)
        } else {
          None
        }
      }
    };

    if let Some(cmd) = cmd.as_mut() {
      cmd.normalize_counts();
    }
    cmd
  }

  fn is_repeatable(&self) -> bool {
    false
  }

  fn as_replay(&self) -> Option<CmdReplay> {
    None
  }

  fn cursor_style(&self) -> CursorStyle {
    CursorStyle::Block(false)
  }

  fn pending_seq(&self) -> Option<VarStr> {
    Some(self.pending_seq.as_str().into())
  }

  fn clamp_cursor(&self) -> bool {
    true
  }
  fn report_mode(&self) -> ModeReport {
    ModeReport::Normal
  }
}
