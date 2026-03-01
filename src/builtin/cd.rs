use ariadne::Fmt;
use yansi::Color;

use crate::{
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult, next_color},
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
	let cd_span = argv.first().unwrap().span.clone();

  let (argv, _) = setup_builtin(Some(argv), job, None)?;
  let argv = argv.unwrap();

  let (new_dir,arg_span) = if let Some((arg, span)) = argv.into_iter().next() {
    (PathBuf::from(arg),Some(span))
  } else {
    (PathBuf::from(env::var("HOME").unwrap()),None)
  };

  if !new_dir.exists() {
		let mut err = ShErr::new(
			ShErrKind::ExecFail,
			span.clone(),
		).labeled(cd_span.clone(), "Failed to change directory");
		if let Some(span) = arg_span {
			err = err.labeled(span, format!("No such file or directory '{}'", new_dir.display().fg(next_color())));
		}
		return Err(err);
  }

  if !new_dir.is_dir() {
    return Err(ShErr::new(ShErrKind::ExecFail, span.clone())
      .labeled(cd_span.clone(), format!("cd: Not a directory '{}'", new_dir.display().fg(next_color()))));
  }

  if let Err(e) = env::set_current_dir(new_dir) {
    return Err(ShErr::new(ShErrKind::ExecFail, span.clone())
      .labeled(cd_span.clone(), format!("cd: Failed to change directory: '{}'", e.fg(Color::Red))));
  }
  let new_dir = env::current_dir().map_err(|e| {
    ShErr::new(ShErrKind::ExecFail, span.clone())
      .labeled(cd_span.clone(), format!("cd: Failed to get current directory: '{}'", e.fg(Color::Red)))
  })?;
  unsafe { env::set_var("PWD", new_dir) };

  state::set_status(0);
  Ok(())
}
