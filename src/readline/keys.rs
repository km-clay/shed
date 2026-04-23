use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

use crate::sherr;
use crate::util::error::ShResult;

// Credit to Rustyline for the design ideas in this module
// https://github.com/kkawakam/rustyline
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeyEvent(pub KeyCode, pub ModKeys);

impl KeyEvent {
  pub fn new(ch: &str, mut mods: ModKeys) -> Self {
    use {KeyCode as K, KeyEvent as E, ModKeys as M};

    let mut graphemes = ch.graphemes(true);

    let first = match graphemes.next() {
      Some(g) => g,
      None => return E(K::Null, mods),
    };

    // If more than one grapheme, it's not a single key event
    if graphemes.next().is_some() {
      return E(K::Null, mods);
    }

    let mut chars = first.chars();

    let single_char = chars.next();
    let is_single_char = chars.next().is_none();

    match single_char {
      Some(c) if is_single_char && c.is_control() => match c {
        '\x00' => E(K::Char('@'), mods | M::CTRL),
        '\x09' => {
          if mods.contains(M::SHIFT) {
            mods.remove(M::SHIFT);
            E(K::BackTab, mods)
          } else {
            E(K::Tab, mods)
          }
        }
        '\x0d' => E(K::Enter, mods),
        '\x08' => E(K::Backspace, mods),
        '\x1b' => E(K::Esc, mods),
        '\x7f' => E(K::Backspace, mods),
        '\u{9b}' => E(K::Esc, mods | M::SHIFT),
        '\x1c' => E(K::Char('\\'), mods | M::CTRL),
        '\x1d' => E(K::Char(']'), mods | M::CTRL),
        '\x1e' => E(K::Char('^'), mods | M::CTRL),
        '\x1f' => E(K::Char('_'), mods | M::CTRL),
        '\x01'..='\x1a' => {
          // Map Ctrl + [a-z] to their corresponding control characters
          let ctrl_char = (c as u8 - 1 + b'a') as char;
          E(K::Char(ctrl_char), mods | M::CTRL)
        }
        _ => E(K::Null, mods),
      },
      Some(c) if is_single_char => {
        if !mods.is_empty() {
          mods.remove(M::SHIFT);
        }
        E(K::Char(c), mods)
      }
      _ => {
        // multi-char grapheme (emoji, accented, etc)
        if !mods.is_empty() {
          mods.remove(M::SHIFT);
        }
        E(K::Grapheme(Arc::from(first)), mods)
      }
    }
  }
  pub fn as_vim_seq(&self) -> ShResult<String> {
    let mut seq = String::new();
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
      KeyCode::UnknownEscSeq => {
        return Err(sherr!(
          ParseErr,
          "Cannot convert unknown escape sequence to Vim key sequence",
        ));
      }
      KeyCode::ExMode => {
        seq.push_str("CMD");
        needs_angle_bracket = true;
      }
      KeyCode::Backspace => {
        seq.push_str("BS");
        needs_angle_bracket = true;
      }
      KeyCode::BackTab => {
        seq.push_str("S-Tab");
        needs_angle_bracket = true;
      }
      KeyCode::BracketedPasteStart => todo!(),
      KeyCode::BracketedPasteEnd => todo!(),
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
        seq.push_str(&format!("F{}", f));
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
      KeyCode::Null => todo!(),
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
      KeyCode::Grapheme(gr) => seq.push_str(gr),
      KeyCode::Verbatim(s) => seq.push_str(s),
    }

    if needs_angle_bracket {
      Ok(format!("<{}>", seq))
    } else {
      Ok(seq)
    }
  }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeyCode {
  UnknownEscSeq,
  Backspace,
  BackTab,
  BracketedPasteStart,
  BracketedPasteEnd,
  Char(char),
  Grapheme(Arc<str>),
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
  Null,
  PageDown,
  PageUp,
  Right,
  Tab,
  Up,

  // weird stuff
  ExMode, // keycode emitted by the <cmd> byte alias in vim keymaps
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
