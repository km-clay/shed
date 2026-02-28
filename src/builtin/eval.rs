use crate::{
  builtin::setup_builtin,
  jobs::JobBldr,
  libsh::error::ShResult,
  parse::{NdRule, Node, execute::exec_input},
  procio::IoStack,
  state,
};

pub fn eval(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (expanded_argv, _guard) = setup_builtin(Some(argv), job, Some((io_stack, node.redirs)))?;
  let expanded_argv = expanded_argv.unwrap();

  if expanded_argv.is_empty() {
    state::set_status(0);
    return Ok(());
  }

  let joined_argv = expanded_argv
    .into_iter()
    .map(|(s, _)| s)
    .collect::<Vec<_>>()
    .join(" ");

  exec_input(joined_argv, None, false)
}
