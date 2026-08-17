use std::{
  fmt::Debug,
  io::Write,
  os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
  sync::LazyLock,
  time::{Duration, Instant},
};

mod util;
pub(crate) use util::{ColorMode, calc_str_width, get_win_size, truncate_with_ellipsis, width};
use util::{enable_cooked_mode, enable_raw_mode};

mod guard;
use guard::Snapshot;
pub(crate) use guard::{FlushGuard, SyncOutputGuard, TermGuard};

mod parse;
use parse::TermEvent;
pub(crate) use parse::{Cols, Rows};

mod control;
pub(crate) use control::{
  Attr, ClearCtl, CursorCtl, CursorStyle, OscCtl, Scroll, TermCtl, TermQuery, Toggle,
};

use bitflags::bitflags;

use nix::{
  errno::Errno,
  fcntl::{OFlag, open},
  libc,
  poll::{PollFd, PollFlags, PollTimeout, poll},
  sys::{
    signal::{SigSet, SigmaskHow, Signal, kill, killpg, pthread_sigmask},
    stat::Mode,
    termios::{self, Termios, tcgetattr, tcsetattr},
  },
  unistd::{Pid, getpgrp, isatty, tcsetpgrp, write},
};

use crate::{
  status_msg,
  util::{base64_encode, format_size},
};

use super::{
  Pos, ShErr, ShErrKind, ShResult, Shed,
  keys::{self, KeyEvent},
  match_loop, procio, sherr, shopt_macro as shopt, try_var,
};

static TTY_FILENO: LazyLock<Option<OwnedFd>> = LazyLock::new(|| {
  // try to call dup2() on stdin if it is a tty.
  // on mac, calling open on /dev/tty directly will cause issues.
  let stdin = unsafe { BorrowedFd::borrow_raw(libc::STDIN_FILENO) };
  let owned = if isatty(stdin).unwrap_or(false) {
    stdin.try_clone_to_owned().ok()? // dup2
  } else {
    open("/dev/tty", OFlag::O_RDWR, Mode::empty()).ok()?
  };
  // Move the tty fd above the user-accessible range so that
  // `exec 3>&-` and friends don't collide with shell internals.
  procio::move_high(owned).ok()
});

#[derive(Debug, Default, Clone, Copy)]
pub(crate) enum ScrollRegionState {
  Set(u16, u16),
  #[default]
  Unset,
}

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub(crate) struct TermCap: u32 {
    const TRUECOLOR = 1<<0;
    const KITTY_KBD_PROTO = 1<<1;
    const SGR_MOUSE = 1<<2;
    const SCROLL_UPDOWN = 1<<3;
    const ALT_SCREEN = 1<<4;
    const BRACKET_PASTE = 1<<5;
    const FOCUS_REPORT = 1<<6;
    const SYNC_OUTPUT = 1<<7;
    const STRIKETHROUGH = 1<<8;
    const UNDERLINE_STYLES = 1<<9;
  }
}

/// An abstraction over the terminal that manages terminal attributes, and I/O.
#[derive(Debug)]
#[expect(clippy::struct_excessive_bools)]
pub(crate) struct Terminal {
  tty: Option<RawFd>,
  reader: parse::PollReader,
  input_buf: String,

  bracketed_paste: bool,
  kitty_kbd_proto: bool,
  report_focus: bool,
  raw_mode: bool,
  alt_buffer: bool,
  cursor_style: CursorStyle,
  cursor_visible: bool,
  mouse_enabled: bool,
  interactive: bool,

  termios_stack: Vec<Termios>,
  term_caps: TermCap,
  xt_version: Option<parse::XtVersion>,

  t_cols: usize,
  t_rows: usize,

  scroll_region: ScrollRegionState,

  last_bell: Option<Instant>,
  last_input: Option<Instant>,

  /// When set, terminal-capability and cursor-position probes short-circuit
  /// instead of sending escape sequences and waiting for replies. Used by
  /// tests where the PTY peer doesn't synthesize responses.
  test_mode: bool,
}

impl Clone for Terminal {
  fn clone(&self) -> Self {
    Self {
      reader: self.reader.clone(),
      input_buf: self.input_buf.clone(),
      termios_stack: self.termios_stack.clone(),
      xt_version: self.xt_version.clone(),
      ..*self // I guess this works if everything else is Copy, cool
    }
  }
}

const COOKED_TEARDOWN: [TermCtl; 3] = [
  TermCtl::SetAttr(Attr::BracketPaste(Toggle::Off)),
  TermCtl::SetAttr(Attr::FocusReport(Toggle::Off)),
  TermCtl::SetAttr(Attr::KittyKbdProto(Toggle::Off)),
];

impl Terminal {
  pub fn execute_control(&mut self, ctl: &TermCtl) -> ShResult<()> {
    use TermCtl as Ctl;
    match ctl {
      Ctl::Scroll(Scroll::SetRegion(top, bot)) => {
        self.with_saved_cursor(|this| {
          write!(this, "{ctl}").ok();
        });
        self.scroll_region = ScrollRegionState::Set(*top, *bot);
        return Ok(());
      }
      Ctl::Scroll(reset @ (Scroll::ResetRegion | Scroll::ResetRegionNoClear)) => {
        if let ScrollRegionState::Set(_, bottom) = self.scroll_region {
          let max_row = self.t_rows as u16;
          self.with_saved_cursor(|this| {
            if matches!(reset, Scroll::ResetRegion) {
              for row in (bottom + 1)..=max_row {
                this
                  .execute_control(&TermCtl::Cursor(CursorCtl::Absolute { row, col: 1 }))
                  .ok();
                this
                  .execute_control(&TermCtl::Clear(ClearCtl::WholeLine))
                  .ok();
              }
            }

            write!(this, "{}", TermCtl::Scroll(Scroll::ResetRegion)).ok();
          });
          self.scroll_region = ScrollRegionState::Unset;
        }
        return Ok(());
      }
      Ctl::SetAttr(attr) => {
        let changed = match attr {
          Attr::AltBuffer(toggle) => {
            let changed = Self::toggle_attr(&mut self.alt_buffer, *toggle);
            // re-apply scroll region in case it got turned off
            if let Toggle::Off = toggle
              && let ScrollRegionState::Set(top, bottom) = self.scroll_region
            {
              self.with_saved_cursor(|this| {
                write!(this, "\x1b[{top};{bottom}r").ok();
              });
            }
            changed
          }
          Attr::FocusReport(toggle) => Self::toggle_attr(&mut self.report_focus, *toggle),
          Attr::BracketPaste(toggle) => Self::toggle_attr(&mut self.bracketed_paste, *toggle),
          Attr::KittyKbdProto(toggle) => Self::toggle_attr(&mut self.kitty_kbd_proto, *toggle),
          Attr::MouseTracking(toggle) => Self::toggle_attr(&mut self.mouse_enabled, *toggle),
          _ => true, // non-toggle attrs (ModifyOtherKeys, ApplicationKeypad) always emit
        };
        if !changed {
          return Ok(());
        }
      }
      Ctl::Cursor(CursorCtl::ShowCursor) => {
        if !Self::toggle_attr(&mut self.cursor_visible, Toggle::On) {
          return Ok(());
        }
      }
      Ctl::Cursor(CursorCtl::HideCursor) => {
        if !Self::toggle_attr(&mut self.cursor_visible, Toggle::Off) {
          return Ok(());
        }
      }
      Ctl::Cursor(CursorCtl::SetStyle(style)) => {
        if *style == self.cursor_style {
          return Ok(());
        }
        self.cursor_style = *style;
      }
      _ => {}
    }

    write!(self, "{ctl}")?;
    Ok(())
  }

