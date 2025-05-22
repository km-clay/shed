use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

// Credit to Rustyline for the design ideas in this module
// https://github.com/kkawakam/rustyline
#[derive(Clone,Debug)]
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
			return E(K::Null, mods); // Or panic, or wrap in Grapheme if desired
		}

		let mut chars = first.chars();

		let single_char = chars.next();
		let is_single_char = chars.next().is_none();

		match single_char {
			Some(c) if is_single_char && c.is_control() => {
				match c {
					'\x00' => E(K::Char('@'), mods | M::CTRL),
					'\x01' => E(K::Char('A'), mods | M::CTRL),
					'\x02' => E(K::Char('B'), mods | M::CTRL),
					'\x03' => E(K::Char('C'), mods | M::CTRL),
					'\x04' => E(K::Char('D'), mods | M::CTRL),
					'\x05' => E(K::Char('E'), mods | M::CTRL),
					'\x06' => E(K::Char('F'), mods | M::CTRL),
					'\x07' => E(K::Char('G'), mods | M::CTRL),
					'\x08' => E(K::Backspace, mods),
					'\x09' => {
						if mods.contains(M::SHIFT) {
							mods.remove(M::SHIFT);
							E(K::BackTab, mods)
						} else {
							E(K::Tab, mods)
						}
					}
					'\x0a' => E(K::Char('J'), mods | M::CTRL),
					'\x0b' => E(K::Char('K'), mods | M::CTRL),
					'\x0c' => E(K::Char('L'), mods | M::CTRL),
					'\x0d' => E(K::Enter, mods),
					'\x0e' => E(K::Char('N'), mods | M::CTRL),
					'\x0f' => E(K::Char('O'), mods | M::CTRL),
					'\x10' => E(K::Char('P'), mods | M::CTRL),
					'\x11' => E(K::Char('Q'), mods | M::CTRL),
					'\x12' => E(K::Char('R'), mods | M::CTRL),
					'\x13' => E(K::Char('S'), mods | M::CTRL),
					'\x14' => E(K::Char('T'), mods | M::CTRL),
					'\x15' => E(K::Char('U'), mods | M::CTRL),
					'\x16' => E(K::Char('V'), mods | M::CTRL),
					'\x17' => E(K::Char('W'), mods | M::CTRL),
					'\x18' => E(K::Char('X'), mods | M::CTRL),
					'\x19' => E(K::Char('Y'), mods | M::CTRL),
					'\x1a' => E(K::Char('Z'), mods | M::CTRL),
					'\x1b' => E(K::Esc, mods),
					'\x1c' => E(K::Char('\\'), mods | M::CTRL),
					'\x1d' => E(K::Char(']'), mods | M::CTRL),
					'\x1e' => E(K::Char('^'), mods | M::CTRL),
					'\x1f' => E(K::Char('_'), mods | M::CTRL),
					'\x7f' => E(K::Backspace, mods),
					'\u{9b}' => E(K::Esc, mods | M::SHIFT),
					_ => E(K::Null, mods),
				}
			}
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
}

#[derive(Clone,Debug)]
pub enum KeyCode {
    UnknownEscSeq,
    Backspace,
    BackTab,
    BracketedPasteStart,
    BracketedPasteEnd,
    Char(char),
		Grapheme(Arc<str>),
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
