use nix::sys::stat;
use scopeguard::guard;

use crate::{
  HashSet,
  eval::{NdRule, Node},
  state::{
    util,
    vars::{Var, VarStr},
  },
  try_var, util as crate_util, var,
};

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

  crate_util::with_saved_status(|| {
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
/// Additionally, stuff like 'umask', 'PWD', and shell options are restored to
/// their previous values on return
pub fn isolation_guard(args: Option<Vec<(VarStr, Span)>>) -> impl Drop {
  let ceiling_guard = scope_ceiling_guard(args);
  let cwd_guard = cwd_guard();
  let umask_guard = umask_guard();
  let shopt_guard = shopt_guard();
  scopeguard::guard((), move |()| {
    drop(shopt_guard);
    drop(cwd_guard);
    drop(umask_guard);
    drop(ceiling_guard);
  })
}

/// Snapshot the shell options, restoring them on drop.
pub fn shopt_guard() -> impl Drop {
  let saved = Shed::shopts(Clone::clone);
  guard(saved, |saved| {
    Shed::shopts_mut(move |o| *o = saved);
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

/// Snapshot and restore the variables used in prefix assignment
pub fn prefix_assign_guard(assignments: &[Node]) -> impl Drop {
  let saved: Vec<(String, Option<Var>)> = assignments
    .iter()
    .filter_map(|a| match &a.class {
      NdRule::Assignment { var, .. } => {
        let raw = var.span.as_str();
        // An indexed assignment (`arr[i]=v`) touches the whole array variable,
        // so snapshot/restore under the base name.
        let name = util::parse_arr_bracket(raw)
          .map_or_else(|| raw.to_string(), |(base, _)| base.to_string());
        Some(name)
      }
      _ => None,
    })
    .map(|name| {
      let prior = Shed::vars(|v| v.try_get_var_meta(&name));
      (name, prior)
    })
    .collect();

  guard(saved, |saved| {
    Shed::vars_mut(|v| {
      for (name, prior) in saved {
        // Clear first so the re-set starts from a clean slate (`set_var` ORs
        // flags onto an existing entry) and the export/envp bookkeeping runs.
        v.unset_var(&name).ok();
        if let Some(var) = prior {
          v.set_var(&name, var.kind().clone(), var.flags()).ok();
        }
      }
    });
  })
}

// ============================================================================
// RedirGuard - RAII I/O redirection restoration
// ============================================================================
