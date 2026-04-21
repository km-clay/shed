use std::{collections::VecDeque, fmt::{Debug, Display}, io::Write, os::fd::RawFd, sync::LazyLock, time::Instant};

use nix::{errno::Errno, fcntl::{FcntlArg, OFlag, fcntl, open}, poll::{PollFd, PollFlags, PollTimeout, poll}, sys::{signal::{SigSet, SigmaskHow, Signal, kill, killpg, pthread_sigmask}, stat::Mode, termios::{self, Termios, tcgetattr, tcsetattr}}, unistd::{Pid, close, getpgrp, isatty, read, tcsetpgrp, write}};
use vte::Perform;

use crate::{libsh::{error::{ShErr, ShErrKind, ShResult}}, procio::borrow_fd, readline::{keys::{KeyCode, KeyEvent, ModKeys}, linebuf::Pos, term::get_win_size}, sherr, state::{read_shopts, with_term}};

/// Write to the internal Terminal buffer
///
/// The given input will be buffered, meaning it won't be sent to the terminal until Terminal::flush() is called
/// Note that this calls with_term() internally.
/// DO NOT call this from within any of the state module accessors (e.g. read_logic, write_meta, etc) as that will cause a deadlock.
#[macro_export]
macro_rules! write_term {
  ($($arg:tt)*) => {{
    use std::io::Write;
    $crate::state::with_term(|t| write!(t, $($arg)*))
  }};
}

/// Write to the internal Terminal buffer, and then flush it
///
/// This sends the given format args directly to the terminal.
/// Note that this calls with_term() internally.
/// DO NOT call this from within any of the state module accessors (e.g. read_logic, write_meta, etc) as that will cause a deadlock.
#[macro_export]
macro_rules! flush_term {
  () => {
    use std::fmt::Write as FmtWrite;
    $crate::state::with_term(|t| t.flush())
  };
  ($($arg:tt)*) => {{
    use std::fmt::Write as FmtWrite;
    $crate::state::with_term(|t| {
      write!(t, $($arg)*)
        t.flush()?;
    })
  }};
}

/// Minimum fd number for shell-internal file descriptors.
pub const MIN_INTERNAL_FD: RawFd = 10;

static TTY_FILENO: LazyLock<Option<RawFd>> = LazyLock::new(|| {
  let fd = open("/dev/tty", OFlag::O_RDWR, Mode::empty()).ok()?;
  // Move the tty fd above the user-accessible range so that
  // `exec 3>&-` and friends don't collide with shell internals.
  let high = fcntl(fd, FcntlArg::F_DUPFD_CLOEXEC(MIN_INTERNAL_FD)).ok()?;
  close(fd).ok();
  Some(high)
});

#[derive(Debug,Clone)]
pub enum TermEvent {
  Key(KeyEvent),
  CursorPos(usize,usize),
  Capabilities(usize),
}

#[derive(Debug,Default,Clone)]
struct EventParser {
  events: VecDeque<TermEvent>,
  ss3_pending: bool
}

impl EventParser {
  pub fn new() -> Self {
    Self {
      events: VecDeque::new(),
      ss3_pending: false,
    }
  }

  pub fn push(&mut self, event: TermEvent) {
    self.events.push_back(event);
  }

  pub fn pop(&mut self) -> Option<TermEvent> {
    self.events.pop_front()
  }
}

