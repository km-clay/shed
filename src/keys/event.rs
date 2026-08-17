use std::fmt::Write;
use std::sync::Arc;

use smol_str::format_smolstr;

use crate::state::vars::VarStr;
use crate::util;

// Credit to Rustyline for the design ideas in this module
// https://github.com/kkawakam/rustyline
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeyEvent(pub KeyCode, pub ModKeys);

impl KeyEvent {
  #[expect(clippy::too_many_lines)]
  pub fn as_vim_seq(&self) -> VarStr {
    let mut seq = util::scratch_buf();
    let KeyEvent(event, mods) = self;
    let mut needs_angle_bracket = false;

    if mods.contains(ModKeys::CTRL) {
      seq.push_str("C-");
      needs_angle_bracket = true;
    }
    if mods.contains(ModKeys::ALT) {
      seq.push_str("A-");
      needs_angle_bracket = true;
    }
    if mods.contains(ModKeys::SHIFT) {
      seq.push_str("S-");
      needs_angle_bracket = true;
    }

    match event {
      KeyCode::ExMode => {
        seq.push_str("CMD");
        needs_angle_bracket = true;
      }
      KeyCode::Backspace => {
        seq.push_str("BS");
        needs_angle_bracket = true;
      }
      KeyCode::Delete => {
        seq.push_str("Del");
        needs_angle_bracket = true;
      }
      KeyCode::Down => {
        seq.push_str("Down");
        needs_angle_bracket = true;
      }
      KeyCode::End => {
        seq.push_str("End");
        needs_angle_bracket = true;
      }
      KeyCode::Enter => {
        seq.push_str("Enter");
        needs_angle_bracket = true;
      }
      KeyCode::Esc => {
        seq.push_str("Esc");
        needs_angle_bracket = true;
      }

      KeyCode::F(f) => {
        let _ = write!(seq, "F{f}");
        needs_angle_bracket = true;
      }
      KeyCode::Home => {
        seq.push_str("Home");
        needs_angle_bracket = true;
      }
      KeyCode::Insert => {
        seq.push_str("Insert");
        needs_angle_bracket = true;
      }
      KeyCode::Left => {
        seq.push_str("Left");
        needs_angle_bracket = true;
      }
      KeyCode::PageDown => {
        seq.push_str("PgDn");
        needs_angle_bracket = true;
      }
      KeyCode::PageUp => {
        seq.push_str("PgUp");
        needs_angle_bracket = true;
      }
      KeyCode::Right => {
        seq.push_str("Right");
        needs_angle_bracket = true;
      }
      KeyCode::Tab => {
        seq.push_str("Tab");
        needs_angle_bracket = true;
      }
      KeyCode::Up => {
        seq.push_str("Up");
        needs_angle_bracket = true;
      }
      KeyCode::Char(ch) => {
        seq.push(*ch);
      }
      KeyCode::Verbatim(s) => seq.push_str(s),
      clk @ (KeyCode::MiddleClick(x, y) | KeyCode::RightClick(x, y) | KeyCode::LeftClick(x, y)) => {
        let name = match clk {
          KeyCode::MiddleClick(_, _) => "MiddleClick",
          KeyCode::RightClick(_, _) => "RightClick",
          KeyCode::LeftClick(_, _) => "LeftClick",
          _ => unreachable!(),
        };
        let _ = write!(seq, "{name}({x},{y})");
        needs_angle_bracket = true;
      }
      KeyCode::ScrollUp => {
        seq.push_str("ScrollUp");
        needs_angle_bracket = true;
      }
      KeyCode::ScrollDown => {
        seq.push_str("ScrollDown");
        needs_angle_bracket = true;
      }
      KeyCode::Back => {
        seq.push_str("Back");
        needs_angle_bracket = true;
      }
      KeyCode::Forward => {
        seq.push_str("Forward");
        needs_angle_bracket = true;
      }
      KeyCode::MousePos(x, y) => {
        let _ = write!(seq, "MousePos({x},{y})");
        needs_angle_bracket = true;
      }
    }

    if needs_angle_bracket {
      format_smolstr!("<{seq}>").into()
    } else {
      seq.into()
    }
  }

