use std::{
  collections::VecDeque,
  env,
  fmt::{Debug, Write},
  io::{BufRead, BufReader, Read},
  os::fd::{AsFd, BorrowedFd, RawFd},
  time::Instant,
};

use nix::{
  errno::Errno,
  libc::{self},
  poll::{self, PollFlags, PollTimeout},
  unistd::isatty,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vte::{Parser, Perform};

pub use crate::libsh::guards::{RawModeGuard, raw_mode};
use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult}, procio::borrow_fd, readline::keys::{KeyCode, ModKeys}, state::read_shopts
};
use crate::{
  sherr,
  state::{read_meta, write_meta},
};

use super::keys::KeyEvent;

pub type Row = u16;
pub type Col = u16;

#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Pos {
  pub col: Col,
  pub row: Row,
}

// I'd like to thank rustyline for this idea
nix::ioctl_read_bad!(win_size, libc::TIOCGWINSZ, libc::winsize);

pub fn get_win_size(fd: RawFd) -> (Col, Row) {
  use std::mem::zeroed;

  if cfg!(test) {
    return (80, 24);
  }

  unsafe {
    let mut size: libc::winsize = zeroed();
    match win_size(fd, &mut size) {
      Ok(0) => {
        /* rustyline code says:
         In linux pseudo-terminals are created with dimensions of
         zero. If host application didn't initialize the correct
         size before start we treat zero size as 80 columns and
         infinite rows
        */
        let cols = if size.ws_col == 0 { 80 } else { size.ws_col };
        let rows = if size.ws_row == 0 {
          u16::MAX
        } else {
          size.ws_row
        };
        (cols, rows)
      }
      _ => (80, 24),
    }
  }
}

pub fn enumerate_lines(
  s: &str,
  left_pad: usize,
  show_numbers: bool,
  offset: usize,
  _total_buf_lines: usize,
) -> String {
  let lines: Vec<&str> = s.split('\n').collect();
  let visible_count = lines.len();
  let max_num_len = (offset + visible_count).to_string().len();
  let mut first = true;
  log::debug!(
    "left_pad: {left_pad}, offset: {offset}, visible_count: {visible_count}, max_num_len: {max_num_len}"
  );
  lines
    .into_iter()
    .enumerate()
    .fold(String::new(), |mut acc, (i, ln)| {
      if first {
        first = false;
      } else {
        acc.push('\n');
      }
      if i == 0 && left_pad > 0 {
        acc.push_str(ln);
      } else {
        let num = (i + offset + 1).to_string();
        let num_pad = max_num_len - num.len();
        // " 2 | " - num + padding + " | "
        let prefix_len = max_num_len + 3; // "N | "
        let trail_pad = left_pad.saturating_sub(prefix_len);
        let prefix = if show_numbers {
          format!("\x1b[0m\x1b[90m{}{num} |\x1b[0m ", " ".repeat(num_pad))
        } else {
          " ".repeat(prefix_len + 1).to_string()
        };
        write!(acc, "{prefix}{}{ln}", " ".repeat(trail_pad)).unwrap();
      }
      acc
    })
}

fn write_all(fd: RawFd, buf: &str) -> nix::Result<()> {
  let mut bytes = buf.as_bytes();
  while !bytes.is_empty() {
    match nix::unistd::write(borrow_fd(fd), bytes) {
      Ok(0) => return Err(Errno::EIO),
      Ok(n) => bytes = &bytes[n..],
      Err(Errno::EINTR) => {}
      Err(r) => return Err(r),
    }
  }
  Ok(())
}

/// Check if a string ends with a newline, ignoring any trailing ANSI escape
/// sequences.
fn ends_with_newline(s: &str) -> bool {
  let bytes = s.as_bytes();
  let mut i = bytes.len();
  while i > 0 {
    // ANSI CSI sequences end with an alphabetic byte (e.g. \x1b[0m)
    if bytes[i - 1].is_ascii_alphabetic() {
      let term = i - 1;
      let mut j = term;
      // Walk back past parameter bytes (digits and ';')
      while j > 0 && (bytes[j - 1].is_ascii_digit() || bytes[j - 1] == b';') {
        j -= 1;
      }
      // Check for CSI introducer \x1b[
      if j >= 2 && bytes[j - 1] == b'[' && bytes[j - 2] == 0x1b {
        i = j - 2;
        continue;
      }
    }
    break;
  }
  i > 0 && bytes[i - 1] == b'\n'
}

pub fn calc_str_width(s: &str) -> u16 {
  let mut esc_seq = 0;
  s.graphemes(true).map(|g| width(g, &mut esc_seq)).sum()
}