impl Perform for EventParser {
  fn print(&mut self, c: char) {
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
      self.push(TermEvent::Key(KeyEvent(KeyCode::Backspace, ModKeys::empty())));
    } else {
      self.push(TermEvent::Key(KeyEvent(KeyCode::Char(c), ModKeys::empty())));
    }
  }

  fn execute(&mut self, byte: u8) {
    log::trace!("execute: {byte:#04x}");
    let event = match byte {
      0x00 => TermEvent::Key(KeyEvent(KeyCode::Char(' '), ModKeys::CTRL)), // Ctrl+Space / Ctrl+@
      0x09 => TermEvent::Key(KeyEvent(KeyCode::Tab, ModKeys::empty())),    // Tab (Ctrl+I)
      0x0a => TermEvent::Key(KeyEvent(KeyCode::Char('j'), ModKeys::CTRL)), // Ctrl+J (linefeed)
      0x0d => TermEvent::Key(KeyEvent(KeyCode::Enter, ModKeys::empty())),  // Carriage return (Ctrl+M)
      0x1b => TermEvent::Key(KeyEvent(KeyCode::Esc, ModKeys::empty())),
      0x7f => TermEvent::Key(KeyEvent(KeyCode::Backspace, ModKeys::empty())),
      0x01..=0x1a => {
        // Ctrl+A through Ctrl+Z (excluding special cases above)
        let c = (b'A' + byte - 1) as char;
        TermEvent::Key(KeyEvent(KeyCode::Char(c), ModKeys::CTRL))
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
      ([], 'R') => {
        let row = params.first().copied().unwrap_or(0) as usize;
        let col = params.get(1).copied().unwrap_or(0) as usize;
        TermEvent::CursorPos(col, row)
      }
      ([], 'A') => {
        let mods = params
          .get(1)
          .map(ModKeys::from)
          .unwrap_or(ModKeys::empty());
        TermEvent::Key(KeyEvent(KeyCode::Up, mods))
      }
      ([], 'B') => {
        let mods = params
          .get(1)
          .map(ModKeys::from)
          .unwrap_or(ModKeys::empty());
        TermEvent::Key(KeyEvent(KeyCode::Down, mods))
      }
      ([], 'C') => {
        let mods = params
          .get(1)
          .map(ModKeys::from)
          .unwrap_or(ModKeys::empty());
        TermEvent::Key(KeyEvent(KeyCode::Right, mods))
      }
      ([], 'D') => {
        let mods = params
          .get(1)
          .map(ModKeys::from)
          .unwrap_or(ModKeys::empty());
        TermEvent::Key(KeyEvent(KeyCode::Left, mods))
      }
      // Home/End: CSI H/F or CSI 1;mod H/F
      ([], 'H') => {
        let mods = params
          .get(1)
          .map(ModKeys::from)
          .unwrap_or(ModKeys::empty());
        TermEvent::Key(KeyEvent(KeyCode::Home, mods))
      }
      ([], 'F') => {
        let mods = params
          .get(1)
          .map(ModKeys::from)
          .unwrap_or(ModKeys::empty());
        TermEvent::Key(KeyEvent(KeyCode::End, mods))
      }
      // Shift+Tab: CSI Z
      ([], 'Z') => TermEvent::Key(KeyEvent(KeyCode::Tab, ModKeys::SHIFT)),
      // Special keys with tilde: CSI num ~ or CSI num;mod ~
      ([], '~') => {
        let key_num = params.first().copied().unwrap_or(0);
        let mods = params
          .get(1)
          .map(ModKeys::from)
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
        TermEvent::Key(KeyEvent(key, mods))
      }
      ([], 'u') => { // kitty keyboard protocol: CSI code;mod;text u
        let codepoint = params.first()
          .copied()
          .unwrap_or(0);
        let mods = params
          .get(1)
          .map(ModKeys::from)
          .unwrap_or(ModKeys::empty());
        let text = params.get(2)
          .copied()
          .unwrap_or(codepoint);

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
            if let Some(mut ch) = char::from_u32(codepoint as u32) {
              if mods.contains(ModKeys::CTRL) && ch.is_ascii_lowercase() {
                // result of using uppercase chars everywhere for Ctrl+char matches
                // TODO: do something about that footgun that isn't this nonsense
                ch = ch.to_ascii_uppercase();
              }
              KeyCode::Char(ch)
            } else {
              return;
            }
          }
        };
        TermEvent::Key(KeyEvent(key, mods))
      }
      ([b'?'], 'u') => {
        // capabilities response
        let cap_num = params.first().copied().unwrap_or(0) as usize;
        TermEvent::Capabilities(cap_num)
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
  parser: vte::Parser,
  collector: EventParser,
  byte_buf: VecDeque<u8>,
  pub verbatim_single: bool,
  pub verbatim: bool,
}

impl Clone for PollReader {
  fn clone(&self) -> Self {
    Self {
      parser: vte::Parser::new(),
      collector: self.collector.clone(),
      byte_buf: self.byte_buf.clone(),
      verbatim_single: self.verbatim_single,
      verbatim: self.verbatim,
    }
  }
}

impl Debug for PollReader {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PollReader")
      .field("collector", &self.collector)
      .field("byte_buf", &self.byte_buf)
      .field("verbatim_single", &self.verbatim_single)
      .field("verbatim", &self.verbatim)
      .finish()
  }
}

