use std::sync::{Arc, atomic::Ordering};

use nix::sys::signal::Signal;

use crate::{
  eval::parse::ast::Ast,
  procio::{self, PipeFrames, Sinks},
  signal,
  state::{
    logic::LogTab,
    scopes,
    shopt::ShOpts,
    timers::Timers,
    vars::{VarName, VarStr, VarTab},
  },
  util::error::LabelBuilder,
};

pub(crate) struct ForkSpec {
  frame: VarTab,
  sinks: Sinks,
  ast: Arc<Ast>,
  shopts: ShOpts,
  status: i32,
  context: Vec<LabelBuilder>,
  logic: LogTab,
  timers: Timers,
  pipe_frames: PipeFrames,
}

#[derive(Debug)]
pub(crate) struct StageResult {
  status: i32,
  var_writes: Vec<(VarName, VarStr)>,
}

impl Default for StageResult {
  fn default() -> Self {
    Self {
      status: signal::signal_status(Signal::SIGABRT),
      var_writes: vec![],
    }
  }
}

impl StageResult {
  pub(crate) fn new(status: i32) -> Self {
    Self {
      status,
      var_writes: procio::take_stage_var_writes(),
    }
  }
  pub(crate) fn status(&self) -> i32 {
    self.status
  }
  pub(crate) fn take_var_writes(&mut self) -> Vec<(VarName, VarStr)> {
    std::mem::take(&mut self.var_writes)
  }
}

impl super::Shed {
  pub(crate) fn fork_spec(sinks: Sinks, ast: Arc<Ast>) -> ForkSpec {
    super::SHED.with(|shed| ForkSpec {
      frame: shed.var_scopes.borrow().flatten_to_frame(),
      sinks,
      ast,
      shopts: shed.shopts.borrow().clone(),
      status: shed.status_code.load(Ordering::Relaxed),
      context: shed.call_context.borrow().clone(),
      logic: shed.logic.borrow().clone(),
      timers: shed.timers.borrow().clone(),
      pipe_frames: shed.pipe_frames.borrow().clone(),
    })
  }
  pub(crate) fn completion_spec() -> ForkSpec {
    let mut parsed = crate::eval::parse::ParsedSrc::new(":".into());
    let _ = parsed.parse_src();
    Self::fork_spec(Sinks::new(), Arc::new(parsed.into_ast()))
  }
  pub(crate) fn install(spec: ForkSpec) -> Arc<Ast> {
    let ForkSpec {
      frame,
      sinks,
      ast,
      shopts,
      status,
      context,
      logic,
      timers,
      pipe_frames,
    } = spec;
    super::SHED.with(|shed| {
      *shed.var_scopes.borrow_mut() = scopes::ScopeStack::from_frame(frame);
      *shed.sinks.borrow_mut() = sinks;
      *shed.shopts.borrow_mut() = shopts;
      *shed.call_context.borrow_mut() = context;
      *shed.logic.borrow_mut() = logic;
      *shed.timers.borrow_mut() = timers;
      *shed.pipe_frames.borrow_mut() = pipe_frames;
      shed.status_code.store(status, Ordering::Relaxed);
    });

    ast
  }
}

#[cfg(test)]
mod tests {
  use crate::set_var;
  use std::{sync::Arc, thread};

  use crate::{
    eval::parse::ParsedSrc,
    procio::Sinks,
    state::{Shed, vars::VarKind},
    tests::testutil::TestGuard,
  };

  fn empty_ast() -> Arc<crate::eval::parse::ast::Ast> {
    let mut parsed = ParsedSrc::new(":".into());
    parsed.parse_src().unwrap();
    Arc::new(parsed.into_ast())
  }

  // A forked timeline inherits a snapshot of the parent's vars, but its own
  // mutations are isolated: the child's write to `x` must not reach the parent.
  #[test]
  fn timeline_fork_isolates_vars() {
    let _g = TestGuard::new();

    set_var!("x", VarKind::Str("parent".into())).unwrap();

    let spec = Shed::fork_spec(Sinks::new(), empty_ast());

    let (inherited, child_val) = thread::spawn(move || {
      Shed::install(spec);
      let inherited = Shed::vars(|v| v.try_get_var("x"));
      set_var!("x", VarKind::Str("child".into())).unwrap();
      let child_val = Shed::vars(|v| v.get_var("x"));
      (inherited, child_val)
    })
    .join()
    .unwrap();

    assert_eq!(
      inherited.unwrap(),
      "parent",
      "child should inherit the snapshot"
    );
    assert_eq!(child_val, "child", "child sees its own write");
    assert_eq!(
      Shed::vars(|v| v.get_var("x")),
      "parent",
      "child's write must not leak to the parent"
    );
  }
}
