use nix::{errno::Errno, unistd::execvpe};

use crate::{
  util::error::ShResult,
  parse::{
    NdRule, Node,
    execute::{ExecArgs, prepare_argv},
  },
  sherr, state,
};

pub fn exec_builtin(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut expanded_argv = prepare_argv(argv)?;
  if !expanded_argv.is_empty() {
    expanded_argv.remove(0);
  }

  if expanded_argv.is_empty() {
    state::set_status(0);
    return Ok(());
  }

  let args = ExecArgs::from_expanded(expanded_argv);

  let cmd = &args.cmd.0;
  let span = args.cmd.1;

  let Err(e) = execvpe(cmd, &args.argv, &args.envp);

  // execvpe only returns on error
  let cmd_str = cmd.to_str().unwrap().to_string();
  match e {
    Errno::ENOENT => Err(sherr!(NotFound @ span.clone(), "exec: command not found: {}", cmd_str)),
    _ => Err(sherr!(Errno(e) @ span, "{e}")),
  }
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::testutil::{TestGuard, test_input};
  // Testing exec is a bit tricky since it replaces the current process, so we just test that it correctly handles the case of no arguments and the case of a nonexistent command. We can't really test that it successfully executes a command since that would replace the test process itself.

  #[test]
  fn exec_no_args_succeeds() {
    let _g = TestGuard::new();
    test_input("exec").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn exec_nonexistent_command_fails() {
    let _g = TestGuard::new();
    let result = test_input(
      "exec _____________no_such_______command_xyz_____________hopefully______this_doesnt______exist_____somewhere_in___your______PATH__________________",
    );
    assert!(result.is_err());
  }
}