impl PollReader {
  pub fn new() -> Self {
    Self {
      parser: vte::Parser::new(),
      collector: EventParser::new(),
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

  pub fn read(&mut self, fd: RawFd) -> ShResult<usize> {
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

  fn readkey(&mut self) -> Result<Option<KeyEvent>, ShErr> {
    if let Some(TermEvent::Key(event)) = self.read_event()? {
      Ok(Some(event))
    } else {
      Ok(None)
    }
  }

  fn read_event(&mut self) -> Result<Option<TermEvent>, ShErr> {
    if self.verbatim_single {
      if let Some(key) = self.read_one_verbatim() {
        self.verbatim_single = false;
        return Ok(Some(TermEvent::Key(key)));
      }
      return Ok(None);
    }
    if self.verbatim {
      if let Some(paste) = self.handle_bracket_paste() {
        return Ok(Some(TermEvent::Key(paste)));
      }
      // If we're in verbatim mode but haven't seen the end marker yet, don't attempt to parse keys
      return Ok(None);
    } else if self.byte_buf.front() == Some(&b'\x1b') {
      if self.byte_buf.len() == 1 {
        // ESC is the only byte - emit standalone Escape
        self.byte_buf.pop_front();
        return Ok(Some(TermEvent::Key(KeyEvent(KeyCode::Esc, ModKeys::empty()))));
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
          return Ok(Some(TermEvent::Key(KeyEvent(
            KeyCode::Char(ch.to_ascii_uppercase()),
            ModKeys::ALT,
          ))));
        }
        _ => {
          // ESC + non-printable/unknown - emit standalone Escape
          self.byte_buf.pop_front();
          return Ok(Some(TermEvent::Key(KeyEvent(KeyCode::Esc, ModKeys::empty()))));
        }
      }
    }
    while let Some(byte) = self.byte_buf.pop_front() {
      self.parser.advance(&mut self.collector, &[byte]);
      if let Some(event) = self.collector.pop() {
        match event {
          TermEvent::Key(KeyEvent(KeyCode::BracketedPasteStart, _)) => {
            if let Some(paste) = self.handle_bracket_paste() {
              return Ok(Some(TermEvent::Key(paste)));
            } else {
              continue;
            }
          }
          _ => return Ok(Some(event)),
        }
      }
    }
    Ok(None)
  }
}

impl Default for PollReader {
  fn default() -> Self {
    Self::new()
  }
}

#[derive(Clone,Copy,Debug,Default,PartialEq)]
pub enum CursorStyle {
  #[default]
  Default,
  Block(bool),
  Underline(bool),
  Beam(bool),
}

impl Display for CursorStyle {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      CursorStyle::Default => write!(f, "\x1b[0 q"),
      CursorStyle::Block(blink) => write!(f, "\x1b[{} q", if *blink { 1 } else { 2 }),
      CursorStyle::Underline(blink) => write!(f, "\x1b[{} q", if *blink { 3 } else { 4 }),
      CursorStyle::Beam(blink) => write!(f, "\x1b[{} q", if *blink { 5 } else { 6 }),
    }
  }
}

/// A guard that flushes the terminal on drop.
///
/// Creating one of these will guarantee that the Terminal writes its buffered input
/// when the scope ends. Used mainly in the interactive loop
pub struct FlushGuard;
impl Drop for FlushGuard {
  fn drop(&mut self) {
    with_term(|t| t.flush()).ok();
  }
}

/// A guard that saves the terminal state on creation and restores it on drop.
///
/// This is returned from any Terminal method that modifies the terminal state.
/// This allows us to scope terminal state changes, and ensures that the terminal state is always restored even if the code panics or returns early.
#[derive(Debug)]
pub struct TermGuard {
  raw_mode: Option<bool>,
  bracketed_paste: Option<bool>,
  kitty_proto: Option<bool>,
  alt_buffer: Option<bool>,
  cursor_style: Option<CursorStyle>,
  cursor_visible: Option<bool>,
  interactive: Option<bool>,
}

impl Drop for TermGuard {
  fn drop(&mut self) {
    with_term(|t| t.load_state(self).ok());
  }
}

/// An abstraction over the terminal that manages terminal attributes, and I/O.
#[derive(Clone,Debug)]
pub struct Terminal {
  tty: Option<RawFd>,
  reader: PollReader,
  input_buf: String,

