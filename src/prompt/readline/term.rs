use std::{
  collections::VecDeque,
  env,
  fmt::{Debug, Write},
  io::{BufRead, BufReader, Read},
  os::fd::{AsFd, BorrowedFd, RawFd},
};

use nix::{
  errno::Errno,
  libc::{self, STDIN_FILENO},
  poll::{self, PollFlags, PollTimeout},
  sys::termios::{self, tcgetattr, tcsetattr},
  unistd::isatty,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use vte::{Parser, Perform};

use crate::{prelude::*, procio::borrow_fd};
use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  prompt::readline::keys::{KeyCode, ModKeys},
};

use super::{keys::KeyEvent, linebuf::LineBuf};

pub fn raw_mode() -> RawModeGuard {
  let orig = termios::tcgetattr(unsafe { BorrowedFd::borrow_raw(STDIN_FILENO) })
    .expect("Failed to get terminal attributes");
  let mut raw = orig.clone();
  termios::cfmakeraw(&mut raw);
  // Keep ISIG enabled so Ctrl+C/Ctrl+Z still generate signals
  raw.local_flags |= termios::LocalFlags::ISIG;
  // Keep OPOST enabled so \n is translated to \r\n on output
  raw.output_flags |= termios::OutputFlags::OPOST;
  termios::tcsetattr(
    unsafe { BorrowedFd::borrow_raw(STDIN_FILENO) },
    termios::SetArg::TCSANOW,
    &raw,
  )
  .expect("Failed to set terminal to raw mode");

	let (cols, rows) = get_win_size(STDIN_FILENO);

  RawModeGuard {
    orig,
    fd: STDIN_FILENO,
  }
}

pub type Row = u16;
pub type Col = u16;

#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Pos {
  col: Col,
  row: Row,
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

fn write_all(fd: RawFd, buf: &str) -> nix::Result<()> {
  let mut bytes = buf.as_bytes();
  while !bytes.is_empty() {
    match nix::unistd::write(unsafe { BorrowedFd::borrow_raw(fd) }, bytes) {
      Ok(0) => return Err(Errno::EIO),
      Ok(n) => bytes = &bytes[n..],
      Err(Errno::EINTR) => {}
      Err(r) => return Err(r),
    }
  }
  Ok(())
}

// Big credit to rustyline for this
fn width(s: &str, esc_seq: &mut u8) -> u16 {
  let w_calc = width_calculator();
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
    w_calc.width(s) as u16
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

pub trait WidthCalculator {
  fn width(&self, text: &str) -> usize;
}

pub trait KeyReader {
  fn read_key(&mut self) -> Result<Option<KeyEvent>, ShErr>;
}

pub trait LineWriter {
  fn clear_rows(&mut self, layout: &Layout) -> ShResult<()>;
  fn redraw(&mut self, prompt: &str, line: &str, new_layout: &Layout) -> ShResult<()>;
  fn flush_write(&mut self, buf: &str) -> ShResult<()>;
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
    let mut width = 0;
    for slice in text.split(ZWJ) {
      width += UnicodeWidth.width(slice);
    }
    width
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
      Err(Errno::EINTR) => {
        Err(Errno::EINTR.into())
      }
      Err(e) => Err(std::io::Error::from_raw_os_error(e as i32)),
    }
  }
}

pub struct RawModeGuard {
  orig: termios::Termios,
  fd: RawFd,
}

impl RawModeGuard {
  /// Disable raw mode temporarily for a specific operation
  pub fn disable_for<F: FnOnce() -> R, R>(&self, func: F) -> R {
    unsafe {
      let fd = BorrowedFd::borrow_raw(self.fd);
      // Temporarily restore the original termios
      termios::tcsetattr(fd, termios::SetArg::TCSANOW, &self.orig)
        .expect("Failed to temporarily disable raw mode");

      // Run the function
      let result = func();

      // Re-enable raw mode
      let mut raw = self.orig.clone();
      termios::cfmakeraw(&mut raw);
      // Keep ISIG enabled so Ctrl+C/Ctrl+Z still generate signals
      raw.local_flags |= termios::LocalFlags::ISIG;
      // Keep OPOST enabled so \n is translated to \r\n on output
      raw.output_flags |= termios::OutputFlags::OPOST;
      termios::tcsetattr(fd, termios::SetArg::TCSANOW, &raw).expect("Failed to re-enable raw mode");

      result
    }
  }

