use nix::sys::stat;
use scopeguard::guard;

use crate::{HashSet, state::vars::VarStr, try_var, util, var};

use super::{
  super::state::scopes::ScopeStack,
  Shed,
  eval::{execute::Dispatcher, lex::Span},
};

// ============================================================================
// ScopeGuard - RAII variable scope management
// ============================================================================

/// Execute commands registered by `defer`
/// Drop variables registered by `local`
fn guard_drop(_: ()) {
  let mut deferred = Shed::vars_mut(|v| v.cur_scope_mut().take_deferred_cmds());

  util::with_saved_status(|| {
    while let Some(cmd) = deferred.pop() {
      let mut dispatcher = Dispatcher::new(vec![cmd], "defer".into());
      if let Err(e) = dispatcher.begin_dispatch() {
        e.print_error();
      }
    }
  });

  Shed::vars_mut(ScopeStack::ascend);
}

/// Descend into a scope that mimics the isolation of forked subshells
///
/// The "global" namespace is still shared, but the 'ceiling' for new variables is set to the current scope,
/// so that they are dropped on return.
///
/// Additionally, stuff like 'umask' and 'PWD' are restored to their previous values on return
pub fn isolation_guard(args: Option<Vec<(VarStr, Span)>>) -> impl Drop {
  let ceiling_guard = scope_ceiling_guard(args);
  let cwd_guard = cwd_guard();
  let umask_guard = umask_guard();
  scopeguard::guard((), move |()| {
    drop(cwd_guard);
    drop(umask_guard);
    drop(ceiling_guard);
  })
}

/// Descend into a new variable scope, with a new argv that shadows the previous one.
///
/// The `local` builtin uses this scope to store its variables.
/// The `defer` builtin registers commands to run when this drops.
pub fn scope_guard(args: Option<Vec<(VarStr, Span)>>) -> impl Drop {
  let arg_vec = args.map(|a| a.into_iter().map(|(s, _)| s).collect::<Vec<_>>());
  Shed::vars_mut(|v| v.descend(arg_vec));
  guard((), guard_drop)
}

pub fn scope_ceiling_guard(args: Option<Vec<(VarStr, Span)>>) -> impl Drop {
  let arg_vec = args.map(|a| a.into_iter().map(|(s, _)| s).collect::<Vec<_>>());
  Shed::vars_mut(|v| v.descend_with_ceiling(arg_vec));
  guard((), guard_drop)
}

pub fn cwd_guard() -> impl Drop {
  let saved = try_var!("PWD");
  guard(saved, |saved| {
    if let Some(cwd) = saved
      && var!("PWD") != cwd
    {
      let _ = std::env::set_current_dir(cwd);
    }
  })
}

pub fn umask_guard() -> impl Drop {
  let saved = try_var!("UMASK");
  guard(saved, |saved| {
    if let Some(umask) = saved
      && var!("UMASK") != umask
      && let Ok(bits) = stat::mode_t::from_str_radix(&umask, 8)
    {
      let _ = stat::umask(stat::Mode::from_bits_truncate(bits));
    }
  })
}

/// Descend into a new variable scope, without using a new argv
/// This is used for stuff like brace groups,
///
/// The `local` builtin uses this scope to store its variables.
/// The `defer` builtin registers commands to run when this drops.
pub fn shared_scope_guard() -> impl Drop {
  Shed::vars_mut(|v| v.descend(None));
  guard((), guard_drop)
}

// ============================================================================
// VarCtxGuard - RAII variable context cleanup
// ============================================================================

pub fn var_ctx_guard(
  vars: HashSet<VarStr>,
) -> scopeguard::ScopeGuard<HashSet<VarStr>, impl FnOnce(HashSet<VarStr>)> {
  guard(vars, |vars| {
    Shed::vars_mut(|v| {
      for var in &vars {
        v.unset_var(var).ok();
      }
    });
  })
}

// ============================================================================
// RedirGuard - RAII I/O redirection restoration
// ============================================================================
