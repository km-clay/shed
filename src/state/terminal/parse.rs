use itertools::Itertools;
use nix::{errno::Errno, unistd::read};
use std::{
  collections::VecDeque,
  fmt::{Debug, Display},
  os::fd::BorrowedFd,
  sync::atomic::Ordering,
};

use crate::{match_loop, procio::bytes_to_string};

use super::{
  ShErr, ShResult,
  keys::{KeyCode, KeyEvent, ModKeys},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rows(pub(crate) usize);
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cols(pub(crate) usize);

#[derive(Debug, Clone)]
pub(crate) enum TermEvent {
  Key(KeyEvent),
  CursorPos(Rows, Cols),
  XtVersion(XtVersion),
  FocusGained,
  FocusLost,
  PrimaryDevAttr,
  KittyKbdFlags,
  Capabilities {
    name: String,
    _value: Option<String>,
  },
}

#[derive(Debug, Clone, Copy)]
enum DcsKind {
  XtGetCap,
  XtVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SemVer {
  major: Option<u32>,
  minor: Option<u32>,
  patch: Option<u32>,
}

impl Display for SemVer {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match (self.major, self.minor, self.patch) {
      (Some(major), Some(minor), Some(patch)) => write!(f, "{major}.{minor}.{patch}"),
      (Some(major), Some(minor), None) => write!(f, "{major}.{minor}"),
      (Some(major), None, None) => write!(f, "{major}"),
      _ => write!(f, "unknown"),
    }
  }
}

macro_rules! semver {
  ($major:expr) => {
    SemVer {
      major: Some($major),
      minor: None,
      patch: None,
    }
  };
  ($major:expr, $minor:expr) => {
    SemVer {
      major: Some($major),
      minor: Some($minor),
      patch: None,
    }
  };
  ($major:expr, $minor:expr, $patch:expr) => {
    SemVer {
      major: Some($major),
      minor: Some($minor),
      patch: Some($patch),
    }
  };
}

#[expect(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum XtVersion {
  Iterm2(SemVer),
  Tmux(SemVer),
  WezTerm,
  Unknown(String),
}

#[expect(dead_code)]
impl XtVersion {
  pub(super) fn parse(raw: &str) -> Self {
    Self::parse_iterm2(raw)
      .or_else(|| Self::parse_tmux(raw))
      .or_else(|| Self::parse_wezterm(raw))
      .unwrap_or_else(|| Self::Unknown(raw.to_string()))
  }

  pub(super) fn has_broken_kitty_kbd(&self) -> bool {
    let Self::Iterm2(ver) = self else {
      return false;
    };

    *ver < semver!(3, 5, 12)
  }

  pub(super) fn needs_wezterm_workaround(&self) -> bool {
    matches!(self, Self::WezTerm)
  }

  pub(super) fn supports_color_theme_reporting(&self) -> bool {
    let Self::Tmux(ver) = self else { return true };

    *ver >= semver!(3, 7)
  }

  fn parse_iterm2(raw: &str) -> Option<Self> {
    let (name, rest) = raw.split_once(' ')?;
    if name != "iTerm2" {
      return None;
    }
    let mut parts = rest.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    let patch = parts.next()?.parse::<u32>().ok()?;
    Some(Self::Iterm2(semver!(major, minor, patch)))
  }

  fn parse_tmux(raw: &str) -> Option<Self> {
    let (name, rest) = raw.split_once(' ')?;
    if name != "tmux" {
      return None;
    }
    let mut parts = rest.split('.');
    // tmux minor may carry a trailing letter (e.g. "3.5a"); take leading digits only.
    let major = parse_leading_u32(parts.next()?)?;
    let minor = parse_leading_u32(parts.next()?)?;
    Some(Self::Tmux(semver!(major, minor)))
  }

  fn parse_wezterm(raw: &str) -> Option<Self> {
    raw.starts_with("WezTerm ").then_some(Self::WezTerm)
  }
}

fn parse_leading_u32(s: &str) -> Option<u32> {
  let end = s
    .bytes()
    .position(|b| !b.is_ascii_digit())
    .unwrap_or(s.len());
  if end == 0 {
    return None;
  }
  s[..end].parse().ok()
}

#[derive(Debug, Default, Clone)]
struct EventParser {
  events: VecDeque<TermEvent>,
  ss3_pending: bool,
  paste_buf: Option<String>,
  dcs_buf: Option<String>,
  dcs_kind: Option<DcsKind>,
  dcs_supported: bool,
}

impl EventParser {
  pub(crate) fn new() -> Self {
    Self {
      events: VecDeque::new(),
      ss3_pending: false,
      paste_buf: None,
      dcs_buf: None,
      dcs_kind: None,
      dcs_supported: false,
    }
  }

  pub(crate) fn push(&mut self, event: TermEvent) {
    self.events.push_back(event);
  }

  pub(crate) fn pop(&mut self) -> Option<TermEvent> {
    self.events.pop_front()
  }