// Big credit to rustyline for this
fn width(s: &str, esc_seq: &mut u8) -> u16 {
  if *esc_seq == 1 {
    if s == "[" {
      // CSI
      *esc_seq = 2;
    } else {
      // two-character sequence
      *esc_seq = 0;
    }
    0
  } else if *esc_seq == 2 {
    if s == ";" || (s.as_bytes()[0] >= b'0' && s.as_bytes()[0] <= b'9') {
      /*} else if s == "m" {
      // last
       *esc_seq = 0;*/
    } else {
      // not supported
      *esc_seq = 0;
    }

    0
  } else if s == "\x1b" {
    *esc_seq = 1;
    0
  } else if s == "\n" {
    0
  } else {
    get_width_calculator().width(s) as u16
  }
}

pub fn width_calculator() -> Box<dyn WidthCalculator> {
  match env::var("TERM_PROGRAM").as_deref() {
    Ok("Apple_Terminal") => Box::new(UnicodeWidth),
    Ok("iTerm.app") => Box::new(UnicodeWidth),
    Ok("WezTerm") => Box::new(UnicodeWidth),
    Err(std::env::VarError::NotPresent) => match std::env::var("TERM").as_deref() {
      Ok("xterm-kitty") => Box::new(NoZwj),
      _ => Box::new(WcWidth),
    },
    _ => Box::new(WcWidth),
  }
}

fn read_digits_until(rdr: &mut TermReader, sep: char) -> ShResult<Option<u32>> {
  let mut num: u32 = 0;
  loop {
    match rdr.next_byte()? as char {
      digit @ '0'..='9' => {
        let digit = digit.to_digit(10).unwrap();
        num = append_digit(num, digit);
        continue;
      }
      c if c == sep => break,
      _ => return Ok(None),
    }
  }
  Ok(Some(num))
}

pub fn append_digit(left: u32, right: u32) -> u32 {
  left.saturating_mul(10).saturating_add(right)
}

pub trait WidthCalculator: Send + Sync {
  fn width(&self, text: &str) -> usize;
}

static WIDTH_CALC: std::sync::OnceLock<Box<dyn WidthCalculator>> = std::sync::OnceLock::new();

pub fn get_width_calculator() -> &'static dyn WidthCalculator {
  WIDTH_CALC.get_or_init(width_calculator).as_ref()
}

pub trait KeyReader {
  fn read_key(&mut self) -> Result<Option<KeyEvent>, ShErr>;
}

pub trait LineWriter {
  fn clear_rows(&mut self, layout: &Layout) -> ShResult<()>;
  fn clear_screen(&mut self) -> ShResult<()>;
  fn move_cursor_to_end(&mut self, _layout: &Layout) -> ShResult<()> {
    Ok(())
  }
  fn redraw(
    &mut self,
    prompt: &str,
    line: &str,
    new_layout: &Layout,
    offset: usize,
    total_buf_lines: usize,
  ) -> ShResult<()>;
  fn flush_write(&mut self, buf: &str) -> ShResult<()>;
  fn send_bell(&mut self) -> ShResult<()>;
}

#[derive(Clone, Copy, Debug)]
pub struct UnicodeWidth;

impl WidthCalculator for UnicodeWidth {
  fn width(&self, text: &str) -> usize {
    text.width()
  }
}

#[derive(Clone, Copy, Debug)]
pub struct WcWidth;

impl WcWidth {
  pub fn cwidth(&self, ch: char) -> usize {
    ch.width().unwrap()
  }
}

impl WidthCalculator for WcWidth {
  fn width(&self, text: &str) -> usize {
    let mut width = 0;
    for ch in text.chars() {
      width += self.cwidth(ch)
    }
    width
  }
}

const ZWJ: char = '\u{200D}';
#[derive(Clone, Copy, Debug)]
pub struct NoZwj;

impl WidthCalculator for NoZwj {
  fn width(&self, text: &str) -> usize {
    if text.contains(ZWJ) {
      // ZWJ sequence renders as a single glyph on supported terminals
      2
    } else {
      UnicodeWidth.width(text)
    }
  }
}

pub struct TermBuffer {
  tty: RawFd,
}

impl TermBuffer {
  pub fn new(tty: RawFd) -> Self {
    assert!(isatty(tty).is_ok_and(|r| r));
    Self { tty }
  }
}

impl Read for TermBuffer {
  fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    assert!(isatty(self.tty).is_ok_and(|r| r));
    let result = nix::unistd::read(self.tty, buf);
    match result {
      Ok(n) => Ok(n),
      Err(Errno::EINTR) => Err(Errno::EINTR.into()),
      Err(e) => Err(std::io::Error::from_raw_os_error(e as i32)),
    }
  }
}

// ============================================================================
// PollReader - non-blocking key reader using vte parser
// ============================================================================

