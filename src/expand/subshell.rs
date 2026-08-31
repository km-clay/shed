use std::os::fd::{AsFd, AsRawFd};

use crate::{
  errln,
  eval::{
    execute,
    parse::{ParsedSrc, node},
  },
  expand::arithmetic,
  lifecycle,
  procio::{self, RedirSet, RedirSpec, RedirType, SinkScope, StdinPipe},
  readline::{self, NestedSub},
  sherr,
  state::{Shed, meta::MetaTab, terminal::Terminal, vars::VarStr},
  util::{error::ShResult, guards},
};

use bstr::ByteSlice;
use nix::errno::Errno;
use nix::sys::wait::{WaitPidFlag as WtFlag, WaitStatus as WtStat, waitpid};
use nix::unistd::{ForkResult, fork};

pub(crate) fn expand_proc_sub(raw: &str, is_input: bool) -> ShResult<String> {
  let (rpipe, wpipe) = procio::pipes_high_no_cloexec()?;
  let rpipe_raw = rpipe.as_raw_fd();
  let wpipe_raw = wpipe.as_raw_fd();

  let (proc_fd, register_fd, redir_type, path) = if is_input {
    (
      rpipe,
      wpipe,
      RedirType::Input,
      format!("/dev/fd/{wpipe_raw}"),
    )
  } else {
    (
      wpipe,
      rpipe,
      RedirType::Output,
      format!("/dev/fd/{rpipe_raw}"),
    )
  };

  let target_fd = match redir_type {
    RedirType::Input => 0,
    RedirType::Output => 1,
    _ => unreachable!(),
  };

  let sink_stdin = (target_fd != 0).then(procio::take_stdin).flatten();
  let stdin_pipe = sink_stdin.is_some().then(StdinPipe::new).transpose()?;

  match unsafe { fork()? } {
    ForkResult::Child => {
      lifecycle::setup_child();

      // Drop our reference to the tty fd before exec; otherwise the
      // orphaned procsub child (we don't wait on it) holds the pty
      // slave open. On macOS that prevents the master from ever
      // returning EOF, which deadlocks TestGuard teardown.
      Shed::term_mut(Terminal::detach_tty);
      drop(register_fd);

      let mut specs = vec![RedirSpec::dup(
        proc_fd.as_raw_fd(),
        target_fd,
        RedirType::Output,
      )];
      let _stdin_r_keep = stdin_pipe.map(|p| p.into_child(&mut specs));
      let redir: RedirSet = specs.into();
      let _guard = redir.apply().or_fatal()?;

      if let Err(e) = execute::exec_nonint(raw.into(), Some("process_sub".into())) {
        e.print_error();

        lifecycle::exit_shed(true, 1);
      }

      lifecycle::exit_shed(true, Shed::get_status());
    }
    ForkResult::Parent { .. } => {
      Shed::meta_mut(|m| m.save_procsub_fd(register_fd));
      // Feed the sink in the background; the procsub child is not waited on, so
      // the feeder thread is detached and ends on its own at EOF/EPIPE.
      if let (Some(pipe), Some(bytes)) = (stdin_pipe, sink_stdin) {
        procio::feed_fd_async(pipe.into_writer(), bytes);
      }
      // Do not wait; process may run in background
      Ok(path)
    }
  }
}

pub(crate) fn is_internal(raw: &str) -> bool {
  let mut parser = ParsedSrc::new(raw.into()).with_name("is_internal check".into());

  if parser.parse_src().is_err() {
    return false;
  }

  let ast = parser.into_ast();
  let roots = ast.roots();

  if !node::nodes_have_only_builtins(&ast, roots.iter().copied()) {
    return false;
  }

  let has_forking_sub = readline::nested_subs(raw).into_iter().any(|sub| match sub {
    NestedSub::Proc => true,
    NestedSub::Cmd(body) => !is_internal(&body.to_str_lossy()),
  });
  if has_forking_sub {
    return false;
  }

  true
}