  pub(crate) fn parse_term_cap(&mut self) {
    let Some(buf) = self.dcs_buf.take() else {
      return;
    };
    let supported = self.dcs_supported;
    self.dcs_kind = None;
    self.dcs_supported = false;

    // Only emit when the terminal reported the cap as supported.
    if !supported {
      return;
    }

    let (name_hex, value_hex) = match buf.split_once('=') {
      Some((n, v)) => (n, Some(v)),
      None => (buf.as_str(), None),
    };
    let Some(name) = Self::decode_hex(name_hex) else {
      return;
    };
    let value = value_hex.and_then(Self::decode_hex);

    self.push(TermEvent::Capabilities {
      name,
      _value: value,
    });
  }

  pub(crate) fn parse_xtversion(&mut self) {
    let Some(buf) = self.dcs_buf.take() else {
      return;
    };
    self.dcs_kind = None;
    let xtver = XtVersion::parse(&buf);
    self.push(TermEvent::XtVersion(xtver));
  }

  pub(crate) fn decode_hex(hex: &str) -> Option<String> {
    if !hex.len().is_multiple_of(2) {
      return None; // Invalid hex string
    }

    let bytes: Option<Vec<u8>> = hex
      .chars()
      .chunks(2)
      .into_iter()
      .map(|chunk| {
        let s: String = chunk.collect();
        u8::from_str_radix(&s, 16).ok()
      })
      .collect();
    bytes.map(bytes_to_string)
  }
}

impl vte::Perform for EventParser {
  fn hook(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
    let params: Vec<u16> = params
      .iter()
      .map(|p| p.first().copied().unwrap_or(0))
      .collect();

    match (intermediates, action) {
      ([b'+'], 'r') => {
        let first = params.first().copied().unwrap_or(0);
        self.dcs_supported = first == 1;
        self.dcs_kind = Some(DcsKind::XtGetCap);

        self.dcs_buf = Some(String::new());
      }
      ([b'>'], '|') => {
        self.dcs_kind = Some(DcsKind::XtVersion);
        self.dcs_buf = Some(String::new());
      }

      _ => {}
    }
  }

  fn put(&mut self, byte: u8) {
    if let Some(buf) = self.dcs_buf.as_mut() {
      buf.push(byte as char);
    }
  }

  fn unhook(&mut self) {
    let Some(kind) = self.dcs_kind else { return };
    match kind {
      DcsKind::XtGetCap => self.parse_term_cap(),
      DcsKind::XtVersion => self.parse_xtversion(),
    }
  }

