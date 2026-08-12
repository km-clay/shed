use crate::{opt, state::vars::VarStr};

use super::{
  opt::{Opt, OptSpec},
  outln,
  readline::stash::{Stash, StashedCmd},
  sherr,
  util::{ShResult, ShResultExt},
};

#[derive(Debug, Default)]
pub(crate) struct StashOpts {
  pub to_save: Vec<StashedCmd>,
  pub to_delete: Vec<VarStr>,
  pub list: bool,
  pub only_named: bool,
  pub only_stack: bool,
}

impl StashOpts {
  pub fn from_opts(opts: &[Opt]) -> ShResult<Self> {
    let mut new = Self::default();

    for opt in opts {
      match opt.key() {
        "save" => {
          let mut args = opt.args().iter().map(|(s, _)| s.clone());

          // length of 'args' is enforced by the opt spec
          let Some(name) = args.next() else {
            return Err(sherr!(ParseErr @ opt.span(), "missing name argument for '{opt}'"));
          };
          let Some(buffer) = args.next() else {
            return Err(sherr!(ParseErr @ opt.span(), "missing buffer argument for '{opt}'"));
          };
          let Some(cursor) = args.next() else {
            return Err(sherr!(ParseErr @ opt.span(), "missing cursor argument for '{opt}'"));
          };

          new.to_save.push(StashedCmd {
            name: Some(name),
            buffer,
            cursor_pos: cursor,
          });
        }
        "delete" => {
          new.to_delete.push(opt.value()?.into());
        }
        "list" => new.list = true,
        "stack" => new.only_stack = true,
        "named" => new.only_named = true,
        _ => return Err(sherr!(ParseErr, "unexpected option {opt} in stash")),
      }
    }

    Ok(new)
  }
}

pub(super) struct StashBuiltin;
impl super::Builtin for StashBuiltin {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("save" | 's', 3),
      opt!("delete" | 'd', 1),
      opt!("list" | 'l'),
      opt!("stack"),
      opt!("named"),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let is_empty = args.no_options();
    let (_, opts) = args.take_argv();

    let stash_opts = StashOpts::from_opts(&opts).promote_err(span.clone())?;
    let stash = Stash::new().promote_err(span.clone())?;

    for cmd in stash_opts.to_save {
      stash.stash_cmd(&cmd).promote_err(span.clone())?;
    }

    for cmd in stash_opts.to_delete {
      stash.delete_cmd(&cmd).promote_err(span.clone())?;
    }

    if stash_opts.list || is_empty {
      let output = stash.list(stash_opts.only_named, stash_opts.only_stack);
      outln!("{output}");
    }

    Ok(())
  }
}

#[cfg(test)]
mod stash_builtin_tests {
  use super::*;
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  /// Drop any leftover stash entries from prior tests in this thread.
  fn fresh_stash() -> Stash {
    let conn = crate::state::util::get_db_conn().expect("test db");
    conn
      .lock()
      .unwrap()
      .execute_batch("DROP TABLE IF EXISTS stash")
      .ok();
    Stash::new().unwrap()
  }

  // ─── no args → list ───────────────────────────────────────────