  bracketed_paste: bool,
  kitty_kbd_proto: bool,
  raw_mode: bool,
  alt_buffer: bool,
  cursor_style: CursorStyle,
  cursor_visible: bool,
  interactive: bool,
  orig_termios: Option<Termios>,

  t_cols: usize,
  t_rows: usize,

  last_bell: Option<Instant>
}

impl Terminal {
  pub const BRACKET_PASTE_ON: &str = "\x1b[?2004h";
  pub const BRACKET_PASTE_OFF: &str = "\x1b[?2004l";
  pub const KITTY_PROTO_ON: &str = "\x1b[>17u";
  pub const KITTY_PROTO_OFF: &str = "\x1b[<u";
  pub const CAP_QUERY: &str = "\x1b[?u";
  pub const ALT_BUFFER_ENTER: &str = "\x1b[?1049h";
  pub const ALT_BUFFER_EXIT: &str = "\x1b[?1049l";
  pub const CURSOR_HIDE: &str = "\x1b[?25l";
  pub const CURSOR_SHOW: &str = "\x1b[?25h";
  pub const CURSOR_QUERY: &str = "\x1b[6n";
  pub const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";
  fn toggle_attr(
    buf: &mut String,
    switch: &mut bool,
    on_ctl: &str,
    off_ctl: &str,
    on: bool,
  ) -> ShResult<()> {
    let control = if on && !*switch {
      on_ctl
    } else if !on && *switch {
      off_ctl
    } else { return Ok(()); };

    buf.push_str(control);

    *switch = on;
    Ok(())
  }

  pub fn new() -> Self {
    let tty = TTY_FILENO.and_then(|fd| isatty(fd).unwrap_or(false).then_some(fd));
    let (cols,rows) = tty.map(get_win_size).unwrap_or((80,24));
    Self {
      tty,
      reader: PollReader::new(),
      input_buf: String::new(),
      bracketed_paste: false,
      kitty_kbd_proto: false,
      alt_buffer: false,
      cursor_style: CursorStyle::Default,
      interactive: false,
      cursor_visible: true,
      raw_mode: false,
      orig_termios: None,
      t_cols: cols as usize,
      t_rows: rows as usize,
      last_bell: None,
    }
  }

  /// Access the underlying tty file descriptor.
  ///
  /// # Safety
  /// The fd is basically guaranteed to be open for the full lifetime of the program,
  /// unless the user does some pathological nonsense like duping another fd to it and then closing it, or something like that.
  /// Even so, accessing it directly is still a smell.
  /// The entire point of the Terminal abstraction is to avoid having to interact with the tty fd directly.
  /// If you find yourself calling this, it may be better to just implement the needed functionality as a method on Terminal instead.
  /// The reason it's 'unsafe' is because you are responsible for whatever footgun you create with this.
  ///
  /// This function checks the fd for validity before returning it.
  pub unsafe fn tty(&self) -> Option<RawFd> {
    let tty = self.tty?;
    let isatty = isatty(tty).unwrap_or(false);
    let get_fd = fcntl(tty, FcntlArg::F_GETFD).is_ok();

    (isatty && get_fd).then_some(tty)
  }

  pub fn isatty(&self) -> bool {
    self.tty.is_some_and(|tty| isatty(tty).unwrap_or(false))
  }

  pub fn interactive(&self) -> bool {
    self.interactive
  }

  pub fn interactive_guard(&mut self, on: bool) -> TermGuard {
    let old = self.interactive;
    self.interactive = on;
    TermGuard {
      interactive: Some(old),
      raw_mode: None,
      bracketed_paste: None,
      kitty_proto: None,
      alt_buffer: None,
      cursor_style: None,
      cursor_visible: None,
    }
  }

  fn save_state(&self) -> TermGuard {
    TermGuard {
      raw_mode: Some(self.raw_mode),
      bracketed_paste: Some(self.bracketed_paste),
      kitty_proto: Some(self.kitty_kbd_proto),
      alt_buffer: Some(self.alt_buffer),
      cursor_style: Some(self.cursor_style),
      cursor_visible: Some(self.cursor_visible),
      interactive: None,
    }
  }