  pub fn code(&self) -> &KeyCode {
    &self.0
  }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyCode {
  Backspace,
  Char(char),
  Verbatim(Arc<str>), // For sequences that should be treated as literal input, not parsed into a KeyCode
  Delete,
  Down,
  End,
  Enter,
  Esc,
  F(u8),
  Home,
  Insert,
  Left,
  PageDown,
  PageUp,
  Right,
  Tab,
  Up,

  // mouse events
  ScrollUp,
  ScrollDown,
  MousePos(usize, usize),
  LeftClick(usize, usize),
  RightClick(usize, usize),
  MiddleClick(usize, usize),
  Back,
  Forward,

  // weird stuff
  ExMode, // keycode emitted by the <cmd> byte alias in vim keymaps
}

impl KeyCode {
  pub fn is_keyboard_key(&self) -> bool {
    !matches!(
      self,
      KeyCode::ScrollUp
        | KeyCode::ScrollDown
        | KeyCode::MousePos(_, _)
        | KeyCode::LeftClick(_, _)
        | KeyCode::RightClick(_, _)
        | KeyCode::MiddleClick(_, _)
        | KeyCode::Back
        | KeyCode::Forward
        | KeyCode::ExMode
        | KeyCode::Verbatim(_)
    )
  }
}

bitflags::bitflags! {
  #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
  pub struct ModKeys: u8 {
    /// Control modifier
    const CTRL  = 1<<3;
    /// Escape or Alt modifier
    const ALT  = 1<<2;
    /// Shift modifier
    const SHIFT = 1<<1;

    /// No modifier
    const NONE = 0;
    /// Ctrl + Shift
    const CTRL_SHIFT = Self::CTRL.bits() | Self::SHIFT.bits();
    /// Alt + Shift
    const ALT_SHIFT = Self::ALT.bits() | Self::SHIFT.bits();
    /// Ctrl + Alt
    const CTRL_ALT = Self::CTRL.bits() | Self::ALT.bits();
    /// Ctrl + Alt + Shift
    const CTRL_ALT_SHIFT = Self::CTRL.bits() | Self::ALT.bits() | Self::SHIFT.bits();
  }
}

impl From<u16> for ModKeys {
  fn from(param: u16) -> Self {
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

impl From<&u16> for ModKeys {
  fn from(value: &u16) -> Self {
    ModKeys::from(*value)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::expand::expand_keymap;

  fn seq_of(code: KeyCode, mods: ModKeys) -> VarStr {
    KeyEvent(code, mods).as_vim_seq()
  }

  /// Round-trip helper: render the event to a vim seq, parse it back, and
  /// require the result to be exactly one `KeyEvent` equal to the input.
  fn assert_round_trips(code: &KeyCode, mods: ModKeys) {
    let original = KeyEvent(code.clone(), mods);
    let seq = original.as_vim_seq();
    let parsed = expand_keymap(&seq.to_str_lossy());
    assert_eq!(
      parsed.len(),
      1,
      "expected single event from {seq:?}, got {parsed:?}"
    );
    assert_eq!(parsed[0], original, "round-trip failed for {seq:?}");
  }

  // ─── Plain char (no mods) — no angle brackets ───────────────────────

  #[test]
  fn as_vim_seq_plain_char() {
    assert_eq!(seq_of(KeyCode::Char('a'), ModKeys::NONE), "a");
    assert_eq!(seq_of(KeyCode::Char('Z'), ModKeys::NONE), "Z");
    assert_eq!(seq_of(KeyCode::Char('5'), ModKeys::NONE), "5");
    assert_eq!(seq_of(KeyCode::Char('!'), ModKeys::NONE), "!");
  }

  // ─── Special keys — angle-bracketed names ───────────────────────────

  #[test]
  fn as_vim_seq_special_keys() {
    assert_eq!(seq_of(KeyCode::Enter, ModKeys::NONE), "<Enter>");
    assert_eq!(seq_of(KeyCode::Esc, ModKeys::NONE), "<Esc>");
    assert_eq!(seq_of(KeyCode::Tab, ModKeys::NONE), "<Tab>");
    assert_eq!(seq_of(KeyCode::Backspace, ModKeys::NONE), "<BS>");
    assert_eq!(seq_of(KeyCode::Delete, ModKeys::NONE), "<Del>");
    assert_eq!(seq_of(KeyCode::Insert, ModKeys::NONE), "<Insert>");
    assert_eq!(seq_of(KeyCode::Home, ModKeys::NONE), "<Home>");
    assert_eq!(seq_of(KeyCode::End, ModKeys::NONE), "<End>");
    assert_eq!(seq_of(KeyCode::PageUp, ModKeys::NONE), "<PgUp>");
    assert_eq!(seq_of(KeyCode::PageDown, ModKeys::NONE), "<PgDn>");
    assert_eq!(seq_of(KeyCode::Up, ModKeys::NONE), "<Up>");
    assert_eq!(seq_of(KeyCode::Down, ModKeys::NONE), "<Down>");
    assert_eq!(seq_of(KeyCode::Left, ModKeys::NONE), "<Left>");
    assert_eq!(seq_of(KeyCode::Right, ModKeys::NONE), "<Right>");
    assert_eq!(seq_of(KeyCode::ExMode, ModKeys::NONE), "<CMD>");
  }

  #[test]
  fn as_vim_seq_function_keys() {
    assert_eq!(seq_of(KeyCode::F(1), ModKeys::NONE), "<F1>");
    assert_eq!(seq_of(KeyCode::F(5), ModKeys::NONE), "<F5>");
    assert_eq!(seq_of(KeyCode::F(12), ModKeys::NONE), "<F12>");
  }

  #[test]
  fn as_vim_seq_mouse_clicks_carry_coords() {
    assert_eq!(
      seq_of(KeyCode::LeftClick(3, 7), ModKeys::NONE),
      "<LeftClick(3,7)>"
    );
    assert_eq!(
      seq_of(KeyCode::MiddleClick(10, 20), ModKeys::NONE),
      "<MiddleClick(10,20)>"
    );
    assert_eq!(
      seq_of(KeyCode::RightClick(0, 0), ModKeys::NONE),
      "<RightClick(0,0)>"
    );
    assert_eq!(
      seq_of(KeyCode::MousePos(5, 9), ModKeys::NONE),
      "<MousePos(5,9)>"
    );
  }

  #[test]
  fn as_vim_seq_scroll_and_history_buttons() {
    assert_eq!(seq_of(KeyCode::ScrollUp, ModKeys::NONE), "<ScrollUp>");
    assert_eq!(seq_of(KeyCode::ScrollDown, ModKeys::NONE), "<ScrollDown>");
    assert_eq!(seq_of(KeyCode::Back, ModKeys::NONE), "<Back>");
    assert_eq!(seq_of(KeyCode::Forward, ModKeys::NONE), "<Forward>");
  }

  #[test]
  fn as_vim_seq_verbatim_emits_raw_string_with_no_brackets() {
    use std::sync::Arc;
    let raw: Arc<str> = Arc::from("abc");
    assert_eq!(seq_of(KeyCode::Verbatim(raw), ModKeys::NONE), "abc");
  }

  // ─── Modifier rendering ──────────────────────────────────────────────

  #[test]
  fn as_vim_seq_single_modifier_with_char() {
    assert_eq!(seq_of(KeyCode::Char('a'), ModKeys::CTRL), "<C-a>");
    assert_eq!(seq_of(KeyCode::Char('a'), ModKeys::ALT), "<A-a>");
    assert_eq!(seq_of(KeyCode::Char('a'), ModKeys::SHIFT), "<S-a>");
  }

  #[test]
  fn as_vim_seq_combined_modifiers_order_is_c_a_s() {
    let all_mods = ModKeys::CTRL | ModKeys::ALT | ModKeys::SHIFT;
    assert_eq!(seq_of(KeyCode::Char('x'), all_mods), "<C-A-S-x>");
  }

  #[test]
  fn as_vim_seq_modifier_on_special_key() {
    assert_eq!(seq_of(KeyCode::Enter, ModKeys::CTRL), "<C-Enter>");
    assert_eq!(seq_of(KeyCode::Tab, ModKeys::SHIFT), "<S-Tab>");
    assert_eq!(
      seq_of(KeyCode::F(5), ModKeys::CTRL | ModKeys::ALT),
      "<C-A-F5>"
    );
  }

  // ─── Round-trip: render then parse ───────────────────────────────────
  //
  // expand_keymap parses the rendered string back into KeyEvents. For
  // the variants both halves understand, the round-trip should be
  // lossless.

  #[test]
  fn round_trip_plain_chars() {
    for ch in ['a', 'Z', '0', '!', '~'] {
      assert_round_trips(&KeyCode::Char(ch), ModKeys::NONE);
    }
  }

  #[test]
  fn round_trip_special_keys() {
    let specials = [
      KeyCode::Enter,
      KeyCode::Esc,
      KeyCode::Tab,
      KeyCode::Backspace,
      KeyCode::Delete,
      KeyCode::Insert,
      KeyCode::Home,
      KeyCode::End,
      KeyCode::PageUp,
      KeyCode::PageDown,
      KeyCode::Up,
      KeyCode::Down,
      KeyCode::Left,
      KeyCode::Right,
      KeyCode::ExMode,
    ];
    for code in specials {
      assert_round_trips(&code, ModKeys::NONE);
    }
  }

  #[test]
  fn round_trip_chars_with_single_modifier() {
    for mods in [ModKeys::CTRL, ModKeys::ALT, ModKeys::SHIFT] {
      assert_round_trips(&KeyCode::Char('a'), mods);
    }
  }

  #[test]
  fn round_trip_chars_with_combined_modifiers() {
    assert_round_trips(&KeyCode::Char('x'), ModKeys::CTRL | ModKeys::ALT);
    assert_round_trips(&KeyCode::Char('x'), ModKeys::CTRL | ModKeys::SHIFT);
    assert_round_trips(&KeyCode::Char('x'), ModKeys::ALT | ModKeys::SHIFT);
    assert_round_trips(
      &KeyCode::Char('x'),
      ModKeys::CTRL | ModKeys::ALT | ModKeys::SHIFT,
    );
  }

  #[test]
  fn round_trip_special_key_with_modifier() {
    assert_round_trips(&KeyCode::Enter, ModKeys::CTRL);
    assert_round_trips(&KeyCode::Tab, ModKeys::SHIFT);
    assert_round_trips(&KeyCode::Esc, ModKeys::ALT);
  }

  #[test]
  fn round_trip_function_keys() {
    for n in 1u8..=12 {
      assert_round_trips(&KeyCode::F(n), ModKeys::NONE);
    }
  }

  #[test]
  fn round_trip_function_keys_with_modifier() {
    assert_round_trips(&KeyCode::F(5), ModKeys::CTRL);
    assert_round_trips(&KeyCode::F(12), ModKeys::CTRL | ModKeys::SHIFT);
  }
}