  #[test]
  fn no_opts_dispatches_to_list() {
    let g = TestGuard::new();
    let stash = fresh_stash();
    stash
      .stash_cmd(&StashedCmd {
        name: Some("test_name".into()),
        buffer: "stashed buffer".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    test_input("stash").unwrap();
    let out = g.read_output();
    assert!(out.contains("test_name"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn get_named_is_exact_case_sensitive_match() {
    // Regression: `get_named` used `LIKE` (ASCII-case-insensitive) while save
    // dedups with `=` (case-sensitive), so `Foo` and `foo` were distinct rows
    // but `pop foo` returned the older `Foo`.
    let _g = TestGuard::new();
    let stash = fresh_stash();
    stash
      .stash_cmd(&StashedCmd {
        name: Some("Foo".into()),
        buffer: "A".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    stash
      .stash_cmd(&StashedCmd {
        name: Some("foo".into()),
        buffer: "B".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    let got = stash.get_named("foo").unwrap();
    assert_eq!(got.map(|c| c.buffer.to_string()), Some("B".to_string()));
  }

  #[test]
  fn get_named_treats_underscore_literally() {
    // `_` is a LIKE wildcard; with exact match it must be literal.
    let _g = TestGuard::new();
    let stash = fresh_stash();
    stash
      .stash_cmd(&StashedCmd {
        name: Some("my_fix".into()),
        buffer: "X".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    assert!(stash.get_named("myXfix").unwrap().is_none());
    assert!(stash.get_named("my_fix").unwrap().is_some());
  }

  #[test]
  fn list_flag_prints_stashes() {
    let g = TestGuard::new();
    let stash = fresh_stash();
    stash
      .stash_cmd(&StashedCmd {
        name: Some("list_me".into()),
        buffer: "buf".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    test_input("stash --list").unwrap();
    let out = g.read_output();
    assert!(out.contains("list_me"), "got: {out:?}");
  }

  #[test]
  fn short_l_flag_prints_stashes() {
    // Regression: `-l` parsed at the getopt layer but had no arm in
    // from_opts, so it always errored with "unexpected option".
    let g = TestGuard::new();
    let stash = fresh_stash();
    stash
      .stash_cmd(&StashedCmd {
        name: Some("list_me_short".into()),
        buffer: "buf".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    test_input("stash -l").unwrap();
    let out = g.read_output();
    assert!(out.contains("list_me_short"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── -d / --delete ────────────────────────────────────────────

  #[test]
  fn dash_d_deletes_by_name() {
    let g = TestGuard::new();
    let stash = fresh_stash();
    stash
      .stash_cmd(&StashedCmd {
        name: Some("kill_me".into()),
        buffer: "buf".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    test_input("stash -d kill_me").unwrap();
    g.read_output();
    test_input("stash --list").unwrap();
    let out = g.read_output();
    assert!(!out.contains("kill_me"), "got: {out:?}");
  }

  #[test]
  fn long_delete_deletes_by_name() {
    let g = TestGuard::new();
    let stash = fresh_stash();
    stash
      .stash_cmd(&StashedCmd {
        name: Some("gone".into()),
        buffer: "buf".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    test_input("stash --delete gone").unwrap();
    g.read_output();
    test_input("stash --list").unwrap();
    let out = g.read_output();
    assert!(!out.contains("gone"), "got: {out:?}");
  }

  // ─── --stack / --named filters ────────────────────────────────

  #[test]
  fn stack_filter_shows_only_stack_entries() {
    let g = TestGuard::new();
    let stash = fresh_stash();
    // A stacked (unnamed) entry.
    stash
      .stash_cmd(&StashedCmd {
        name: None,
        buffer: "stack_buf".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    // A named entry.
    stash
      .stash_cmd(&StashedCmd {
        name: Some("named_one".into()),
        buffer: "named_buf".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    test_input("stash --list --stack").unwrap();
    let out = g.read_output();
    assert!(out.contains("stack_buf"), "got: {out:?}");
    // --stack filter should hide the named entry's name in the listing.
    assert!(!out.contains("named_one"), "got: {out:?}");
  }

  #[test]
  fn named_filter_shows_only_named_entries() {
    let g = TestGuard::new();
    let stash = fresh_stash();
    stash
      .stash_cmd(&StashedCmd {
        name: None,
        buffer: "anon_buf".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    stash
      .stash_cmd(&StashedCmd {
        name: Some("the_name".into()),
        buffer: "named_buf".into(),
        cursor_pos: "0".into(),
      })
      .unwrap();
    test_input("stash --list --named").unwrap();
    let out = g.read_output();
    assert!(out.contains("the_name"), "got: {out:?}");
    assert!(!out.contains("anon_buf"), "got: {out:?}");
  }
}
