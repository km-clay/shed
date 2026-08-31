//! RAII guards for terminal state and output
//!
//! [`TermGuard`] snapshots terminal attributes (raw mode, alt buffer, cursor,
//! mouse, …) and restores them on drop; [`SyncOutputGuard`] brackets synchronized
//! output and [`FlushGuard`] flushes the buffer on drop.

use crate::queue_term;

use super::{CursorStyle, ScrollRegionState, Shed};

/*
 * These two structs get their own module because the public API is the only way
 * that these should ever be interacted with. TermGuard is actually quite dangerous
 * unless strictly used through the API. This is because of the 'active' flag, which
 * can cause RefCell panics if mismanaged.
 */

/// A guard that saves the terminal state on creation and restores it on drop.
///
/// This is returned from any Terminal method that modifies the terminal state.
/// This allows us to scope terminal state changes, and ensures that the terminal state is always restored
/// even if the code panics or returns early.
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
  scroll_region: Option<ScrollRegionState>,

  /// This determines whether the drop impl will actually restore the state or not.
  /// Also prevents any of the builder methods from modifying the guard after it has been activated.
  active: bool,
}

/// A macro to generate builder methods for [`TermGuard`].
macro_rules! builder_methods {
  ($($name1:ident,$name2:ident: $ty:ty),* $(,)?) => {
    impl TermGuard {
      $(
      pub(crate) fn $name1(mut self, value: $ty) -> Self {
        if self.active {
          return self;
        }
        self.$name2 = Some(value);
        self
      }
      #[allow(dead_code)]
      pub(crate) fn $name2(&self) -> Option<$ty> {
        self.$name2
      }
      )*
    }
  };
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

// generate the getter/setters
builder_methods! {
  with_raw_mode,raw_mode: bool,
  with_bracketed_paste,bracketed_paste: bool,
  with_kitty_proto,kitty_proto: bool,
  with_report_focus,report_focus: bool,
  with_alt_buffer,alt_buffer: bool,
  with_cursor_style,cursor_style: CursorStyle,
  with_cursor_visible,cursor_visible: bool,
  with_mouse_support,mouse_support: bool,
  with_interactive,interactive: bool,
  with_termios_depth,termios_depth: usize,
  with_scroll_region,scroll_region: ScrollRegionState,
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
