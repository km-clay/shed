use crate::state::{Shed, terminal::Terminal};
use crate::util::posix_extension::execvpe;
use nix::errno::Errno;

use super::{
  eval::execute::ExecArgs,
  sherr,
  util::{ShResult, with_status},
};

pub(super) struct Exec;
impl super::Builtin for Exec {
  fn is_special(&self) -> bool {
    true
  }

  fn always_forks(&self) -> bool {
    true
  }

  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    if args.no_arguments() {
      return with_status(0);
    }

    let (arg_vec, _) = args.take_argv();

    let args = ExecArgs::from_expanded(arg_vec);

    let cmd = &args.cmd.0;
    let span = args.cmd.1;

    let _term_guard = Shed::term_mut(Terminal::prepare_for_exec);

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
  use crate::tests::testutil::{TestGuard, test_input};
  // Testing exec is a bit tricky since it replaces the current process, so we just test that it correctly handles the case of no arguments and the case of a nonexistent command. We can't really test that it successfully executes a command since that would replace the test process itself.

  #[test]
  fn exec_no_args_succeeds() {
    let _g = TestGuard::new();
    test_input("exec").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn exec_nonexistent_command_fails() {
    let _g = TestGuard::new();
    test_input(
      "exec _____________no_such_______command_xyz_____________hopefully______this_doesnt______exist_____somewhere_in___your______PATH__________________",
    ).ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