	pub fn with_cooked_mode<F, R>(f: F) -> R
	where F: FnOnce() -> R {
		let raw = tcgetattr(borrow_fd(STDIN_FILENO)).expect("Failed to get terminal attributes");
		let mut cooked = raw.clone();
		cooked.local_flags |= termios::LocalFlags::ICANON | termios::LocalFlags::ECHO;
		cooked.input_flags |= termios::InputFlags::ICRNL;
		tcsetattr(borrow_fd(STDIN_FILENO), termios::SetArg::TCSANOW, &cooked).expect("Failed to set cooked mode");
		let res = f();
		tcsetattr(borrow_fd(STDIN_FILENO), termios::SetArg::TCSANOW, &raw).expect("Failed to restore raw mode");
		res
	}
}

impl Drop for RawModeGuard {
  fn drop(&mut self) {
    unsafe {
      let _ = termios::tcsetattr(
        BorrowedFd::borrow_raw(self.fd),
        termios::SetArg::TCSANOW,
        &self.orig,
      );
    }
  }
}

// ============================================================================
// PollReader - non-blocking key reader using vte parser
// ============================================================================

struct KeyCollector {
  events: VecDeque<KeyEvent>,
}

impl KeyCollector {
  fn new() -> Self {
    Self {
      events: VecDeque::new(),
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
    if bits & 1 != 0 { mods |= ModKeys::SHIFT; }
    if bits & 2 != 0 { mods |= ModKeys::ALT; }
    if bits & 4 != 0 { mods |= ModKeys::CTRL; }
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
    if c == '\x7f' {
      self.push(KeyEvent(KeyCode::Backspace, ModKeys::empty()));
    } else {
      self.push(KeyEvent(KeyCode::Char(c), ModKeys::empty()));
    }
  }

  fn execute(&mut self, byte: u8) {
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

  fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, action: char) {
    let params: Vec<u16> = params.iter()
      .map(|p| p.first().copied().unwrap_or(0))
      .collect();

    let event = match (intermediates, action) {
      // Arrow keys: CSI A/B/C/D or CSI 1;mod A/B/C/D
      ([], 'A') => {
        let mods = params.get(1).map(|&m| Self::parse_modifiers(m)).unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Up, mods)
      }
      ([], 'B') => {
        let mods = params.get(1).map(|&m| Self::parse_modifiers(m)).unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Down, mods)
      }
      ([], 'C') => {
        let mods = params.get(1).map(|&m| Self::parse_modifiers(m)).unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Right, mods)
      }
      ([], 'D') => {
        let mods = params.get(1).map(|&m| Self::parse_modifiers(m)).unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Left, mods)
      }
      // Home/End: CSI H/F or CSI 1;mod H/F
      ([], 'H') => {
        let mods = params.get(1).map(|&m| Self::parse_modifiers(m)).unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::Home, mods)
      }
      ([], 'F') => {
        let mods = params.get(1).map(|&m| Self::parse_modifiers(m)).unwrap_or(ModKeys::empty());
        KeyEvent(KeyCode::End, mods)
      }
      // Shift+Tab: CSI Z
      ([], 'Z') => {
        KeyEvent(KeyCode::Tab, ModKeys::SHIFT)
      }
      // Special keys with tilde: CSI num ~ or CSI num;mod ~
      ([], '~') => {
        let key_num = params.first().copied().unwrap_or(0);
        let mods = params.get(1).map(|&m| Self::parse_modifiers(m)).unwrap_or(ModKeys::empty());
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
          _ => return,
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
    // SS3 sequences (ESC O P/Q/R/S for F1-F4)
    if intermediates == [b'O'] {
      let key = match byte {
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return,
      };
      self.push(KeyEvent(key, ModKeys::empty()));
    }
  }
}

pub struct PollReader {
  parser: Parser,
  collector: KeyCollector,
}

impl PollReader {
  pub fn new() -> Self {
    Self {
      parser: Parser::new(),
      collector: KeyCollector::new(),
    }
  }

  pub fn feed_bytes(&mut self, bytes: &[u8]) {
    if bytes == [b'\x1b'] {
      // Single escape byte - user pressed ESC key
      self.collector.push(KeyEvent(KeyCode::Esc, ModKeys::empty()));
      return;
    }

    // Feed all bytes through vte parser
    self.parser.advance(&mut self.collector, bytes);
  }
}