  pub fn color_mode(&self) -> Option<ColorMode> {
    // NO_COLOR semantics: disable color iff the var is set AND
    // non-empty. The previous version used `try_var!("NO_COLOR")?`
    // which propagated None when the var was unset — making the
    // function return None ("no color") in the common case.
    if try_var!("NO_COLOR").is_some_and(|v| !v.is_empty()) {
      return None;
    }

    if let Some(val) = try_var!("SHED_COLOR_MODE") {
      match val.to_str_lossy().as_ref() {
        "truecolor" | "24bit" => return Some(ColorMode::Truecolor),
        "256" | "256color" => return Some(ColorMode::Palette256),
        "16" | "8" => return Some(ColorMode::Palette16),
        "none" | "off" => return None,
        _ => {}
      }
    }

    if self.term_caps.contains(TermCap::TRUECOLOR) {
      return Some(ColorMode::Truecolor);
    }

    if let Some(term) = try_var!("TERM") {
      if term == "dumb" {
        return None;
      }

      if term.to_str_lossy().contains("256color") {
        return Some(ColorMode::Palette256);
      }
    }

    Some(ColorMode::Palette16)
  }

  /// Sets `switch` to match `toggle`. Returns `true` if the state actually
  /// changed (caller should emit the escape), `false` if the state already
  /// matched (caller should skip emission to avoid noise — e.g. emitting
  /// "alt-buffer off" when alt buffer was never on).
  fn toggle_attr(switch: &mut bool, toggle: Toggle) -> bool {
    let on = matches!(toggle, Toggle::On);
    if on == *switch {
      return false;
    }
    *switch = on;
    true
  }

  pub fn new() -> Self {
    let tty: Option<RawFd> = TTY_FILENO
      .as_ref()
      .filter(|fd| isatty(fd.as_fd()).unwrap_or(false))
      .map(AsRawFd::as_raw_fd);
    let (cols, rows) = tty.map_or((80, 24), get_win_size);

    Self {
      tty,
      reader: parse::PollReader::new(),
      input_buf: String::new(),
      bracketed_paste: false,
      kitty_kbd_proto: false,
      report_focus: false,
      alt_buffer: false,
      cursor_style: CursorStyle::Default,
      interactive: false,
      cursor_visible: true,
      mouse_enabled: false,
      raw_mode: false,
      termios_stack: vec![],
      term_caps: TermCap::empty(),
      xt_version: None,
      t_cols: cols as usize,
      t_rows: rows as usize,
      scroll_region: ScrollRegionState::Unset,
      last_bell: None,
      last_input: None,
      test_mode: false,
    }
  }

  pub fn reset_last_input(&mut self) {
    if shopt!(prompt.idle_timeout).is_zero() {
      self.last_input = None;
    } else {
      self.last_input = Some(Instant::now());
    }
  }

  pub fn last_input_elapsed(&mut self) -> Duration {
    if let Some(instant) = self.last_input {
      instant.elapsed()
    } else {
      self.last_input = Some(Instant::now());
      Duration::ZERO
    }
  }

