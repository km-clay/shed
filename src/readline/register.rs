use std::{cell::RefCell, fmt::Display};

use itertools::Itertools;

use crate::{
  HashMap,
  expand::alias,
  keys::KeyEvent,
  procio,
  readline::linebuf::MotionKind,
  sherr,
  state::{Shed, cmd, meta::UtilKind, vars::VarStr},
  status_msg, try_var,
  util::{self, error::ShErr},
};

use super::linebuf::{Line, Lines};

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
  REGISTERS.with(|regs| regs.borrow().read(ch))
}

pub(crate) fn write_register(ch: Option<char>, buf: RegisterContent) {
  REGISTERS.with(|regs| regs.borrow_mut().write(ch, buf));
}

pub(crate) fn append_register(ch: Option<char>, buf: RegisterContent) {
  REGISTERS.with(|regs| regs.borrow_mut().append(ch, buf));
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

#[derive(Copy, Clone, Debug)]
pub(crate) enum ClipboardProvider {
  WlCopy,
  Wayclip,
  Xsel,
  Xclip,
  Tmux,
  Termux,
  PbCopy,
  Osc52, // ...
}

impl Default for ClipboardProvider {
  fn default() -> Self {
    Self::detect()
  }
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum Selection {
  Primary,
  Clipboard,
}

impl TryFrom<char> for Selection {
  type Error = ShErr;
  fn try_from(value: char) -> Result<Self, Self::Error> {
    match value {
      '*' => Ok(Self::Primary),
      '+' => Ok(Self::Clipboard),
      _ => Err(sherr!(ParseErr, "Invalid selection register: {value}")),
    }
  }
}

fn wayland_set() -> bool {
  try_var!("WAYLAND_DISPLAY").is_some()
}

fn display_set() -> bool {
  try_var!("DISPLAY").is_some()
}

fn tmux_set() -> bool {
  try_var!("TMUX").is_some()
}

fn exists(cmd: &str) -> bool {
  cmd::which_util(cmd).is_some_and(|u| matches!(u.kind(), UtilKind::Command(_)))
}

impl ClipboardProvider {
  pub fn detect() -> Self {
    if exists("pbcopy") {
      return Self::PbCopy;
    }

    if wayland_set() {
      if exists("wl-copy") {
        return Self::WlCopy;
      }
      if exists("wayclip") {
        return Self::Wayclip;
      }
    }

    if display_set() {
      if exists("xsel") {
        return Self::Xsel;
      }
      if exists("xclip") {
        return Self::Xclip;
      }
    }

    if exists("termux-clipboard-set") {
      return Self::Termux;
    }

    if tmux_set() && exists("tmux") {
      return Self::Tmux;
    }

    Self::Osc52
  }

  pub fn copy(self, sel: Selection, content: &RegisterContent) {
    let text = content.to_string();
    match self.copy_argv(sel) {
      Some(argv) => {
        let res = util::with_saved_status(|| {
          procio::capture_command(
            argv.as_bytes(),
            Some(text.as_bytes()),
            Some(&("clipboard copy".into())),
          )
        });

        if let Err(e) = res {
          status_msg!("clipboard copy failed: {e}");
        }
      }
      None => {
        Shed::term_mut(|t| t.emit_osc_copy(matches!(sel, Selection::Primary), &text)).ok();
        //emit osc52
      }
    }
  }

  pub fn paste(self, sel: Selection) -> Option<RegisterContent> {
    let argv = self.paste_argv(sel)?;
    let out = util::with_saved_status(|| {
      procio::capture_command(argv.as_bytes(), None, Some(&("clipboard paste".into()))).ok()
    })?;

    Some(RegisterContent::Span(Lines::to_lines(&out).into_vec()))
  }

  pub fn copy_argv(self, sel: Selection) -> Option<&'static str> {
    Some(match (self, sel) {
      (Self::WlCopy, Selection::Clipboard) => "wl-copy",
      (Self::WlCopy, Selection::Primary) => "wl-copy --primary",
      (Self::Wayclip, Selection::Primary) => "wayclip --primary",
      (Self::Xsel, Selection::Clipboard) => "xsel --clipboard --input",
      (Self::Xsel, Selection::Primary) => "xsel --primary --input",
      (Self::Xclip, Selection::Clipboard) => "xclip -selection clipboard",
      (Self::Xclip, Selection::Primary) => "xclip -selection primary",
      (Self::Wayclip, _) => "waycopy",
      (Self::Termux, _) => "termux-clipboard-set",
      (Self::Tmux, _) => "tmux load-buffer -",
      (Self::PbCopy, _) => "pbcopy",
      (Self::Osc52, _) => return None,
    })
  }

  pub fn paste_argv(self, sel: Selection) -> Option<&'static str> {
    Some(match (self, sel) {
      (Self::WlCopy, Selection::Clipboard) => "wl-paste",
      (Self::WlCopy, Selection::Primary) => "wl-paste --primary",
      (Self::Xsel, Selection::Clipboard) => "xsel --clipboard --output",
      (Self::Xsel, Selection::Primary) => "xsel --primary --output",
      (Self::Xclip, Selection::Clipboard) => "xclip -selection clipboard -o",
      (Self::Xclip, Selection::Primary) => "xclip -selection primary -o",
      (Self::Wayclip, _) => "waypaste",
      (Self::Termux, _) => "termux-clipboard-get",
      (Self::Tmux, _) => "tmux save-buffer -",
      (Self::PbCopy, _) => "pbpaste",
      (Self::Osc52, _) => return None,
    })
  }
}

