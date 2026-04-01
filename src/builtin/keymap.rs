use crate::{
  expand::expand_keymap,
  getopt::{Opt, OptArg, OptSpec, get_opts_from_tokens},
  libsh::error::{ShResult, ShResultExt},
  parse::{NdRule, Node},
  prelude::*,
  readline::keys::KeyEvent,
  sherr,
  state::{self, write_logic},
};

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct KeyMapFlags: u32 {
    const NORMAL 			= 0b00000001;
    const INSERT 			= 0b00000010;
    const VISUAL 			= 0b00000100;
    const EX 					= 0b00001000;
    const OP_PENDING 	= 0b00010000;
    const REPLACE 		= 0b00100000;
    const VERBATIM 		= 0b01000000;
    const EMACS   		= 0b10000000;
  }
}

pub struct KeyMapOpts {
  remove: Option<String>,
  flags: KeyMapFlags,
}
impl KeyMapOpts {
  pub fn from_opts(opts: &[Opt]) -> ShResult<Self> {
    let mut flags = KeyMapFlags::empty();
    let mut remove = None;
    for opt in opts {
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
            return Err(sherr!(ExecFail, "Duplicate --remove option for keymap"));
          }
          remove = Some(arg.clone());
        }
        _ => {
          return Err(sherr!(ExecFail, "Invalid option for keymap: {:?}", opt,));
        }
      }
    }
    if flags.is_empty() {
      return Err(sherr!(ExecFail, "At least one mode option must be specified for keymap").with_note("Use -e for emacs mode, -n for normal mode, -i for insert mode, -v for visual mode, -x for ex mode, and -o for operator-pending mode".to_string()));
    }
    Ok(Self { remove, flags })
  }
  pub fn keymap_opts() -> [OptSpec; 8] {
    [
      OptSpec {
        opt: Opt::Short('n'), // normal mode
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('e'), // emacs mode
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('i'), // insert mode
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('v'), // visual mode
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('x'), // ex mode
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Short('o'), // operator-pending mode
        takes_arg: OptArg::None,
      },
      OptSpec {
        opt: Opt::Long("remove".into()),
        takes_arg: OptArg::Single,
      },
      OptSpec {
        opt: Opt::Short('r'), // replace mode
        takes_arg: OptArg::None,
      },
    ]
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

pub fn keymap(node: Node) -> ShResult<()> {
  let span = node.get_span();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (mut argv, opts) = get_opts_from_tokens(argv, &KeyMapOpts::keymap_opts())?;
  let opts = KeyMapOpts::from_opts(&opts).promote_err(span.clone())?;
  if let Some(to_rm) = opts.remove {
    write_logic(|l| l.remove_keymap(&to_rm));
    state::set_status(0);
    return Ok(());
  }

  if !argv.is_empty() {
    argv.remove(0);
  }

  let Some((keys, _)) = argv.first() else {
    return Err(sherr!(
      ExecFail @ span,
      "missing keys argument",
    ));
  };

  let Some((action, _)) = argv.get(1) else {
    return Err(sherr!(
      ExecFail @ span,
      "missing action argument",
    ));
  };

  let keymap = KeyMap {
    flags: opts.flags,
    keys: keys.clone(),
    action: action.clone(),
  };

  write_logic(|l| l.insert_keymap(keymap));

  state::set_status(0);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::expand::expand_keymap;
  use crate::getopt::Opt;
  use crate::state::{self, read_logic};
  use crate::testutil::{TestGuard, test_input};

  // ===================== KeyMapOpts parsing =====================

  #[test]
  fn opts_normal_mode() {
    let opts = KeyMapOpts::from_opts(&[Opt::Short('n')]).unwrap();
    assert!(opts.flags.contains(KeyMapFlags::NORMAL));
  }

  #[test]
  fn opts_insert_mode() {
    let opts = KeyMapOpts::from_opts(&[Opt::Short('i')]).unwrap();
    assert!(opts.flags.contains(KeyMapFlags::INSERT));
  }

  #[test]
  fn opts_multiple_modes() {
    let opts = KeyMapOpts::from_opts(&[Opt::Short('n'), Opt::Short('i')]).unwrap();
    assert!(opts.flags.contains(KeyMapFlags::NORMAL));
    assert!(opts.flags.contains(KeyMapFlags::INSERT));
  }

  #[test]
  fn opts_no_mode_errors() {
    let result = KeyMapOpts::from_opts(&[]);
    assert!(result.is_err());
  }

  #[test]
  fn opts_remove() {
    let opts = KeyMapOpts::from_opts(&[
      Opt::Short('n'),
      Opt::LongWithArg("remove".into(), "jk".into()),
    ])
    .unwrap();
    assert_eq!(opts.remove, Some("jk".into()));
  }

  #[test]
  fn opts_duplicate_remove_errors() {
    let result = KeyMapOpts::from_opts(&[
      Opt::Short('n'),
      Opt::LongWithArg("remove".into(), "jk".into()),
      Opt::LongWithArg("remove".into(), "kj".into()),
    ]);
    assert!(result.is_err());
  }

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
    let result = test_input("keymap -n");
    assert!(result.is_err());
  }

  #[test]
  fn keymap_missing_action() {
    let _g = TestGuard::new();
    let result = test_input("keymap -n jk");
    assert!(result.is_err());
  }
}
