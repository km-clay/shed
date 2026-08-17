use crate::opt;

use super::{
  ShResult, Shed,
  opt::OptSpec,
  outln, sherr,
  state::logic::{AutoCmd, AutoCmdKind},
  with_status,
};

pub(super) struct AutoCmdBuiltin;
impl super::Builtin for AutoCmdBuiltin {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("clear" | 'c')]
  }
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut clear = false;
    for opt in args.options() {
      match opt.key() {
        "clear" => clear = true,
        _ => return Err(sherr!(ExecFail, "unexpected option: {opt}")),
      }
    }

    let mut arg_vec = args.arguments();

    let Some((kind, kind_span)) = arg_vec.next() else {
      return display_autocmds(None);
    };

    let Ok(autocmd_kind) = kind.to_str_lossy().parse::<AutoCmdKind>() else {
      return Err(sherr!(
          ExecFail @ kind_span.clone(),
          "invalid autocmd kind: {kind}",
      ));
    };

    if clear {
      Shed::logic_mut(|l| l.clear_autocmds(autocmd_kind));
      return with_status(0);
    }

    let Some((autocmd_cmd, _)) = arg_vec.next() else {
      return display_autocmds(Some(autocmd_kind));
    };

    let autocmd = AutoCmd::new(autocmd_kind, autocmd_cmd.clone());

    Shed::logic_mut(|l| l.insert_autocmd(autocmd));

    with_status(0)
  }
}

fn display_autocmds(kind: Option<AutoCmdKind>) -> ShResult<()> {
  let mut lines: Vec<String> = vec![];
  match kind {
    Some(kind) => {
      lines.extend(
        Shed::logic(|l| l.get_autocmds(kind))
          .into_iter()
          .map(|ac| ac.to_string()),
      );
    }
    None => {
      for kind in AutoCmdKind::iter() {
        lines.extend(
          Shed::logic(|l| l.get_autocmds(kind))
            .into_iter()
            .map(|ac| ac.to_string()),
        );
      }
    }
  }
  let out = lines.join("\n");
  outln!("{out}");

  with_status(0)
}

#[cfg(test)]
mod tests {
  use crate::{
    state::{self, Shed, logic::AutoCmdKind},
    tests::testutil::{TestGuard, test_input},
  };

  // ===================== Registration =====================

  #[test]
  fn register_pre_cmd() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo hello'").unwrap();

    let cmds = Shed::logic(|l| l.get_autocmds(AutoCmdKind::PreCmd));
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].command(), "echo hello");
  }

  #[test]
  fn register_post_cmd() {
    let _guard = TestGuard::new();
    test_input("autocmd post-cmd 'echo done'").unwrap();

    let cmds = Shed::logic(|l| l.get_autocmds(AutoCmdKind::PostCmd));
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].command(), "echo done");
  }

  #[test]
  fn register_multiple_same_kind() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo first'").unwrap();
    test_input("autocmd pre-cmd 'echo second'").unwrap();

    let cmds = Shed::logic(|l| l.get_autocmds(AutoCmdKind::PreCmd));
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].command(), "echo first");
    assert_eq!(cmds[1].command(), "echo second");
  }

  #[test]
  fn register_different_kinds() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo pre'").unwrap();
    test_input("autocmd post-cmd 'echo post'").unwrap();

    assert_eq!(
      Shed::logic(|l| l.get_autocmds(AutoCmdKind::PreCmd)).len(),
      1
    );
    assert_eq!(
      Shed::logic(|l| l.get_autocmds(AutoCmdKind::PostCmd)).len(),
      1
    );
  }

  // ===================== Clear =====================

  #[test]
  fn clear_autocmds() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo a'").unwrap();
    test_input("autocmd pre-cmd 'echo b'").unwrap();
    assert_eq!(
      Shed::logic(|l| l.get_autocmds(AutoCmdKind::PreCmd)).len(),
      2
    );

    test_input("autocmd -c pre-cmd").unwrap();
    assert_eq!(
      Shed::logic(|l| l.get_autocmds(AutoCmdKind::PreCmd)).len(),
      0
    );
  }

  #[test]
  fn clear_only_affects_specified_kind() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo pre'").unwrap();
    test_input("autocmd post-cmd 'echo post'").unwrap();

    test_input("autocmd -c pre-cmd").unwrap();
    assert_eq!(
      Shed::logic(|l| l.get_autocmds(AutoCmdKind::PreCmd)).len(),
      0
    );
    assert_eq!(
      Shed::logic(|l| l.get_autocmds(AutoCmdKind::PostCmd)).len(),
      1
    );
  }

  #[test]
  fn clear_empty_is_noop() {
    let _guard = TestGuard::new();
    // Clearing when nothing is registered should not error
    test_input("autocmd -c pre-cmd").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Listing =====================

  #[test]
  fn no_args_lists_all_autocmds() {
    let g = TestGuard::new();
    test_input("autocmd pre-cmd 'echo list_me_pre'").unwrap();
    test_input("autocmd post-cmd 'echo list_me_post'").unwrap();
    g.read_output();

    test_input("autocmd").unwrap();
    let out = g.read_output();
    assert!(out.contains("list_me_pre"), "got: {out:?}");
    assert!(out.contains("list_me_post"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn kind_only_lists_autocmds_for_that_kind() {
    let g = TestGuard::new();
    test_input("autocmd pre-cmd 'echo pre_only'").unwrap();
    test_input("autocmd post-cmd 'echo post_only'").unwrap();
    g.read_output();

    test_input("autocmd pre-cmd").unwrap();
    let out = g.read_output();
    assert!(out.contains("pre_only"), "got: {out:?}");
    assert!(!out.contains("post_only"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Error Cases =====================

  #[test]
  fn invalid_kind() {
    let _guard = TestGuard::new();
    test_input("autocmd not-a-real-kind 'echo hi'").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== All valid kind strings =====================

  #[test]
  fn all_kinds_parse() {
    let _guard = TestGuard::new();
    let kinds = [
      "pre-cmd",
      "post-cmd",
      "pre-change-dir",
      "post-change-dir",
      "on-job-finish",
      "pre-prompt",
      "post-prompt",
      "pre-mode-change",
      "post-mode-change",
      "on-history-open",
      "on-history-close",
      "on-history-select",
      "on-completion-start",
      "on-completion-cancel",
      "on-completion-select",
      "on-exit",
    ];
    for kind in kinds {
      test_input(format!("autocmd {kind} 'true'")).unwrap();
    }
  }

  // ===================== Execution =====================

  #[test]
  fn exec_fires_autocmd() {
    let guard = TestGuard::new();
    // Register a post-change-dir autocmd and trigger it via cd
    test_input("autocmd post-change-dir 'echo changed'").unwrap();
    guard.read_output();

    test_input("cd /tmp").unwrap();
    let out = guard.read_output();
    assert!(out.contains("changed"));
  }

  #[test]
  fn exec_preserves_status() {
    let _guard = TestGuard::new();
    // autocmd exec should restore the status code from before it ran
    test_input("autocmd post-change-dir 'false'").unwrap();

    test_input("true").unwrap();
    assert_eq!(state::Shed::get_status(), 0);

    test_input("cd /tmp").unwrap();
    // cd itself succeeds, autocmd runs `false` but status should be
    // restored to cd's success
    assert_eq!(state::Shed::get_status(), 0);
  }
}
