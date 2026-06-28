use std::{path::Path, sync::atomic::Ordering};

use nix::unistd::{Pid, isatty};

use crate::{
  eval::execute::exec_int,
  procio::{bytes_to_string, read_input},
};

use super::{
  ShResult, Shed, errln,
  eval::execute::{exec_dash_c, exec_nonint},
  expand_keymap, interactive, lifecycle, procio, sherr,
  signal::QUIT_CODE,
  state, status_msg,
};

pub fn dispatch_input(mut args: lifecycle::ShedArgs) -> ShResult<()> {
  if args.edit_script {
    // in this arm, we interpret the input we are given as a sequence of keys
    // for the line editor to consume and execute
    let input = if let Some(ref cmd) = args.command {
      cmd.clone()
    } else if args.stdin {
      // explicit `-s`: read stdin as the script, script_args are positional
      bytes_to_string(read_input()?)
    } else if !args.script_args.is_empty() {
      let path = args.script_args.remove(0);
      std::fs::read_to_string(path)?
    } else if !isatty(procio::stdin_fileno()).unwrap_or(false) {
      bytes_to_string(read_input()?)
    } else {
      // no input provided, just run interactively
      status_msg!("warning: --script was passed but no input was given");
      return interactive::shed_interactive(&args, None);
    };

    let keys = expand_keymap(&input);
    interactive::shed_interactive(&args, Some(keys))
  } else if let Some(cmd) = args.command {
    exec_dash_c(cmd, args.script_args)
  } else if args.stdin {
    // explicit `-s`: read stdin, script_args are positional
    read_commands(args.script_args)
  } else if !args.script_args.is_empty() {
    let path = args.script_args.remove(0);
    run_script(path, args.script_args)
  } else if !isatty(procio::stdin_fileno()).unwrap_or(false) {
    read_commands(args.script_args)
  } else {
    interactive::shed_interactive(&args, None)
  }
}

pub(crate) fn read_commands(args: Vec<String>) -> ShResult<()> {
  let bytes = read_input()?;
  let commands = bytes_to_string(bytes);

  Shed::vars_mut(|v| {
    let scope = v.cur_scope_mut();
    let zero = scope.sh_argv().front().cloned().unwrap_or_default();
    scope.sh_argv_mut().clear();
    scope.bpush_arg(zero);
    for arg in args {
      scope.bpush_arg(arg.into());
    }
  });

  exec_nonint(commands.into(), None)
}

pub(crate) fn run_script<P: AsRef<Path>>(path: P, args: Vec<String>) -> ShResult<()> {
  let path = path.as_ref();
  let source_path = state::util::display_path(path);

  if !path.is_file() {
    errln!("shed: Failed to open input file: {source_path}");
    QUIT_CODE.store(1, Ordering::SeqCst);
    return Err(sherr!(CleanExit(1), "input file not found",));
  }
  let Ok(input) = std::fs::read_to_string(path) else {
    errln!("shed: Failed to read input file: {source_path}");
    QUIT_CODE.store(1, Ordering::SeqCst);
    return Err(sherr!(CleanExit(1), "failed to read input file",));
  };

  let path_str = path.to_string_lossy().to_string();
  Shed::vars_mut(|v| {
    v.set_param(state::vars::ShellParam::ShellName, &path_str); // $0
    let scope = v.cur_scope_mut();
    scope.sh_argv_mut().clear();
    scope.bpush_arg(path_str.into());
    for arg in args {
      scope.bpush_arg(arg.into());
    }
  });

  let controller = Shed::term(|t| t.controller());

  if let Some(pid) = controller
    && pid == Pid::this()
  {
    exec_int(input.into(), Some(source_path.into()))
  } else {
    exec_nonint(input.into(), Some(source_path.into()))
  }
}

#[cfg(test)]
mod dispatch_input_tests {
  //! Tests for `dispatch_input`'s routing logic.
  //!
  //! What's covered: the `exec_dash_c` and `read_commands` branches —
  //! both reachable through `TestGuard` without touching signal handlers,
  //! sockets, or rc files.
  //!
  //! What's not covered: every `edit_script=true` arm and the no-input
  //! interactive fallback route to `interactive::shed_interactive`,
  //! which installs process-wide signal handlers, opens a control
  //! socket on disk, and may execute the user's real ~/.shedrc — not
  //! safe to invoke from tests without a hermetic harness. The
  //! `run_script` arm is also gated by stdin being a real tty, which
  //! `TestGuard` can't currently provide.

