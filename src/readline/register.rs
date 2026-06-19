use std::{cell::RefCell, fmt::Display};

use itertools::Itertools;

use crate::{HashMap, readline::linebuf::MotionKind, state::vars::VarStr, util};

use super::{
  super::keys::KeyEvent,
  expand::expand_keymap,
  linebuf::{Line, Lines},
};

thread_local! {
  pub static REGISTERS: RefCell<Registers> = RefCell::new(Registers::new());

  #[cfg(test)]
  pub static SAVED_REGISTERS: RefCell<Option<Registers>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub fn save_registers() {
  SAVED_REGISTERS.with(|saved| {
    let mut saved = saved.borrow_mut();
    *saved = Some(REGISTERS.with(|regs| regs.borrow().clone()));
  });
}

#[cfg(test)]
pub fn restore_registers() {
  SAVED_REGISTERS.with(|saved| {
    let mut saved = saved.borrow_mut();
    if let Some(regs) = saved.take() {
      REGISTERS.with(|r| *r.borrow_mut() = regs);
    }
  });
}

pub(crate) fn read_register(ch: Option<char>) -> Option<RegisterContent> {
  REGISTERS.with(|regs| regs.borrow().get_reg(ch).map(|r| r.content().clone()))
}

pub(crate) fn write_register(ch: Option<char>, buf: RegisterContent) {
  REGISTERS.with(|regs| {
    if let Some(r) = regs.borrow_mut().get_reg_mut(ch) {
      r.write(buf);
    }
  });
}

pub(crate) fn append_register(ch: Option<char>, buf: RegisterContent) {
  REGISTERS.with(|regs| {
    if let Some(r) = regs.borrow_mut().get_reg_mut(ch) {
      r.append(buf);
    }
  });
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RegisterName {
  name: Option<char>,
  append: bool,
}

impl RegisterName {
  pub fn new(name: Option<char>) -> Self {
    let Some(ch) = name else {
      return Self::default();
    };

    let append = ch.is_uppercase();
    let name = ch.to_ascii_lowercase();
    Self {
      name: Some(name),
      append,
    }
  }
  pub fn name(self) -> Option<char> {
    self.name
  }
  pub fn display(self) -> Option<char> {
    let name = self.name?;
    if self.append {
      Some(name.to_ascii_uppercase())
    } else {
      Some(name)
    }
  }
  pub fn is_none(self) -> bool {
    self.name.is_none()
  }
  pub fn write_to_register(self, buf: RegisterContent) {
    if self.append {
      append_register(self.name, buf);
    } else {
      write_register(self.name, buf);
    }
  }
  pub fn read_from_register(self) -> Option<RegisterContent> {
    read_register(self.name)
  }
}

impl Default for RegisterName {
  fn default() -> Self {
    Self {
      name: None,
      append: false,
    }
  }
}

impl From<char> for RegisterName {
  fn from(value: char) -> Self {
    Self::new(Some(value))
  }
}

#[derive(Default, Clone, Debug)]
pub(crate) enum RegisterContent {
  Span(Vec<Line>),
  Line(Vec<Line>),
  Block(Vec<Line>),
  Macro(Vec<KeyEvent>),
  #[default]
  Empty,
}
impl RegisterContent {
  pub fn from_extracted(content: Lines, motion: &MotionKind) -> Self {
    match motion {
      MotionKind::Char { .. } => RegisterContent::Span(content.into_vec()),
      MotionKind::Line { .. } => RegisterContent::Line(content.into_vec()),
      MotionKind::Block { .. } => RegisterContent::Block(content.into_vec()),
    }
  }
}

impl Display for RegisterContent {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Block(s) | Self::Line(s) | Self::Span(s) => {
        let joined = s
          .iter()
          .map(ToString::to_string)
          .collect::<Vec<_>>()
          .join("\n");

        write!(f, "{joined}")
      }
      Self::Macro(keys) => {
        let expanded = keys.iter().map(KeyEvent::as_vim_seq).join("");
        write!(f, "{expanded}")
      }
      Self::Empty => write!(f, ""),
    }
  }
}

#[derive(Default, Clone, Debug)]
pub struct Registers(HashMap<char, Register>);

impl Registers {
  pub fn new() -> Self {
    let mut regs = HashMap::default();
    for c in 'a'..='z' {
      regs.insert(c, Register::default());
    }
    regs.insert('"', Register::default()); // 'default' register
    Self(regs)
  }
  pub fn get_reg(&self, name: Option<char>) -> Option<&Register> {
    let key = match name {
      None | Some('"') => '"',
      Some(c) if c.is_ascii_alphabetic() => c.to_ascii_lowercase(),
      _ => return None,
    };

    self.0.get(&key.to_ascii_lowercase())
  }
  pub fn get_reg_mut(&mut self, name: Option<char>) -> Option<&mut Register> {
    let key = match name {
      None | Some('"') => '"',
      Some(c) if c.is_ascii_alphabetic() => c.to_ascii_lowercase(),
      _ => return None,
    };

    self.0.get_mut(&key.to_ascii_lowercase())
  }
}

#[derive(Clone, Default, Debug)]
pub struct Register {
  content: RegisterContent,
}

