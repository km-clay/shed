use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node, execute::prepare_argv},
  state::{self, write_vars},
};

pub fn shift(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }
  let mut argv = argv.into_iter();

  if let Some((arg, span)) = argv.next() {
    let Ok(count) = arg.parse::<usize>() else {
      return Err(ShErr::at(ShErrKind::ExecFail, span, "Expected a number in shift args"));
    };
    for _ in 0..count {
      write_vars(|v| v.cur_scope_mut().fpop_arg());
    }
  }

  state::set_status(0);
  Ok(())
}
