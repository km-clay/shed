use nix::{errno::Errno, unistd::execvpe};

use crate::{
  builtin::setup_builtin,
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node, execute::ExecArgs},
  procio::IoStack,
  state,
};

pub fn exec_builtin(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (expanded_argv, guard) = setup_builtin(Some(argv), job, Some((io_stack, node.redirs)))?;
  let expanded_argv = expanded_argv.unwrap();
  if let Some(g) = guard {
    // Persist redirections so they affect the entire shell,
    // not just this command call
    g.persist()
  }

  if expanded_argv.is_empty() {
    state::set_status(0);
    return Ok(());
  }

  let args = ExecArgs::from_expanded(expanded_argv)?;

  let cmd = &args.cmd.0;
  let span = args.cmd.1;

  let Err(e) = execvpe(cmd, &args.argv, &args.envp);

  // execvpe only returns on error
  let cmd_str = cmd.to_str().unwrap().to_string();
  match e {
    Errno::ENOENT => Err(ShErr::full(ShErrKind::CmdNotFound(cmd_str), "", span)),
    _ => Err(ShErr::full(ShErrKind::Errno(e), format!("{e}"), span)),
  }
}
