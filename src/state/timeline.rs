use std::sync::{Arc, atomic::Ordering};

use nix::sys::signal::Signal;

use crate::{
  eval::parse::ast::Ast,
  procio::Sinks,
  state::{jobs::SIG_EXIT_OFFSET, logic::LogTab, scopes, shopt::ShOpts, vars::VarTab},
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
}

#[derive(Debug)]
pub(crate) struct StageResult {
  status: i32,
}

impl Default for StageResult {
  fn default() -> Self {
    Self {
      status: SIG_EXIT_OFFSET + Signal::SIGABRT as i32,
    }
  }
}

impl StageResult {
  pub(crate) fn new(status: i32) -> Self {
    Self { status }
  }
  pub(crate) fn status(&self) -> i32 {
    self.status
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
    })
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
    } = spec;
    super::SHED.with(|shed| {
      *shed.var_scopes.borrow_mut() = scopes::ScopeStack::from_frame(frame);
      *shed.sinks.borrow_mut() = sinks;
      *shed.shopts.borrow_mut() = shopts;
      *shed.call_context.borrow_mut() = context;
      *shed.logic.borrow_mut() = logic;
      shed.status_code.store(status, Ordering::Relaxed);
    });

    ast
  }
}

#[cfg(test)]
mod tests {
  use std::{sync::Arc, thread};

  use crate::{
    eval::parse::ParsedSrc,
    procio::Sinks,
    state::{
      Shed,
      vars::{VarFlags, VarKind},
    },
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

    Shed::vars_mut(|v| v.set_var("x", VarKind::Str("parent".into()), VarFlags::empty())).unwrap();

    let spec = Shed::fork_spec(Sinks::new(), empty_ast());

    let (inherited, child_val) = thread::spawn(move || {
      Shed::install(spec);
      let inherited = Shed::vars(|v| v.try_get_var("x"));
      Shed::vars_mut(|v| v.set_var("x", VarKind::Str("child".into()), VarFlags::empty())).unwrap();
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
