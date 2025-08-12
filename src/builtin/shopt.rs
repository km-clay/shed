use crate::{
  jobs::JobBldr,
  libsh::error::{ShResult, ShResultExt},
  parse::{NdRule, Node},
  prelude::*,
  procio::{borrow_fd, IoStack},
  state::write_shopts,
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

  let (argv, io_frame) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

  let mut io_frame = io_frame.unwrap();
  io_frame.redirect()?;
  for (arg, span) in argv {
    let Some(mut output) = write_shopts(|s| s.query(&arg)).blame(span)? else {
      continue;
    };

    let output_channel = borrow_fd(STDOUT_FILENO);
    output.push('\n');

    if let Err(e) = write(output_channel, output.as_bytes()) {
      io_frame.restore()?;
      return Err(e.into());
    }
  }
  io_frame.restore()?;

  Ok(())
}
