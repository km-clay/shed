use crate::{
  expand::expand_keymap,
  getopt::{Opt, OptSpec},
  prelude::*,
  readline::keys::KeyEvent,
  sherr,
  state::write_logic,
  util::{error::ShResult, with_status},
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMapMatch {
  NoMatch,
  IsPrefix,
  IsExact,
}

#[derive(Debug, Clone)]
pub struct KeyMap {
  pub flags: KeyMapFlags,
  pub keys: String,
  pub action: String,
}

impl KeyMap {
  pub fn keys_expanded(&self) -> Vec<KeyEvent> {
    expand_keymap(&self.keys)
  }
  pub fn action_expanded(&self) -> Vec<KeyEvent> {
    expand_keymap(&self.action)
  }
  pub fn compare(&self, other: &[KeyEvent]) -> KeyMapMatch {
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

pub(super) struct KeyMapBuiltin;
impl super::Builtin for KeyMapBuiltin {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('n'), // normal mode
      OptSpec::flag('e'), // emacs mode
      OptSpec::flag('i'), // insert mode
      OptSpec::flag('v'), // visual mode
      OptSpec::flag('x'), // ex mode
      OptSpec::flag('o'), // operator-pending mode
      OptSpec::flag('r'), // replace mode
      OptSpec::single_arg("remove"),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let mut flags = KeyMapFlags::empty();
    let mut remove = None;
    for opt in args.opts {
      match opt {
        Opt::Short('n') => flags |= KeyMapFlags::NORMAL,
        Opt::Short('i') => flags |= KeyMapFlags::INSERT,
        Opt::Short('v') => flags |= KeyMapFlags::VISUAL,
        Opt::Short('x') => flags |= KeyMapFlags::EX,
        Opt::Short('o') => flags |= KeyMapFlags::OP_PENDING,
        Opt::Short('r') => flags |= KeyMapFlags::REPLACE,
        Opt::Short('e') => flags |= KeyMapFlags::EMACS,
        Opt::LongWithArg(name, arg) if name == "remove" => {
          if remove.is_some() {
            return Err(sherr!(ExecFail @ span, "Duplicate --remove option for keymap"));
          }
          remove = Some(arg.clone());
        }
        _ => {
          return Err(sherr!(ExecFail @ span, "Invalid option for keymap: {opt:?}"));
        }
      }
    }
    if flags.is_empty() {
      return Err(sherr!(
        ExecFail,
        "At least one mode option must be specified for keymap",
      ).with_note(
        "Use -e for emacs mode, -n for normal mode, -i for insert mode, -v for visual mode, -x for ex mode, and -o for operator-pending mode".to_string(),
      ));
    }

    if let Some(keys) = remove {
      write_logic(|l| l.remove_keymap(&keys));
      return with_status(0);
    }

    let Some((keys, _)) = args.argv.first() else {
      return Err(sherr!(
        ExecFail @ span,
        "missing keys argument",
      ));
    };

    let Some((action, _)) = args.argv.get(1) else {
      return Err(sherr!(
        ExecFail @ span,
        "missing action argument",
      ));
    };

    let keymap = KeyMap {
      flags,
      keys: keys.clone(),
      action: action.clone(),
    };

    write_logic(|l| l.insert_keymap(keymap));

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::expand::expand_keymap;
  use crate::state::{self, read_logic};
  use crate::testutil::{TestGuard, test_input};

  // ===================== KeyMap::compare =====================

  #[test]
  fn compare_exact_match() {
    let km = KeyMap {
      flags: KeyMapFlags::NORMAL,
      keys: "jk".into(),
      action: "<ESC>".into(),
    };
    let keys = expand_keymap("jk");
    assert_eq!(km.compare(&keys), KeyMapMatch::IsExact);
  }

  #[test]
  fn compare_prefix_match() {
    let km = KeyMap {
      flags: KeyMapFlags::NORMAL,
      keys: "jk".into(),
      action: "<ESC>".into(),
    };
    let keys = expand_keymap("j");
    assert_eq!(km.compare(&keys), KeyMapMatch::IsPrefix);
  }

  #[test]
  fn compare_no_match() {
    let km = KeyMap {
      flags: KeyMapFlags::NORMAL,
      keys: "jk".into(),
      action: "<ESC>".into(),
    };
    let keys = expand_keymap("zz");
    assert_eq!(km.compare(&keys), KeyMapMatch::NoMatch);
  }

  // ===================== Registration via test_input =====================

  #[test]
  fn keymap_register() {
    let _g = TestGuard::new();
    test_input("keymap -n jk '<ESC>'").unwrap();

    let maps = read_logic(|l| l.keymaps_filtered(KeyMapFlags::NORMAL, &expand_keymap("jk")));
    assert!(!maps.is_empty());
  }

  #[test]
  fn keymap_register_insert() {
    let _g = TestGuard::new();
    test_input("keymap -i jk '<ESC>'").unwrap();

    let maps = read_logic(|l| l.keymaps_filtered(KeyMapFlags::INSERT, &expand_keymap("jk")));
    assert!(!maps.is_empty());
  }

  #[test]
  fn keymap_overwrite() {
    let _g = TestGuard::new();
    test_input("keymap -n jk '<ESC>'").unwrap();
    test_input("keymap -n jk 'dd'").unwrap();

    let maps = read_logic(|l| l.keymaps_filtered(KeyMapFlags::NORMAL, &expand_keymap("jk")));
    assert_eq!(maps.len(), 1);
    assert_eq!(maps[0].action, "dd");
  }

  #[test]
  fn keymap_remove() {
    let _g = TestGuard::new();
    test_input("keymap -n jk '<ESC>'").unwrap();
    test_input("keymap -n --remove jk").unwrap();

    let maps = read_logic(|l| l.keymaps_filtered(KeyMapFlags::NORMAL, &expand_keymap("jk")));
    assert!(maps.is_empty());
  }

  #[test]
  fn keymap_status_zero() {
    let _g = TestGuard::new();
    test_input("keymap -n jk '<ESC>'").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  // ===================== Error cases =====================

  #[test]
  fn keymap_missing_keys() {
    let _g = TestGuard::new();
    test_input("keymap -n").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn keymap_missing_action() {
    let _g = TestGuard::new();
    test_input("keymap -n jk").ok();
    assert_ne!(state::get_status(), 0);
  }
}