struct KeyCollector {
  events: VecDeque<KeyEvent>,
  ss3_pending: bool,
}

impl KeyCollector {
  fn new() -> Self {
    Self {
      events: VecDeque::new(),
      ss3_pending: false,
    }
  }

  fn push(&mut self, event: KeyEvent) {
    self.events.push_back(event);
  }

  fn pop(&mut self) -> Option<KeyEvent> {
    self.events.pop_front()
  }

  /// Parse modifier bits from CSI parameter (e.g., 1;5A means Ctrl+Up)
  fn parse_modifiers(param: u16) -> ModKeys {
    // CSI modifiers: param = 1 + (shift) + (alt*2) + (ctrl*4) + (meta*8)
    let bits = param.saturating_sub(1);
    let mut mods = ModKeys::empty();
    if bits & 1 != 0 {
      mods |= ModKeys::SHIFT;
    }
    if bits & 2 != 0 {
      mods |= ModKeys::ALT;
    }
    if bits & 4 != 0 {
      mods |= ModKeys::CTRL;
    }
    mods
  }
}

impl Default for KeyCollector {
  fn default() -> Self {
    Self::new()
  }
}

impl Perform for KeyCollector {
  fn print(&mut self, c: char) {
    // vte routes 0x7f (DEL) to print instead of execute
    if self.ss3_pending {
      self.ss3_pending = false;
      match c {
        'A' => {
          self.push(KeyEvent(KeyCode::Up, ModKeys::empty()));
          return;
        }
        'B' => {
          self.push(KeyEvent(KeyCode::Down, ModKeys::empty()));
          return;
        }
        'C' => {
          self.push(KeyEvent(KeyCode::Right, ModKeys::empty()));
          return;
        }
        'D' => {
          self.push(KeyEvent(KeyCode::Left, ModKeys::empty()));
          return;
        }
        'H' => {
          self.push(KeyEvent(KeyCode::Home, ModKeys::empty()));
          return;
        }
        'F' => {
          self.push(KeyEvent(KeyCode::End, ModKeys::empty()));
          return;
        }
        'P' => {
          self.push(KeyEvent(KeyCode::F(1), ModKeys::empty()));
          return;
        }
        'Q' => {
          self.push(KeyEvent(KeyCode::F(2), ModKeys::empty()));
          return;
        }
        'R' => {
          self.push(KeyEvent(KeyCode::F(3), ModKeys::empty()));
          return;
        }
        'S' => {
          self.push(KeyEvent(KeyCode::F(4), ModKeys::empty()));
          return;
        }
        _ => {}
      }
    }

    if c == '\x7f' {
      self.push(KeyEvent(KeyCode::Backspace, ModKeys::empty()));
    } else {
      self.push(KeyEvent(KeyCode::Char(c), ModKeys::empty()));
    }
  }

  fn execute(&mut self, byte: u8) {
    log::trace!("execute: {byte:#04x}");
    let event = match byte {
      0x00 => KeyEvent(KeyCode::Char(' '), ModKeys::CTRL), // Ctrl+Space / Ctrl+@
      0x09 => KeyEvent(KeyCode::Tab, ModKeys::empty()),    // Tab (Ctrl+I)
      0x0a => KeyEvent(KeyCode::Char('j'), ModKeys::CTRL), // Ctrl+J (linefeed)
      0x0d => KeyEvent(KeyCode::Enter, ModKeys::empty()),  // Carriage return (Ctrl+M)
      0x1b => KeyEvent(KeyCode::Esc, ModKeys::empty()),
      0x7f => KeyEvent(KeyCode::Backspace, ModKeys::empty()),
      0x01..=0x1a => {
        // Ctrl+A through Ctrl+Z (excluding special cases above)
        let c = (b'A' + byte - 1) as char;
        KeyEvent(KeyCode::Char(c), ModKeys::CTRL)
      }
      _ => return,
    };
    self.push(event);
  }