  fn print(&mut self, c: char) {
    if let Some(buf) = self.paste_buf.as_mut() {
      buf.push(c);
      return;
    }

    // vte routes 0x7f (DEL) to print instead of execute
    if self.ss3_pending {
      self.ss3_pending = false;
      match c {
        'A' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::Up, ModKeys::empty())));
          return;
        }
        'B' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::Down, ModKeys::empty())));
          return;
        }
        'C' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::Right, ModKeys::empty())));
          return;
        }
        'D' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::Left, ModKeys::empty())));
          return;
        }
        'H' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::Home, ModKeys::empty())));
          return;
        }
        'F' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::End, ModKeys::empty())));
          return;
        }
        'P' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::F(1), ModKeys::empty())));
          return;
        }
        'Q' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::F(2), ModKeys::empty())));
          return;
        }
        'R' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::F(3), ModKeys::empty())));
          return;
        }
        'S' => {
          self.push(TermEvent::Key(KeyEvent(KeyCode::F(4), ModKeys::empty())));
          return;
        }
        _ => {}
      }
    }

    if c == '\x7f' {
      self.push(TermEvent::Key(KeyEvent(
        KeyCode::Backspace,
        ModKeys::empty(),
      )));
    } else {
      self.push(TermEvent::Key(KeyEvent(KeyCode::Char(c), ModKeys::empty())));
    }
  }

  fn execute(&mut self, byte: u8) {
    if let Some(buf) = self.paste_buf.as_mut() {
      buf.push(byte as char);
      return;
    }
    let event = match byte {
      0x00 => TermEvent::Key(KeyEvent(KeyCode::Char(' '), ModKeys::CTRL)), // Ctrl+Space / Ctrl+@
      0x09 => TermEvent::Key(KeyEvent(KeyCode::Tab, ModKeys::empty())),    // Tab (Ctrl+I)
      0x0a => TermEvent::Key(KeyEvent(KeyCode::Char('j'), ModKeys::CTRL)), // Ctrl+J (linefeed)
      0x0d => TermEvent::Key(KeyEvent(KeyCode::Enter, ModKeys::empty())), // Carriage return (Ctrl+M)
      0x1b => TermEvent::Key(KeyEvent(KeyCode::Esc, ModKeys::empty())),
      0x7f => TermEvent::Key(KeyEvent(KeyCode::Backspace, ModKeys::empty())),
      0x01..=0x1a => {
        // Ctrl+A through Ctrl+Z (excluding special cases above)
        let c = (b'a' + byte - 1) as char;
        TermEvent::Key(KeyEvent(KeyCode::Char(c), ModKeys::CTRL))
      }
      _ => return,
    };
    self.push(event);
  }

  #[expect(clippy::too_many_lines)]
  fn csi_dispatch(
    &mut self,
    params: &vte::Params,
    intermediates: &[u8],
    _ignore: bool,
    action: char,
  ) {
    let params: Vec<u16> = params
      .iter()
      .map(|p| p.first().copied().unwrap_or(0))
      .collect();

    let event = match (intermediates, action) {
      // Arrow keys: CSI A/B/C/D or CSI 1;mod A/B/C/D
      ([], 'R') => {
        let row = params.first().copied().unwrap_or(0) as usize;
        let col = params.get(1).copied().unwrap_or(0) as usize;
        TermEvent::CursorPos(Rows(row), Cols(col))
      }
      ([], 'A') => {
        let mods = params.get(1).map_or(ModKeys::empty(), ModKeys::from);
        TermEvent::Key(KeyEvent(KeyCode::Up, mods))
      }
      ([], 'B') => {
        let mods = params.get(1).map_or(ModKeys::empty(), ModKeys::from);
        TermEvent::Key(KeyEvent(KeyCode::Down, mods))
      }
      ([], 'C') => {
        let mods = params.get(1).map_or(ModKeys::empty(), ModKeys::from);
        TermEvent::Key(KeyEvent(KeyCode::Right, mods))
      }
      ([], 'D') => {
        let mods = params.get(1).map_or(ModKeys::empty(), ModKeys::from);
        TermEvent::Key(KeyEvent(KeyCode::Left, mods))
      }
      // Home/End: CSI H/F or CSI 1;mod H/F
      ([], 'H') => {
        let mods = params.get(1).map_or(ModKeys::empty(), ModKeys::from);
        TermEvent::Key(KeyEvent(KeyCode::Home, mods))
      }
      ([], 'F') => {
        let mods = params.get(1).map_or(ModKeys::empty(), ModKeys::from);
        TermEvent::Key(KeyEvent(KeyCode::End, mods))
      }
      // Shift+Tab: CSI Z
      ([], 'Z') => TermEvent::Key(KeyEvent(KeyCode::Tab, ModKeys::SHIFT)),
      // Focus events, CSI I/O
      ([], 'I') => TermEvent::FocusGained,
      ([], 'O') => TermEvent::FocusLost,

      // Special keys with tilde: CSI num ~ or CSI num;mod ~
      ([], '~') => {
        let key_num = params.first().copied().unwrap_or(0);
        let mods = params.get(1).map_or(ModKeys::empty(), ModKeys::from);
        let key = match key_num {
          1 | 7 => KeyCode::Home,
          2 => KeyCode::Insert,
          3 => KeyCode::Delete,
          4 | 8 => KeyCode::End,
          5 => KeyCode::PageUp,
          6 => KeyCode::PageDown,
          15 => KeyCode::F(5),
          17 => KeyCode::F(6),
          18 => KeyCode::F(7),
          19 => KeyCode::F(8),
          20 => KeyCode::F(9),
          21 => KeyCode::F(10),
          23 => KeyCode::F(11),
          24 => KeyCode::F(12),
          200 => {
            self.paste_buf = Some(String::new());
            return;
          }
          201 => {
            if let Some(buf) = self.paste_buf.take() {
              self.events.push_back(TermEvent::Key(KeyEvent(
                KeyCode::Verbatim(buf.into()),
                ModKeys::empty(),
              )));
            }
            return;
          }
          _ => return,
        };
        TermEvent::Key(KeyEvent(key, mods))
      }
      ([], 'u') => {
        // kitty keyboard protocol: CSI code;mod;text u
        let codepoint = params.first().copied().unwrap_or(0);
        let mods = params.get(1).map_or(ModKeys::empty(), ModKeys::from);
        let text = params.get(2).copied().unwrap_or(codepoint);

        let (ch, mods) = if text != codepoint && mods.contains(ModKeys::SHIFT) {
          // Kitty reported something like 'Shift+7' and text is '&'
          // So we remove the SHIFT modifier and use the actual text

          (text, mods & !ModKeys::SHIFT)
        } else {
          (codepoint, mods)
        };

        let key = match ch {
          9 => KeyCode::Tab,
          13 => KeyCode::Enter,
          27 => KeyCode::Esc,
          127 => KeyCode::Backspace,
          _ => {
            if let Some(c) = char::from_u32(u32::from(ch)) {
              KeyCode::Char(c)
            } else {
              return;
            }
          }
        };
        TermEvent::Key(KeyEvent(key, mods))
      }
      ([b'?'], 'c') => TermEvent::PrimaryDevAttr,
      ([b'?'], 'u') => TermEvent::KittyKbdFlags,
      // SGR mouse: CSI < button;x;y M/m (ignore mouse events for now)
      ([b'<'], dir @ ('M' | 'm')) => {
        if dir == 'm' {
          return;
        } // release event

        let button = params.first().copied().unwrap_or(0);
        match button {
          64 => TermEvent::Key(KeyEvent(KeyCode::ScrollUp, ModKeys::empty())),
          65 => TermEvent::Key(KeyEvent(KeyCode::ScrollDown, ModKeys::empty())),
          128 => TermEvent::Key(KeyEvent(KeyCode::Back, ModKeys::empty())),
          129 => TermEvent::Key(KeyEvent(KeyCode::Forward, ModKeys::empty())),
          _ => {
            let col = params.get(1).copied().unwrap_or(0) as usize;
            let row = params.get(2).copied().unwrap_or(0) as usize;

            match button {
              0 => TermEvent::Key(KeyEvent(KeyCode::LeftClick(row, col), ModKeys::empty())),
              1 => TermEvent::Key(KeyEvent(KeyCode::MiddleClick(row, col), ModKeys::empty())),
              2 => TermEvent::Key(KeyEvent(KeyCode::RightClick(row, col), ModKeys::empty())),
              35 => TermEvent::Key(KeyEvent(KeyCode::MousePos(row, col), ModKeys::empty())),
              _ => {
                // Other mouse events we don't care about
                return;
              }
            }
          }
        }
      }
      _ => return,
    };
    self.push(event);
  }

  fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
    // SS3 sequences
    if byte == b'O' {
      self.ss3_pending = true;
    }
  }
}

