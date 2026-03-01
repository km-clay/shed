use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node, execute::prepare_argv},
  prelude::*,
  state::{self, source_file},
};

pub fn source(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }

  for (arg, span) in argv {
    let path = PathBuf::from(arg);
    if !path.exists() {
      return Err(ShErr::at(ShErrKind::ExecFail, span, format!("source: File '{}' not found", path.display())));
    }
    if !path.is_file() {
      return Err(ShErr::at(ShErrKind::ExecFail, span, format!("source: Given path '{}' is not a file", path.display())));
    }
    source_file(path)?;
  }

  state::set_status(0);
  Ok(())
}
