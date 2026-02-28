use crate::{
  jobs::JobBldr,
  libsh::error::{ShResult, ShResultExt},
  parse::{NdRule, Node},
  prelude::*,
  procio::{IoStack, borrow_fd},
  state::{self, write_shopts},
};

use super::setup_builtin;

pub fn shopt(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(Some(argv), job, Some((io_stack, node.redirs)))?;
  let argv = argv.unwrap();

  if argv.is_empty() {
    let mut output = write_shopts(|s| s.display_opts())?;

    let output_channel = borrow_fd(STDOUT_FILENO);
    output.push('\n');

    write(output_channel, output.as_bytes())?;
    state::set_status(0);
    return Ok(());
  }

  for (arg, span) in argv {
    let Some(mut output) = write_shopts(|s| s.query(&arg)).blame(span)? else {
      continue;
    };

    let output_channel = borrow_fd(STDOUT_FILENO);
    output.push('\n');

    write(output_channel, output.as_bytes())?;
  }

  state::set_status(0);
  Ok(())
}
