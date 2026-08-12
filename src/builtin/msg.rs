use crate::{opt, procio::bytes_to_string, socket_msg, state::vars::VarStr};

use super::{
  Shed, join_raw_args,
  opt::OptSpec,
  outln, sherr, status_msg, system_msg,
  util::{ShResult, with_status},
};

pub(super) struct Msg;
impl super::Builtin for Msg {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("status" | 's'),
      opt!("system" | 'S'),
      opt!("broadcast" | 'b'),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let mut system = false;
    let mut status = false;
    let mut broadcast = false;

    let (arg_vec, opts) = args.take_argv();

    for opt in &opts {
      match opt.key() {
        "system" => system = true,
        "status" => status = true,
        "broadcast" => broadcast = true,
        _ => {
          return Err(sherr!(ExecFail, "msg: Unexpected flag '{opt}'",));
        }
      }
    }

    let input = if arg_vec.is_empty() {
      self.get_input(&mut args).map(|s| {
        let mut s = bytes_to_string(s);
        s.truncate(s.trim_end_matches('\n').len());
        VarStr::from(s)
      })
    } else {
      None
    };

    if input.is_none() && arg_vec.is_empty() {
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

    let msg: VarStr = input.unwrap_or_else(|| join_raw_args(arg_vec).0);

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
  fn msg_piped_leading_newlines_strips_only_trailing() {
    // Regression: `trim_matches` computed a both-ends-trimmed length but
    // `truncate` cut from the end, so `\n\nhello\n` became `\n\nhel` (leading
    // blanks kept, content lopped off). Only trailing newlines should go.
    let _g = TestGuard::new();
    drain_all();
    // `msg`'s side effect (posting to the status queue) only reaches the parent
    // when the final pipeline stage runs in-process; the default `all` forks it.
    test_input("shopt core.pipeline_style=last; printf '\\n\\nhello\\n' | msg").unwrap();
    assert_eq!(Shed::pop_status_msg().as_deref(), Some("\n\nhello"));
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
