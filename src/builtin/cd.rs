use ariadne::{Fmt, Label, Span};

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
	let src = span.source();
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
		let color = next_color();
		let mut err = ShErr::new(
			ShErrKind::ExecFail,
			span.clone(),
		).with_label(src.clone(), Label::new(cd_span.clone()).with_color(color).with_message("Failed to change directory"));
		if let Some(span) = arg_span {
			let color = next_color();
			err = err.with_label(src.clone(), Label::new(span).with_color(color).with_message(format!("No such file or directory '{}'", new_dir.display().fg(color))));
		}
		return Err(err);
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