  fn csi_dispatch(
    &mut self,
    params: &vte::Params,
    intermediates: &[u8],
    _ignore: bool,
    action: char,
  ) {
    log::trace!(
      "CSI dispatch: params={params:?}, intermediates={intermediates:?}, action={action:?}"
    );
    let params: Vec<u16> = params
      .iter()
      .map(|p| p.first().copied().unwrap_or(0))
      .collect();

    let event = match (intermediates, action) {
      // Arrow keys: CSI A/B/C/D or CSI 1;mod A/B/C/D
      ([], 'A') => {
        let mods = params
          .get(1)
          .map(|&m| Self::parse_modifiers(m))
          .unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Up, mods)
      }
      ([], 'B') => {
        let mods = params
          .get(1)
          .map(|&m| Self::parse_modifiers(m))
          .unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Down, mods)
      }
      ([], 'C') => {
        let mods = params
          .get(1)
          .map(|&m| Self::parse_modifiers(m))
          .unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Right, mods)
      }
      ([], 'D') => {
        let mods = params
          .get(1)
          .map(|&m| Self::parse_modifiers(m))
          .unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Left, mods)
      }
      // Home/End: CSI H/F or CSI 1;mod H/F
      ([], 'H') => {
        let mods = params
          .get(1)
          .map(|&m| Self::parse_modifiers(m))
          .unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Home, mods)
      }
      ([], 'F') => {
        let mods = params
          .get(1)
          .map(|&m| Self::parse_modifiers(m))
          .unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::End, mods)
      }
      // Shift+Tab: CSI Z
      ([], 'Z') => KeyEvent(KeyCode::Tab, ModKeys::SHIFT),
      // Special keys with tilde: CSI num ~ or CSI num;mod ~
      ([], '~') => {
        let key_num = params.first().copied().unwrap_or(0);
        let mods = params
          .get(1)
          .map(|&m| Self::parse_modifiers(m))
          .unwrap_or(ModKeys::empty());
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
          200 => KeyCode::BracketedPasteStart,
          201 => KeyCode::BracketedPasteEnd,
          _ => return,
        };
        KeyEvent(key, mods)
      }
      ([], 'u') => {
        let codepoint = params.first().copied().unwrap_or(0);
        let mods = params
          .get(1)
          .map(|&m| Self::parse_modifiers(m))
          .unwrap_or(ModKeys::empty());
        let key = match codepoint {
          9 => KeyCode::Tab,
          13 => KeyCode::Enter,
          27 => KeyCode::Esc,
          127 => KeyCode::Backspace,
          _ => {
            if let Some(ch) = char::from_u32(codepoint as u32) {
              KeyCode::Char(ch)
            } else {
              return;
            }
          }
        };
        KeyEvent(key, mods)
      }
      // SGR mouse: CSI < button;x;y M/m (ignore mouse events for now)
      ([b'<'], 'M') | ([b'<'], 'm') => {
        return;
      }
      _ => return,
    };
    self.push(event);
  }

  fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
    log::trace!("ESC dispatch: intermediates={intermediates:?}, byte={byte:#04x}");
    // SS3 sequences
    if byte == b'O' {
      self.ss3_pending = true;
    }
  }
}

pub struct PollReader {
  parser: Parser,
  collector: KeyCollector,
  byte_buf: VecDeque<u8>,
  pub verbatim_single: bool,
  pub verbatim: bool,
}

impl PollReader {
  pub fn new() -> Self {
    Self {
      parser: Parser::new(),
      collector: KeyCollector::new(),
      byte_buf: VecDeque::new(),
      verbatim_single: false,
      verbatim: false,
    }
  }

  pub fn handle_bracket_paste(&mut self) -> Option<KeyEvent> {
    let end_marker = b"\x1b[201~";
    let mut raw = vec![];
    while let Some(byte) = self.byte_buf.pop_front() {
      raw.push(byte);
      if raw.ends_with(end_marker) {
        // Strip the end marker from the raw sequence
        raw.truncate(raw.len() - end_marker.len());
        let paste = String::from_utf8_lossy(&raw).to_string();
        self.verbatim = false;
        return Some(KeyEvent(KeyCode::Verbatim(paste.into()), ModKeys::empty()));
      }
    }

    self.verbatim = true;
    self.byte_buf.extend(raw);
    None
  }

  pub fn read_one_verbatim(&mut self) -> Option<KeyEvent> {
    if self.byte_buf.is_empty() {
      return None;
    }
    let bytes: Vec<u8> = self.byte_buf.drain(..).collect();
    let verbatim_str = String::from_utf8_lossy(&bytes).to_string();
    Some(KeyEvent(
      KeyCode::Verbatim(verbatim_str.into()),
      ModKeys::empty(),
    ))
  }

  pub fn feed_bytes(&mut self, bytes: &[u8]) {
    self.byte_buf.extend(bytes);
  }
}

impl Default for PollReader {
  fn default() -> Self {
    Self::new()
  }
}

