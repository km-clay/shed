use crate::{
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node},
  prelude::*,
  state::{self, source_file},
};

use super::setup_builtin;

pub fn source(node: Node, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _) = setup_builtin(Some(argv), job, None)?;
  let argv = argv.unwrap();

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