  use super::*;
  use crate::lifecycle::ShedArgs;
  use crate::state;
  use crate::tests::testutil::TestGuard;

  /// Build a minimally-set `ShedArgs` for the non-interactive paths.
  fn args(command: Option<&str>, stdin: bool, script_args: Vec<String>) -> ShedArgs {
    ShedArgs {
      command: command.map(String::from),
      script_args,
      version: false,
      interactive: false,
      stdin,
      login_shell: false,
      welcome: false,
      noexec: false,
      rc_path: None,
      no_rc: true,
      set: vec![],
      edit_script: false,
    }
  }

  // ─── -c <command> path ──────────────────────────────────────────

  #[test]
  fn dispatch_command_runs_exec_dash_c() {
    let g = TestGuard::new();
    dispatch_input(args(Some("echo cmd_route"), false, vec![])).unwrap();
    let out = g.read_output();
    assert!(out.contains("cmd_route"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn dispatch_command_passes_script_args_as_positional() {
    let g = TestGuard::new();
    // For -c, script_args become $0, $1, ... so the printed args should
    // show 'hello' and 'world'.
    dispatch_input(args(
      Some("echo $1 $2"),
      false,
      vec!["progname".into(), "hello".into(), "world".into()],
    ))
    .unwrap();
    let out = g.read_output();
    assert!(out.contains("hello world"), "got: {out:?}");
  }

  #[test]
  fn dispatch_command_nonzero_exit_propagates_status() {
    let _g = TestGuard::new();
    dispatch_input(args(Some("false"), false, vec![])).ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── read-from-stdin path (explicit -s flag) ────────────────────

  #[test]
  fn dispatch_stdin_flag_routes_to_read_commands() {
    let mut g = TestGuard::new();
    g.feed_stdin(b"echo from_stdin\n");
    dispatch_input(args(None, true, vec![])).unwrap();
    let out = g.read_output();
    assert!(out.contains("from_stdin"), "got: {out:?}");
  }

  #[test]
  fn dispatch_stdin_passes_script_args_as_positional() {
    let mut g = TestGuard::new();
    g.feed_stdin(b"echo $1 $2\n");
    // For the stdin path, script_args are pushed wholesale as $1, $2,
    // ... after preserving the existing $0 (unlike -c, which consumes
    // the first as the script name).
    dispatch_input(args(None, true, vec!["one".into(), "two".into()])).unwrap();
    let out = g.read_output();
    assert!(out.contains("one two"), "got: {out:?}");
  }

  // ─── auto-stdin path (no flags, stdin is a pipe → not-a-tty) ────

  #[test]
  fn dispatch_no_flags_with_non_tty_stdin_reads_commands() {
    // TestGuard's stdin is a pipe, so isatty(stdin)=false and the
    // implicit-stdin branch fires without the user setting -s.
    let mut g = TestGuard::new();
    g.feed_stdin(b"echo implicit_stdin\n");
    dispatch_input(args(None, false, vec![])).unwrap();
    let out = g.read_output();
    assert!(out.contains("implicit_stdin"), "got: {out:?}");
  }

  // ─── precedence: -c beats every other arm ───────────────────────

  #[test]
  fn dispatch_command_beats_stdin_flag() {
    let mut g = TestGuard::new();
    // If precedence were wrong, the stdin contents would run and
    // produce 'should_not_run'; -c's command should win.
    g.feed_stdin(b"echo should_not_run\n");
    dispatch_input(args(Some("echo cmd_wins"), true, vec![])).unwrap();
    let out = g.read_output();
    assert!(out.contains("cmd_wins"), "got: {out:?}");
    assert!(!out.contains("should_not_run"), "got: {out:?}");
  }

  // ─── empty stdin is OK (read returns 0 immediately) ─────────────

  #[test]
  fn dispatch_stdin_empty_input_is_noop() {
    let mut g = TestGuard::new();
    g.feed_stdin(b""); // closes write end → immediate EOF
    dispatch_input(args(None, true, vec![])).unwrap();
    // Nothing should have run; status stays whatever the previous test
    // left, so just check we returned without erroring and produced no
    // shell output.
    let out = g.read_output();
    assert_eq!(out, "");
  }
}