impl KeyReader for PollReader {
  fn read_key(&mut self) -> Result<Option<KeyEvent>, ShErr> {
    if self.verbatim_single {
      if let Some(key) = self.read_one_verbatim() {
        self.verbatim_single = false;
        return Ok(Some(key));
      }
      return Ok(None);
    }
    if self.verbatim {
      if let Some(paste) = self.handle_bracket_paste() {
        return Ok(Some(paste));
      }
      // If we're in verbatim mode but haven't seen the end marker yet, don't attempt to parse keys
      return Ok(None);
    } else if self.byte_buf.front() == Some(&b'\x1b') {
      if self.byte_buf.len() == 1 {
        // ESC is the only byte - emit standalone Escape
        self.byte_buf.pop_front();
        return Ok(Some(KeyEvent(KeyCode::Esc, ModKeys::empty())));
      }
      match self.byte_buf.get(1) {
        Some(b'[') | Some(b'O') => {
          // Valid CSI/SS3 prefix - fall through to the parser below
        }
        Some(&b) if b >= 0x20 && b != 0x7f => {
          // ESC + printable char - interpret as Alt+<char>
          self.byte_buf.pop_front(); // consume ESC
          self.byte_buf.pop_front(); // consume the char
          let ch = b as char;
          return Ok(Some(KeyEvent(
            KeyCode::Char(ch.to_ascii_uppercase()),
            ModKeys::ALT,
          )));
        }
        _ => {
          // ESC + non-printable/unknown - emit standalone Escape
          self.byte_buf.pop_front();
          return Ok(Some(KeyEvent(KeyCode::Esc, ModKeys::empty())));
        }
      }
    }
    while let Some(byte) = self.byte_buf.pop_front() {
      self.parser.advance(&mut self.collector, &[byte]);
      if let Some(key) = self.collector.pop() {
        match key {
          KeyEvent(KeyCode::BracketedPasteStart, _) => {
            if let Some(paste) = self.handle_bracket_paste() {
              return Ok(Some(paste));
            } else {
              continue;
            }
          }
          _ => return Ok(Some(key)),
        }
      }
    }
    Ok(None)
  }
}

// ============================================================================
// TermReader - blocking key reader (original implementation)
// ============================================================================

pub struct TermReader {
  buffer: BufReader<TermBuffer>,
}

impl TermReader {
  pub fn new(tty: RawFd) -> Self {
    Self {
      buffer: BufReader::new(TermBuffer::new(tty)),
    }
  }

  /// Execute some logic in raw mode
  ///
  /// Saves the termios before running the given function.
  /// If the given function panics, the panic will halt momentarily to restore
  /// the termios
  pub fn poll(&mut self, timeout: PollTimeout) -> ShResult<bool> {
    if !self.buffer.buffer().is_empty() {
      return Ok(true);
    }

    let mut fds = [poll::PollFd::new(self.as_fd(), PollFlags::POLLIN)];
    let r = poll::poll(&mut fds, timeout);
    match r {
      Ok(n) => Ok(n != 0),
      Err(Errno::EINTR) => Ok(false),
      Err(e) => Err(e.into()),
    }
  }

  pub fn next_byte(&mut self) -> std::io::Result<u8> {
    let mut buf = [0u8];
    let _n = self.buffer.read(&mut buf)?;
    Ok(buf[0])
  }

  pub fn peek_byte(&mut self) -> std::io::Result<u8> {
    let buf = self.buffer.fill_buf()?;
    if buf.is_empty() {
      Err(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "EOF",
      ))
    } else {
      Ok(buf[0])
    }
  }

  pub fn consume_byte(&mut self) {
    self.buffer.consume(1);
  }

  pub fn parse_esc_seq(&mut self) -> ShResult<KeyEvent> {
    let mut seq = vec![0x1b];

    let b1 = self.peek_byte()?;
    self.consume_byte();
    seq.push(b1);

    match b1 {
      b'[' => {
        let b2 = self.peek_byte()?;
        self.consume_byte();
        seq.push(b2);

        match b2 {
          b'A' => Ok(KeyEvent(KeyCode::Up, ModKeys::empty())),
          b'B' => Ok(KeyEvent(KeyCode::Down, ModKeys::empty())),
          b'C' => Ok(KeyEvent(KeyCode::Right, ModKeys::empty())),
          b'D' => Ok(KeyEvent(KeyCode::Left, ModKeys::empty())),
          b'1'..=b'9' => {
            let mut digits = vec![b2];

            loop {
              let b = self.peek_byte()?;
              seq.push(b);
              self.consume_byte();

              if b == b'~' || b == b';' {
                break;
              } else if b.is_ascii_digit() {
                digits.push(b);
              } else {
                break;
              }
            }

            let key = match digits.as_slice() {
              [b'1'] => KeyCode::Home,
              [b'3'] => KeyCode::Delete,
              [b'4'] => KeyCode::End,
              [b'5'] => KeyCode::PageUp,
              [b'6'] => KeyCode::PageDown,
              [b'7'] => KeyCode::Home, // xterm alternate
              [b'8'] => KeyCode::End,  // xterm alternate

              [b'1', b'5'] => KeyCode::F(5),
              [b'1', b'7'] => KeyCode::F(6),
              [b'1', b'8'] => KeyCode::F(7),
              [b'1', b'9'] => KeyCode::F(8),
              [b'2', b'0'] => KeyCode::F(9),
              [b'2', b'1'] => KeyCode::F(10),
              [b'2', b'3'] => KeyCode::F(11),
              [b'2', b'4'] => KeyCode::F(12),
              _ => KeyCode::Esc,
            };

            Ok(KeyEvent(key, ModKeys::empty()))
          }
          _ => Ok(KeyEvent(KeyCode::Esc, ModKeys::empty())),
        }
      }
      b'O' => {
        let b2 = self.peek_byte()?;
        self.consume_byte();
        seq.push(b2);

        let key = match b2 {
          b'P' => KeyCode::F(1),
          b'Q' => KeyCode::F(2),
          b'R' => KeyCode::F(3),
          b'S' => KeyCode::F(4),
          _ => KeyCode::Esc,
        };

        Ok(KeyEvent(key, ModKeys::empty()))
      }
      _ => Ok(KeyEvent(KeyCode::Esc, ModKeys::empty())),
    }
  }
}