  fn load_state(&mut self, guard: &TermGuard) -> ShResult<()> {
    let mut wrote_seq = false;
    if let Some(raw_mode) = guard.raw_mode {
      self.toggle_raw_mode(raw_mode)?; // restore raw mode first so escape sequences work
      wrote_seq = true;
    }
    if let Some(bracketed_paste) = guard.bracketed_paste {
      self.toggle_bracketed_paste(bracketed_paste)?;
      wrote_seq = true;
    }
    if let Some(kitty_proto) = guard.kitty_proto {
      self.toggle_kitty_proto(kitty_proto)?;
      wrote_seq = true;
    }
    if let Some(alt_buffer) = guard.alt_buffer {
      self.toggle_alt_buffer(alt_buffer)?;
      wrote_seq = true;
    }
    if let Some(cursor_visible) = guard.cursor_visible {
      self.toggle_cursor_visibility(cursor_visible)?;
      wrote_seq = true;
    }
    if let Some(cursor_style) = guard.cursor_style {
      self.set_cursor_style(cursor_style)?;
      wrote_seq = true;
    }
    if let Some(interactive) = guard.interactive {
      self.interactive = interactive;
    }

    if wrote_seq {
      self.flush()?; // flush restore sequences immediately
    }
    Ok(())
  }

  pub fn update_t_dims(&mut self) {
    let Some(tty) = self.tty else { return };
    let (cols,rows) = get_win_size(tty);
    self.t_cols = cols as usize;
    self.t_rows = rows as usize;
  }

  pub fn poll(&mut self, timeout: PollTimeout) -> ShResult<i32> {
    let Some(tty) = self.tty else { return Ok(0) };
    let poll_fd = PollFd::new(borrow_fd(tty), PollFlags::POLLIN);
    Ok(poll(&mut [poll_fd], timeout)?)
  }

  pub fn check_term_capabilities(&mut self) -> ShResult<Option<TermEvent>> {
    let Some(tty) = self.tty else { return Ok(None) };

    self.write_direct(Self::CAP_QUERY)?;

    if self.poll(PollTimeout::from(50u8))? == 0 {
      // timeout - assume we didn't get a response
      return Ok(None);
    }

    self.reader.read(tty)?;

    while let Some(event) = self.reader.read_event()? {
      if let TermEvent::Capabilities(_) = event {
        return Ok(Some(event));
      }
    }

    Ok(None)
  }

  pub fn get_cursor_pos(&mut self) -> ShResult<Option<(usize,usize)>> {
    let Some(tty) = self.tty else { return Ok(None) };

    // ask the terminal where our cursor is
    self.write_direct(Self::CURSOR_QUERY)?;

    if self.poll(PollTimeout::from(50u8))? == 0 {
      // timeout - assume we didn't get a response
      return Ok(None);
    }

    self.reader.read(tty)?;

    while let Some(event) = self.reader.read_event()? {
      let TermEvent::CursorPos(row, col) = event else { continue };
      return Ok(Some((col, row)));
    }
    Ok(None)
  }

  /// Called before the prompt is drawn. If we are not on column 1, push a vid-inverted '%' and then a '\n\r'.
  ///
  /// Aping zsh with this but it's a nice feature.
  pub fn fix_cursor_column(&mut self) -> ShResult<()> {
    let Some((_, c)) = self.get_cursor_pos()? else {
      return Ok(());
    };

    if c != 1 {
      self.input_buf.push_str("\x1b[7m%\x1b[0m\n\r");
    }
    Ok(())
  }

