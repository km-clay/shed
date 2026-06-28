use super::{
  ShResult, Shed,
  getopt::{Opt, OptSpec},
  outln, sherr, with_status,
};

use super::keys::{KeyMap, KeyMapFlags};

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

    if args.argv.is_empty() && remove.is_none() {
      display_keymaps(flags);
      return with_status(0);
    }

    if flags.is_empty() {
      return Err(sherr!(
        ExecFail,
        "At least one mode option must be specified for keymap",
      ).with_note(
        "Use -e for emacs mode, -n for normal mode, -i for insert mode, -v for visual mode, -x for ex mode, and -o for operator-pending mode".into(),
      ));
    }

    if let Some(keys) = remove {
      Shed::logic_mut(|l| l.remove_keymap(&keys));
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

    Shed::logic_mut(|l| l.insert_keymap(keymap));

    with_status(0)
  }
}

fn display_keymaps(mut flags: KeyMapFlags) {
  if flags.is_empty() {
    flags = KeyMapFlags::all();
  }

  let lines = Shed::logic(|l| l.keymaps_filtered(flags, &[]))
    .into_iter()
    .map(|km| km.to_string())
    .collect::<Vec<String>>()
    .join("\n");

  outln!("{lines}");
}

#[cfg(test)]
mod tests {
  use crate::{
    expand::expand_keymap,
    keys::{KeyMap, KeyMapFlags, KeyMapMatch},
    state::{self, Shed},
    tests::testutil::{TestGuard, test_input},
  };

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

    let maps = Shed::logic(|l| l.keymaps_filtered(KeyMapFlags::NORMAL, &expand_keymap("jk")));
    assert!(!maps.is_empty());
  }

  #[test]
  fn keymap_register_insert() {
    let _g = TestGuard::new();
    test_input("keymap -i jk '<ESC>'").unwrap();

    let maps = Shed::logic(|l| l.keymaps_filtered(KeyMapFlags::INSERT, &expand_keymap("jk")));
    assert!(!maps.is_empty());
  }

  #[test]
  fn keymap_overwrite() {
    let _g = TestGuard::new();
    test_input("keymap -n jk '<ESC>'").unwrap();
    test_input("keymap -n jk 'dd'").unwrap();

    let maps = Shed::logic(|l| l.keymaps_filtered(KeyMapFlags::NORMAL, &expand_keymap("jk")));
    assert_eq!(maps.len(), 1);
    assert_eq!(maps[0].action, "dd");
  }

  #[test]
  fn keymap_remove() {
    let _g = TestGuard::new();
    test_input("keymap -n jk '<ESC>'").unwrap();
    test_input("keymap -n --remove jk").unwrap();

    let maps = Shed::logic(|l| l.keymaps_filtered(KeyMapFlags::NORMAL, &expand_keymap("jk")));
    assert!(maps.is_empty());
  }

  #[test]
  fn keymap_status_zero() {
    let _g = TestGuard::new();
    test_input("keymap -n jk '<ESC>'").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Listing =====================

  #[test]
  fn keymap_no_args_lists_all_keymaps() {
    let g = TestGuard::new();
    test_input("keymap -n list_normal '<ESC>'").unwrap();
    test_input("keymap -i list_insert '<ESC>'").unwrap();
    g.read_output();

    test_input("keymap").unwrap();
    let out = g.read_output();
    assert!(out.contains("list_normal"), "got: {out:?}");
    assert!(out.contains("list_insert"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn keymap_mode_only_lists_for_that_mode() {
    let g = TestGuard::new();
    test_input("keymap -n only_normal '<ESC>'").unwrap();
    test_input("keymap -i only_insert '<ESC>'").unwrap();
    g.read_output();

    test_input("keymap -n").unwrap();
    let out = g.read_output();
    assert!(out.contains("only_normal"), "got: {out:?}");
    assert!(!out.contains("only_insert"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Error cases =====================

  #[test]
  fn keymap_missing_action() {
    let _g = TestGuard::new();
    test_input("keymap -n jk").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