impl KeyReader for TermReader {
  fn read_key(&mut self) -> Result<Option<KeyEvent>, ShErr> {
    use core::str;

    let mut collected = Vec::with_capacity(4);

    loop {
      let byte = self.next_byte()?;
      collected.push(byte);

      // If it's an escape seq, delegate to ESC sequence handler
      if collected[0] == 0x1b && collected.len() == 1 && self.poll(PollTimeout::ZERO)? {
        return self.parse_esc_seq().map(Some);
      }

      // Try parse as valid UTF-8
      if let Ok(s) = str::from_utf8(&collected) {
        return Ok(Some(KeyEvent::new(s, ModKeys::empty())));
      }

      // UTF-8 max 4 bytes - if it’s invalid at this point, bail
      if collected.len() >= 4 {
        break;
      }
    }

    Ok(None)
  }
}

impl AsFd for TermReader {
  fn as_fd(&self) -> BorrowedFd<'_> {
    let fd = self.buffer.get_ref().tty;
    borrow_fd(fd)
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
  pub prompt_end: Pos,
  pub cursor: Pos,
  pub end: Pos,
  pub psr_end: Option<Pos>,
  pub t_cols: u16,
}

impl Layout {
  pub fn new() -> Self {
    Self {
      prompt_end: Pos::default(),
      cursor: Pos::default(),
      end: Pos::default(),
      psr_end: None,
      t_cols: 0,
    }
  }
  pub fn from_parts(
		term_width: u16,
		prompt: &str,
		to_cursor: &str,
		to_end: &str,
	) -> Self {
    let prompt_end = Self::calc_pos(term_width, prompt, Pos { col: 0, row: 0 }, 0, false);
    let cursor = Self::calc_pos(term_width, to_cursor, prompt_end, prompt_end.col, true);
    let end = Self::calc_pos(term_width, to_end, prompt_end, prompt_end.col, false);
    Layout {
      prompt_end,
      cursor,
      end,
      psr_end: None,
      t_cols: term_width,
    }
  }

  fn is_ctl_char(gr: &str) -> bool {
    !gr.is_empty() && gr.as_bytes()[0] <= 0x1F && gr != "\n" && gr != "\t" && gr != "\r"
  }

  pub fn calc_pos(term_width: u16, s: &str, orig: Pos, left_margin: u16, raw_calc: bool) -> Pos {
    const TAB_STOP: u16 = 8;
    let mut pos = orig;
    let mut esc_seq = 0;
    for c in s.graphemes(true) {
      if c == "\n" {
        pos.row += 1;
        pos.col = left_margin;
      }
      let c_width = if c == "\t" {
        TAB_STOP - (pos.col % TAB_STOP)
      } else if raw_calc && Self::is_ctl_char(c) {
        2
      } else {
        width(c, &mut esc_seq)
      };
      pos.col += c_width;
      if pos.col > term_width {
        pos.row += 1;
        pos.col = c_width;
      }
    }
    if pos.col >= term_width {
      pos.row += 1;
      pos.col = 0;
    }

    pos
  }
}

impl Default for Layout {
  fn default() -> Self {
    Self::new()
  }
}

#[derive(Clone, Debug, Default)]
pub struct TermWriter {
  last_bell: Option<Instant>,
  out: RawFd,
  pub t_cols: Col, // terminal width
  buffer: String,
}

