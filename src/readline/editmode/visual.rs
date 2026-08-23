use std::iter::Peekable;
use std::str::Chars;

use crate::state::vars::VarStr;

use super::{
  CmdReplay, CmdState, E, EditMode, K, M, ModeReport, ParseResult, ViParser, common_cmds,
  editcmd::{Anchor, Cmd, CmdFlags, EditCmd, Motion, Verb},
  key, motion,
  parse::CallbackResult,
  register::RegisterName,
  state::terminal::CursorStyle,
  verb,
};

#[derive(Debug)]
pub struct ViVisual {
  pending_seq: String,
  parser: ViParser,
  cmds: Vec<EditCmd>,
  repeat_count: u16,
}

impl ViVisual {
  pub fn new() -> Self {
    Self {
      pending_seq: String::new(),
      parser: Self::parser(),
      cmds: vec![],
      repeat_count: 0,
    }
  }
  pub fn clear_cmd(&mut self) {
    self.pending_seq = String::new();
  }
  pub fn take_cmd(&mut self) -> String {
    std::mem::take(&mut self.pending_seq)
  }
  pub fn register_cmd(&mut self, cmd: &EditCmd) {
    self.cmds.push(cmd.clone());
  }

  fn validate_combination(verb: Option<&Verb>, motion: Option<&Motion>) -> CmdState {
    if verb.is_none() {
      match motion {
        Some(_) => return CmdState::Complete,
        None => return CmdState::Pending,
      }
    }
    if motion.is_none()
      && let Some(verb) = verb
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
  fn parser() -> ViParser {
    ViParser::new(None, Some(Self::parse_verb), Self::validate_combination)
  }
  #[expect(clippy::too_many_lines)]
  pub fn parse_verb(chars: &mut Peekable<Chars<'_>>, count: usize) -> CallbackResult<Cmd<Verb>> {
    use CallbackResult as C;
    let register = RegisterName::default();

    let Some(ch) = chars.next() else {
      return C::pending();
    };

    match ch {
      'g' => {
        let Some(ch) = chars.peek() else {
          return C::pending();
        };
        match ch {
          'v' => C::complete(EditCmd {
            register,
            verb: Some(verb!(Verb::VisualModeSelectLast)),
            motion: None,
            raw_seq: String::new(),
            flags: CmdFlags::empty(),
          }),
          '?' => C::complete(EditCmd {
            register,
            verb: Some(verb!(Verb::Rot13)),
            motion: None,
            raw_seq: String::new(),
            flags: CmdFlags::empty(),
          }),
          _ => C::no_match(),
        }
      }
      '.' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::RepeatLast)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      ':' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::ExMode)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'X' | 'D' => C::complete(EditCmd {
        register,
        verb: Some(verb!(Verb::Delete)),
        motion: Some(motion!(Motion::WholeLine)),
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'Y' => C::complete(EditCmd {
        register,
        verb: Some(verb!(Verb::Yank)),
        motion: Some(motion!(Motion::WholeLine)),
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'R' | 'C' => C::complete(EditCmd {
        register,
        verb: Some(verb!(Verb::Change)),
        motion: Some(motion!(Motion::WholeLine)),
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      '>' => C::complete(EditCmd {
        register,
        verb: Some(verb!(Verb::Indent)),
        motion: Some(motion!(Motion::WholeLine)),
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      '<' => C::complete(EditCmd {
        register,
        verb: Some(verb!(Verb::Dedent)),
        motion: Some(motion!(Motion::WholeLine)),
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      '=' => C::complete(EditCmd {
        register,
        verb: Some(verb!(Verb::Equalize)),
        motion: Some(motion!(Motion::WholeLine)),
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'p' | 'P' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::Put(Anchor::Before))),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'x' | 's' | 'd' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::Delete)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'r' => {
        let Some(ch) = chars.next() else {
          return C::pending();
        };
        C::complete(EditCmd {
          register,
          verb: Some(verb!(Verb::ReplaceChar(ch))),
          motion: None,
          raw_seq: String::new(),
          flags: CmdFlags::empty(),
        })
      }
      '~' => C::complete(EditCmd {
        register,
        verb: Some(verb!(Verb::ToggleCaseRange)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'u' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::ToLower)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'S' | 'c' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::Change)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'U' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::ToUpper)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'O' | 'o' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::SwapVisualAnchor)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'A' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::InsertMode)),
        motion: Some(motion!(Motion::ForwardChar)),
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'I' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::InsertMode)),
        motion: Some(motion!(Motion::StartOfLine)),
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'J' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::JoinLines)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      'y' => C::complete(EditCmd {
        register,
        verb: Some(verb!(count, Verb::Yank)),
        motion: None,
        raw_seq: String::new(),
        flags: CmdFlags::empty(),
      }),
      // In visual mode i/a introduce a text object that reshapes the selection,
      // rather than entering insert/append the way common_verb would treat them.
      'i' | 'a' => match ViParser::parse_text_obj(ch, chars, count) {
        C::Partial(motion) => C::complete(EditCmd {
          register,
          verb: None,
          motion: Some(motion),
          raw_seq: String::new(),
          flags: CmdFlags::empty(),
        }),
        C::Complete(res) => C::Complete(res),
        C::NoMatch => C::no_match(),
      },
      _ => C::no_match(),
    }
  }
}

impl Default for ViVisual {
  fn default() -> Self {
    Self::new()
  }
}

impl EditMode for ViVisual {
  #[expect(clippy::too_many_lines)]
  fn handle_key(&mut self, key: E) -> Option<EditCmd> {
    let mut cmd: Option<EditCmd> = match key {
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
        flags: CmdFlags::empty(),
      }),
      E(K::ExMode, _) => {
        return Some(EditCmd {
          register: RegisterName::default(),
          verb: Some(verb!(Verb::ExMode)),
          motion: None,
          raw_seq: String::new(),
          flags: CmdFlags::default(),
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
          flags: CmdFlags::empty(),
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
          flags: CmdFlags::empty(),
        })
      }
      key!(Ctrl + 'g') => {
        self.pending_seq.clear();
        Some(EditCmd {
          register: RegisterName::default(),
          verb: Some(verb!(Verb::PrintPosition)),
          motion: None,
          raw_seq: String::new(),
          flags: CmdFlags::empty(),
        })
      }
      key!(Ctrl + 'd') => {
        self.pending_seq.clear();
        Some(EditCmd {
          register: RegisterName::default(),
          verb: None,
          motion: Some(motion!(Motion::HalfScreenDown)),
          raw_seq: String::new(),
          flags: CmdFlags::empty(),
        })
      }
      key!(Ctrl + 'u') => {
        self.pending_seq.clear();
        Some(EditCmd {
          register: RegisterName::default(),
          verb: None,
          motion: Some(motion!(Motion::HalfScreenUp)),
          raw_seq: String::new(),
          flags: CmdFlags::empty(),
        })
      }
      key!(Ctrl + 'r') => {
        let mut chars = self.pending_seq.chars().peekable();
        let count = Self::parse_count(&mut chars).unwrap_or(1);
        Some(EditCmd {
          register: RegisterName::default(),
          verb: Some(verb!(count, Verb::Redo)),
          motion: None,
          raw_seq: self.take_cmd(),
          flags: CmdFlags::empty(),
        })
      }
      E(K::Esc, _) => Some(EditCmd {
        register: RegisterName::default(),
        verb: Some(verb!(Verb::NormalMode)),
        motion: Some(motion!(Motion::Null)),
        raw_seq: self.take_cmd(),
        flags: CmdFlags::empty(),
      }),
      _ => {
        if let Some(cmd) = common_cmds(&key) {
          self.clear_cmd();
          Some(cmd)
        } else {
          None
        }
      }
    };

    if let Some(cmd) = cmd.as_mut() {
      cmd.normalize_counts();
    }
    if let Some(cmd) = cmd.as_ref()
      && !matches!(
        cmd.verb.as_ref().map(|v| &v.1),
        Some(Verb::NormalMode | Verb::ExMode | Verb::Undo | Verb::Redo)
      )
    {
      self.register_cmd(cmd);
    }
    cmd
  }

  fn is_repeatable(&self) -> bool {
    true
  }

  fn as_replay(&self) -> Option<CmdReplay> {
    if self.cmds.is_empty() {
      None
    } else {
      Some(CmdReplay::mode(self.cmds.clone(), self.repeat_count))
    }
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
    ModeReport::Visual
  }
}
