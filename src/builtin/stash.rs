use crate::state::vars::VarStr;

use super::{
  getopt::{Opt, OptSpec},
  match_loop, outln,
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
  pub fn from_opts(opts: Vec<Opt>) -> ShResult<Self> {
    let mut new = Self::default();
    let mut opt_iter = opts.into_iter();

    match_loop!(opt_iter.next() => opt, {
      Opt::ShortWithList('s',mut args) => {
        // length of 'args' is enforced by the opt spec
        let cursor = args.pop().unwrap();
        let buffer = args.pop().unwrap();
        let name = args.pop().unwrap();
        new.to_save.push(StashedCmd {
          name: Some(name),
          buffer,
          cursor_pos: cursor,
        });
      }
      Opt::LongWithList(opt, mut args) => {
        let "save" = opt.as_str() else {
          return Err(sherr!(ParseErr, "unexpected option {opt} in stash"))
        };

        // length of 'args' is enforced by the opt spec
        let cursor = args.pop().unwrap();
        let buffer = args.pop().unwrap();
        let name = args.pop().unwrap();
        new.to_save.push(StashedCmd {
          name: Some(name),
          buffer,
          cursor_pos: cursor,
        });
      }
      Opt::ShortWithArg('d', arg) => {
        new.to_delete.push(arg);
      }
      Opt::LongWithArg(opt, arg) => {
        match opt.as_str() {
          "delete" => new.to_delete.push(arg),
          _ => return Err(sherr!(ParseErr, "unexpected option {opt} in stash"))
        }
      }
      Opt::Long(arg) => {
        match arg.as_str() {
          "list" => new.list = true,
          "stack" => new.only_stack = true,
          "named" => new.only_named = true,
          _ => return Err(sherr!(ParseErr, "unexpected option {arg} in stash"))
        }
      }
      _ => return Err(sherr!(ParseErr, "unexpected option {opt} in stash"))
    });

    Ok(new)
  }
}

pub(super) struct StashBuiltin;
impl super::Builtin for StashBuiltin {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::exact_args('s', 3),
      OptSpec::exact_args("save", 3),
      OptSpec::single_arg('d'),
      OptSpec::single_arg("delete"),
      OptSpec::flag('l'),
      OptSpec::flag("list"),
      OptSpec::flag("stack"),
      OptSpec::flag("named"),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let is_empty = args.opts.is_empty();
    let stash_opts = StashOpts::from_opts(args.opts).promote_err(span.clone())?;
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