impl TermWriter {
  pub fn new(out: RawFd) -> Self {
    let (t_cols, _) = get_win_size(out);
    Self {
      last_bell: None,
      out,
      t_cols,
      buffer: String::new(),
    }
  }
  pub fn get_cursor_movement(&self, old: Pos, new: Pos) -> ShResult<String> {
    let mut buffer = String::new();
    let err = |_| {
      ShErr::simple(
        ShErrKind::InternalErr,
        "Failed to write to cursor movement buffer",
      )
    };

    match new.row.cmp(&old.row) {
      std::cmp::Ordering::Greater => {
        let shift = new.row - old.row;
        match shift {
          1 => buffer.push_str("\x1b[B"),
          _ => write!(buffer, "\x1b[{shift}B").map_err(err)?,
        }
      }
      std::cmp::Ordering::Less => {
        let shift = old.row - new.row;
        match shift {
          1 => buffer.push_str("\x1b[A"),
          _ => write!(buffer, "\x1b[{shift}A").map_err(err)?,
        }
      }
      std::cmp::Ordering::Equal => { /* Do nothing */ }
    }

    match new.col.cmp(&old.col) {
      std::cmp::Ordering::Greater => {
        let shift = new.col - old.col;
        match shift {
          1 => buffer.push_str("\x1b[C"),
          _ => write!(buffer, "\x1b[{shift}C").map_err(err)?,
        }
      }
      std::cmp::Ordering::Less => {
        let shift = old.col - new.col;
        match shift {
          1 => buffer.push_str("\x1b[D"),
          _ => write!(buffer, "\x1b[{shift}D").map_err(err)?,
        }
      }
      std::cmp::Ordering::Equal => { /* Do nothing */ }
    }
    Ok(buffer)
  }
  pub fn move_cursor(&mut self, old: Pos, new: Pos) -> ShResult<()> {
    self.buffer.clear();
    let movement = self.get_cursor_movement(old, new)?;

    write_all(self.out, &movement)?;
    Ok(())
  }

  pub fn update_t_cols(&mut self) {
    let (t_cols, _) = get_win_size(self.out);
    self.t_cols = t_cols;
  }

  /// Called before the prompt is drawn. If we are not on column 1, push a vid-inverted '%' and then a '\n\r'.
  ///
  /// Aping zsh with this but it's a nice feature.
  pub fn fix_cursor_column(&mut self, rdr: &mut TermReader) -> ShResult<()> {
    let Some((_, c)) = self.get_cursor_pos(rdr)? else {
      return Ok(());
    };

    if c != 1 {
      self.flush_write("\x1b[7m%\x1b[0m\n\r")?;
    }
    Ok(())
  }

  pub fn get_cursor_pos(&mut self, rdr: &mut TermReader) -> ShResult<Option<(usize, usize)>> {
    // Ping the cursor's position
    self.flush_write("\x1b[6n")?;

    if !rdr.poll(PollTimeout::from(255u8))? {
      return Ok(None);
    }

    if rdr.next_byte()? as char != '\x1b' {
      return Ok(None);
    }

    if rdr.next_byte()? as char != '[' {
      return Ok(None);
    }

    let row = read_digits_until(rdr, ';')?;

    let col = read_digits_until(rdr, 'R')?;
    let pos = if let Some(row) = row
      && let Some(col) = col
    {
      Some((row as usize, col as usize))
    } else {
      None
    };

    Ok(pos)
  }

  pub fn move_cursor_at_leftmost(
    &mut self,
    rdr: &mut TermReader,
    use_newline: bool,
  ) -> ShResult<()> {
    let result = rdr.poll(PollTimeout::ZERO)?;
    if result {
      // The terminals reply is going to be stuck behind the currently buffered output
      // So let's get out of here
      return Ok(());
    }

    // Ping the cursor's position
    self.flush_write("\x1b[6n\n")?;

    if !rdr.poll(PollTimeout::from(255u8))? {
      return Ok(());
    }

    if rdr.next_byte()? as char != '\x1b' {
      return Ok(());
    }

    if rdr.next_byte()? as char != '[' {
      return Ok(());
    }

    if read_digits_until(rdr, ';')?.is_none() {
      return Ok(());
    }

    // We just consumed everything up to the column number, so let's get that now
    let col = read_digits_until(rdr, 'R')?;

    // The cursor is not at the leftmost, so let's fix that
    if col != Some(1) {
      if use_newline {
        // We use '\n' instead of '\r' sometimes because if there's a bunch of garbage
        // on this line, It might pollute the prompt/line buffer if those are
        // shorter than said garbage
        self.flush_write("\n")?;
      } else {
        // Sometimes though, we know that there's nothing to the right of the cursor
        // after moving So we just move to the left.
        self.flush_write("\r")?;
      }
    }

    Ok(())
  }
}