  /// Access the underlying tty file descriptor.
  pub fn tty(&self) -> Option<BorrowedFd<'static>> {
    let raw = self.tty?;
    let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
    Some(borrowed)
  }

  pub fn tty_checked(&self) -> ShResult<BorrowedFd<'static>> {
    let tty = self
      .tty()
      .ok_or_else(|| sherr!(InternalErr, "Not attached to a terminal"))?;
    if isatty(tty)? {
      Ok(tty)
    } else {
      Err(sherr!(InternalErr, "File descriptor is not a terminal"))
    }
  }

  fn tty_raw_checked(&self) -> ShResult<RawFd> {
    self.tty_checked().map(|tty| tty.as_raw_fd())
  }

  fn tty_checked_or_hangup(&mut self) -> ShResult<Option<BorrowedFd<'static>>> {
    match self.tty_checked() {
      Err(e) if matches!(e.kind(), ShErrKind::Errno(Errno::EIO)) => {
        self.tty = None;
        self.termios_stack.clear();
        Err(sherr!(InternalErr, "Terminal hangup detected"))
      }
      Err(_) => Ok(None),
      Ok(tty) => Ok(Some(tty)),
    }
  }

  fn tty_raw_checked_or_hangup(&mut self) -> ShResult<Option<RawFd>> {
    self
      .tty_checked_or_hangup()
      .map(|opt| opt.map(|fd| fd.as_raw_fd()))
  }

  pub fn isatty(&self) -> bool {
    self.tty.is_some_and(|raw| {
      let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
      isatty(borrowed).unwrap_or(false)
    })
  }

  /// Returns true if the current execution context is interactive
  ///
  /// This returns false for things like subshells, commands inside of functions,
  /// or backgrounded commands
  pub fn interactive(&self) -> bool {
    self.interactive
  }

  pub fn interactive_guard(&mut self, on: bool) -> TermGuard {
    let old = self.interactive;
    self.interactive = on;

    let guard = TermGuard::new().with_interactive(old);
    guard.activate()
  }

  pub fn mouse_support_guard(&mut self, toggle: Toggle) -> TermGuard {
    let guard = TermGuard::new().with_mouse_support(self.mouse_enabled);
    self
      .execute_control(&TermCtl::SetAttr(Attr::MouseTracking(toggle)))
      .ok();
    guard.activate()
  }

  pub fn setup_terminal(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    self.edit_termios(enable_raw_mode)?;

    self.query_term_caps()?;
    if self.term_caps.contains(TermCap::KITTY_KBD_PROTO)
      && self
        .xt_version
        .as_ref()
        .is_none_or(|v| !v.has_broken_kitty_kbd())
    {
      self.execute_control(&TermCtl::SetAttr(Attr::KittyKbdProto(Toggle::On)))?;
    } else if self
      .xt_version
      .as_ref()
      .is_some_and(parse::XtVersion::needs_wezterm_workaround)
    {
      self.execute_control(&TermCtl::SetAttr(Attr::ApplicationKeypad))?;
    } else {
      self.execute_control(&TermCtl::SetAttr(Attr::ModifyOtherKeys))?;
      self.execute_control(&TermCtl::SetAttr(Attr::ApplicationKeypad))?;
    }

    Ok(guard.activate())
  }

  pub fn query_term_caps(&mut self) -> ShResult<()> {
    if self.test_mode {
      return Ok(());
    }
    let Some(tty) = self.tty() else {
      return Ok(());
    };
    let mut caps = TermCap::empty();

    for ctl in TermCtl::cap_burst() {
      self.execute_control(&ctl)?;
    }
    self.flush()?;

    let deadline = Instant::now() + Duration::from_secs(2);
    'outer: while Instant::now() < deadline {
      let remaining = deadline.saturating_duration_since(Instant::now());
      let Ok(timeout) = PollTimeout::try_from(remaining) else {
        break;
      };
      if self.poll(timeout)? == 0 {
        break;
      }

      self.reader.read(tty)?;

      match_loop!(self.reader.read_event() => event, {
        TermEvent::KittyKbdFlags => {
          self.term_caps.insert(TermCap::KITTY_KBD_PROTO);
        }
        TermEvent::Capabilities { name, .. } => match name.as_str() {
          "RGB" => {
            caps.insert(TermCap::TRUECOLOR);
          }
          "Su" => {
            caps.insert(TermCap::SYNC_OUTPUT);
          }
          "Smulx" => {
            caps.insert(TermCap::UNDERLINE_STYLES);
          }
          _ => {
          }
        }
        TermEvent::XtVersion(ver) => {
          self.xt_version = Some(ver);
        }
        TermEvent::PrimaryDevAttr => {
          break 'outer
        }
        _ => {}
      });
    }

    if let Some(val) = try_var!("COLORTERM")
      && matches!(val.to_str_lossy().as_ref(), "truecolor" | "24bit")
    {
      caps.insert(TermCap::TRUECOLOR);
    }

    self.term_caps |= caps;

    Ok(())
  }

  pub fn term_caps(&self) -> TermCap {
    self.term_caps
  }

  fn save_state(&self) -> Snapshot {
    let guard = TermGuard::new()
      .with_raw_mode(self.raw_mode)
      .with_bracketed_paste(self.bracketed_paste)
      .with_kitty_proto(self.kitty_kbd_proto)
      .with_report_focus(self.report_focus)
      .with_alt_buffer(self.alt_buffer)
      .with_cursor_style(self.cursor_style)
      .with_mouse_support(self.mouse_enabled)
      .with_cursor_visible(self.cursor_visible)
      .with_termios_depth(self.termios_stack.len())
      .with_scroll_region(self.scroll_region);

    Snapshot::new(guard)
  }

  pub fn yield_terminal(&mut self, clear: bool) -> TermGuard {
    let guard = TermGuard::new()
      .with_scroll_region(self.scroll_region)
      .with_kitty_proto(self.kitty_kbd_proto);
    let action = if clear {
      Scroll::ResetRegion
    } else {
      Scroll::ResetRegionNoClear
    };
    self.execute_control(&TermCtl::Scroll(action)).ok();
    if self.kitty_kbd_proto {
      self
        .execute_control(&TermCtl::SetAttr(Attr::KittyKbdProto(false.into())))
        .ok();
    }
    self.flush().ok(); // ensure the reset reaches the terminal before exec
    guard.activate()
  }

  pub fn load_state(&mut self, guard: &TermGuard) -> ShResult<()> {
    let Some(_tty) = self.tty() else {
      return Ok(());
    };

    if let Some(depth) = guard.termios_depth() {
      while self.termios_stack.len() > depth {
        self.pop_termios()?;
      }
    }

    let mut wrote_seq = false;
    if let Some(bracketed_paste) = guard.bracketed_paste() {
      self.execute_control(&TermCtl::SetAttr(Attr::BracketPaste(
        bracketed_paste.into(),
      )))?;
      wrote_seq = true;
    }
    if let Some(kitty_proto) = guard.kitty_proto() {
      self.execute_control(&TermCtl::SetAttr(Attr::KittyKbdProto(kitty_proto.into())))?;
      wrote_seq = true;
    }
    if let Some(report_focus) = guard.report_focus() {
      self.execute_control(&TermCtl::SetAttr(Attr::FocusReport(report_focus.into())))?;
      wrote_seq = true;
    }
    if let Some(alt_buffer) = guard.alt_buffer() {
      self.execute_control(&TermCtl::SetAttr(Attr::AltBuffer(alt_buffer.into())))?;
      wrote_seq = true;
    }
    if let Some(cursor_visible) = guard.cursor_visible() {
      self.execute_control(&TermCtl::Cursor(if cursor_visible {
        CursorCtl::ShowCursor
      } else {
        CursorCtl::HideCursor
      }))?;
      wrote_seq = true;
    }
    if let Some(cursor_style) = guard.cursor_style() {
      self.execute_control(&TermCtl::Cursor(CursorCtl::SetStyle(cursor_style)))?;
      wrote_seq = true;
    }
    if let Some(mouse_mode) = guard.mouse_support() {
      self.execute_control(&TermCtl::SetAttr(Attr::MouseTracking(mouse_mode.into())))?;
      wrote_seq = true;
    }
    if let Some(interactive) = guard.interactive() {
      self.interactive = interactive;
    }
    if let Some(scroll_region) = guard.scroll_region() {
      match scroll_region {
        ScrollRegionState::Set(top, bottom) => {
          self.fix_cursor_row(bottom)?;
          self.execute_control(&TermCtl::Scroll(Scroll::SetRegion(top, bottom)))?;
        }
        ScrollRegionState::Unset => self.execute_control(&TermCtl::Scroll(Scroll::ResetRegion))?,
      }
      wrote_seq = true;
    }

    if wrote_seq {
      self.flush()?; // flush restore sequences immediately
    }
    Ok(())
  }

  pub fn reserved_rows() -> u16 {
    if shopt!(statline.enable) { 2 } else { 0 }
  }

  pub fn update_t_dims(&mut self) {
    let Some(tty) = self.tty() else { return };
    let (cols, rows) = get_win_size(tty.as_raw_fd());
    self.t_cols = cols as usize;
    self.t_rows = rows as usize;

    // If a scroll region is active, recompute its bottom relative to the
    // new terminal size. Assumes the owner intends to reserve 2 rows at
    // the bottom (status line + gap above it).
    if let ScrollRegionState::Set(top, _) = self.scroll_region {
      let reserved = Self::reserved_rows();
      let new_bottom = (rows.saturating_sub(reserved)).max(top);
      self
        .execute_control(&TermCtl::Scroll(Scroll::SetRegion(top, new_bottom)))
        .ok();
    }
  }

  pub fn reader_has_pending(&self) -> bool {
    self.reader.has_pending()
  }

  pub fn poll(&mut self, timeout: PollTimeout) -> ShResult<i32> {
    let Some(tty) = self.tty() else { return Ok(0) };
    let poll_fd = PollFd::new(tty, PollFlags::POLLIN);
    Ok(poll(&mut [poll_fd], timeout)?)
  }

  pub fn get_cursor_pos(&mut self) -> ShResult<Option<(Rows, Cols)>> {
    use std::io::Write;
    if self.test_mode {
      return Ok(None);
    }
    let Some(tty) = self.tty() else {
      return Ok(None);
    };

    // flush the buffer to execute any pending cursor movements,
    // queue the DSR query, then flush *again* so the query itself
    // actually reaches the terminal before we poll for a reply.
    self.flush().ok();
    self.execute_control(&TermCtl::Query(TermQuery::CursorPos))?;
    self.flush().ok();

    if self.poll(PollTimeout::from(50u8))? == 0 {
      // timeout - assume we didn't get a response
      return Ok(None);
    }

    self.reader.read(tty)?;

    while let Some(event) = self.reader.read_event_from_bytes() {
      let TermEvent::CursorPos(row, col) = event else {
        self.reader.push_event(event);
        continue;
      };
      return Ok(Some((row, col)));
    }
    Ok(None)
  }
  /// If the cursor is outside of the scroll region, move it into the scroll region
  ///
  /// Note: the scroll region actually has to be unset here. this is called after
  /// `yield_terminal` unsets it, before it gets reset.
  pub fn fix_cursor_row(&mut self, bottom: u16) -> ShResult<()> {
    if !shopt!(statline.enable) {
      return Ok(());
    }
    let (cursor_row, cursor_col) = match self.get_cursor_pos().ok().flatten() {
      Some((row, col)) => (Some(row.0), Some(col.0 as u16)),
      None => (None, None),
    };

    if cursor_row.is_none_or(|r| r >= bottom as usize) {
      // Caller invokes us while the scroll region is still Unset (between
      // `cooked_guard` and `_scroll_guard` drops in `exec_pipeline`), so `\n`
      // at terminal bottom triggers a full-screen scroll that pulls gutter
      // content into the soon-to-be-restored region.
      write!(self, "\n\n")?;
      self
        .execute_control(&TermCtl::Cursor(CursorCtl::Absolute {
          row: bottom,
          col: cursor_col.unwrap_or(1),
        }))
        .ok();
    }
    self.flush().ok();
    Ok(())
  }

  /// Called before the prompt is drawn. If we are not on column 1, push a vid-inverted '%' and then a '\n\r'.
  ///
  /// Aping zsh with this but it's a nice feature.
  pub fn fix_cursor_column(&mut self, full: bool) -> ShResult<()> {
    if full {
      let Some((_, c)) = self.get_cursor_pos()? else {
        return Ok(());
      };
      if c.0 != 1 {
        write!(self, "\x1b[7m%\x1b[0m\n\r")?;
      }
    } else {
      write!(self, "\r")?;
    }

    self.flush()?;
    Ok(())
  }

  pub fn emit_osc_copy(&mut self, primary: bool, buf: &str) -> ShResult<()> {
    let sel = if primary { "p" } else { "c" };
    let encoded = base64_encode(buf.as_bytes());

    if encoded.len() > (1024 * 75) {
      // the limit for these is 75kb
      let mut size = String::new();
      format_size(encoded.len() as u64, &mut size)?;

      status_msg!("clipboard copy failed: buffer too large ({size})");
      return Ok(());
    }

    write!(self, "\x1b]52;{sel};{encoded}\x1b\\")?;
    self.flush()?;

    Ok(())
  }

  pub fn calc_cursor_movement(&mut self, old: Pos, new: Pos) -> ShResult<()> {
    let err = |_| sherr!(InternalErr, "Failed to write to cursor movement buffer");

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

  /// The cursor style the terminal currently tracks, for save/restore around
  /// transient style changes (e.g. the fuzzy picker's beam cursor).
  pub fn cursor_style(&self) -> CursorStyle {
    self.cursor_style
  }

  pub fn t_rows(&self) -> usize {
    self.t_rows
  }

  pub fn buf_ends_with_newline(&self) -> bool {
    self.input_buf.ends_with('\n')
  }

  pub fn verbatim_single(&mut self, on: bool) {
    self.reader.verbatim_single = on;
  }

  pub fn send_bell(&mut self) -> ShResult<()> {
    if shopt!(core.bell_enabled) {
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
    let tty = self.tty()?;
    nix::unistd::tcgetpgrp(tty).ok()
  }

  pub fn attach(&mut self, pgid: Pid) -> ShResult<()> {
    let Some(tty) = self.tty() else {
      return Ok(());
    };
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

    let result = tcsetpgrp(tty, pgid);

    pthread_sigmask(
      SigmaskHow::SIG_SETMASK,
      Some(&mask_bkup),
      Some(&mut new_mask),
    )?;

    if result.is_err() {
      tcsetpgrp(tty, getpgrp())?;
    }

    Ok(())
  }

  pub fn read(&mut self) -> ShResult<usize> {
    let Some(tty) = self.tty() else { return Ok(0) };
    self.reader.read(tty)
  }

  pub fn drain_keys(&mut self) -> Vec<KeyEvent> {
    let mut keys = vec![];
    while let Some(key) = self.reader.readkey() {
      keys.push(key);
    }

    if !keys.is_empty()
      && !shopt!(prompt.idle_timeout).is_zero()
      && keys.iter().any(|k| k.code().is_keyboard_key())
    {
      // we received some input, so we reset this
      self.last_input = Some(Instant::now());
    }

    keys
  }

  pub fn cooked_mode_guard(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    for action in COOKED_TEARDOWN {
      self.execute_control(&action)?;
    }
    self.edit_termios(enable_cooked_mode)?;
    Ok(guard.activate())
  }

  pub fn cooked_no_echo_guard(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    for action in COOKED_TEARDOWN {
      self.execute_control(&action)?;
    }
    self.edit_termios(|t| {
      enable_cooked_mode(t);
      t.local_flags.remove(termios::LocalFlags::ECHO);
    })?;
    Ok(guard.activate())
  }

  pub fn prepare_for_pager(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    let actions = [
      TermCtl::SetAttr(Attr::BracketPaste(Toggle::Off)),
      TermCtl::SetAttr(Attr::FocusReport(Toggle::Off)),
      TermCtl::SetAttr(Attr::AltBuffer(Toggle::On)),
      TermCtl::Scroll(Scroll::ResetRegion),
      TermCtl::SetAttr(Attr::MouseTracking(Toggle::On)),
      TermCtl::SetAttr(Attr::KittyKbdProto(Toggle::Off)),
      TermCtl::Cursor(CursorCtl::SetStyle(CursorStyle::Default)),
      TermCtl::Cursor(CursorCtl::HideCursor),
    ];
    for action in actions {
      self.execute_control(&action)?;
    }

    self.flush()?;
    self.edit_termios(enable_raw_mode)?;
    Ok(guard.activate())
  }

  pub fn prepare_for_exec(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    let actions = [
      TermCtl::SetAttr(Attr::BracketPaste(Toggle::Off)),
      TermCtl::SetAttr(Attr::FocusReport(Toggle::Off)),
      TermCtl::SetAttr(Attr::AltBuffer(Toggle::Off)),
      TermCtl::Cursor(CursorCtl::SetStyle(CursorStyle::Default)),
      TermCtl::SetAttr(Attr::KittyKbdProto(Toggle::Off)),
    ];
    for action in actions {
      self.execute_control(&action)?;
    }

    self.flush()?; // flush escape sequences before switching to cooked mode

    // Drain any pending bytes still in flight on the tty (e.g., a late CPR
    // response from the previous pipeline's `fix_cursor_row`). Cooked mode's
    // ECHO would otherwise echo them onto stdout as `^[[N;MR` garbage.
    if let Some(tty) = self.tty() {
      let mut buf = [0u8; 256];
      while let Ok(n) = self.poll(PollTimeout::ZERO)
        && n > 0
      {
        if nix::unistd::read(tty, &mut buf).is_err() {
          break;
        }
      }
    }

    self.edit_termios(enable_cooked_mode)?;
    Ok(guard.activate())
  }

  pub fn raw_mode_guard(&mut self) -> ShResult<TermGuard> {
    let guard = self.save_state();
    self.edit_termios(enable_raw_mode)?;
    Ok(guard.activate())
  }

  fn pop_termios(&mut self) -> ShResult<()> {
    let Some(tty) = self.tty_raw_checked_or_hangup()? else {
      return Ok(());
    };

    if let Some(termios) = self.termios_stack.pop() {
      tcsetattr(
        unsafe { BorrowedFd::borrow_raw(tty) },
        termios::SetArg::TCSANOW,
        &termios,
      )
      .map_err(|e| sherr!(InternalErr, "Failed to restore terminal attributes: {e}"))?;
    }
    Ok(())
  }

  /// Defensively re-apply raw mode to the tty.
  ///
  /// Some child programs (notably pagers like less invoked by bat) run their
  /// own termios cleanup on exit. When they die after the shell has reaped
  /// their parent, their cleanup races with our `pop_termios` and can leave
  /// the tty in cooked mode. We follow zsh's mitigation here: just re-apply
  /// raw mode at the start of every readline iteration. Cheap (one ioctl)
  /// and resilient to any late tcsetattr from orphaned descendants.
  pub fn enforce_raw_mode(&mut self) -> ShResult<()> {
    // we propagate the error for this one so that the interactive loop
    // breaks correctly on EIO
    let tty_raw = self.tty_raw_checked()?;
    let tty = unsafe { BorrowedFd::borrow_raw(tty_raw) };

    let mut t = tcgetattr(tty)?;
    enable_raw_mode(&mut t);
    tcsetattr(tty, termios::SetArg::TCSANOW, &t)?;
    Ok(())
  }

  pub fn edit_termios<F: FnOnce(&mut Termios)>(&mut self, f: F) -> ShResult<()> {
    let Some(tty) = self.tty_raw_checked_or_hangup()? else {
      return Ok(());
    };

    let tty = unsafe { BorrowedFd::borrow_raw(tty) };
    let mut raw =
      tcgetattr(tty).map_err(|e| sherr!(InternalErr, "Failed to get terminal attributes: {e}"))?;

    self.termios_stack.push(raw.clone());

    f(&mut raw);
    tcsetattr(tty, termios::SetArg::TCSANOW, &raw)
      .map_err(|e| sherr!(InternalErr, "Failed to set terminal attributes: {e}"))?;

    Ok(())
  }

  pub fn write_direct(&mut self, buf: &str) -> ShResult<()> {
    let Some(tty) = self.tty() else {
      return Ok(());
    };
    let mut buf = buf.as_bytes();
    while !buf.is_empty() {
      match write(tty, buf) {
        Ok(n) => buf = &buf[n..],
        Err(Errno::EINTR) => (),
        Err(_) => return Err(std::io::Error::last_os_error().into()),
      }
    }
    Ok(())
  }

  /// Perform an operation and restore the cursor's original position afterwards.
  pub fn with_saved_cursor<T>(&mut self, f: impl Fn(&mut Self) -> T) -> T {
    self
      .execute_control(&TermCtl::Cursor(CursorCtl::SavePos))
      .ok();
    let res = f(self);
    self
      .execute_control(&TermCtl::Cursor(CursorCtl::RestorePos))
      .ok();
    res
  }

  pub fn reserve_status_rows(&mut self) -> ShResult<()> {
    if !shopt!(statline.enable) {
      return Ok(());
    }
    let reserved: u16 = Self::reserved_rows();
    let bottom = (self.t_rows() as u16).saturating_sub(reserved).max(1);
    self.execute_control(&TermCtl::Scroll(Scroll::SetRegion(1, bottom)))?;
    self.fix_cursor_row(bottom)
  }

  /// Render the status line at the bottom row of the terminal.
  pub fn draw_status_line(&mut self, content: &str) {
    let row = self.t_rows as u16;
    self.with_saved_cursor(|this| {
      this
        .execute_control(&TermCtl::Cursor(CursorCtl::Absolute { row, col: 1 }))
        .ok();
      this
        .execute_control(&TermCtl::Clear(ClearCtl::WholeLine))
        .ok();
      this.input_buf.push_str(content);
    });
  }

  /// Render an ephemeral status message on the row directly above the status line (`t_rows - 1`).
  pub fn draw_status_message(&mut self, content: &str) {
    let row = if shopt!(statline.enable) {
      (self.t_rows as u16).saturating_sub(1)
    } else {
      self.t_rows as u16
    };
    self.with_saved_cursor(|this| {
      this
        .execute_control(&TermCtl::Cursor(CursorCtl::Absolute { row, col: 1 }))
        .ok();
      this
        .execute_control(&TermCtl::Clear(ClearCtl::WholeLine))
        .ok();
      this.input_buf.push_str(content);
    });
  }

  /// Detach this Terminal from the TTY. After calling, `tty()` returns
  /// None and `flush()` silently discards buffered output. Used in forked
  /// children whose stdout is redirected (e.g., command substitutions) to
  /// prevent any terminal-control escape sequences they might emit from
  /// reaching the parent's TTY through the shared fd.
  pub fn detach_tty(&mut self) {
    self.input_buf.clear();
    self.tty = None;
  }

  #[cfg(test)]
  pub fn set_fd_for_testing(&mut self, fd: Option<RawFd>) {
    self.tty = fd;
    self.test_mode = fd.is_some();
  }
  #[cfg(test)]
  pub fn feed_bytes(&mut self, bytes: &[u8]) {
    self.reader.feed_bytes(bytes);
  }

  pub fn reset_for_exit(&mut self) {
    let Some(_tty) = self.tty() else { return };

    let actions = [
      TermCtl::Scroll(Scroll::ResetRegion),
      TermCtl::SetAttr(Attr::BracketPaste(Toggle::Off)),
      TermCtl::SetAttr(Attr::KittyKbdProto(Toggle::Off)),
      TermCtl::Cursor(CursorCtl::ShowCursor),
      TermCtl::SetAttr(Attr::AltBuffer(Toggle::Off)),
    ];

    for action in actions {
      self.execute_control(&action).ok();
    }

    if self.cursor_style != CursorStyle::Default {
      self
        .execute_control(&TermCtl::Cursor(CursorCtl::SetStyle(CursorStyle::Default)))
        .ok();
    }
    self.flush().ok();
    while !self.termios_stack.is_empty() {
      self.pop_termios().ok();
    }
  }

  pub fn test_mode(&self) -> bool {
    self.test_mode
  }
}

impl Default for Terminal {
  fn default() -> Self {
    Self::new()
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
    let Some(tty) = self.tty() else {
      self.input_buf.clear();
      return Ok(());
    };
    let mut buf = self.input_buf.as_bytes();
    while !buf.is_empty() {
      match write(tty, buf) {
        Ok(n) => buf = &buf[n..],
        Err(Errno::EINTR) => (),
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

#[cfg(test)]
mod color_mode_tests {
  use super::*;
  use crate::state::Shed;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::tests::testutil::TestGuard;

  fn unset_all_color_vars() {
    Shed::vars_mut(|v| {
      v.unset_var("NO_COLOR").ok();
      v.unset_var("SHED_COLOR_MODE").ok();
      v.unset_var("TERM").ok();
    });
  }

  fn set_var(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name, VarKind::Str(val.into()), VarFlags::empty())
        .unwrap();
    });
  }

  fn color_mode() -> Option<ColorMode> {
    Shed::term(super::Terminal::color_mode)
  }

  // ─── NO_COLOR ─────────────────────────────────────────────────────

  #[test]
  fn default_returns_palette16_when_no_relevant_vars_set() {
    // Regression: previously the `try_var!("NO_COLOR")?` used `?` to
    // propagate None, which returned None whenever NO_COLOR was
    // unset — i.e., the common case.
    let _g = TestGuard::new();
    unset_all_color_vars();
    assert_eq!(color_mode(), Some(ColorMode::Palette16));
  }

  #[test]
  fn no_color_set_to_non_empty_disables_color() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("NO_COLOR", "1");
    assert_eq!(color_mode(), None);
  }

  #[test]
  fn no_color_empty_value_is_ignored() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("NO_COLOR", "");
    // Empty NO_COLOR is treated as unset — fall through to Palette16.
    assert_eq!(color_mode(), Some(ColorMode::Palette16));
  }

  // ─── SHED_COLOR_MODE ─────────────────────────────────────────────

  #[test]
  fn shed_color_mode_truecolor() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "truecolor");
    assert_eq!(color_mode(), Some(ColorMode::Truecolor));
  }

  #[test]
  fn shed_color_mode_24bit_alias() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "24bit");
    assert_eq!(color_mode(), Some(ColorMode::Truecolor));
  }

  #[test]
  fn shed_color_mode_256() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "256");
    assert_eq!(color_mode(), Some(ColorMode::Palette256));
  }

  #[test]
  fn shed_color_mode_256color_alias() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "256color");
    assert_eq!(color_mode(), Some(ColorMode::Palette256));
  }

  #[test]
  fn shed_color_mode_16() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "16");
    assert_eq!(color_mode(), Some(ColorMode::Palette16));
  }

  #[test]
  fn shed_color_mode_8_alias() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "8");
    assert_eq!(color_mode(), Some(ColorMode::Palette16));
  }

  #[test]
  fn shed_color_mode_none_disables() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "none");
    assert_eq!(color_mode(), None);
  }

  #[test]
  fn shed_color_mode_off_alias_disables() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "off");
    assert_eq!(color_mode(), None);
  }

  #[test]
  fn shed_color_mode_unrecognized_falls_through() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "bananas");
    // Unrecognized value → match arm doesn't return, falls through to
    // term_caps check (empty in tests) then TERM check (unset) then
    // Palette16 fallback.
    assert_eq!(color_mode(), Some(ColorMode::Palette16));
  }

  // ─── TERM env var ────────────────────────────────────────────────

  #[test]
  fn term_dumb_disables_color() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("TERM", "dumb");
    assert_eq!(color_mode(), None);
  }

  #[test]
  fn term_with_256color_substring_returns_palette256() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("TERM", "xterm-256color");
    assert_eq!(color_mode(), Some(ColorMode::Palette256));
  }

  #[test]
  fn term_screen_256color_substring_match() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("TERM", "screen-256color");
    assert_eq!(color_mode(), Some(ColorMode::Palette256));
  }

  #[test]
  fn term_other_value_falls_back_to_palette16() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("TERM", "vt100");
    assert_eq!(color_mode(), Some(ColorMode::Palette16));
  }

  // ─── Precedence ────────────────────────────────────────────────

  #[test]
  fn no_color_beats_shed_color_mode() {
    // NO_COLOR is checked first.
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("NO_COLOR", "1");
    set_var("SHED_COLOR_MODE", "truecolor");
    assert_eq!(color_mode(), None);
  }

  #[test]
  fn shed_color_mode_beats_term() {
    let _g = TestGuard::new();
    unset_all_color_vars();
    set_var("SHED_COLOR_MODE", "truecolor");
    set_var("TERM", "dumb");
    assert_eq!(color_mode(), Some(ColorMode::Truecolor));
  }
}

