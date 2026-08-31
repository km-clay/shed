use nix::sys::stat;

use crate::{
  HashSet,
  eval::{
    execute,
    lex::Span,
    parse::{
      NdRule,
      ast::{Ast, NodeId},
    },
  },
  state::{
    Shed, params,
    scopes::ScopeStack,
    vars::{Var, VarFlags, VarStr},
  },
  try_var, util, var,
};

// ============================================================================
// ScopeGuard - run a closure on drop (local replacement for the scopeguard crate)
// ============================================================================

/// Wraps a value with a closure that runs when the guard is dropped, receiving
/// the value. Derefs to the value so it stays usable until then. The `Option`s
/// let `drop` (which only has `&mut self`) move both out to call the `FnOnce`.
pub(crate) struct ScopeGuard<T, F: FnOnce(T)> {
  value: Option<T>,
  dropfn: Option<F>,
}

impl<T, F: FnOnce(T)> std::ops::Deref for ScopeGuard<T, F> {
  type Target = T;
  fn deref(&self) -> &T {
    self.value.as_ref().unwrap()
  }
}

impl<T, F: FnOnce(T)> std::ops::DerefMut for ScopeGuard<T, F> {
  fn deref_mut(&mut self) -> &mut T {
    self.value.as_mut().unwrap()
  }
}

impl<T, F: FnOnce(T)> Drop for ScopeGuard<T, F> {
  fn drop(&mut self) {
    if let (Some(value), Some(dropfn)) = (self.value.take(), self.dropfn.take()) {
      dropfn(value);
    }
  }
}

/// Run `dropfn(value)` when the returned guard drops.
pub(crate) fn guard<T, F: FnOnce(T)>(value: T, dropfn: F) -> ScopeGuard<T, F> {
  ScopeGuard {
    value: Some(value),
    dropfn: Some(dropfn),
  }
}

/// Run the given statements when the enclosing scope exits (in reverse order of
/// declaration, like RAII). Replacement for `scopeguard::defer!`.
#[macro_export]
macro_rules! defer {
  ($($body:tt)*) => {
    let _guard = $crate::util::guards::guard((), move |()| { $($body)* });
  };
}

// ============================================================================
// ScopeGuard - RAII variable scope management
// ============================================================================

/// Execute commands registered by `defer`
/// Drop variables registered by `local`
fn guard_drop(_: ()) {
  if Shed::vars(ScopeStack::has_deferred_cmds) {
    util::with_saved_status(execute::dispatch_deferred_cmds);
  }

  Shed::vars_mut(ScopeStack::ascend);
}

/// Descend into a scope that mimics the isolation of forked subshells
///
/// The "global" namespace is still shared, but the 'ceiling' for new variables is set to the current scope,
/// so that they are dropped on return.
///
/// Additionally, stuff like 'umask', 'PWD', and shell options are restored to
/// their previous values on return
///
/// ## Safety
/// This calls `Shed::meta_mut` internally.
/// If this is called inside of another `meta()`/`meta_mut()` call, that is a `RefCell` panic.
pub fn isolation_guard(args: Option<Vec<(VarStr, Span)>>) -> impl Drop {
  let ceiling_guard = scope_ceiling_guard(args);
  let cwd_guard = cwd_guard();
  let umask_guard = umask_guard();
  let shopt_guard = shopt_guard();
  let fork = Shed::meta_mut(|m| m.enter_fork(false));
  guard((), move |()| {
    drop(shopt_guard);
    drop(cwd_guard);
    drop(umask_guard);
    drop(ceiling_guard);
    drop(fork);
  })
}

/// Snapshot the shell options, restoring them on drop.
pub fn shopt_guard() -> impl Drop {
  let saved = Shed::shopts(Clone::clone);
  guard(saved, |saved| {
    Shed::shopts_mut(move |o| *o = saved);
  })
}

fn make_arg_vec(args: Option<Vec<(VarStr, Span)>>) -> Option<Vec<VarStr>> {
  args.map(|a| a.into_iter().map(|(s, _)| s).collect::<Vec<_>>())
}

pub fn scope_ceiling_guard(args: Option<Vec<(VarStr, Span)>>) -> impl Drop {
  let arg_vec = make_arg_vec(args);
  Shed::vars_mut(|v| v.descend_with_ceiling(arg_vec));
  guard((), guard_drop)
}

pub fn function_scope_guard(args: Option<Vec<(VarStr, Span)>>) -> impl Drop {
  let arg_vec = make_arg_vec(args);
  Shed::vars_mut(|v| v.descend_into_function(arg_vec));
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
      && let Ok(bits) = stat::mode_t::from_str_radix(&umask.to_str_lossy(), 8)
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
) -> ScopeGuard<HashSet<VarStr>, impl FnOnce(HashSet<VarStr>)> {
  guard(vars, |vars| {
    Shed::vars_mut(|v| {
      for var in &vars {
        v.unset_var(&var.to_str_lossy()).ok();
      }
    });
  })
}

/// Snapshot and restore the variables used in prefix assignment
pub fn prefix_assign_guard(tree: &Ast, assignments: &[NodeId]) -> impl Drop {
  let saved: Vec<(String, Option<Var>)> = assignments
    .iter()
    .filter_map(|a| match &tree[*a].class {
      NdRule::Assignment { var, .. } => {
        // An indexed assignment (`arr[i]=v`) touches the whole array variable,
        // so snapshot/restore under the base name.
        let name = params::parse_arr_bracket(tree[*var].span.as_bytes())
          .map_or_else(|| tree[*var].span.as_var_str(), |(base, _)| base);
        Some(name)
      }
      _ => None,
    })
    .map(|name| {
      let prior = Shed::vars(|v| v.try_get_var_meta(&name.to_str_lossy()));
      (name.to_str_lossy().to_string(), prior)
    })
    .collect();

  guard(saved, |saved| {
    Shed::vars_mut(|v| {
      for (name, prior) in saved {
        match prior {
          Some(var) => {
            v.update_var(&name, var.kind().clone()).ok();
            if !var.flags().contains(VarFlags::EXPORT) {
              v.unexport_var(&name);
            }
          }
          // Didn't exist before the prefix assignment → remove what it created.
          None => {
            v.unset_var(&name).ok();
          }
        }
      }
    });
  })
}

// ============================================================================
// RedirGuard - RAII I/O redirection restoration
// ============================================================================