impl LineWriter for TermWriter {
  fn clear_rows(&mut self, layout: &Layout) -> ShResult<()> {
    self.buffer.clear();
    // Account for lines that may have wrapped due to terminal resize.
    // If a PSR was drawn, the last row extended to the old terminal width.
    // When the terminal shrinks, that row wraps into extra physical rows.
    let mut rows_to_clear = layout.end.row;
    if layout.psr_end.is_some() && layout.t_cols > self.t_cols && self.t_cols > 0 {
      let extra = (layout.t_cols.saturating_sub(1)) / self.t_cols;
      rows_to_clear += extra;
    }
    let cursor_row = layout.cursor.row;

    let cursor_motion = rows_to_clear.saturating_sub(cursor_row);
    if cursor_motion > 0 {
      write!(self.buffer, "\x1b[{cursor_motion}B").unwrap()
    }

    log::debug!(
      "rows to clear: {rows_to_clear}, cursor row: {cursor_row}, cursor motion: {cursor_motion}"
    );

    for _ in 0..rows_to_clear {
      self.buffer.push_str("\x1b[2K\x1b[A");
    }
    self.buffer.push_str("\x1b[2K\r"); // Clear line and return to column 0
    write_all(self.out, self.buffer.as_str())?;
    self.buffer.clear();
    Ok(())
  }

  fn move_cursor_to_end(&mut self, layout: &Layout) -> ShResult<()> {
    self.buffer.clear();
    let mut end = layout.end.row;
    if layout.psr_end.is_some() && layout.t_cols > self.t_cols && self.t_cols > 0 {
      let extra = (layout.t_cols.saturating_sub(1)) / self.t_cols;
      end += extra;
    }
    let cursor_row = layout.cursor.row;

    let cursor_motion = end.saturating_sub(cursor_row);
    if cursor_motion > 0 {
      write!(self.buffer, "\x1b[{cursor_motion}B").unwrap();
    }

    write_all(self.out, self.buffer.as_str())?;
    self.buffer.clear();

    Ok(())
  }

  fn clear_screen(&mut self) -> ShResult<()> {
    self.buffer.clear();
    self.buffer.push_str("\x1b[2J\x1b[H"); // Clear entire screen and move cursor to home
    write_all(self.out, self.buffer.as_str())?;
    self.buffer.clear();
    Ok(())
  }

  fn redraw(
    &mut self,
    prompt: &str,
    line: &str,
    new_layout: &Layout,
    offset: usize,
    total_buf_lines: usize,
  ) -> ShResult<()> {
    let err = |_| sherr!(InternalErr, "Failed to write to LineWriter internal buffer");
    self.buffer.clear();
    self.buffer.push_str("\x1b[J"); // Clear from cursor to end of screen to erase any remnants of the old line after the prompt

    let end = new_layout.end;
    let cursor = new_layout.cursor;

    if read_meta(|m| m.system_msg_pending()) {
      let mut system_msg = String::new();
      while let Some(msg) = write_meta(|m| m.pop_system_message()) {
        writeln!(system_msg, "{msg}").map_err(err)?;
      }
      self.buffer.push_str(&system_msg);
    }

    self.buffer.push_str(prompt);
    let prompt_end = Layout::calc_pos(self.t_cols, prompt, Pos { col: 0, row: 0 }, 0, false);
    let multiline = line.contains('\n') || prompt_end.col == 0;
    if multiline {
      let show_numbers = read_shopts(|o| o.line.line_numbers);
      let display_line = enumerate_lines(
        line,
        prompt_end.col as usize,
        show_numbers,
        offset,
        total_buf_lines,
      );
      self.buffer.push_str(&display_line);
    } else {
      self.buffer.push_str(line);
    }

    if end.col == 0 && end.row > prompt_end.row && !ends_with_newline(&self.buffer) {
      // The line has wrapped. We need to use our own line break.
      self.buffer.push('\n');
    }

    let movement = self.get_cursor_movement(end, cursor)?;
    write!(self.buffer, "{}", &movement).map_err(err)?;

    write_all(self.out, self.buffer.as_str())?;
    Ok(())
  }

  fn flush_write(&mut self, buf: &str) -> ShResult<()> {
    write_all(self.out, buf)?;
    Ok(())
  }

  fn send_bell(&mut self) -> ShResult<()> {
    if read_shopts(|o| o.core.bell_enabled) {
      // we use a cooldown because I don't like having my ears assaulted by 1 million bells
      // whenever i finish clearing the line using backspace.
      let now = Instant::now();

      // surprisingly, a fixed cooldown like '100' is actually more annoying than 1 million bells.
      // I've found this range of 50-150 to be the best balance
      let cooldown = rand::random_range(50..150);
      let should_send = match self.last_bell {
        None => true,
        Some(time) => now.duration_since(time).as_millis() > cooldown,
      };
      if should_send {
        self.flush_write("\x07")?;
        self.last_bell = Some(now);
      }
    }
    Ok(())
  }
}
