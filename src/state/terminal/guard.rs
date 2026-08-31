use crate::queue_term;

use super::{CursorStyle, Shed};

/*
 * These two structs get their own module because the public API is the only way
 * that these should ever be interacted with. TermGuard is actually quite dangerous
 * unless strictly used through the API. This is because of the 'active' flag, which
 * can cause RefCell panics if mismanaged.
 */

/// A guard that saves the terminal state on creation and restores it on drop.
///
/// This is returned from any Terminal method that modifies the terminal state.
/// This allows us to scope terminal state changes, and ensures that the terminal state is always restored even if the code panics or returns early.
#[derive(Debug)]
pub(crate) struct TermGuard {
  raw_mode: Option<bool>,
  bracketed_paste: Option<bool>,
  kitty_proto: Option<bool>,
  alt_buffer: Option<bool>,
  report_focus: Option<bool>,
  cursor_style: Option<CursorStyle>,
  cursor_visible: Option<bool>,
  mouse_support: Option<bool>,
  interactive: Option<bool>,
  termios_depth: Option<usize>,
  /// Outer Option: did this guard capture the scroll region?
  /// Inner Option: was a scroll region active at capture time?
  scroll_region: Option<super::ScrollRegionState>,

  /// This determines whether the drop impl will actually restore the state or not.
  active: bool,
}

impl TermGuard {
  pub(crate) fn new() -> Self {
    Self {
      raw_mode: None,
      bracketed_paste: None,
      kitty_proto: None,
      report_focus: None,
      alt_buffer: None,
      cursor_style: None,
      cursor_visible: None,
      mouse_support: None,
      interactive: None,
      termios_depth: None,
      scroll_region: None,
      active: false,
    }
  }
  pub(crate) fn with_raw_mode(mut self, raw_mode: bool) -> Self {
    if self.active {
      return self;
    } // enforce that we can't modify an active guard
    self.raw_mode = Some(raw_mode);
    self
  }
  pub(crate) fn with_bracketed_paste(mut self, bracketed_paste: bool) -> Self {
    if self.active {
      return self;
    }
    self.bracketed_paste = Some(bracketed_paste);
    self
  }
  pub(crate) fn with_kitty_proto(mut self, kitty_proto: bool) -> Self {
    if self.active {
      return self;
    }
    self.kitty_proto = Some(kitty_proto);
    self
  }
  pub(crate) fn with_report_focus(mut self, report_focus: bool) -> Self {
    if self.active {
      return self;
    }
    self.report_focus = Some(report_focus);
    self
  }
  pub(crate) fn with_alt_buffer(mut self, alt_buffer: bool) -> Self {
    if self.active {
      return self;
    }
    self.alt_buffer = Some(alt_buffer);
    self
  }
  pub(crate) fn with_cursor_style(mut self, cursor_style: CursorStyle) -> Self {
    if self.active {
      return self;
    }
    self.cursor_style = Some(cursor_style);
    self
  }
  pub(crate) fn with_cursor_visible(mut self, cursor_visible: bool) -> Self {
    if self.active {
      return self;
    }
    self.cursor_visible = Some(cursor_visible);
    self
  }
  pub(crate) fn with_mouse_support(mut self, mouse_support: bool) -> Self {
    if self.active {
      return self;
    }
    self.mouse_support = Some(mouse_support);
    self
  }
  pub(crate) fn with_interactive(mut self, interactive: bool) -> Self {
    if self.active {
      return self;
    }
    self.interactive = Some(interactive);
    self
  }
  pub(crate) fn with_termios_depth(mut self, termios_depth: usize) -> Self {
    if self.active {
      return self;
    }
    self.termios_depth = Some(termios_depth);
    self
  }
  pub(crate) fn with_scroll_region(mut self, scroll_region: super::ScrollRegionState) -> Self {
    if self.active {
      return self;
    }
    self.scroll_region = Some(scroll_region);
    self
  }
  pub(crate) fn bracketed_paste(&self) -> Option<bool> {
    self.bracketed_paste
  }
  pub(crate) fn kitty_proto(&self) -> Option<bool> {
    self.kitty_proto
  }
  pub(crate) fn report_focus(&self) -> Option<bool> {
    self.report_focus
  }
  pub(crate) fn alt_buffer(&self) -> Option<bool> {
    self.alt_buffer
  }
  pub(crate) fn cursor_style(&self) -> Option<CursorStyle> {
    self.cursor_style
  }
  pub(crate) fn cursor_visible(&self) -> Option<bool> {
    self.cursor_visible
  }
  pub(crate) fn mouse_support(&self) -> Option<bool> {
    self.mouse_support
  }
  pub(crate) fn interactive(&self) -> Option<bool> {
    self.interactive
  }
  pub(crate) fn termios_depth(&self) -> Option<usize> {
    self.termios_depth
  }
  pub(crate) fn scroll_region(&self) -> Option<super::ScrollRegionState> {
    self.scroll_region
  }

  pub(crate) fn activate(self) -> Self {
    if self.active {
      return self;
    }
    Self {
      active: true,
      ..self
    }
  }
}

impl Default for TermGuard {
  fn default() -> Self {
    Self::new()
  }
}

impl Drop for TermGuard {
  fn drop(&mut self) {
    // if we are not active, that means we are still inside of Shed::term_mut()
    if !self.active {
      return;
    }

    // which means this call would result in a RefCell panic
    Shed::term_mut(|t| t.load_state(self).ok());
  }
}

/// `Terminal::save_state()` returns this.
///
/// The point is to make it so that returning an inactive `TermGuard` is impossible.
pub(super) struct Snapshot(TermGuard);
impl Snapshot {
  pub(super) fn new(mut guard: TermGuard) -> Self {
    guard.active = false; // enforce this invariant
    Self(guard)
  }
  /// Set the inner guard to active and return it. This should be the only way to ever get an active `TermGuard`
  pub(super) fn activate(self) -> TermGuard {
    self.0.activate()
  }
}

pub(crate) struct SyncOutputGuard;

impl SyncOutputGuard {
  pub(crate) fn begin() -> Option<Self> {
    let supported = Shed::term(|t| t.term_caps().contains(super::TermCap::SYNC_OUTPUT));

    supported.then(|| {
      queue_term!(TermCtl::SyncStart).ok();
      Self
    })
  }
}

impl Drop for SyncOutputGuard {
  fn drop(&mut self) {
    queue_term!(TermCtl::SyncEnd).ok();
  }
}

/// A guard that flushes the terminal on drop.
///
/// Creating one of these will guarantee that the Terminal writes its buffered input
/// when the scope ends. Used mainly in the interactive loop
pub(crate) struct FlushGuard;
impl Drop for FlushGuard {
  fn drop(&mut self) {
    Shed::term_mut(std::io::Write::flush).ok();
  }
}
