use crate::{
  libsh::error::{ShResult, ShResultExt},
  parse::{NdRule, Node, execute::prepare_argv},
  prelude::*,
  procio::borrow_fd,
  state::{self, write_shopts},
};

pub fn shopt(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }

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
