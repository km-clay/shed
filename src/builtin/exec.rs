use nix::{errno::Errno, unistd::execvpe};

use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node, execute::{ExecArgs, prepare_argv}},
  state,
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
  if !expanded_argv.is_empty() { expanded_argv.remove(0); }

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
    Errno::ENOENT => Err(
			ShErr::new(ShErrKind::NotFound, span.clone())
				.labeled(span, format!("exec: command not found: {}", cmd_str))
		),
    _ => Err(ShErr::at(ShErrKind::Errno(e), span, format!("{e}"))),
  }
}
