use nix::{errno::Errno, unistd::execvpe};

use crate::{
  parse::{
    NdRule, Node,
    execute::{ExecArgs, prepare_argv},
  },
  sherr, state,
  util::{error::ShResult, with_status},
};

pub(super) struct Exec;
impl super::Builtin for Exec {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      return with_status(0);
    }

    let args = ExecArgs::from_expanded(args.argv);

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
    test_input(
      "exec _____________no_such_______command_xyz_____________hopefully______this_doesnt______exist_____somewhere_in___your______PATH__________________",
    ).ok();
    assert_ne!(state::get_status(), 0);
  }
}
