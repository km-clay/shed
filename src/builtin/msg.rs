use crate::socket_msg;

use super::{
  Shed,
  getopt::{Opt, OptSpec},
  join_raw_args, outln, sherr, status_msg, system_msg,
  util::{ShResult, with_status},
};

pub(super) struct Msg;
impl super::Builtin for Msg {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('s'),
      OptSpec::flag('S'),
      OptSpec::flag('b'),
      OptSpec::flag("status"),
      OptSpec::flag("system"),
      OptSpec::flag("broadcast"),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let mut system = false;
    let mut status = false;
    let mut broadcast = false;

    for opt in &args.opts {
      match opt {
        Opt::Short('S') => system = true,
        Opt::Short('s') => status = true,
        Opt::Short('b') => broadcast = true,
        Opt::Long(o) if o.as_str() == "system" => system = true,
        Opt::Long(o) if o.as_str() == "status" => status = true,
        Opt::Long(o) if o.as_str() == "broadcast" => broadcast = true,
        _ => {
          return Err(sherr!(ExecFail, "msg: Unexpected flag '{opt}'",));
        }
      }
    }

    let input = self
      .get_input(&mut args)
      .map(|s| s.trim_matches('\n').to_string());

    if input.is_none() && args.argv.is_empty() {
      // argv is empty → list past messages and exit; nothing to post.
      let history = if system {
        Shed::system_msg_hist()
      } else {
        Shed::status_msg_hist()
      };

      for msg in history {
        let formatted = msg.with_timestamp();
        outln!("{formatted}");
      }

      return with_status(0);
    }

    let msg = input.unwrap_or_else(|| join_raw_args(args.argv).0);

    if broadcast {
      // sends to all socket subscribers
      socket_msg!("{msg}");
    }

    if system {
      system_msg!("{msg}");
    }

    // defaults to status messages if no flag is provided, but if both are provided we post to both
    if status || (!system && !broadcast) {
      status_msg!("{msg}");
    }

    with_status(0)
  }
}

#[cfg(test)]
#[expect(non_snake_case)] // test names deliberately preserve the -s vs -S case distinction
mod msg_tests {
  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, test_input};

  /// Drain both queues so we have a clean slate; prior tests in this
  /// thread may have left messages behind (`TestGuard` restores Shed
  /// state but the message queues are not part of that save/restore).
  fn drain_all() {
    while Shed::pop_status_msg().is_some() {}
    while Shed::pop_system_msg().is_some() {}
  }

  // ─── default: posts to status queue ────────────────────────────────

  #[test]
  fn msg_with_no_flags_posts_to_status_queue() {
    let _g = TestGuard::new();
    drain_all();
    test_input("msg hello").unwrap();
    assert_eq!(Shed::pop_status_msg().as_deref(), Some("hello"));
    assert_eq!(Shed::pop_system_msg(), None);
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn msg_joins_multiple_args_with_spaces() {
    let _g = TestGuard::new();
    drain_all();
    test_input("msg foo bar biz").unwrap();
    assert_eq!(Shed::pop_status_msg().as_deref(), Some("foo bar biz"));
  }

  // ─── short flags ──────────────────────────────────────────────────

  #[test]
  fn msg_dash_s_explicit_status() {
    let _g = TestGuard::new();
    drain_all();
    test_input("msg -s hello").unwrap();
    assert_eq!(Shed::pop_status_msg().as_deref(), Some("hello"));
    assert_eq!(Shed::pop_system_msg(), None);
  }

  #[test]
  fn msg_dash_S_posts_to_system_queue() {
    let g = TestGuard::new();
    drain_all();
    test_input("msg -S important").unwrap();
    let out = g.read_output();
    assert!(out.contains("important"), "got: {out:?}");
    // -S alone shouldn't also post to status queue.
    assert_eq!(Shed::pop_status_msg(), None);
  }

  #[test]
  fn msg_both_s_and_S_posts_to_both_queues() {
    let g = TestGuard::new();
    drain_all();
    test_input("msg -s -S double").unwrap();
    assert_eq!(Shed::pop_status_msg().as_deref(), Some("double"));
    let out = g.read_output();
    assert!(out.contains("double"), "got: {out:?}");
  }

  // ─── long flags ──────────────────────────────────────────────────

  #[test]
  fn msg_long_status_flag() {
    let _g = TestGuard::new();
    drain_all();
    test_input("msg --status sticky").unwrap();
    assert_eq!(Shed::pop_status_msg().as_deref(), Some("sticky"));
    assert_eq!(Shed::pop_system_msg(), None);
  }

  #[test]
  fn msg_long_system_flag() {
    let g = TestGuard::new();
    drain_all();
    test_input("msg --system alert").unwrap();
    let out = g.read_output();
    assert!(out.contains("alert"), "got: {out:?}");
    assert_eq!(Shed::pop_status_msg(), None);
  }
}