#[derive(Default, Clone, Debug)]
pub struct Registers {
  registers: HashMap<char, Register>,
  clipboard: ClipboardProvider,
}

impl Registers {
  pub fn new() -> Self {
    let mut regs = HashMap::default();
    for c in 'a'..='z' {
      regs.insert(c, Register::default());
    }
    regs.insert('"', Register::default()); // 'default' register
    regs.insert('+', Register::default()); // system clipboard register
    regs.insert('*', Register::default()); // primary selection register
    Self {
      registers: regs,
      clipboard: ClipboardProvider::default(),
    }
  }
  pub fn resolve_key(key: Option<char>) -> Option<char> {
    match key.unwrap_or('"') {
      '"' => Some('"'),
      '+' => Some('+'),
      '*' => Some('*'),
      c if c.is_ascii_alphabetic() => Some(c.to_ascii_lowercase()),
      _ => None,
    }
  }
  pub fn read(&self, name: Option<char>) -> Option<RegisterContent> {
    let key = Self::resolve_key(name)?;
    if let Ok(sel) = Selection::try_from(key)
      && let Some(content) = self.clipboard.paste(sel)
    {
      return Some(content);
    }

    self.registers.get(&key).map(|r| r.content().clone())
  }
  pub fn write(&mut self, name: Option<char>, buf: RegisterContent) {
    self.write_inner(name, buf, false);
  }
  pub fn append(&mut self, name: Option<char>, buf: RegisterContent) {
    self.write_inner(name, buf, true);
  }
  pub fn write_inner(&mut self, name: Option<char>, buf: RegisterContent, append: bool) {
    let Some(key) = Self::resolve_key(name) else {
      return;
    };
    if let Ok(sel) = Selection::try_from(key) {
      self.clipboard.copy(sel, &buf);
    }

    if let Some(r) = self.registers.get_mut(&key) {
      if append {
        r.append(buf);
      } else {
        r.write(buf);
      }
    }
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
        a.extend(alias::expand_keymap(&text));
      }

      (
        // macro-into-text: render keys as a vim-style string, push as one Line
        C::Span(a) | C::Line(a) | C::Block(a),
        C::Macro(b),
      ) => {
        let rendered: VarStr = b
          .iter()
          .fold(util::scratch_buf(), |mut buf, key| {
            buf.extend_from_slice(key.as_vim_seq().as_bytes());
            buf
          })
          .into();
        let mut line = crate::readline::linebuf::Line::default();
        line.push_str(&rendered.to_str_lossy());
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
