use crate::{
  libsh::error::ShResult,
  parse::{NdRule, Node, execute::{exec_input, prepare_argv}},
  state,
};

pub fn eval(node: Node) -> ShResult<()> {
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

  let joined_argv = expanded_argv
    .into_iter()
    .map(|(s, _)| s)
    .collect::<Vec<_>>()
    .join(" ");

  exec_input(joined_argv, None, false, Some("eval".into()))
}