  pub fn calc_cursor_movement(&mut self, old: Pos, new: Pos) -> ShResult<()> {
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
          1 => self.input_buf.push_str("\x1b[B"),
          _ => write!(self, "\x1b[{shift}B").map_err(err)?,
        }
      }
      std::cmp::Ordering::Less => {
        let shift = old.row - new.row;
        match shift {
          1 => self.input_buf.push_str("\x1b[A"),
          _ => write!(self, "\x1b[{shift}A").map_err(err)?,
        }
      }
      std::cmp::Ordering::Equal => { /* Do nothing */ }
    }

    match new.col.cmp(&old.col) {
      std::cmp::Ordering::Greater => {
        let shift = new.col - old.col;
        match shift {
          1 => self.input_buf.push_str("\x1b[C"),
          _ => write!(self, "\x1b[{shift}C").map_err(err)?,
        }
      }
      std::cmp::Ordering::Less => {
        let shift = old.col - new.col;
        match shift {
          1 => self.input_buf.push_str("\x1b[D"),
          _ => write!(self, "\x1b[{shift}D").map_err(err)?,
        }
      }
      std::cmp::Ordering::Equal => { /* Do nothing */ }
    }

    Ok(())
  }

  pub fn t_cols(&self) -> usize {
    self.t_cols
  }

  pub fn t_rows(&self) -> usize {
    self.t_rows
  }

  pub fn t_size(&self) -> (usize, usize) {
    (self.t_cols, self.t_rows)
  }

  pub fn buf_ends_with_newline(&self) -> bool {
    self.input_buf.ends_with('\n')
  }

  pub fn verbatim_single(&mut self, on: bool) {
    self.reader.verbatim_single = on;
  }

  pub fn send_bell(&mut self) -> ShResult<()> {
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
        self.write_direct("\x07")?;
        self.last_bell = Some(now);
      }
    }
    Ok(())
  }

  pub fn controller(&self) -> Option<Pid> {
    let tty = self.tty?;
    nix::unistd::tcgetpgrp(borrow_fd(tty)).ok()
  }

  pub fn attach(&mut self, pgid: Pid) -> ShResult<()> {
    let Some(tty) = self.tty else { return Ok(()); };
    // If we aren't attached to a terminal, the pgid already controls it, or the
    // process group does not exist Then return ok
    let term_controller = self.controller().unwrap_or(Pid::this());
    let isatty = self.isatty();
    if !isatty || pgid == term_controller || killpg(pgid, None).is_err() {
      return Ok(());
    }

    if pgid == getpgrp() && term_controller != getpgrp() {
      kill(term_controller, Signal::SIGTTOU).ok();
    }

    let mut new_mask = SigSet::empty();
    let mut mask_bkup = SigSet::empty();

    new_mask.add(Signal::SIGTSTP);
    new_mask.add(Signal::SIGTTIN);
    new_mask.add(Signal::SIGTTOU);

    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&new_mask), Some(&mut mask_bkup))?;

    let result = tcsetpgrp(borrow_fd(tty), pgid);

    pthread_sigmask(
      SigmaskHow::SIG_SETMASK,
      Some(&mask_bkup),
      Some(&mut new_mask),
    )?;

    if let Err(e) = result {
      log::error!("Failed to set terminal process group: {e}");
      tcsetpgrp(borrow_fd(tty), getpgrp())?;
    }

    Ok(())
  }

  pub fn fd_is_tty(&self, other: RawFd) -> bool {
    let Some(tty) = self.tty else { return false };
    other == tty
  }

  pub fn read(&mut self) -> ShResult<usize> {
    let Some(tty) = self.tty else { return Ok(0) };
    self.reader.read(tty)
  }

  pub fn drain_keys(&mut self) -> ShResult<Vec<KeyEvent>> {
    let mut keys = vec![];
    while let Some(key) = self.reader.readkey()? {
      keys.push(key);
    }
    Ok(keys)
  }

  pub fn feed_bytes(&mut self, bytes: &[u8]) {
    self.reader.feed_bytes(bytes);
  }

  pub fn cooked_mode_guard(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    self.toggle_bracketed_paste(false)?;
    self.toggle_raw_mode(false)?;
    Ok(guard)
  }

  pub fn prepare_for_pager(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    self.toggle_raw_mode(true)?;
    self.toggle_bracketed_paste(false)?;
    self.toggle_alt_buffer(true)?;
    self.set_cursor_style(CursorStyle::Default)?;
    self.toggle_cursor_visibility(false)?;
    self.flush()?;
    Ok(guard)
  }

  pub fn prepare_for_exec(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    self.toggle_bracketed_paste(false)?;
    self.toggle_alt_buffer(false)?;
    self.set_cursor_style(CursorStyle::Default)?;
    self.toggle_raw_mode(false)?;
    self.toggle_kitty_proto(false)?;
    self.flush()?; // flush escape sequences before switching to cooked mode
    Ok(guard)
  }

  pub fn raw_mode_guard(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    self.toggle_raw_mode(true)?;
    Ok(guard)
  }

  pub fn toggle_raw_mode(&mut self, on: bool) -> ShResult<()> {
    let Some(tty) = self.tty else { return Ok(()) };
    if on && !self.raw_mode {
      let orig = tcgetattr(borrow_fd(tty))
        .map_err(|e| sherr!(InternalErr, "Failed to get terminal attributes: {e}"))?;

      let mut raw = orig.clone();

      termios::cfmakeraw(&mut raw);
      // Keep ISIG enabled so Ctrl+C/Ctrl+Z still generate signals
      raw.local_flags |= termios::LocalFlags::ISIG;
      // Keep OPOST enabled so \n is translated to \r\n on output
      raw.output_flags |= termios::OutputFlags::OPOST;

      tcsetattr(borrow_fd(tty), termios::SetArg::TCSANOW, &raw)
        .map_err(|e| sherr!(InternalErr, "Failed to enable raw mode: {e}"))?;

      self.orig_termios = Some(orig);
      self.raw_mode = true;

    } else if !on && self.raw_mode {
      if let Some(ref orig) = self.orig_termios {
        tcsetattr(borrow_fd(tty), termios::SetArg::TCSANOW, orig)
          .map_err(|e| sherr!(InternalErr, "Failed to disable raw mode: {e}"))?;
      }
      self.raw_mode = false;
    }
    Ok(())
  }

  pub fn orig_termios(&self) -> Option<&Termios> {
    self.orig_termios.as_ref()
  }

  pub fn is_raw(&self) -> bool {
    self.raw_mode
  }
  pub fn write_direct(&mut self, buf: &str) -> ShResult<()> {
    let Some(tty) = self.tty else { return Ok(()); };
    let mut buf = buf.as_bytes();
    while !buf.is_empty() {
      match write(borrow_fd(tty), buf) {
        Ok(n) => buf = &buf[n..],
        Err(Errno::EINTR) => continue,
        Err(_) => return Err(std::io::Error::last_os_error().into()),
      }
    }
    Ok(())
  }

  pub fn set_cursor_style(&mut self, style: CursorStyle) -> ShResult<()> {
    let style_raw = style.to_string();
    self.write_all(style_raw.as_bytes())?;
    self.cursor_style = style;
    Ok(())
  }

  pub fn toggle_cursor_visibility(&mut self, visible: bool) -> ShResult<()> {
    Self::toggle_attr(
      &mut self.input_buf,
      &mut self.cursor_visible,
      Self::CURSOR_SHOW,
      Self::CURSOR_HIDE,
      visible,
    )
  }

  pub fn toggle_alt_buffer(&mut self, on: bool) -> ShResult<()> {
    Self::toggle_attr(
      &mut self.input_buf,
      &mut self.alt_buffer,
      Self::ALT_BUFFER_ENTER,
      Self::ALT_BUFFER_EXIT,
      on,
    )
  }

  pub fn toggle_bracketed_paste(&mut self, on: bool) -> ShResult<()> {
    Self::toggle_attr(
      &mut self.input_buf,
      &mut self.bracketed_paste,
      Self::BRACKET_PASTE_ON,
      Self::BRACKET_PASTE_OFF,
      on,
    )
  }

  pub fn toggle_kitty_proto(&mut self, on: bool) -> ShResult<()> {
    Self::toggle_attr(
      &mut self.input_buf,
      &mut self.kitty_kbd_proto,
      Self::KITTY_PROTO_ON,
      Self::KITTY_PROTO_OFF,
      on,
    )
  }

  #[cfg(test)]
  pub fn set_fd_for_testing(&mut self, fd: Option<RawFd>) {
    self.tty = fd;
  }
}

impl Default for Terminal {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Terminal {
  fn drop(&mut self) {
    self.toggle_bracketed_paste(false).ok();
    self.toggle_kitty_proto(false).ok();
    self.toggle_raw_mode(false).ok();
    self.toggle_alt_buffer(false).ok();
    if self.cursor_style != CursorStyle::Default {
      self.set_cursor_style(CursorStyle::Default).ok();
    }
  }
}

impl std::io::Write for Terminal {
  fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    match std::str::from_utf8(buf) {
      Ok(s) => self.input_buf.push_str(s),
      Err(_) => self.input_buf.push_str(&String::from_utf8_lossy(buf)),
    }
    Ok(buf.len())
  }
  fn flush(&mut self) -> std::io::Result<()> {
    let Some(tty) = self.tty else {
      self.input_buf.clear();
      return Ok(())
    };
    let mut buf = self.input_buf.as_bytes();
    while !buf.is_empty() {
      match write(borrow_fd(tty), buf) {
        Ok(n) => buf = &buf[n..],
        Err(Errno::EINTR) => continue,
        Err(_) => {
          self.input_buf.clear();
          return Err(std::io::Error::last_os_error());
        }
      }
    }
    self.input_buf.clear();
    Ok(())
  }
}