impl Register {
  pub fn content(&self) -> &RegisterContent {
    &self.content
  }
  pub fn write(&mut self, buf: RegisterContent) {
    self.content = buf;
  }
  pub fn append(&mut self, buf: RegisterContent) {
    use RegisterContent as C;
    if matches!(buf, RegisterContent::Empty) {
      return;
    }
    if matches!(self.content, RegisterContent::Empty) {
      self.content = buf;
      return;
    }

    match (&mut self.content, buf) {
      // same-shape text-into-text: extend in place
      (
        C::Span(a) | C::Line(a) | C::Block(a),
        C::Span(mut b) | C::Line(mut b) | C::Block(mut b),
      ) => {
        a.append(&mut b);
      }
      // macro-into-macro: extend in place
      (C::Macro(a), C::Macro(mut b)) => {
        a.append(&mut b);
      }

      (
        // text-into-macro: parse the text as a key sequence
        C::Macro(a),
        C::Span(b) | C::Line(b) | C::Block(b),
      ) => {
        let text = b
          .iter()
          .map(ToString::to_string)
          .collect::<Vec<_>>()
          .join("\n");
        a.extend(expand_keymap(&text));
      }

      (
        // macro-into-text: render keys as a vim-style string, push as one Line
        C::Span(a) | C::Line(a) | C::Block(a),
        C::Macro(b),
      ) => {
        let rendered: VarStr = b
          .iter()
          .fold(util::scratch_buf(), |mut buf, key| {
            buf.push_str(key.as_vim_seq().as_str());
            buf
          })
          .into();
        let mut line = crate::readline::linebuf::Line::default();
        line.push_str(&rendered);
        a.push(line);
      }
      // both Empty cases handled above
      (C::Empty, _) | (_, C::Empty) => unreachable!(),
    }
  }
}

#[cfg(test)]
mod register_append_tests {
  use super::*;
  use crate::readline::linebuf::Line;

  fn line(s: &str) -> Line {
    let mut l = Line::default();
    l.push_str(s);
    l
  }

  fn reg_with(content: RegisterContent) -> Register {
    let mut r = Register::default();
    r.write(content);
    r
  }

  // ─── Empty source is a no-op ─────────────────────────────────────

  #[test]
  fn appending_empty_into_existing_is_noop() {
    let mut r = reg_with(RegisterContent::Span(vec![line("hello")]));
    r.append(RegisterContent::Empty);
    match r.content() {
      RegisterContent::Span(lines) => {
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "hello");
      }
      other => panic!("expected Span, got {other:?}"),
    }
  }

  // ─── Empty target adopts the new content ────────────────────────

  #[test]
  fn appending_into_empty_overwrites() {
    let mut r = Register::default();
    r.append(RegisterContent::Span(vec![line("first")]));
    match r.content() {
      RegisterContent::Span(lines) => {
        assert_eq!(lines[0].to_string(), "first");
      }
      other => panic!("expected Span, got {other:?}"),
    }
  }

  // ─── Same-shape text-into-text ───────────────────────────────────

  #[test]
  fn span_into_span_extends() {
    let mut r = reg_with(RegisterContent::Span(vec![line("a"), line("b")]));
    r.append(RegisterContent::Span(vec![line("c")]));
    match r.content() {
      RegisterContent::Span(lines) => {
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].to_string(), "c");
      }
      other => panic!("expected Span, got {other:?}"),
    }
  }

  #[test]
  fn line_into_block_extends_in_place() {
    let mut r = reg_with(RegisterContent::Block(vec![line("x")]));
    r.append(RegisterContent::Line(vec![line("y")]));
    match r.content() {
      RegisterContent::Block(lines) => assert_eq!(lines.len(), 2),
      other => panic!("expected Block, got {other:?}"),
    }
  }

  // ─── Macro-into-macro ────────────────────────────────────────────

  #[test]
  fn macro_into_macro_extends() {
    use crate::keys::{KeyCode, KeyEvent, ModKeys};
    let mut r = reg_with(RegisterContent::Macro(vec![KeyEvent(
      KeyCode::Char('a'),
      ModKeys::empty(),
    )]));
    r.append(RegisterContent::Macro(vec![KeyEvent(
      KeyCode::Char('b'),
      ModKeys::empty(),
    )]));
    match r.content() {
      RegisterContent::Macro(keys) => assert_eq!(keys.len(), 2),
      other => panic!("expected Macro, got {other:?}"),
    }
  }

  // ─── Text-into-macro: expand_keymap parses ──────────────────────

  #[test]
  fn text_into_macro_parses_as_keys() {
    use crate::keys::KeyEvent;
    let mut r = reg_with(RegisterContent::Macro(Vec::<KeyEvent>::new()));
    r.append(RegisterContent::Span(vec![line("ab")]));
    match r.content() {
      RegisterContent::Macro(keys) => {
        // expand_keymap("ab") produces 2 key events.
        assert_eq!(keys.len(), 2);
      }
      other => panic!("expected Macro, got {other:?}"),
    }
  }

  // ─── Macro-into-text: renders as vim seq, pushed as one Line ────

  #[test]
  fn macro_into_text_renders_to_line() {
    use crate::keys::{KeyCode, KeyEvent, ModKeys};
    let mut r = reg_with(RegisterContent::Span(vec![line("existing")]));
    r.append(RegisterContent::Macro(vec![
      KeyEvent(KeyCode::Char('a'), ModKeys::empty()),
      KeyEvent(KeyCode::Char('b'), ModKeys::empty()),
    ]));
    match r.content() {
      RegisterContent::Span(lines) => {
        // Original line + one rendered macro line.
        assert_eq!(lines.len(), 2);
        // Rendered "ab" as vim seq is "ab".
        assert_eq!(lines[1].to_string(), "ab");
      }
      other => panic!("expected Span, got {other:?}"),
    }
  }
}
