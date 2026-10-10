use std::{
  io::{self, Write},
  sync::Arc,
};

use crate::{
  procio::{Sink, SinkIo},
  sherr, signal,
  state::{
    Shed,
    vars::{VarFlags, VarKind, VarStr},
  },
  util::{self, error::ShResultExt},
};

use super::{Builtin, BuiltinArgs, ShResult, opt};

mod float;
mod int;

pub(super) use float::{ReadFloat, WriteFloat};
pub(super) use int::{ReadInt, WriteInt};

enum ReadLimit {
  AtMost(usize),
  Exactly(usize),
}

impl Default for ReadLimit {
  fn default() -> Self {
    Self::Exactly(1)
  }
}

impl ReadLimit {
  fn at_most(arg: &VarStr) -> ShResult<Self> {
    let n = arg
      .to_str_lossy()
      .parse::<usize>()
      .map_err(|_| sherr!(ParseErr, "invalid integer for -n: {arg}"))?;
    if n == 0 {
      return Err(sherr!(
        ParseErr,
        "invalid integer for -n: {arg} (must be > 0)"
      ));
    }
    Ok(Self::AtMost(n))
  }
  fn exactly(arg: &VarStr) -> ShResult<Self> {
    let n = arg
      .to_str_lossy()
      .parse::<usize>()
      .map_err(|_| sherr!(ParseErr, "invalid integer for -N: {arg}"))?;
    if n == 0 {
      return Err(sherr!(
        ParseErr,
        "invalid integer for -N: {arg} (must be > 0)"
      ));
    }
    Ok(Self::Exactly(n))
  }

  fn wanted(&self) -> usize {
    match self {
      Self::AtMost(n) | Self::Exactly(n) => *n,
    }
  }
}

fn fill(reader: &Arc<dyn Sink>, buf: &mut [u8]) -> ShResult<usize> {
  let mut got = 0;
  while got < buf.len() {
    match reader.read(&mut buf[got..]) {
      Ok(0) => break,
      Ok(n) => got += n,
      Err(e) if e.kind() == io::ErrorKind::Interrupted => {
        signal::check_signals()?;
      }
      Err(e) => return Err(sherr!(ExecFail, "failed to read from stdin: {e}")),
    }
  }
  Ok(got)
}

fn emit(
  vals: impl Iterator<Item = VarStr>,
  writer: &mut SinkIo,
  args: &BuiltinArgs,
) -> ShResult<()> {
  if let Some(name) = args.opt_value("array") {
    Shed::vars_mut(|v| v.set_var(&name.to_str_lossy(), VarKind::arr(vals), VarFlags::empty()))
      .promote_err(args.cmd_span())?;
  } else {
    for (i, v) in vals.enumerate() {
      if i > 0 {
        writer.write_all(b"\n").ok();
      }
      writer.write_all(&v).ok();
    }
  }

  util::with_status(0)
}
