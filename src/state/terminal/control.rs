#![allow(dead_code)]
//! Typed terminal control sequences and their escape code formatting.
//!
//! [`TermCtl`] is used by [`Terminal::execute_control`](super::Terminal::execute_control), which
//! writes the display implementations directly to the terminal, with
//! zero intermediate allocations. For external consumers, the [`exec_term!`](crate::exec_term!)
//! and [`queue_term!`](crate::queue_term!) macros can be used for ergonomic access
//! to this API.
//!
//! The reason why these macros can only be used by external consumers,
//! is because they call [`Shed::term_mut`](super::Shed::term_mut) internally, which
//! will panic if called inside of any of the [`Terminal`](super::Terminal) methods.
//!
//! Starting to look an awful lot like crossterm around here...

use std::fmt::Display;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Toggle {
  On,
  Off,
}

impl From<Toggle> for char {
  /// Conversion to DEC private mode "high/low" (set/reset).
  ///
  /// `Toggle::On` returns `h`,
  /// `Toggle::Off` returns `l`.
  fn from(value: Toggle) -> Self {
    match value {
      Toggle::On => 'h',  // high
      Toggle::Off => 'l', // low
    }
  }
}

impl From<bool> for Toggle {
  fn from(b: bool) -> Self {
    if b { Toggle::On } else { Toggle::Off }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TermCtl {
  Cursor(CursorCtl),
  Clear(ClearCtl),
  PrintChar(char),
  SetAttr(Attr),
  Scroll(Scroll),
  Osc(OscCtl),
  Query(TermQuery),

  /// Output sync start marker
  ///
  /// Terminals that support output sync use these to buffer output
  /// The terminal will buffer input until it sees the end marker,
  /// then draw/execute everything at once.
  SyncStart,
  /// Output sync end marker
  ///
  /// See [`TermCtl::SyncStart`]
  SyncEnd,

  RingBell,
}

impl TermCtl {
  pub(crate) fn cap_burst() -> Vec<Self> {
    vec![
      Self::Query(TermQuery::KittyKbdFlags),
      Self::Query(TermQuery::Capability(CapQuery::SyncOutput)),
      Self::Query(TermQuery::Capability(CapQuery::TrueColor)),
      Self::Query(TermQuery::Capability(CapQuery::UnderlineColor)),
      Self::Query(TermQuery::Version),
      Self::Query(TermQuery::DeviceAttrs),
    ]
  }
}

impl Display for TermCtl {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Cursor(ctl) => ctl.fmt(f),
      Self::Clear(ctl) => ctl.fmt(f),
      Self::PrintChar(c) => write!(f, "{c}"),
      Self::SetAttr(ctl) => ctl.fmt(f),
      Self::Scroll(ctl) => ctl.fmt(f),
      Self::Osc(ctl) => ctl.fmt(f),
      Self::Query(ctl) => ctl.fmt(f),
      Self::SyncStart => write!(f, "\u{1b}[?2026h"),
      Self::SyncEnd => write!(f, "\u{1b}[?2026l"),
      Self::RingBell => write!(f, "\x07"),
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Scroll {
  Up(u16),
  Down(u16),
  InsertLines(u16),
  DeleteLines(u16),

  /// Set the size of the scroll region
  ///
  /// Any rows outside the region are unaffected by scroll operations, and the cursor is allowed to move freely in and out of the region. The top and bottom parameters are 1-indexed.
  /// We use this for `shed`'s status line.
  SetRegion(u16, u16),
  /// Reset the scroll region
  ///
  /// See [`Scroll::SetRegion`].
  ResetRegion,
  ResetRegionNoClear, // Same, without clearing the area outside the scroll region
}

impl Display for Scroll {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      // Count-based scroll ops: 0 = no-op, emit nothing.
      Scroll::Up(0) | Scroll::Down(0) | Scroll::InsertLines(0) | Scroll::DeleteLines(0) => Ok(()),
      Scroll::Up(n) => write!(f, "\x1b[{n}S"),
      Scroll::Down(n) => write!(f, "\x1b[{n}T"),
      Scroll::SetRegion(top, bottom) => write!(f, "\x1b[{};{}r", (*top).max(1), (*bottom).max(1)),
      Scroll::ResetRegionNoClear | Scroll::ResetRegion => write!(f, "\x1b[r"),
      Scroll::InsertLines(n) => write!(f, "\x1b[{n}L"),
      Scroll::DeleteLines(n) => write!(f, "\x1b[{n}M"),
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TermQuery {
  CursorPos,
  KittyKbdFlags,
  Version,
  DeviceAttrs,

  /// `XTGETTCAP` queries. The names come from `terminfo`, and are hex-encoded
  /// per the `XTGETTCAP` protocol.
  Capability(CapQuery),
}

impl Display for TermQuery {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      TermQuery::CursorPos => write!(f, "\x1b[6n"),
      TermQuery::KittyKbdFlags => write!(f, "\x1b[?u"),
      TermQuery::Version => write!(f, "\x1b[>q"),
      TermQuery::DeviceAttrs => write!(f, "\x1b[c"),
      TermQuery::Capability(cap) => {
        write!(f, "\x1bP+q")?;
        cap.fmt(f)?;
        write!(f, "\x1b\\")
      }
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CapQuery {
  SyncOutput,
  TrueColor,
  /// Styled underlines (`Smulx`). Terminals advertising this also support an
  /// underline color independent of the foreground (SGR 58/59).
  UnderlineColor,
}

impl Display for CapQuery {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let name = match self {
      Self::SyncOutput => "Su",
      Self::TrueColor => "RGB",
      Self::UnderlineColor => "Smulx",
    };

    // these have to be hex encoded
    for b in name.bytes() {
      write!(f, "{b:02X}")?;
    }

    Ok(())
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum OscCtl {
  /// Prompt start marker
  ///
  /// Supported by some modern terminals, used for navigation via hotkeys
  /// For instance, `Ctrl+Shift+(Z/X)` in kitty jumps to the previous prompt.
  PromptStart,
  /// Prompt end marker
  ///
  /// See [`OscCtl::PromptStart`]
  PromptEnd,

  /// Execution start marker
  ///
  /// Supported by some modern terminals, used to signal the start of a command's output alongside the [`OscCtl::ExecEnd`] marker.
  ExecStart,
  /// Execution end marker, with exit code.
  ///
  /// See [`OscCtl::ExecStart`].
  ExecEnd(i32), // exit code
}

impl Display for OscCtl {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      OscCtl::PromptStart => write!(f, "\x1b]133;A\x07"),
      OscCtl::PromptEnd => write!(f, "\x1b]133;B\x07"),
      OscCtl::ExecStart => write!(f, "\x1b]133;C\x07"),
      OscCtl::ExecEnd(code) => write!(f, "\x1b]133;D;{code}\x07"),
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Attr {
  FocusReport(Toggle),
  AltBuffer(Toggle),
  SyncOutput(Toggle),
  BracketPaste(Toggle),
  KittyKbdProto(Toggle),
  MouseTracking(Toggle),

  /// Lets terminals distinguish Ctrl+<key> combos that otherwise collapse into a single byte
  ModifyOtherKeys,
  /// Swaps numpad keys into a different escape set
  ApplicationKeypad,
}

impl Display for Attr {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Attr::ModifyOtherKeys => write!(f, "\x1b[>4;1m"),
      Attr::ApplicationKeypad => write!(f, "\x1b="),

      // Kitty's flags use a stack instead of the usual 'h/l' toggles.
      // In theory this arbitrarily nested child processes ask for what flags they want.
      // Admittedly we don't do a good job of tracking this currently, so we just
      // clobber the stack with `\x1b[=0`.
      // FIXME: Actually handle this properly, somehow.
      Attr::KittyKbdProto(Toggle::On) => write!(f, "\x1b[>17u"),
      Attr::KittyKbdProto(Toggle::Off) => write!(f, "\x1b[=0u"),
      Attr::FocusReport(toggle) => write!(f, "\x1b[?1004{}", char::from(*toggle)),
      Attr::AltBuffer(toggle) => write!(f, "\x1b[?1049{}", char::from(*toggle)),
      Attr::SyncOutput(toggle) => write!(f, "\x1b[?2026{}", char::from(*toggle)),
      Attr::BracketPaste(toggle) => write!(f, "\x1b[?2004{}", char::from(*toggle)),
      Attr::MouseTracking(toggle) => {
        // Mouse tracking has three related modes that we want to toggle together:
        // 1000: basic tracking (press/release)
        // 1003: all events (including mouse move)
        // 1006: SGR extended mode (coordinates in decimal, not hex)
        let ch = char::from(*toggle);
        write!(f, "\x1b[?1000{ch}\x1b[?1003{ch}\x1b[?1006{ch}")
      }
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ClearCtl {
  LineToEnd,
  LineToStart,
  WholeLine,
  ScreenFromCursor,
  ScreenToCursor,
  WholeScreen,
  ClearScrollback, // xterm thing
}

impl Display for ClearCtl {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      ClearCtl::LineToEnd => write!(f, "\x1b[K"),
      ClearCtl::LineToStart => write!(f, "\x1b[1K"),
      ClearCtl::WholeLine => write!(f, "\x1b[2K"),
      ClearCtl::ScreenFromCursor => write!(f, "\x1b[J"),
      ClearCtl::ScreenToCursor => write!(f, "\x1b[1J"),
      ClearCtl::WholeScreen => write!(f, "\x1b[2J"),
      ClearCtl::ClearScrollback => write!(f, "\x1b[3J"),
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CursorCtl {
  Absolute { row: u16, col: u16 },
  Home,
  Col(u16),
  Up(u16),
  Down(u16),
  Forward(u16),
  Backward(u16),
  NextLine,
  LinesDown(u16),
  PrevLine,
  LinesUp(u16),
  SavePos,
  RestorePos,

  ShowCursor,
  HideCursor,
  SetStyle(CursorStyle),
}

impl Display for CursorCtl {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      CursorCtl::Up(0)
      | CursorCtl::Down(0)
      | CursorCtl::Forward(0)
      | CursorCtl::Backward(0)
      | CursorCtl::LinesDown(0)
      | CursorCtl::LinesUp(0) => Ok(()),
      CursorCtl::Absolute { row, col } => write!(f, "\x1b[{};{}H", (*row).max(1), (*col).max(1)),
      CursorCtl::Home => write!(f, "\x1b[H"),
      CursorCtl::Col(x) => write!(f, "\x1b[{}G", (*x).max(1)),
      // Movement-by-count variants: 0 means "no movement," emit nothing.
      // Absolute-position variants (Col, Absolute) keep .max(1) since per
      // ANSI a parameter of 0 is treated as 1.
      CursorCtl::Up(n) => write!(f, "\x1b[{n}A"),
      CursorCtl::Down(n) => write!(f, "\x1b[{n}B"),
      CursorCtl::Forward(n) => write!(f, "\x1b[{n}C"),
      CursorCtl::Backward(n) => write!(f, "\x1b[{n}D"),
      CursorCtl::NextLine => write!(f, "\x1b[E"),
      CursorCtl::LinesDown(n) => write!(f, "\x1b[{n}E"),
      CursorCtl::PrevLine => write!(f, "\x1b[F"),
      CursorCtl::LinesUp(n) => write!(f, "\x1b[{n}F"),
      CursorCtl::SavePos => write!(f, "\x1b7"),
      CursorCtl::RestorePos => write!(f, "\x1b8"),
      CursorCtl::ShowCursor => write!(f, "\x1b[?25h"),
      CursorCtl::HideCursor => write!(f, "\x1b[?25l"),
      CursorCtl::SetStyle(style) => style.fmt(f),
    }
  }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum CursorStyle {
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