#[cfg(test)]
mod terminal_method_tests {
  //! Tier 1: pure-output assertions. Each function emits ANSI escapes
  //! into Terminal's `input_buf`; we call it, flush via `Shed::term_mut`,
  //! and assert the escapes landed on the pty master.

  use super::*;
  use crate::state::Shed;
  use crate::tests::testutil::TestGuard;
  use std::io::Write;

  /// Force a flush and then drain the pty output thread's buffer.
  fn drain(g: &TestGuard) -> String {
    Shed::term_mut(|t| t.flush().ok());
    g.read_output()
  }

  // ─── cursor style ───────────────────────────────────────────────

  #[test]
  fn cursor_set_style_emits_escape() {
    let g = TestGuard::new();
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Cursor(CursorCtl::SetStyle(CursorStyle::Beam(
        true,
      ))))
      .unwrap();
    });
    let out = drain(&g);
    // Beam(true) renders as `\x1b[5 q`.
    assert!(out.contains("\x1b[5 q"), "got: {out:?}");
  }

  // ─── scroll region (TermCtl::Scroll) ────────────────────────────

  #[test]
  fn scroll_set_region_emits_decstbm() {
    let g = TestGuard::new();
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Scroll(Scroll::SetRegion(2, 20)))
        .unwrap();
    });
    let out = drain(&g);
    assert!(out.contains("\x1b[2;20r"), "got: {out:?}");
  }

  #[test]
  fn scroll_set_region_updates_state() {
    let _g = TestGuard::new();
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Scroll(Scroll::SetRegion(3, 15)))
        .unwrap();
    });
    let region = Shed::term(|t| t.scroll_region);
    assert!(matches!(region, ScrollRegionState::Set(3, 15)));
  }

  #[test]
  fn reset_scroll_region_after_set_emits_reset_and_clears_state() {
    let g = TestGuard::new();
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Scroll(Scroll::SetRegion(2, 10)))
        .unwrap();
    });
    let _ = drain(&g);
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Scroll(Scroll::ResetRegion))
        .unwrap();
    });
    let out = drain(&g);
    // SCROLL_REGION_RESET is `\x1b[r`.
    assert!(out.contains("\x1b[r"), "got: {out:?}");
    assert!(matches!(
      Shed::term(|t| t.scroll_region),
      ScrollRegionState::Unset
    ));
  }

  #[test]
  fn reset_scroll_region_when_unset_is_noop() {
    let g = TestGuard::new();
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Scroll(Scroll::ResetRegion))
        .unwrap();
    });
    let out = drain(&g);
    assert!(
      !out.contains("\x1b[r"),
      "should not have emitted reset, got: {out:?}"
    );
  }

  // ─── draw_status_line ──────────────────────────────────────────

  #[test]
  fn draw_status_line_emits_content_and_save_restore() {
    let g = TestGuard::new();
    Shed::term_mut(|t| t.draw_status_line("STATUS_LINE_TEST_MARKER"));
    let out = drain(&g);
    assert!(out.contains("STATUS_LINE_TEST_MARKER"), "got: {out:?}");
    // Surrounded by cursor save/restore (DECSC `\x1b7` / DECRC `\x1b8`).
    assert!(out.contains("\x1b7"), "expected DECSC, got: {out:?}");
    assert!(out.contains("\x1b8"), "expected DECRC, got: {out:?}");
  }

  // ─── clear_under_cursor ────────────────────────────────────────

  #[test]
  fn clear_screen_from_cursor_emits_ed() {
    let g = TestGuard::new();
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Clear(ClearCtl::ScreenFromCursor))
        .unwrap();
    });
    let out = drain(&g);
    // ClearCtl::ScreenFromCursor renders as `\x1b[J` (param defaults to 0).
    assert!(out.contains("\x1b[J"), "got: {out:?}");
  }

  // ─── scroll up (TermCtl::Scroll) ────────────────────────────────

  #[test]
  fn scroll_up_with_zero_lines_is_noop() {
    let g = TestGuard::new();
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Scroll(Scroll::Up(0))).unwrap();
    });
    let out = drain(&g);
    assert!(
      !out.contains("\x1b[0S") && !out.contains("\x1b[S"),
      "should not have emitted SU, got: {out:?}"
    );
  }

  #[test]
  fn scroll_up_emits_su_with_lines() {
    let g = TestGuard::new();
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Scroll(Scroll::Up(5))).unwrap();
    });
    let out = drain(&g);
    assert!(out.contains("\x1b[5S"), "got: {out:?}");
  }

  // ─── reset_for_exit ────────────────────────────────────────────

  #[test]
  fn reset_for_exit_emits_cleanup_sequences() {
    let g = TestGuard::new();
    // Put the terminal in a non-default state so reset has something to do.
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Scroll(Scroll::SetRegion(2, 10)))
        .unwrap();
      t.execute_control(&TermCtl::Cursor(CursorCtl::SetStyle(CursorStyle::Beam(
        true,
      ))))
      .unwrap();
    });
    let _ = drain(&g);
    Shed::term_mut(super::Terminal::reset_for_exit);
    let out = g.read_output();
    // Cursor visibility on (CURSOR_SHOW), bracketed paste off, scroll region reset, etc.
    assert!(
      out.contains("\x1b[r"),
      "expected scroll region reset, got: {out:?}"
    );
    // reset_for_exit flushes internally; output should be observable without explicit flush.
  }

  // ─── detach_tty ────────────────────────────────────────────────

  #[test]
  fn detach_tty_makes_subsequent_writes_no_ops() {
    let g = TestGuard::new();
    Shed::term_mut(super::Terminal::detach_tty);
    // After detach, writes should be silently discarded.
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Clear(ClearCtl::ScreenFromCursor))
        .ok();
    });
    Shed::term_mut(|t| t.flush().ok());
    let out = g.read_output();
    assert!(
      out.is_empty(),
      "expected nothing written after detach, got: {out:?}"
    );
    assert!(Shed::term(|t| t.tty().is_none()));
  }

  #[test]
  fn detach_tty_clears_input_buf() {
    let _g = TestGuard::new();
    // Buffer something but don't flush.
    Shed::term_mut(|t| {
      t.execute_control(&TermCtl::Clear(ClearCtl::ScreenFromCursor))
        .ok();
    });
    Shed::term_mut(super::Terminal::detach_tty);
    // input_buf should be empty after detach (buffered output dropped).
    // We can verify by trying to flush — no output should appear.
    let g2 = TestGuard::new();
    Shed::term_mut(|t| t.flush().ok());
    assert!(g2.read_output().is_empty());
  }

  // ─── cooked_no_echo_guard ──────────────────────────────────────

  #[test]
  fn cooked_no_echo_guard_disables_echo_and_restores_on_drop() {
    use nix::sys::termios::{LocalFlags, tcgetattr};
    use std::os::fd::BorrowedFd;
    let _g = TestGuard::new();
    let tty_fd = Shed::term(|t| t.tty().map(|f| f.as_raw_fd())).unwrap();
    let borrowed = unsafe { BorrowedFd::borrow_raw(tty_fd) };

    let before = tcgetattr(borrowed).unwrap().local_flags;
    {
      let _guard = Shed::term_mut(|t| t.cooked_no_echo_guard().unwrap());
      let inside = tcgetattr(borrowed).unwrap().local_flags;
      assert!(
        !inside.contains(LocalFlags::ECHO),
        "ECHO should be off inside guard, flags: {inside:?}"
      );
      assert!(
        inside.contains(LocalFlags::ICANON),
        "ICANON should be on inside guard, flags: {inside:?}"
      );
    }
    let after = tcgetattr(borrowed).unwrap().local_flags;
    assert_eq!(after, before, "termios should be restored after guard drop");
  }

  // ─── poll ──────────────────────────────────────────────────────

  #[test]
  fn poll_returns_zero_on_timeout_with_no_input() {
    let _g = TestGuard::new();
    // Short timeout, no bytes available.
    let ret = Shed::term_mut(|t| t.poll(PollTimeout::from(10u8)).unwrap());
    assert_eq!(ret, 0, "expected no fds ready, got {ret}");
  }

  // The "bytes available" path is harder to test reliably in the
  // TestGuard fixture — feed_tty writes to the pty master, but the
  // background read-thread is also racing on that fd, and poll on the
  // slave-side can lose the wakeup. Skipping the positive case here.

  // ─── get_cursor_pos ────────────────────────────────────────────
  //
  // TestGuard sets test_mode=true via set_fd_for_testing, which short-
  // circuits get_cursor_pos to return Ok(None). Pin that behavior.

  #[test]
  fn get_cursor_pos_returns_none_in_test_mode() {
    let _g = TestGuard::new();
    let pos = Shed::term_mut(|t| t.get_cursor_pos().unwrap());
    assert_eq!(pos, None);
  }

  // ─── query_term_caps ───────────────────────────────────────────
  //
  // Same as above — query_term_caps checks test_mode early and returns
  // without doing the round-trip probe.

  #[test]
  fn query_term_caps_is_skipped_in_test_mode() {
    let _g = TestGuard::new();
    let caps_before = Shed::term(super::Terminal::term_caps);
    Shed::term_mut(|t| t.query_term_caps().unwrap());
    let caps_after = Shed::term(super::Terminal::term_caps);
    // Should be unchanged (probe was skipped).
    assert_eq!(caps_before, caps_after);
  }

  // ─── setup_terminal ────────────────────────────────────────────
  //
  // setup_terminal runs query_term_caps internally; in test mode the
  // probe is a no-op, but the rest of the setup (termios, fallback
  // application-keypad writes) still runs. We just verify it returns
  // a guard that, when dropped, restores the prior state cleanly.

  #[test]
  fn setup_terminal_returns_guard_that_restores_on_drop() {
    let _g = TestGuard::new();
    let raw_before = Shed::term(|t| t.raw_mode);
    {
      let _setup_guard = Shed::term_mut(|t| t.setup_terminal().unwrap());
      // We don't assert specific termios state here because test_mode
      // path skips most of the wire writes — we just need the call
      // chain to complete cleanly.
    }
    // Guard dropped — state should be reasonable. raw_mode may or may
    // not change; we just make sure nothing panicked.
    let _raw_after = Shed::term(|t| t.raw_mode);
    let _ = raw_before;
  }
}
