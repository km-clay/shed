use std::fmt::Display;

use bitflags::bitflags;

use crate::{
  expand::{alias, escape},
  state::vars::VarStr,
};

use super::KeyEvent;

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct KeyMapFlags: u32 {
    const NORMAL 			= 1<<0;
    const INSERT 			= 1<<1;
    const VISUAL 			= 1<<2;
    const EX 					= 1<<3;
    const OP_PENDING 	= 1<<4;
    const REPLACE 		= 1<<5;
    const VERBATIM 		= 1<<6;
    const EMACS   		= 1<<7;
    const REMOTE   		= 1<<8;
  }
}

impl Display for KeyMapFlags {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "-")?;
    for flag in self.iter() {
      match flag {
        KeyMapFlags::INSERT => write!(f, "i")?,
        KeyMapFlags::NORMAL => write!(f, "n")?,
        KeyMapFlags::VISUAL => write!(f, "v")?,
        KeyMapFlags::EX => write!(f, "x")?,
        KeyMapFlags::OP_PENDING => write!(f, "o")?,
        KeyMapFlags::REPLACE => write!(f, "r")?,
        KeyMapFlags::VERBATIM => write!(f, "V")?,
        KeyMapFlags::EMACS => write!(f, "e")?,
        KeyMapFlags::REMOTE => write!(f, "R")?,
        _ => break,
      }
    }
    Ok(())
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyMapMatch {
  NoMatch,
  IsPrefix,
  IsExact,
}

#[derive(Debug, Clone)]
pub(crate) struct KeyMap {
  pub flags: KeyMapFlags,
  pub keys: VarStr,
  pub action: VarStr,
}

impl KeyMap {
  pub(crate) fn keys_expanded(&self) -> Vec<KeyEvent> {
    alias::expand_keymap(&self.keys.to_str_lossy())
  }
  pub(crate) fn action_expanded(&self) -> Vec<KeyEvent> {
    alias::expand_keymap(&self.action.to_str_lossy())
  }
  pub(crate) fn compare(&self, other: &[KeyEvent]) -> KeyMapMatch {
    let ours = self.keys_expanded();
    if other == ours {
      KeyMapMatch::IsExact
    } else if ours.starts_with(other) {
      KeyMapMatch::IsPrefix
    } else {
      KeyMapMatch::NoMatch
    }
  }
}

impl Display for KeyMap {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let flags = self.flags.to_string();
    let keys = escape::shell_quote(&self.keys.to_str_lossy());
    let action = escape::shell_quote(&self.action.to_str_lossy());

    write!(f, "keymap {flags} {keys} {action}")
  }
}
