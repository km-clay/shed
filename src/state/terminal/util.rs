use std::os::fd::RawFd;

use nix::{
  libc,
  sys::termios::{self, Termios},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use super::try_var;

pub(crate) fn calc_str_width(s: &str) -> usize {
  let mut esc_seq = 0;
  s.graphemes(true).map(|g| width(g, &mut esc_seq)).sum()
}

pub(crate) fn truncate_visual(s: &str, max_width: usize) -> String {
  let mut out = String::new();
  let mut visible = 0;
  let mut esc_seq = 0u8;
  let mut wrote_anything_visible = false;

  for g in s.graphemes(true) {
    let w = width(g, &mut esc_seq);
    if esc_seq == 0 && visible + w > max_width {
      break;
    }
    out.push_str(g);
    visible += w;
    if w > 0 {
      wrote_anything_visible = true;
    }
  }

  if wrote_anything_visible {
    out.push_str("\x1b[0m");
  }
  out
}

pub(crate) fn truncate_with_ellipsis(s: &str, max_width: usize) -> String {
  if calc_str_width(s) <= max_width {
    return s.to_string();
  }
  if max_width <= 3 {
    // Not enough room even for the ellipsis itself; just hard-truncate.
    return truncate_visual(s, max_width);
  }
  let mut out = truncate_visual(s, max_width - 3);
  out.push_str("...");
  out
}

// Big credit to rustyline for this
pub(crate) fn width(s: &str, esc_seq: &mut u8) -> usize {
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
    get_width_calculator().width(s)
  }
}

pub(crate) trait WidthCalculator: Send + Sync {
  fn width(&self, text: &str) -> usize;
}

static WIDTH_CALC: std::sync::OnceLock<Box<dyn WidthCalculator>> = std::sync::OnceLock::new();

pub(crate) fn get_width_calculator() -> &'static dyn WidthCalculator {
  WIDTH_CALC.get_or_init(width_calculator).as_ref()
}

fn is_visual_control(c: char) -> bool {
  matches!(c, '\x00'..='\x08' | '\x0b'..='\x1f' | '\x7f')
}

fn cwidth(ch: char) -> usize {
  if is_visual_control(ch) {
    2
  } else {
    ch.width().unwrap_or(0)
  }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UnicodeWidth;

impl WidthCalculator for UnicodeWidth {
  fn width(&self, text: &str) -> usize {
    text.chars().map(cwidth).sum()
  }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WcWidth;

impl WidthCalculator for WcWidth {
  fn width(&self, text: &str) -> usize {
    text.chars().map(cwidth).sum()
  }
}

const ZWJ: char = '\u{200D}';
#[derive(Clone, Copy, Debug)]
pub(crate) struct NoZwj;

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

pub(crate) fn width_calculator() -> Box<dyn WidthCalculator> {
  match try_var!("TERM_PROGRAM")
    .as_ref()
    .map(|v| v.to_str_lossy())
    .as_deref()
  {
    Some("Apple_Terminal" | "iTerm.app" | "WezTerm") => Box::new(UnicodeWidth),
    Some(_) => Box::new(WcWidth),
    None => match try_var!("TERM")
      .as_ref()
      .map(|v| v.to_str_lossy())
      .as_deref()
    {
      Some("xterm-kitty") => Box::new(NoZwj),
      _ => Box::new(WcWidth),
    },
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorMode {
  Truecolor,
  Palette256,
  Palette16,
}

// I'd like to thank rustyline for this idea
nix::ioctl_read_bad!(win_size, libc::TIOCGWINSZ, libc::winsize);

/// Get the dimensions of thejterminal.
///
/// Returned as (cols,rows)
pub(crate) fn get_win_size(fd: RawFd) -> (u16, u16) {
  use std::mem::zeroed;

  if cfg!(test) {
    return (80, 24);
  }

  unsafe {
    let mut size: libc::winsize = zeroed();
    match win_size(fd, &raw mut size) {
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

pub(super) fn enable_raw_mode(term: &mut Termios) {
  termios::cfmakeraw(term);
  // Keep ISIG enabled so Ctrl+C/Ctrl+Z still generate signals
  term.local_flags |= termios::LocalFlags::ISIG;
  // Keep OPOST enabled so \n is translated to \r\n on output
  term.output_flags |= termios::OutputFlags::OPOST;
}

pub(super) fn enable_cooked_mode(term: &mut Termios) {
  term.local_flags |= termios::LocalFlags::ICANON
    | termios::LocalFlags::ECHO
    | termios::LocalFlags::ECHOE
    | termios::LocalFlags::ECHOK
    | termios::LocalFlags::ECHONL
    | termios::LocalFlags::ISIG
    | termios::LocalFlags::IEXTEN;
  term.input_flags |= termios::InputFlags::ICRNL | termios::InputFlags::IXON;
  term.output_flags |= termios::OutputFlags::OPOST;
  // Restore VMIN/VTIME to canonical mode defaults
  term.control_chars[termios::SpecialCharacterIndices::VMIN as usize] = 1;
  term.control_chars[termios::SpecialCharacterIndices::VTIME as usize] = 0;
}

#[cfg(test)]
mod truncate_visual_tests {
  use super::*;

  const RESET: &str = "\x1b[0m";

  #[test]
  fn empty_string_returns_empty_no_reset() {
    // Nothing visible was written → no SGR reset appended.
    assert_eq!(truncate_visual("", 10), "");
  }

  #[test]
  fn short_string_passes_through_with_reset() {
    // Plain text shorter than max_width returns the full string +
    // trailing SGR reset.
    let out = truncate_visual("hi", 10);
    assert_eq!(out, format!("hi{RESET}"));
  }

  #[test]
  fn exact_fit_passes_through() {
    let out = truncate_visual("hello", 5);
    assert_eq!(out, format!("hello{RESET}"));
  }

  #[test]
  fn over_long_is_truncated() {
    let out = truncate_visual("hello world", 5);
    assert_eq!(out, format!("hello{RESET}"));
  }

  #[test]
  fn zero_max_width_with_only_visible_input_drops_everything() {
    let out = truncate_visual("hello", 0);
    // No visible char fits, so wrote_anything_visible stays false and
    // the reset is *not* appended.
    assert_eq!(out, "");
  }

  #[test]
  fn ansi_escape_does_not_count_against_width() {
    // The CSI sequence itself contributes width 0, so even with a
    // tight budget the visible chars after still survive.
    let input = "\x1b[31mhi\x1b[0m";
    let out = truncate_visual(input, 2);
    // Both visible chars + the inline escapes survive; an extra
    // reset is then appended.
    assert_eq!(out, format!("\x1b[31mhi\x1b[0m{RESET}"));
  }

  #[test]
  fn wide_grapheme_counted_as_its_width() {
    // CJK character takes width 2 in monospace terminals.
    let out = truncate_visual("漢字", 2);
    // Only one CJK char fits in width 2.
    assert_eq!(out, format!("漢{RESET}"));
  }

  #[test]
  fn ansi_only_input_writes_no_reset() {
    // The bytes are pushed into the output (esc_seq path doesn't
    // break the loop), but no visible char triggered the reset.
    // We're really just pinning the wrote_anything_visible branch.
    let input = "\x1b[31m";
    let out = truncate_visual(input, 5);
    assert_eq!(out, "\x1b[31m");
  }

  #[test]
  fn truncation_breaks_before_overrun() {
    // "abcdef" with width 4 stops after "abcd"; the 'e' check sees
    // visible(4) + w(1) > 4 and breaks before pushing.
    let out = truncate_visual("abcdef", 4);
    assert_eq!(out, format!("abcd{RESET}"));
  }
}