pub(crate) struct PollReader {
  parser: vte::Parser,
  collector: EventParser,
  byte_buf: VecDeque<u8>,
  pending_events: VecDeque<TermEvent>,
  pub verbatim_single: bool,
}

impl Clone for PollReader {
  fn clone(&self) -> Self {
    Self {
      parser: vte::Parser::new(),
      collector: self.collector.clone(),
      byte_buf: self.byte_buf.clone(),
      pending_events: self.pending_events.clone(),
      verbatim_single: self.verbatim_single,
    }
  }
}

impl Debug for PollReader {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    // vte::Parser does not implement Debug
    f.debug_struct("PollReader")
      .field("collector", &self.collector)
      .field("byte_buf", &self.byte_buf)
      .field("verbatim_single", &self.verbatim_single)
      .field("pending_events", &self.pending_events)
      .finish_non_exhaustive()
  }
}

impl PollReader {
  pub(super) fn new() -> Self {
    Self {
      parser: vte::Parser::new(),
      collector: EventParser::new(),
      byte_buf: VecDeque::new(),
      pending_events: VecDeque::new(),
      verbatim_single: false,
    }
  }

  pub(super) fn read_one_verbatim(&mut self) -> Option<KeyEvent> {
    if self.byte_buf.is_empty() {
      return None;
    }
    let bytes: Vec<u8> = self.byte_buf.drain(..).collect();
    let verbatim_str = bytes_to_string(bytes);
    Some(KeyEvent(
      KeyCode::Verbatim(verbatim_str.into()),
      ModKeys::empty(),
    ))
  }

  pub(super) fn feed_bytes(&mut self, bytes: &[u8]) {
    self.byte_buf.extend(bytes);
  }

  pub(super) fn read(&mut self, fd: BorrowedFd) -> ShResult<usize> {
    let mut buffer = [0u8; 1024];
    match read(fd, &mut buffer) {
      Ok(0) => {
        // EOF
        Err(ShErr::loop_break(0))
      }
      Ok(n) => {
        self.feed_bytes(&buffer[..n]);
        Ok(n)
      }
      Err(Errno::EINTR) => {
        // Interrupted, continue to handle signals
        Err(ShErr::loop_continue(0))
      }
      Err(e) => Err(e.into()),
    }
  }

  pub(super) fn readkey(&mut self) -> Option<KeyEvent> {
    match_loop!(self.read_event() => ev, {
      TermEvent::Key(event) => return Some(event),
      TermEvent::FocusGained => {
        log::debug!("Focus gained");
        crate::signal::FOCUS_GAINED.store(true, Ordering::SeqCst);
      }
      _ => {}
    });
    None
  }

  pub(super) fn push_event(&mut self, event: TermEvent) {
    self.pending_events.push_back(event);
  }

  pub(super) fn has_pending(&self) -> bool {
    !self.pending_events.is_empty() || !self.byte_buf.is_empty()
  }

