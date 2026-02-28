use crate::{
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node},
  prelude::*,
  state::{self},
};

use super::setup_builtin;

pub fn cd(node: Node, job: &mut JobBldr) -> ShResult<()> {
  let span = node.get_span();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _) = setup_builtin(Some(argv), job, None)?;
  let argv = argv.unwrap();

  let new_dir = if let Some((arg, _)) = argv.into_iter().next() {
    PathBuf::from(arg)
  } else {
    PathBuf::from(env::var("HOME").unwrap())
  };

  if !new_dir.exists() {
    return Err(ShErr::full(
      ShErrKind::ExecFail,
      format!("cd: No such file or directory '{}'", new_dir.display()),
      span,
    ));
  }

  if !new_dir.is_dir() {
    return Err(ShErr::full(
      ShErrKind::ExecFail,
      format!("cd: Not a directory '{}'", new_dir.display()),
      span,
    ));
  }

  if let Err(e) = env::set_current_dir(new_dir) {
    return Err(ShErr::full(
      ShErrKind::ExecFail,
      format!("cd: Failed to change directory: {}", e),
      span,
    ));
  }
  let new_dir = env::current_dir().map_err(|e| {
    ShErr::full(
      ShErrKind::ExecFail,
      format!("cd: Failed to get current directory: {}", e),
      span,
    )
  })?;
  unsafe { env::set_var("PWD", new_dir) };

  state::set_status(0);
  Ok(())
}
