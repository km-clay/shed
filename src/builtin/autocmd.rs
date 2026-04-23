use crate::{
  getopt::{Opt, OptSpec},
  sherr,
  state::{AutoCmd, AutoCmdKind, write_logic},
  util::{error::ShResult, with_status},
};

pub struct AutoCmdOpts {
  clear: bool,
}
pub fn get_autocmd_opts(opts: &[Opt]) -> ShResult<AutoCmdOpts> {
  let mut autocmd_opts = AutoCmdOpts { clear: false };

  let mut opts = opts.iter();
  while let Some(arg) = opts.next() {
    match arg {
      Opt::Short('c') => {
        autocmd_opts.clear = true;
      }
      _ => {
        return Err(sherr!(ExecFail, "unexpected option: {}", arg,));
      }
    }
  }

  Ok(autocmd_opts)
}

pub(super) struct AutoCmdBuiltin;
impl super::Builtin for AutoCmdBuiltin {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::flag('c')]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let mut clear = false;
    let mut argv = args.argv.iter();
    for opt in args.opts {
      match opt {
        Opt::Short('c') => clear = true,
        _ => return Err(sherr!(ExecFail, "unexpected option: {}", opt,)),
      }
    }

    let Some(autocmd_kind) = argv.next() else {
      return Err(sherr!(
          ExecFail @ span,
          "expected an autocmd kind",
      ));
    };

    let Ok(autocmd_kind) = autocmd_kind.0.parse::<AutoCmdKind>() else {
      return Err(sherr!(
          ExecFail @ autocmd_kind.1.clone(),
          "invalid autocmd kind: {}", autocmd_kind.0,
      ));
    };

    if clear {
      write_logic(|l| l.clear_autocmds(autocmd_kind));
      return with_status(0);
    }

    let Some(autocmd_cmd) = argv.next() else {
      return Err(sherr!(
          ExecFail @ span,
          "expected an autocmd command",
      ));
    };

    let autocmd = AutoCmd {
      kind: autocmd_kind,
      command: autocmd_cmd.0.clone(),
    };

    write_logic(|l| l.insert_autocmd(autocmd));

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{self, AutoCmdKind, read_logic};
  use crate::testutil::{TestGuard, test_input};

  // ===================== Registration =====================

  #[test]
  fn register_pre_cmd() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo hello'").unwrap();

    let cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd));
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].command, "echo hello");
  }

  #[test]
  fn register_post_cmd() {
    let _guard = TestGuard::new();
    test_input("autocmd post-cmd 'echo done'").unwrap();

    let cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::PostCmd));
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].command, "echo done");
  }

  #[test]
  fn register_multiple_same_kind() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo first'").unwrap();
    test_input("autocmd pre-cmd 'echo second'").unwrap();

    let cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd));
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].command, "echo first");
    assert_eq!(cmds[1].command, "echo second");
  }

  #[test]
  fn register_different_kinds() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo pre'").unwrap();
    test_input("autocmd post-cmd 'echo post'").unwrap();

    assert_eq!(read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd)).len(), 1);
    assert_eq!(
      read_logic(|l| l.get_autocmds(AutoCmdKind::PostCmd)).len(),
      1
    );
  }

  // ===================== Clear =====================

  #[test]
  fn clear_autocmds() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo a'").unwrap();
    test_input("autocmd pre-cmd 'echo b'").unwrap();
    assert_eq!(read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd)).len(), 2);

    test_input("autocmd -c pre-cmd").unwrap();
    assert_eq!(read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd)).len(), 0);
  }

  #[test]
  fn clear_only_affects_specified_kind() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd 'echo pre'").unwrap();
    test_input("autocmd post-cmd 'echo post'").unwrap();

    test_input("autocmd -c pre-cmd").unwrap();
    assert_eq!(read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd)).len(), 0);
    assert_eq!(
      read_logic(|l| l.get_autocmds(AutoCmdKind::PostCmd)).len(),
      1
    );
  }

  #[test]
  fn clear_empty_is_noop() {
    let _guard = TestGuard::new();
    // Clearing when nothing is registered should not error
    test_input("autocmd -c pre-cmd").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  // ===================== Error Cases =====================

  #[test]
  fn missing_kind() {
    let _guard = TestGuard::new();
    test_input("autocmd").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn invalid_kind() {
    let _guard = TestGuard::new();
    test_input("autocmd not-a-real-kind 'echo hi'").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn missing_command() {
    let _guard = TestGuard::new();
    test_input("autocmd pre-cmd").ok();
    assert_ne!(state::get_status(), 0);
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
    assert_eq!(state::get_status(), 0);

    test_input("cd /tmp").unwrap();
    // cd itself succeeds, autocmd runs `false` but status should be
    // restored to cd's success
    assert_eq!(state::get_status(), 0);
  }
}