  pub(super) fn read_event(&mut self) -> Option<TermEvent> {
    if let Some(ev) = self.pending_events.pop_front() {
      return Some(ev);
    }
    self.read_event_from_bytes()
  }
  pub(super) fn read_event_from_bytes(&mut self) -> Option<TermEvent> {
    if self.verbatim_single {
      if let Some(key) = self.read_one_verbatim() {
        self.verbatim_single = false;
        return Some(TermEvent::Key(key));
      }
      return None;
    }
    if self.byte_buf.front() == Some(&b'\x1b') {
      if self.byte_buf.len() == 1 {
        // ESC is the only byte - emit standalone Escape
        self.byte_buf.pop_front();
        return Some(TermEvent::Key(KeyEvent(KeyCode::Esc, ModKeys::empty())));
      }
      match self.byte_buf.get(1) {
        Some(b'[' | b'O' | b'P' | b']' | b'_') => {
          // Valid CSI/SS3/DCS/OSC/APC prefix - fall through to the parser below
        }
        Some(&b) if b >= 0x20 && b != 0x7f => {
          // ESC + printable char - interpret as Alt+<char>
          self.byte_buf.pop_front(); // consume ESC
          self.byte_buf.pop_front(); // consume the char
          let ch = b as char;
          return Some(TermEvent::Key(KeyEvent(
            KeyCode::Char(ch.to_ascii_uppercase()),
            ModKeys::ALT,
          )));
        }
        _ => {
          // ESC + non-printable/unknown - emit standalone Escape
          self.byte_buf.pop_front();
          return Some(TermEvent::Key(KeyEvent(KeyCode::Esc, ModKeys::empty())));
        }
      }
    }
    while let Some(byte) = self.byte_buf.pop_front() {
      self.parser.advance(&mut self.collector, &[byte]);
      if let Some(event) = self.collector.pop() {
        return Some(event);
      }
    }
    None
  }
}

impl Default for PollReader {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::keys::{KeyCode, KeyEvent, ModKeys};

  /// Feed bytes through a fresh vte parser into an `EventParser` and
  /// return every event produced.
  fn feed(bytes: &[u8]) -> Vec<TermEvent> {
    let mut parser = vte::Parser::new();
    let mut ep = EventParser::new();
    parser.advance(&mut ep, bytes);
    let mut events = vec![];
    while let Some(e) = ep.pop() {
      events.push(e);
    }
    events
  }

  /// Convenience: expect exactly one Key event with the given code and mods.
  fn expect_key(bytes: &[u8], code: &KeyCode, mods: ModKeys) {
    let events = feed(bytes);
    assert_eq!(
      events.len(),
      1,
      "expected exactly 1 event for {bytes:?}, got {events:?}"
    );
    match &events[0] {
      TermEvent::Key(KeyEvent(c, m)) => {
        assert_eq!(c, code, "key code mismatch for {bytes:?}");
        assert_eq!(m, &mods, "mod mismatch for {bytes:?}");
      }
      other => panic!("expected Key event for {bytes:?}, got {other:?}"),
    }
  }

  // ─── CursorPos: CSI <row>;<col> R ───────────────────────────────────

  #[test]
  fn csi_cursor_pos() {
    let events = feed(b"\x1b[27;1R");
    assert_eq!(events.len(), 1);
    match &events[0] {
      TermEvent::CursorPos(Rows(r), Cols(c)) => {
        assert_eq!(*r, 27);
        assert_eq!(*c, 1);
      }
      other => panic!("expected CursorPos, got {other:?}"),
    }
  }