pub(crate) fn internal_cmd_sub(raw: &str) -> ShResult<VarStr> {
  let sink_scope = SinkScope::new();
  let _ceiling = guards::isolation_guard(None);

  if let Err(e) = execute::exec_nonint(raw.into(), Some("command_sub".into())) {
    e.print_error();
  }

  let scope = sink_scope.take();

  if scope.was_truncated() {
    Shed::set_status(procio::SINK_TRUNCATED_STATUS);
    let size = scope.limit();

    errln!("shed: command sub truncated (exceeded {size})");
  }

  Shed::meta_mut(|m| m.set_last_cmdsub_status(Shed::get_status()));

  let output = VarStr::from(scope.into_buf().trim_end_with(|c| c == '\n'));

  Ok(output)
}

/// Get the command output of a given command input as a String
pub(crate) fn expand_cmd_sub(raw: &str) -> ShResult<VarStr> {
  if raw.starts_with('(') && raw.ends_with(')') {
    return arithmetic::expand_arithmetic_wrapped(raw.as_bytes());
  }
  // command subs add an xtrace layer
  let _xtrace = Shed::meta_mut(MetaTab::xtrace_descend);

  if is_internal(raw) {
    return internal_cmd_sub(raw);
  }

  let (rpipe, wpipe) = procio::pipes_high()?;

  // If this fork happens while an in-process pipeline stdin sink is live,
  // materialize it onto the child's fd 0 so a forked child (e.g. an external
  // command inside the sub) can still read the piped input.
  let sink_stdin = procio::take_stdin();
  let stdin_pipe = sink_stdin.is_some().then(StdinPipe::new).transpose()?;

  match unsafe { fork()? } {
    ForkResult::Child => {
      lifecycle::setup_child();

      let mut specs = vec![RedirSpec::dup(wpipe.as_raw_fd(), 1, RedirType::Output)];
      let _stdin_r_keep = stdin_pipe.map(|p| p.into_child(&mut specs));
      let redir: RedirSet = specs.into();
      let _redir_guard = redir.apply().or_fatal()?;

      execute::catch_exit(
        || execute::exec_input(raw.into(), Some("command_sub".into())),
        execute::exit_with,
      );

      let code = Shed::get_status();
      lifecycle::exit_shed(true, code);
    }
    ForkResult::Parent { child } => {
      drop(wpipe);

      let feeder = match (stdin_pipe, sink_stdin) {
        (Some(pipe), Some(bytes)) => Some(procio::feed_fd_async(pipe.into_writer(), bytes)),
        _ => None,
      };

      // Read output first (before waiting) to avoid deadlock if
      // child fills pipe buffer
      let sink = procio::read_to_sink(rpipe.as_fd())?;
      if let Some(handle) = feeder {
        let _ = handle.join();
      }
      let truncated = sink.was_truncated();
      let size = sink.limit();
      let output = VarStr::from(sink.into_buf().trim_end_with(|c| c == '\n'));

      // Wait for child with EINTR retry
      let status = loop {
        match waitpid(child, Some(WtFlag::WUNTRACED)) {
          Ok(status) => break status,
          Err(Errno::EINTR) => (),
          Err(e) => return Err(e.into()),
        }
      };

      match status {
        WtStat::Exited(_, code) => {
          Shed::set_status(code);
          // Truncation takes precedence over the child's own exit code.
          if truncated {
            Shed::set_status(procio::SINK_TRUNCATED_STATUS);
            errln!("shed: command sub truncated (exceeded {size})");
          }

          Shed::meta_mut(|m| m.set_last_cmdsub_status(Shed::get_status()));
          Ok(output)
        }
        _ => Err(sherr!(InternalErr, "Command sub failed")),
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::tests::testutil::TestGuard;

  // ===================== Command Substitution (TestGuard) =====================

  #[test]
  fn cmd_sub_echo() {
    let _guard = TestGuard::new();
    let result = expand_cmd_sub("echo hello").unwrap();
    assert_eq!(result, "hello");
  }

  // ===================== is_internal fast-path gate (#145) =====================
  //
  // A forking substitution buried in a word must disqualify the in-process
  // path; a genuinely all-builtin body (even with all-builtin nesting) must
  // stay in-process. Output alone can't distinguish the paths, so gate the
  // decision directly.

  #[test]
  fn is_internal_plain_builtin_body() {
    let _g = TestGuard::new();
    assert!(is_internal("echo hi"));
    assert!(is_internal("printf '%s' x"));
  }

  #[test]
  fn is_internal_all_builtin_nesting_stays_inprocess() {
    let _g = TestGuard::new();
    assert!(is_internal(r#"echo "$(echo 1)""#));
    assert!(is_internal(r#"echo "$(( 2 + 3 ))""#));
    assert!(is_internal(r#"echo "$( { echo y; } )""#));
  }

  #[test]
  fn is_internal_nested_external_forks() {
    let _g = TestGuard::new();
    assert!(!is_internal(r#"echo "$(echo 1 | cat)""#));
    assert!(!is_internal(r#"echo "$(echo "$(echo 1 | cat)")""#));
    assert!(!is_internal(r#"echo "`echo 7 | cat`""#));
  }

  #[test]
  fn is_internal_nested_sub_forks_without_external() {
    // Subshell and redirect both fork despite containing only builtins — the
    // holes an "external command present" heuristic would miss.
    let _g = TestGuard::new();
    assert!(!is_internal(r#"echo "$( (echo x) )""#));
    assert!(!is_internal(r#"echo "$(read v < /dev/null; echo $v)""#));
  }

  #[test]
  fn is_internal_sub_in_param_exp_and_arith() {
    let _g = TestGuard::new();
    assert!(!is_internal(r#"echo "${foo:+$(echo 1 | cat)}""#));
    assert!(!is_internal(r#"echo "$(( $(echo 3 | cat) + 1 ))""#));
  }

  #[test]
  fn is_internal_single_quoted_sub_is_literal() {
    // A `$(…)` inside single quotes is literal text, not a substitution.
    let _g = TestGuard::new();
    assert!(is_internal(r"echo '$(echo nope | cat)'"));
  }

  #[test]
  fn is_internal_function_body_forking_sub() {
    // A function whose body embeds a forking sub in a word must disqualify the
    // caller — the AST walk alone can't see into the body's word tokens. (#145)
    let _g = TestGuard::new();
    expand_cmd_sub(r#"f() { echo "$(echo 1 | cat)"; }"#).unwrap();
    assert!(!is_internal("f"));

    expand_cmd_sub(r#"g() { echo "$( (echo x) )"; }"#).unwrap();
    assert!(!is_internal("g"));
  }

  #[test]
  fn is_internal_all_builtin_function_stays_inprocess() {
    let _g = TestGuard::new();
    expand_cmd_sub(r#"h() { echo "$(echo 1)"; }"#).unwrap();
    assert!(is_internal("h"));
  }

  #[test]
  fn cmd_sub_trailing_newlines_stripped() {
    let _guard = TestGuard::new();
    let result = expand_cmd_sub("printf 'hello\\n\\n'").unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn cmd_sub_arithmetic() {
    let result = expand_cmd_sub("(1+2)").unwrap();
    assert_eq!(result, "3");
  }

  #[test]
  fn cmd_sub_only_final_newline_is_stripped() {
    // Internal newlines must survive; just the trailing run is removed.
    let _g = TestGuard::new();
    let result = expand_cmd_sub("printf 'a\\nb\\nc\\n'").unwrap();
    assert_eq!(result, "a\nb\nc");
  }

  #[test]
  fn cmd_sub_empty_output() {
    let _g = TestGuard::new();
    let result = expand_cmd_sub("true").unwrap();
    assert_eq!(result, "");
  }

  #[test]
  fn cmd_sub_inprocess_isolates_cd() {
    use crate::state::{Shed, vars::VarFlags, vars::VarKind};
    use crate::tests::testutil::canon;

    let _g = TestGuard::new();
    let start = std::env::current_dir().unwrap();
    // cwd_guard keys off `$PWD`; give it a baseline to save/compare against.
    Shed::vars_mut(|v| {
      v.set_var(
        "PWD",
        VarKind::string(start.to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .ok();

    let tmp = tempfile::TempDir::new().unwrap();
    // `cd` is builtin-only, so this takes the in-process path (internal_cmd_sub).
    let _ = expand_cmd_sub(&format!("cd {}", canon(tmp.path()).display()));

    let after = std::env::current_dir().unwrap();
    // Restore before asserting so a regression can't leak into sibling tests.
    let _ = std::env::set_current_dir(&start);
    assert_eq!(
      canon(&after),
      canon(&start),
      "in-process command substitution leaked `cd` into the parent cwd"
    );
  }

  #[test]
  fn cmd_sub_runs_and_captures_in_sub_exit_trap() {
    // `trap` forces a fork; the forked child must run its own EXIT trap on the
    // way out (via exit_shed) and the output must land in the captured sub — not
    // leak to the parent. Exercises trap-forces-fork + setup_child + exit_shed.
    let _g = TestGuard::new();
    let result = expand_cmd_sub("trap 'echo trapped' EXIT; true").unwrap();
    assert_eq!(result, "trapped");
  }

  #[test]
  fn cmd_sub_sets_status_to_child_exit_code() {
    // `(exit N)` would hit the arithmetic fast-path; use a bare
    // command that genuinely exits with the desired status.
    let _g = TestGuard::new();
    expand_cmd_sub("false").unwrap();
    assert_eq!(crate::state::Shed::get_status(), 1);
  }

  #[test]
  fn cmd_sub_zero_status_on_success() {
    let _g = TestGuard::new();
    expand_cmd_sub("true").unwrap();
    assert_eq!(crate::state::Shed::get_status(), 0);
  }

  #[test]
  fn cmd_sub_arithmetic_distinguished_from_subshell_grouping() {
    // The outer-parens-check fast-path routes "(N+M)" to the arithmetic
    // expander, not to fork+exec. Verify by giving an arithmetic input
    // that wouldn't be valid as a shell command.
    let result = expand_cmd_sub("(10*5)").unwrap();
    assert_eq!(result, "50");
  }

  #[test]
  fn cmd_sub_large_output_does_not_deadlock() {
    // Parent reads from the pipe before waitpid; otherwise a child
    // writing more than the pipe buffer would block forever. Build
    // the payload via shell-only string doubling + `echo` (builtin,
    // so no execve / ARG_MAX involvement) — no PATH dependency.
    let _g = TestGuard::new();
    // 2^18 = 262144 chars — comfortably above a typical 64KB pipe buf.
    let result = expand_cmd_sub(
      "s=x; for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18; do s=$s$s; done; echo \"$s\"",
    )
    .unwrap();
    assert_eq!(result.len(), 1 << 18);
    assert!(result.to_str_lossy().chars().all(|c| c == 'x'));
  }

  // ===================== expand_proc_sub =====================

  #[test]
  fn proc_sub_input_returns_dev_fd_path() {
    // is_input=true: path points at the writer fd we hold open in the
    // parent (so we could write through it); the format is the
    // /dev/fd/N path.
    let _g = TestGuard::new();
    let path = expand_proc_sub("echo hello", true).unwrap();
    assert!(
      path.starts_with("/dev/fd/"),
      "expected /dev/fd/... path, got: {path:?}"
    );
  }

  #[test]
  fn proc_sub_output_returns_dev_fd_path() {
    // is_input=false: path points at the reader fd; same shape.
    // Use a self-terminating command — an unread procsub keeps its
    // child alive forever otherwise, and the orphan deadlocks
    // TestGuard teardown on macOS (master close blocks waiting for
    // the slave fds the orphan inherited).
    let _g = TestGuard::new();
    let path = expand_proc_sub("true", false).unwrap();
    assert!(
      path.starts_with("/dev/fd/"),
      "expected /dev/fd/... path, got: {path:?}"
    );
  }

  #[test]
  fn proc_sub_input_path_is_readable_with_command_output() {
    // <(cmd) — reading from the returned path should yield the
    // command's stdout. This exercises the full plumbing: dup target
    // fd 1 in the child, parent reads via /dev/fd.
    let _g = TestGuard::new();
    let path = expand_proc_sub("echo proc_sub_marker_xyz", false).unwrap();
    // Open the path and read; the child writes 'proc_sub_marker_xyz\n'.
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("proc_sub_marker_xyz"), "got: {content:?}");
  }
}
