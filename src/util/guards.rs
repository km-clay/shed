use std::collections::HashSet;

use scopeguard::guard;

use crate::parse::lex::Span;
use crate::procio::IoFrame;
use crate::state::write_vars;

// ============================================================================
// ScopeGuard - RAII variable scope management
// ============================================================================

pub fn scope_guard(args: Option<Vec<(String, Span)>>) -> impl Drop {
  let argv = args.map(|a| a.into_iter().map(|(s, _)| s).collect::<Vec<_>>());
  write_vars(|v| v.descend(argv));
  guard((), |_| {
    write_vars(|v| v.ascend());
  })
}

pub fn shared_scope_guard() -> impl Drop {
  write_vars(|v| v.descend(None));
  guard((), |_| {
    write_vars(|v| v.ascend());
  })
}

// ============================================================================
// VarCtxGuard - RAII variable context cleanup
// ============================================================================

pub fn var_ctx_guard(
  vars: HashSet<String>,
) -> scopeguard::ScopeGuard<HashSet<String>, impl FnOnce(HashSet<String>)> {
  guard(vars, |vars| {
    write_vars(|v| {
      for var in &vars {
        v.unset_var(var).ok();
      }
    });
  })
}

// ============================================================================
// RedirGuard - RAII I/O redirection restoration
// ============================================================================

#[derive(Debug)]
pub struct RedirGuard(pub(crate) IoFrame);

impl RedirGuard {
  pub(crate) fn new(frame: IoFrame) -> Self {
    Self(frame)
  }
  pub fn persist(mut self) {
    use nix::unistd::close;
    if let Some(saved) = self.0.saved_io.take() {
      close(saved.0).ok();
      close(saved.1).ok();
      close(saved.2).ok();
    }
  }
}

impl Drop for RedirGuard {
  fn drop(&mut self) {
    self.0.restore().ok();
  }
}