  #[test]
  fn csi_cursor_pos_missing_params_zeros() {
    let events = feed(b"\x1b[R");
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], TermEvent::CursorPos(Rows(0), Cols(0))));
  }

  // ─── Arrow keys: CSI A/B/C/D ─────────────────────────────────────────

  #[test]
  fn csi_arrow_up_plain() {
    expect_key(b"\x1b[A", &KeyCode::Up, ModKeys::empty());
  }

  #[test]
  fn csi_arrow_down_plain() {
    expect_key(b"\x1b[B", &KeyCode::Down, ModKeys::empty());
  }

  #[test]
  fn csi_arrow_right_plain() {
    expect_key(b"\x1b[C", &KeyCode::Right, ModKeys::empty());
  }

  #[test]
  fn csi_arrow_left_plain() {
    expect_key(b"\x1b[D", &KeyCode::Left, ModKeys::empty());
  }

  #[test]
  fn csi_arrow_up_with_shift() {
    // CSI 1;2 A — mod 2 = Shift
    expect_key(b"\x1b[1;2A", &KeyCode::Up, ModKeys::SHIFT);
  }

  #[test]
  fn csi_arrow_down_with_ctrl() {
    // mod 5 = Ctrl
    expect_key(b"\x1b[1;5B", &KeyCode::Down, ModKeys::CTRL);
  }

  // ─── Home/End: CSI H/F ──────────────────────────────────────────────

  #[test]
  fn csi_home_plain() {
    expect_key(b"\x1b[H", &KeyCode::Home, ModKeys::empty());
  }

  #[test]
  fn csi_end_plain() {
    expect_key(b"\x1b[F", &KeyCode::End, ModKeys::empty());
  }

  #[test]
  fn csi_home_with_mods() {
    expect_key(b"\x1b[1;5H", &KeyCode::Home, ModKeys::CTRL);
  }

  // ─── Shift+Tab: CSI Z ───────────────────────────────────────────────

  #[test]
  fn csi_shift_tab() {
    expect_key(b"\x1b[Z", &KeyCode::Tab, ModKeys::SHIFT);
  }

  // ─── Tilde sequences: CSI <num>~ ────────────────────────────────────

  #[test]
  fn csi_tilde_home_via_1() {
    expect_key(b"\x1b[1~", &KeyCode::Home, ModKeys::empty());
  }

  #[test]
  fn csi_tilde_home_via_7() {
    expect_key(b"\x1b[7~", &KeyCode::Home, ModKeys::empty());
  }

  #[test]
  fn csi_tilde_insert() {
    expect_key(b"\x1b[2~", &KeyCode::Insert, ModKeys::empty());
  }

  #[test]
  fn csi_tilde_delete() {
    expect_key(b"\x1b[3~", &KeyCode::Delete, ModKeys::empty());
  }

  #[test]
  fn csi_tilde_end_via_4() {
    expect_key(b"\x1b[4~", &KeyCode::End, ModKeys::empty());
  }

  #[test]
  fn csi_tilde_end_via_8() {
    expect_key(b"\x1b[8~", &KeyCode::End, ModKeys::empty());
  }

  #[test]
  fn csi_tilde_pageup() {
    expect_key(b"\x1b[5~", &KeyCode::PageUp, ModKeys::empty());
  }

  #[test]
  fn csi_tilde_pagedown() {
    expect_key(b"\x1b[6~", &KeyCode::PageDown, ModKeys::empty());
  }

  #[test]
  fn csi_tilde_f5_through_f12() {
    expect_key(b"\x1b[15~", &KeyCode::F(5), ModKeys::empty());
    expect_key(b"\x1b[17~", &KeyCode::F(6), ModKeys::empty());
    expect_key(b"\x1b[18~", &KeyCode::F(7), ModKeys::empty());
    expect_key(b"\x1b[19~", &KeyCode::F(8), ModKeys::empty());
    expect_key(b"\x1b[20~", &KeyCode::F(9), ModKeys::empty());
    expect_key(b"\x1b[21~", &KeyCode::F(10), ModKeys::empty());
    expect_key(b"\x1b[23~", &KeyCode::F(11), ModKeys::empty());
    expect_key(b"\x1b[24~", &KeyCode::F(12), ModKeys::empty());
  }

  #[test]
  fn csi_tilde_with_mods() {
    expect_key(b"\x1b[5;5~", &KeyCode::PageUp, ModKeys::CTRL);
  }

  #[test]
  fn csi_tilde_unknown_num_produces_no_event() {
    let events = feed(b"\x1b[99~");
    assert!(
      events.is_empty(),
      "unknown tilde num should not produce an event, got {events:?}"
    );
  }

  // ─── Kitty kbd: CSI <code>;<mod>;<text> u ───────────────────────────

  #[test]
  fn csi_kitty_tab() {
    expect_key(b"\x1b[9u", &KeyCode::Tab, ModKeys::empty());
  }

  #[test]
  fn csi_kitty_enter() {
    expect_key(b"\x1b[13u", &KeyCode::Enter, ModKeys::empty());
  }

  #[test]
  fn csi_kitty_esc() {
    expect_key(b"\x1b[27u", &KeyCode::Esc, ModKeys::empty());
  }

  #[test]
  fn csi_kitty_backspace() {
    expect_key(b"\x1b[127u", &KeyCode::Backspace, ModKeys::empty());
  }

  #[test]
  fn csi_kitty_plain_char() {
    // 'a' = 97
    expect_key(b"\x1b[97u", &KeyCode::Char('a'), ModKeys::empty());
  }

  #[test]
  fn csi_kitty_char_with_ctrl() {
    expect_key(b"\x1b[97;5u", &KeyCode::Char('a'), ModKeys::CTRL);
  }

  #[test]
  fn csi_kitty_shift_disambiguation_uses_text() {
    // codepoint=55 ('7'), mod=2 (SHIFT), text=38 ('&').
    // Handler should drop the SHIFT modifier and report the actual char.
    expect_key(b"\x1b[55;2;38u", &KeyCode::Char('&'), ModKeys::empty());
  }

  // ─── Private-mode replies: CSI ? <num> c/u ───────────────────────────

  #[test]
  fn csi_primary_dev_attr() {
    let events = feed(b"\x1b[?1;2c");
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], TermEvent::PrimaryDevAttr));
  }

  #[test]
  fn csi_kitty_kbd_flags() {
    let events = feed(b"\x1b[?5u");
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], TermEvent::KittyKbdFlags));
  }

  // ─── SGR mouse: CSI < <btn>;<x>;<y> M ───────────────────────────────

  #[test]
  fn csi_mouse_release_produces_no_event() {
    // lowercase 'm' = release event, ignored.
    let events = feed(b"\x1b[<0;10;5m");
    assert!(events.is_empty());
  }

  #[test]
  fn csi_mouse_scroll_up() {
    expect_key(b"\x1b[<64;0;0M", &KeyCode::ScrollUp, ModKeys::empty());
  }

  #[test]
  fn csi_mouse_scroll_down() {
    expect_key(b"\x1b[<65;0;0M", &KeyCode::ScrollDown, ModKeys::empty());
  }

  #[test]
  fn csi_mouse_back_button() {
    expect_key(b"\x1b[<128;0;0M", &KeyCode::Back, ModKeys::empty());
  }

  #[test]
  fn csi_mouse_forward_button() {
    expect_key(b"\x1b[<129;0;0M", &KeyCode::Forward, ModKeys::empty());
  }

  #[test]
  fn csi_mouse_left_click_carries_position() {
    expect_key(
      b"\x1b[<0;12;7M",
      &KeyCode::LeftClick(7, 12),
      ModKeys::empty(),
    );
  }

  #[test]
  fn csi_mouse_middle_click() {
    expect_key(
      b"\x1b[<1;3;9M",
      &KeyCode::MiddleClick(9, 3),
      ModKeys::empty(),
    );
  }

  #[test]
  fn csi_mouse_right_click() {
    expect_key(
      b"\x1b[<2;15;20M",
      &KeyCode::RightClick(20, 15),
      ModKeys::empty(),
    );
  }

  #[test]
  fn csi_mouse_position_report() {
    expect_key(b"\x1b[<35;4;2M", &KeyCode::MousePos(2, 4), ModKeys::empty());
  }

  #[test]
  fn csi_mouse_unknown_button_produces_no_event() {
    let events = feed(b"\x1b[<99;0;0M");
    assert!(events.is_empty());
  }

  // ─── Unknown sequence: no event ──────────────────────────────────────

  #[test]
  fn csi_unknown_final_byte_produces_no_event() {
    let events = feed(b"\x1b[?123!");
    assert!(events.is_empty());
  }

  // ===================== XtVersion::parse_iterm2 =====================

  fn parse_iterm2(raw: &str) -> Option<XtVersion> {
    XtVersion::parse_iterm2(raw)
  }

  #[test]
  fn parse_iterm2_full_version() {
    let v = parse_iterm2("iTerm2 3.5.12").unwrap();
    match v {
      XtVersion::Iterm2(sv) => assert_eq!(sv, semver!(3, 5, 12)),
      other => panic!("expected Iterm2, got {other:?}"),
    }
  }

  #[test]
  fn parse_iterm2_zero_version() {
    let v = parse_iterm2("iTerm2 0.0.0").unwrap();
    match v {
      XtVersion::Iterm2(sv) => assert_eq!(sv, semver!(0, 0, 0)),
      other => panic!("got {other:?}"),
    }
  }

  #[test]
  fn parse_iterm2_wrong_program_name() {
    assert!(parse_iterm2("Alacritty 0.13.0").is_none());
    assert!(parse_iterm2("xterm 1.2.3").is_none());
  }

  #[test]
  fn parse_iterm2_no_space_returns_none() {
    assert!(parse_iterm2("iTerm2").is_none());
    assert!(parse_iterm2("nothing").is_none());
    assert!(parse_iterm2("").is_none());
  }

  #[test]
  fn parse_iterm2_missing_patch_returns_none() {
    // "iTerm2 3.5" — third .parse() encounters None.
    assert!(parse_iterm2("iTerm2 3.5").is_none());
  }

  #[test]
  fn parse_iterm2_missing_minor_and_patch_returns_none() {
    assert!(parse_iterm2("iTerm2 3").is_none());
  }

  #[test]
  fn parse_iterm2_non_numeric_components_return_none() {
    assert!(parse_iterm2("iTerm2 a.b.c").is_none());
    assert!(parse_iterm2("iTerm2 3.5.x").is_none());
    assert!(parse_iterm2("iTerm2 3.y.12").is_none());
  }

  #[test]
  fn parse_iterm2_extra_trailing_components_ignored() {
    // parts.next() only takes the first three; anything after is fine.
    let v = parse_iterm2("iTerm2 3.5.12.beta").unwrap();
    match v {
      XtVersion::Iterm2(sv) => assert_eq!(sv, semver!(3, 5, 12)),
      other => panic!("got {other:?}"),
    }
  }

  // ─── via the public XtVersion::parse entry point ────────────────

  #[test]
  fn xtversion_parse_iterm2_route() {
    let v = XtVersion::parse("iTerm2 3.5.12");
    assert!(matches!(v, XtVersion::Iterm2(_)));
  }

  #[test]
  fn xtversion_parse_iterm2_invalid_falls_to_unknown() {
    let v = XtVersion::parse("iTerm2 abc.def.ghi");
    assert!(matches!(v, XtVersion::Unknown(_)));
  }

  // ===================== PollReader::read_event =====================

  fn pr_with_bytes(bytes: &[u8]) -> PollReader {
    let mut r = PollReader::new();
    r.feed_bytes(bytes);
    r
  }

  #[test]
  fn read_event_empty_buf_returns_none() {
    let mut r = PollReader::new();
    let ev = r.read_event();
    assert!(ev.is_none());
  }

  #[test]
  fn read_event_lone_esc_byte_returns_escape_key() {
    let mut r = pr_with_bytes(b"\x1b");
    let ev = r.read_event().unwrap();
    match ev {
      TermEvent::Key(KeyEvent(KeyCode::Esc, m)) => {
        assert_eq!(m, ModKeys::empty());
      }
      other => panic!("expected Esc key, got {other:?}"),
    }
  }

  #[test]
  fn read_event_esc_plus_printable_returns_alt_key() {
    // ESC + 'a' → Alt+A (upper-cased per the implementation).
    let mut r = pr_with_bytes(b"\x1ba");
    let ev = r.read_event().unwrap();
    match ev {
      TermEvent::Key(KeyEvent(KeyCode::Char(c), m)) => {
        assert_eq!(c, 'A');
        assert_eq!(m, ModKeys::ALT);
      }
      other => panic!("expected Alt+char, got {other:?}"),
    }
  }

  #[test]
  fn read_event_esc_plus_unhandled_returns_lone_escape() {
    // ESC + 0x7f (DEL) — falls into the "unknown" arm → standalone Esc.
    let mut r = pr_with_bytes(b"\x1b\x7f");
    let ev = r.read_event().unwrap();
    assert!(matches!(ev, TermEvent::Key(KeyEvent(KeyCode::Esc, _))));
  }

  #[test]
  fn read_event_csi_prefix_parses_through() {
    // ESC + '[' starts a CSI sequence; feed cursor-pos response.
    let mut r = pr_with_bytes(b"\x1b[5;10R");
    let ev = r.read_event().unwrap();
    match ev {
      TermEvent::CursorPos(Rows(r), Cols(c)) => {
        assert_eq!(r, 5);
        assert_eq!(c, 10);
      }
      other => panic!("expected CursorPos, got {other:?}"),
    }
  }

  #[test]
  fn read_event_plain_printable_char_returns_key() {
    let mut r = pr_with_bytes(b"x");
    let ev = r.read_event().unwrap();
    match ev {
      TermEvent::Key(KeyEvent(KeyCode::Char(c), m)) => {
        assert_eq!(c, 'x');
        assert_eq!(m, ModKeys::empty());
      }
      other => panic!("expected key 'x', got {other:?}"),
    }
  }

  #[test]
  fn read_event_consumes_bytes() {
    let mut r = pr_with_bytes(b"ab");
    let _ = r.read_event().unwrap();
    // After consuming 'a', the buf still has 'b'.
    let ev = r.read_event().unwrap();
    match ev {
      TermEvent::Key(KeyEvent(KeyCode::Char(c), _)) => {
        assert_eq!(c, 'b');
      }
      other => panic!("expected 'b', got {other:?}"),
    }
  }

  // ===================== EventParser::print =====================
  // Indirectly drive `print` via the feed() helper. SS3 sequences are
  // ESC O <char>.

  #[test]
  fn print_plain_char_emits_key() {
    expect_key(b"a", &KeyCode::Char('a'), ModKeys::empty());
  }

  #[test]
  fn print_del_byte_emits_backspace() {
    expect_key(b"\x7f", &KeyCode::Backspace, ModKeys::empty());
  }

  #[test]
  fn print_ss3_capital_a_is_up() {
    expect_key(b"\x1bOA", &KeyCode::Up, ModKeys::empty());
  }

  #[test]
  fn print_ss3_capital_b_is_down() {
    expect_key(b"\x1bOB", &KeyCode::Down, ModKeys::empty());
  }

  #[test]
  fn print_ss3_capital_c_is_right() {
    expect_key(b"\x1bOC", &KeyCode::Right, ModKeys::empty());
  }

  #[test]
  fn print_ss3_capital_d_is_left() {
    expect_key(b"\x1bOD", &KeyCode::Left, ModKeys::empty());
  }

  #[test]
  fn print_ss3_capital_h_is_home() {
    expect_key(b"\x1bOH", &KeyCode::Home, ModKeys::empty());
  }

  #[test]
  fn print_ss3_capital_f_is_end() {
    expect_key(b"\x1bOF", &KeyCode::End, ModKeys::empty());
  }

  #[test]
  fn print_ss3_p_q_r_s_are_f1_f4() {
    expect_key(b"\x1bOP", &KeyCode::F(1), ModKeys::empty());
    expect_key(b"\x1bOQ", &KeyCode::F(2), ModKeys::empty());
    expect_key(b"\x1bOR", &KeyCode::F(3), ModKeys::empty());
    expect_key(b"\x1bOS", &KeyCode::F(4), ModKeys::empty());
  }

  #[test]
  fn print_ss3_unrecognized_char_falls_through() {
    // ESC O Z — Z isn't in the SS3 table; falls through to the Char path.
    let events = feed(b"\x1bOZ");
    // The 'Z' should appear as a Char event somewhere.
    let has_z = events
      .iter()
      .any(|e| matches!(e, TermEvent::Key(KeyEvent(KeyCode::Char('Z'), _))));
    assert!(has_z, "expected Char('Z') event, got: {events:?}");
  }
}
