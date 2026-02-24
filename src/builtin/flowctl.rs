use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{execute::prepare_argv, NdRule, Node},
  prelude::*,
};

pub fn flowctl(node: Node, kind: ShErrKind) -> ShResult<()> {
  use ShErrKind::*;
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };
  let mut code = 0;

  let mut argv = prepare_argv(argv)?;
  let cmd = argv.remove(0).0;

  if !argv.is_empty() {
    let (arg, span) = argv.into_iter().next().unwrap();

    let Ok(status) = arg.parse::<i32>() else {
      return Err(ShErr::full(
        ShErrKind::SyntaxErr,
        format!("{cmd}: Expected a number"),
        span,
      ));
    };

    code = status;
  }

  let (kind,message) = match kind {
    LoopContinue(_) => (LoopContinue(code), "'continue' found outside of loop"),
    LoopBreak(_) => (LoopBreak(code), "'break' found outside of loop"),
    FuncReturn(_) => (FuncReturn(code), "'return' found outside of function"),
    CleanExit(_) => (CleanExit(code), ""),
    _ => unreachable!(),
  };

  Err(ShErr::simple(kind, message))
}
