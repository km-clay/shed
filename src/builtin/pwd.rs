use crate::{
  libsh::error::ShResult,
  parse::{NdRule, Node},
  prelude::*,
  procio::borrow_fd,
  state,
};

pub fn pwd(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv: _,
  } = node.class
  else {
    unreachable!()
  };

  let stdout = borrow_fd(STDOUT_FILENO);

  let mut curr_dir = env::current_dir().unwrap().to_str().unwrap().to_string();
  curr_dir.push('\n');
  write(stdout, curr_dir.as_bytes())?;

  state::set_status(0);
  Ok(())
}
