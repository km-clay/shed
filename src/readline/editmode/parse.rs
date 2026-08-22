use std::{iter::Peekable, str::Chars};

use crate::util;

use super::{
  CmdState,
  editcmd::{Anchor, Bound, Cmd, CmdFlags, Dest, Direction, EditCmd, Motion, TextObj, Verb, Word},
  linebuf::Grapheme,
  motion,
  register::RegisterName,
  verb,
};

/// The result of a single `ViParser` run
pub enum ParseResult {
  Complete(Box<EditCmd>),
  Pending,
  Invalid,
}

pub enum CallbackResult<T> {
  Complete(ParseResult),
  Partial(T),
  NoMatch,
}

impl<T> CallbackResult<T> {
  pub fn complete(cmd: EditCmd) -> Self {
    CallbackResult::Complete(ParseResult::Complete(Box::new(cmd)))
  }
  pub const fn pending() -> Self {
    CallbackResult::Complete(ParseResult::Pending)
  }
  pub const fn invalid() -> Self {
    CallbackResult::Complete(ParseResult::Invalid)
  }
  pub const fn no_match() -> Self {
    CallbackResult::NoMatch
  }
  pub fn partial(cmd: T) -> Self {
    CallbackResult::Partial(cmd)
  }
}

type MotionCallback =
  fn(&mut Peekable<Chars<'_>>, Option<&Cmd<Verb>>, usize) -> CallbackResult<Cmd<Motion>>;
type VerbCallback = fn(&mut Peekable<Chars<'_>>, usize) -> CallbackResult<Cmd<Verb>>;
type Validator = fn(Option<&Verb>, Option<&Motion>) -> CmdState;

#[derive(Clone, Debug)]
pub struct ViParser {
  motion_callback: Option<MotionCallback>,
  verb_callback: Option<VerbCallback>,
  validator: Validator,
}

impl ViParser {
  pub fn new(
    motion_callback: Option<MotionCallback>,
    verb_callback: Option<VerbCallback>,
    validator: Validator,
  ) -> Self {
    Self {
      motion_callback,
      verb_callback,
      validator,
    }
  }
  pub fn try_parse(&self, pending_seq: &str) -> ParseResult {
    use CallbackResult as C;
    use ParseResult as P;
    let mut chars = pending_seq.chars().peekable();
    let chars_iter = &mut chars;

    let register = Self::reg_parse(chars_iter);

    let mut chars_clone = chars_iter.clone();
    let verb_count = Self::parse_count(&mut chars_clone).unwrap_or(1);
    let verb = match self.parse_verb(&mut chars_clone, verb_count) {
      C::Partial(verb) => {
        *chars_iter = chars_clone;
        Some(verb)
      }
      C::Complete(res) => match res {
        P::Complete(mut cmd) => {
          if cmd.register.is_none() {
            cmd.register = register;
          }
          cmd.raw_seq = pending_seq.to_string();
          return P::Complete(cmd);
        }
        _ => return res,
      },
      C::NoMatch => None,
    };

    let motion_count = Self::parse_count(chars_iter).unwrap_or(1);
    let motion = match self.parse_motion(chars_iter, verb.as_ref(), motion_count) {
      C::Partial(motion) => Some(motion),
      C::Complete(res) => match res {
        P::Complete(mut cmd) => {
          cmd.register = register;
          cmd.verb.clone_from(&verb);
          cmd.raw_seq = pending_seq.to_string();
          return P::Complete(cmd);
        }
        _ => return res,
      },
      C::NoMatch => {
        return P::Invalid;
      }
    };

    match (self.validator)(verb.as_ref().map(|v| &v.1), motion.as_ref().map(|m| &m.1)) {
      CmdState::Complete => P::Complete(Box::new(EditCmd {
        register,
        verb,
        motion,
        raw_seq: pending_seq.to_string(),
        flags: CmdFlags::empty(),
      })),
      CmdState::Pending => P::Pending,
      CmdState::Invalid => P::Invalid,
    }
  }
  fn parse_count(chars: &mut Peekable<Chars<'_>>) -> Option<usize> {
    if chars
      .peek()
      .is_none_or(|ch| *ch == '0' || !ch.is_ascii_digit())
    {
      return None;
    }
    let mut count = util::scratch_str();

    while chars.peek().is_some_and(char::is_ascii_digit) {
      count.push(chars.next().unwrap());
    }

    count.parse::<usize>().ok()
  }
  fn parse_motion(
    &self,
    chars: &mut Peekable<Chars<'_>>,
    verb: Option<&Cmd<Verb>>,
    count: usize,
  ) -> CallbackResult<Cmd<Motion>> {
    let mut chars_clone = chars.clone();
    match self.motion_callback {
      Some(cb) => match (cb)(&mut chars_clone, verb, count) {
        CallbackResult::NoMatch => Self::common_motion(chars, verb, count),
        result => {
          *chars = chars_clone;
          result
        }
      },
      None => Self::common_motion(chars, verb, count),
    }
  }
  fn parse_verb(&self, chars: &mut Peekable<Chars<'_>>, count: usize) -> CallbackResult<Cmd<Verb>> {
    let mut chars_clone = chars.clone();
    match self.verb_callback {
      Some(cb) => match (cb)(&mut chars_clone, count) {
        CallbackResult::NoMatch => Self::common_verb(chars, count),
        result => {
          *chars = chars_clone;
          result
        }
      },
      None => Self::common_verb(chars, count),
    }
  }
  #[expect(clippy::too_many_lines)]
  fn common_motion(
    chars: &mut Peekable<Chars<'_>>,
    verb: Option<&Cmd<Verb>>,
    count: usize,
  ) -> CallbackResult<Cmd<Motion>> {
    use CallbackResult as C;
    let Some(ch) = chars.next() else {
      return C::pending();
    };

    match (ch, &verb) {
      ('d', Some(Cmd(_, Verb::Delete)))
      | ('y', Some(Cmd(_, Verb::Yank)))
      | ('=', Some(Cmd(_, Verb::Equalize)))
      | ('>', Some(Cmd(_, Verb::Indent)))
      | ('<', Some(Cmd(_, Verb::Dedent)))
      | ('c', Some(Cmd(_, Verb::Change)))
      | ('U', Some(Cmd(_, Verb::ToUpper)))
      | ('u', Some(Cmd(_, Verb::ToLower)))
      | ('~', Some(Cmd(_, Verb::ToggleCaseRange))) => {
        return C::partial(motion!(count, Motion::WholeLine));
      }
      _ => {}
    }

    if let Some(motion) = Motion::word_motion(&[ch]) {
      return C::partial(motion!(count, motion));
    }

    match ch {
      '%' => C::partial(motion!(count, Motion::ToDelimMatch)),
      ';' => C::partial(motion!(count, Motion::RepeatMotion)),
      ',' => C::partial(motion!(count, Motion::RepeatMotionRev)),
      'n' => C::partial(motion!(count, Motion::RepeatSearch)),
      'N' => C::partial(motion!(count, Motion::RepeatSearchRev)),
      '|' => C::partial(motion!(count, Motion::ToColumn)),
      '0' => C::partial(motion!(count, Motion::StartOfLine)),
      '^' => C::partial(motion!(count, Motion::StartOfFirstWord)),
      '$' => C::partial(motion!(count, Motion::EndOfLine)),
      'G' => C::partial(motion!(count, Motion::EndOfBuffer)),
      'k' => C::partial(motion!(count, Motion::LineUp)),
      'j' => C::partial(motion!(count, Motion::LineDown)),
      'h' => C::partial(motion!(count, Motion::BackwardChar)),
      'l' => C::partial(motion!(count, Motion::ForwardChar)),
      ')' => C::partial(motion!(
        count,
        Motion::TextObj(TextObj::Sentence(Direction::Forward))
      )),
      '(' => C::partial(motion!(
        count,
        Motion::TextObj(TextObj::Sentence(Direction::Backward))
      )),
      '}' => C::partial(motion!(
        count,
        Motion::TextObj(TextObj::Paragraph(Direction::Forward))
      )),
      '{' => C::partial(motion!(
        count,
        Motion::TextObj(TextObj::Paragraph(Direction::Backward))
      )),
      'f' => {
        let Some(ch) = chars.next() else {
          return C::pending();
        };
        C::partial(motion!(
          count,
          Motion::CharSearch(Direction::Forward, Dest::On, Grapheme::from(ch))
        ))
      }
      'F' => {
        let Some(ch) = chars.next() else {
          return C::pending();
        };
        C::partial(motion!(
          count,
          Motion::CharSearch(Direction::Backward, Dest::On, Grapheme::from(ch))
        ))
      }
      't' => {
        let Some(ch) = chars.next() else {
          return C::pending();
        };
        C::partial(motion!(
          count,
          Motion::CharSearch(Direction::Forward, Dest::Before, Grapheme::from(ch))
        ))
      }
      'T' => {
        let Some(ch) = chars.next() else {
          return C::pending();
        };
        C::partial(motion!(
          count,
          Motion::CharSearch(Direction::Backward, Dest::Before, Grapheme::from(ch))
        ))
      }
      'g' => {
        let Some(&next_ch) = chars.peek() else {
          return C::pending();
        };
        if let Some(motion) = Motion::word_motion(&[ch, next_ch]) {
          chars.next();
          return C::partial(motion!(count, motion));
        }
        match next_ch {
          // vim's alternate doubled-operator spellings: gUgU / gugu / g~g~
          // repeat the g-prefixed verb to act on the whole line.
          'U' if matches!(verb, Some(Cmd(_, Verb::ToUpper))) => {
            chars.next();
            C::partial(motion!(count, Motion::WholeLine))
          }
          'u' if matches!(verb, Some(Cmd(_, Verb::ToLower))) => {
            chars.next();
            C::partial(motion!(count, Motion::WholeLine))
          }
          '~' if matches!(verb, Some(Cmd(_, Verb::ToggleCaseRange))) => {
            chars.next();
            C::partial(motion!(count, Motion::WholeLine))
          }
          'g' => {
            chars.next();
            C::partial(motion!(count, Motion::StartOfBuffer))
          }
          '_' => {
            chars.next();
            C::partial(motion!(count, Motion::EndOfLastWord))
          }
          _ => C::invalid(),
        }
      }
      ']' => {
        let Some(ch) = chars.peek() else {
          return C::pending();
        };
        match ch {
          ')' => {
            chars.next();
            C::partial(motion!(count, Motion::ToParen(Direction::Forward)))
          }
          '}' => {
            chars.next();
            C::partial(motion!(count, Motion::ToBrace(Direction::Forward)))
          }
          _ => C::invalid(),
        }
      }
      '[' => {
        let Some(ch) = chars.peek() else {
          return C::pending();
        };
        match ch {
          '(' => {
            chars.next();
            C::partial(motion!(count, Motion::ToParen(Direction::Backward)))
          }
          '{' => {
            chars.next();
            C::partial(motion!(count, Motion::ToBrace(Direction::Backward)))
          }
          _ => C::invalid(),
        }
      }

      ch if ch == 'i' || ch == 'a' => Self::parse_text_obj(ch, chars, count),
      _ => C::no_match(),
    }
  }
  /// Parse a text object given its leading bound char (`i` for inside, `a` for
  /// around) and the object char that follows. Shared by the operator-pending
  /// motion parser and visual mode, where `i`/`a` introduce a text object.
  pub(super) fn parse_text_obj(
    bound_ch: char,
    chars: &mut Peekable<Chars<'_>>,
    count: usize,
  ) -> CallbackResult<Cmd<Motion>> {
    use CallbackResult as C;
    let bound = match bound_ch {
      'i' => Bound::Inside,
      'a' => Bound::Around,
      _ => return C::no_match(),
    };
    let Some(next_ch) = chars.next() else {
      return C::pending();
    };
    let obj = match next_ch {
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
      _ => return C::invalid(),
    };
    C::partial(motion!(count, Motion::TextObj(obj)))
  }
  #[expect(clippy::too_many_lines)]
  fn common_verb(chars: &mut Peekable<Chars<'_>>, count: usize) -> CallbackResult<Cmd<Verb>> {
    use CallbackResult as C;
    let Some(ch) = chars.next() else {
      return C::pending();
    };
    match ch {
      'g' => {
        let Some(ch) = chars.peek() else {
          return C::pending();
        };
        match ch {
          'v' => {
            chars.next();
            C::complete(EditCmd {
              verb: Some(verb!(count, Verb::VisualModeSelectLast)),
              ..Default::default()
            })
          }
          '~' => {
            chars.next();
            C::partial(verb!(count, Verb::ToggleCaseRange))
          }
          'u' => {
            chars.next();
            C::partial(verb!(count, Verb::ToLower))
          }
          'U' => {
            chars.next();
            C::partial(verb!(count, Verb::ToUpper))
          }
          '?' => {
            chars.next();
            C::partial(verb!(count, Verb::Rot13))
          }
          // we return no_match here
          // so that it falls through to common_motion()'s
          // 'g' parsing
          _ => C::no_match(),
        }
      }
      'q' => {
        let Some(reg) = chars.next() else {
          return C::pending();
        };
        log::debug!("record macro reg: {reg}");
        let register: RegisterName = reg.into();
        C::complete(EditCmd {
          register,
          verb: Some(verb!(Verb::RecordMacro)),
          motion: None,
          ..Default::default()
        })
      }
      '@' => {
        let Some(reg) = chars.next() else {
          return C::pending();
        };
        let register: RegisterName = if reg == '@' {
          RegisterName::new(None)
        } else {
          reg.into()
        };
        C::complete(EditCmd {
          register,
          verb: Some(verb!(count, Verb::PlayMacro)),
          motion: None,
          ..Default::default()
        })
      }
      'Q' => C::complete(EditCmd {
        register: RegisterName::new(None),
        verb: Some(verb!(count, Verb::PlayMacro)),
        ..Default::default()
      }),
      '.' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::RepeatLast)),
        ..Default::default()
      }),
      'x' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Delete)),
        motion: Some(motion!(Motion::ForwardCharForced)),
        ..Default::default()
      }),
      'X' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Delete)),
        motion: Some(motion!(Motion::BackwardChar)),
        ..Default::default()
      }),
      's' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Change)),
        motion: Some(motion!(Motion::ForwardChar)),
        ..Default::default()
      }),
      'S' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Change)),
        motion: Some(motion!(Motion::WholeLine)),
        ..Default::default()
      }),
      'p' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Put(Anchor::After))),
        ..Default::default()
      }),
      'P' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Put(Anchor::Before))),
        ..Default::default()
      }),
      '>' => C::partial(verb!(count, Verb::Indent)),
      '<' => C::partial(verb!(count, Verb::Dedent)),
      'r' => {
        let Some(replacement) = chars.next() else {
          return C::pending();
        };
        C::complete(EditCmd {
          verb: Some(verb!(Verb::ReplaceCharInplace(replacement, count as u16))),
          ..Default::default()
        })
      }
      'R' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::ReplaceMode)),
        ..Default::default()
      }),
      '~' => C::complete(EditCmd {
        verb: Some(verb!(Verb::ToggleCaseInplace(count as u16))),
        ..Default::default()
      }),
      'u' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Undo)),
        ..Default::default()
      }),
      'v' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::VisualMode)),
        ..Default::default()
      }),
      'V' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::VisualModeLine)),
        ..Default::default()
      }),
      'o' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::InsertModeLineBreak(Anchor::After))),
        ..Default::default()
      }),
      'O' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::InsertModeLineBreak(Anchor::Before))),
        ..Default::default()
      }),
      'a' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::InsertMode)),
        motion: Some(motion!(Motion::ForwardChar)),
        ..Default::default()
      }),
      'A' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::InsertMode)),
        motion: Some(motion!(Motion::EndOfLine)),
        ..Default::default()
      }),
      ':' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::ExMode)),
        ..Default::default()
      }),
      'i' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::InsertMode)),
        ..Default::default()
      }),
      'I' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::InsertMode)),
        motion: Some(motion!(Motion::StartOfFirstWord)),
        ..Default::default()
      }),
      'J' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::JoinLines)),
        ..Default::default()
      }),
      'y' => C::partial(verb!(count, Verb::Yank)),
      'd' => C::partial(verb!(count, Verb::Delete)),
      'c' => C::partial(verb!(count, Verb::Change)),
      'Y' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Yank)),
        motion: Some(motion!(Motion::EndOfLine)),
        ..Default::default()
      }),
      'D' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Delete)),
        motion: Some(motion!(Motion::EndOfLine)),
        ..Default::default()
      }),
      'C' => C::complete(EditCmd {
        verb: Some(verb!(count, Verb::Change)),
        motion: Some(motion!(Motion::EndOfLine)),
        ..Default::default()
      }),
      '=' => C::partial(verb!(count, Verb::Equalize)),
      dir @ ('/' | '?') => {
        // search mode, return an entire command here
        let verb = Some(verb!(
          count,
          match dir {
            '/' => Verb::SearchMode,
            '?' => Verb::RevSearchMode,
            _ => unreachable!(),
          }
        ));
        C::complete(EditCmd {
          verb,
          ..Default::default()
        })
      }
      _ => C::no_match(),
    }
  }
  fn reg_parse(chars: &mut Peekable<Chars<'_>>) -> RegisterName {
    let chars_clone = chars.clone();
    let _count = Self::parse_count(chars);

    let Some('"') = chars.peek() else {
      *chars = chars_clone;
      return RegisterName::default();
    };
    chars.next();

    let Some(reg_name) = chars.next() else {
      return RegisterName::default();
    };
    match reg_name {
      'a'..='z' | 'A'..='Z' | '+' | '*' => RegisterName::new(Some(reg_name)),
      _ => RegisterName::default(),
    }
  }
}