impl Default for PollReader {
  fn default() -> Self {
    Self::new()
  }
}

impl KeyReader for PollReader {
  fn read_key(&mut self) -> Result<Option<KeyEvent>, ShErr> {
    Ok(self.collector.pop())
  }
}

// ============================================================================
// TermReader - blocking key reader (original implementation)
// ============================================================================

pub struct TermReader {
  buffer: BufReader<TermBuffer>,
}

impl Default for TermReader {
  fn default() -> Self {
    Self::new()
  }
}

impl TermReader {
  pub fn new() -> Self {
    Self {
      buffer: BufReader::new(TermBuffer::new(1)),
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

      // UTF-8 max 4 bytes — if it’s invalid at this point, bail
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
    unsafe { BorrowedFd::borrow_raw(fd) }
  }
}

pub struct Layout {
  pub w_calc: Box<dyn WidthCalculator>,
  pub prompt_end: Pos,
  pub cursor: Pos,
  pub end: Pos,
}

impl Debug for Layout {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    writeln!(f, "Layout: ")?;
    writeln!(f, "\tPrompt End: {:?}", self.prompt_end)?;
    writeln!(f, "\tCursor: {:?}", self.cursor)?;
    writeln!(f, "\tEnd: {:?}", self.end)
  }
}

impl Layout {
  pub fn new() -> Self {
    let w_calc = width_calculator();
    Self {
      w_calc,
      prompt_end: Pos::default(),
      cursor: Pos::default(),
      end: Pos::default(),
    }
  }
  pub fn from_parts(
    tab_stop: u16,
    term_width: u16,
    prompt: &str,
    to_cursor: &str,
    to_end: &str,
  ) -> Self {
    let prompt_end = Self::calc_pos(tab_stop, term_width, prompt, Pos { col: 0, row: 0 });
    let cursor = Self::calc_pos(tab_stop, term_width, to_cursor, prompt_end);
    let end = Self::calc_pos(tab_stop, term_width, to_end, prompt_end);
    Layout {
      w_calc: width_calculator(),
      prompt_end,
      cursor,
      end,
    }
  }

  pub fn calc_pos(tab_stop: u16, term_width: u16, s: &str, orig: Pos) -> Pos {
    let mut pos = orig;
    let mut esc_seq = 0;
    for c in s.graphemes(true) {
      if c == "\n" {
        pos.row += 1;
        pos.col = 0;
      }
      let c_width = if c == "\t" {
        tab_stop - (pos.col % tab_stop)
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

pub struct TermWriter {
  out: RawFd,
  t_cols: Col, // terminal width
  buffer: String,
  w_calc: Box<dyn WidthCalculator>,
  tab_stop: u16,
}

impl TermWriter {
  pub fn new(out: RawFd) -> Self {
    let w_calc = width_calculator();
    let (t_cols, _) = get_win_size(out);
    Self {
      out,
      t_cols,
      buffer: String::new(),
      w_calc,
      tab_stop: 8, // TODO: add a way to configure this
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
    let rows_to_clear = layout.end.row;
    let cursor_row = layout.cursor.row;

    let cursor_motion = rows_to_clear.saturating_sub(cursor_row);
    if cursor_motion > 0 {
      write!(self.buffer, "\x1b[{cursor_motion}B").unwrap()
    }

    for _ in 0..rows_to_clear {
      self.buffer.push_str("\x1b[2K\x1b[A");
    }
    self.buffer.push_str("\x1b[2K\r"); // Clear line and return to column 0
    write_all(self.out, self.buffer.as_str())?;
    self.buffer.clear();
    Ok(())
  }

  fn redraw(&mut self, prompt: &str, line: &str, new_layout: &Layout) -> ShResult<()> {
    let err = |_| {
      ShErr::simple(
        ShErrKind::InternalErr,
        "Failed to write to LineWriter internal buffer",
      )
    };
    self.buffer.clear();

    let end = new_layout.end;
    let cursor = new_layout.cursor;

    self.buffer.push_str(prompt);
    self.buffer.push_str(&line.to_string());

    if end.col == 0 && end.row > 0 && !self.buffer.ends_with('\n') {
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
}
