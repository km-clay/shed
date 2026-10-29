//! The variable scope stack
//!
//! [`ScopeStack`] — the nested variable environments (globals, function locals, and
//! `local` declarations) that variable lookups walk.

use std::cell::OnceCell;
use std::collections::VecDeque;
use std::os::unix::ffi::OsStringExt;

use bstr::ByteSlice;
use itertools::Itertools;

use super::{
  ShResult, Shed,
  meta::MetaTab,
  sherr,
  vars::{self, ArrIndex, ShellParam, Var, VarFlags, VarKind, VarKindTag, VarName, VarTab},
};
use crate::{expand::Words, util::strops::ParseBytes};
use crate::{
  HashMap,
  state::vars::{ShStr, ShStrSliceExt},
};
use crate::{shopt, varstr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplitKind {
  Values,
  Keys,
}

#[derive(Clone, Default, Debug)]
pub(crate) struct ScopeStack {
  /// ALWAYS keep one scope.
  /// The bottom scope is the global variable space.
  /// Scopes that come after that are pushed in functions,
  /// and only contain variables that are defined using `local`.
  scopes: Vec<VarTab>,
  depth : u32,

  /// Global parameters such as $!, $$, etc
  global_params: HashMap<ShellParam, ShStr>,

  launch_env: OnceCell<HashMap<ShStr, Var>>,
}

impl ScopeStack {
  pub(crate) fn new() -> Self {
    let new = Self::from_frame(VarTab::new());
    new.launch_env.set(new.cur_scope().vars().clone()).ok();
    new
  }
  pub(crate) fn from_frame(frame: VarTab) -> Self {
    let mut new = Self::default();
    new.scopes.push(frame);
    let shell_name = std::env::args_os()
      .next()
      .map_or_else(|| ShStr::from("shed"), |a| ShStr::from(a.into_vec()));
    new.global_params.insert(ShellParam::ShellName, shell_name);
    new
  }
  pub(crate) fn descend(&mut self, argv: Option<Vec<ShStr>>) {
    let mut new_vars = VarTab::bare();
    if let Some(argv) = argv {
      new_vars.extend_args(argv);
    }
    self.scopes.push(new_vars);
    self.depth += 1;
  }
  pub(crate) fn descend_with_ceiling(&mut self, argv: Option<Vec<ShStr>>) {
    self.descend(argv);
    if let Some(scope) = self.scopes.last_mut() {
      scope.set_kind(vars::ScopeKind::Ceiling);
    }
  }
  pub(crate) fn descend_into_function(&mut self, argv: Option<Vec<ShStr>>) {
    self.descend(argv);
    if let Some(scope) = self.scopes.last_mut() {
      scope.set_kind(vars::ScopeKind::Function);
    }
  }
  /// Forget user-defined shell variables
  ///
  /// Unsets all global variables that are not managed by the shell, and then restores
  /// the env vars inherited by the shell so that critical vars like `PATH` and `HOME` are not unset
  pub(super) fn forget_userspace_globals(&mut self, exclude: &[ShStr]) {
    let Some(scope) = self.scopes.first_mut() else {
      return;
    };
    scope
      .vars_mut()
      .retain(|k, v| v.flags().contains(VarFlags::SHELL) || exclude.contains(k));

    let Some(launch_vars) = self.launch_env.get() else {
      return;
    };

    for (name, var) in launch_vars {
      if !var.flags().contains(VarFlags::SHELL) && !exclude.contains(name) {
        scope.put_var(name, var.clone());
      }
    }
  }
  pub(super) fn clear_deferred_cmds(&mut self) {
    self.cur_scope_mut().take_deferred_cmds();
  }
  pub(crate) fn put_var(&mut self, name: &[u8], var: Var) {
    let Some(scope) = self.scopes.first_mut() else {
      return;
    };

    scope.put_var(name, var);
  }
  pub(crate) fn ascend(&mut self) {
    if self.depth >= 1 {
      let popped = self.scopes.pop();
      // Popped exports may have shadowed an outer binding; invalidate the
      // envp cache so the next fork rebuilds from the remaining scopes.
      if popped.is_some_and(|s| {
        s.vars()
          .values()
          .any(|v| v.flags().contains(VarFlags::EXPORT))
      }) {
        Shed::meta_mut(MetaTab::clear_envp);
      }
      self.depth -= 1;
    }
  }
  pub(crate) fn cur_scope(&self) -> &VarTab {
    self.scopes.last().unwrap()
  }
  pub(crate) fn cur_scope_mut(&mut self) -> &mut VarTab {
    self.scopes.last_mut().unwrap()
  }
  pub(crate) fn sh_argv(&self) -> &VecDeque<ShStr> {
    for scope in self.scopes_rev() {
      let argv = scope.sh_argv();
      if !argv.is_empty() {
        return argv;
      }
      if scope.is_function() {
        break;
      }
    }

    self.cur_scope().sh_argv()
  }
  fn find_scope_idx<F: Fn(&VarTab) -> bool>(&self, f: F) -> Option<usize> {
    self.scopes.iter().rposition(f)
  }
  fn local_scope_idx(&self) -> Option<usize> {
    self.find_scope_idx(|s| s.is_function() || s.is_ceiling())
  }
  pub(crate) fn sh_argv_scope_mut(&mut self) -> &mut VarTab {
    let idx = self.find_scope_idx(VarTab::is_function).unwrap_or(0);
    self.scopes.get_mut(idx).unwrap()
  }
  pub(crate) fn sh_argv_scope(&self) -> &VarTab {
    let idx = self.find_scope_idx(VarTab::is_function).unwrap_or(0);
    self.scopes.get(idx).unwrap()
  }
  pub(crate) fn unset_var(&mut self, var_name: &[u8]) -> ShResult<()> {
    for scope in self.bounded_scopes_rev_mut() {
      if scope.var_exists(var_name) {
        return scope.unset_var(var_name);
      }
    }
    Err(sherr!(ExecFail, "Variable '{}' not found", var_name.to_str_lossy()))
  }
  /// Indexed counterpart to `unset_var`: removes a single array element from
  /// the scope that owns the array. `idx` must already be resolved.
  pub(crate) fn unset_index(&mut self, var_name: &[u8], idx: ArrIndex) -> ShResult<()> {
    for scope in self.bounded_scopes_rev_mut() {
      if scope.var_exists(var_name) {
        return scope.unset_index(var_name, idx);
      }
    }
    Ok(())
  }
  pub(crate) fn unexport_var(&mut self, var_name: &[u8]) {
    for scope in self.bounded_scopes_rev_mut() {
      if scope.var_exists(var_name) {
        scope.unexport_var(var_name);
        return;
      }
    }
  }
  pub(crate) fn var_exists(&self, var_name: &[u8]) -> bool {
    for scope in self.scopes_rev() {
      if scope.var_exists(var_name) {
        return true;
      }
    }
    if ShellParam::is_param(var_name) && let Ok(param) = var_name.parse_bytes::<ShellParam>() {
      return self.global_params.contains_key(&param);
    }
    false
  }
  pub(crate) fn flatten_vars(&self) -> HashMap<ShStr, Var> {
    let mut flat: HashMap<ShStr, Var> = self
      .scopes_iter()
      .flat_map(VarTab::vars)
      .map(|(k, v)| (k.clone(), v.clone()))
      .collect();

    for (k, v) in std::env::vars_os() {
      flat
        .entry(ShStr::from(k.into_vec()))
        .or_insert_with(|| Var::new(VarKind::Str(ShStr::from(v.into_vec())), VarFlags::EXPORT));
    }

    flat
  }
  pub(crate) fn flatten_to_frame(&self) -> VarTab {
    let mut frame = VarTab::bare();

    for scope in self.scopes_rev() {
      for (var_name, var) in scope.vars() {
        frame.put_var(var_name, var.clone());
      }
    }

    let argv_scope = self.sh_argv_scope();
    for (param, val) in argv_scope.params() {
      frame.set_param(*param, val);
    }
    frame.sh_argv_mut().clone_from(argv_scope.sh_argv());

    frame
  }
  pub(crate) fn has_deferred_cmds(&self) -> bool {
    self.cur_scope().has_deferred_cmds()
  }

  fn element_of(target: &VarName) -> ShResult<Option<&ArrIndex>> {
    if target.slice_start().is_some() || target.slice_len().is_some() {
      return Err(sherr!(
        ExecFail,
        "'{}' is a slice; use a whole variable or a single index",
        target.name()
      ));
    }

    Ok(target.index())
  }
  pub(crate) fn read_name(&self, src: &VarName) -> ShResult<Option<VarKind>> {
    let name = src.name();

    let Some(idx) = Self::element_of(src)? else {
      return Ok(self.try_get_var_meta(&name).map(Var::into_kind));
    };

    if !self.index_is_set(&name, idx) {
      return Ok(None);
    }

    Ok(Some(VarKind::Str(self.index_var(&name, idx)?)))
  }
  pub(crate) fn take_name(&mut self, src: &VarName) -> ShResult<Option<VarKind>> {
    let name = src.name();

    let Some(idx) = Self::element_of(src)? else {
      return Ok(self.try_take_var(&name)?.map(Var::into_kind));
    };

    if !self.index_is_set(&name, idx) {
      return Ok(None);
    }

    self.take_index_var(&name, idx).map(|v| v.map(VarKind::Str))
  }
  pub(crate) fn put_name(&mut self, dst: &VarName, val: VarKind) -> ShResult<()> {
    let name = dst.name();

    let Some(idx) = Self::element_of(dst)? else {
      return self.set_var(&name, val, VarFlags::empty());
    };

    let scalar = match val {
      VarKind::Str(s) => s,
      VarKind::Int(i) => varstr!("{i}"),
      _ => {
        return Err(sherr!(
          ExecFail,
          "Cannot assign array/assoc value to single element '{}'",
          dst.name()
        ));
      }
    };

    self.set_var_indexed(&name, idx.clone(), scalar, VarFlags::empty())
  }
  pub(crate) fn move_var(&mut self, src: &VarName, dst: &VarName) -> ShResult<bool> {
    let dst_name = dst.name();

    if self
      .get_local_var_flags(&dst_name)
      .is_some_and(|flags| flags.contains(VarFlags::READONLY))
    {
      return Err(sherr!(ExecFail, "Variable '{dst_name}' is readonly"));
    }

    let Some(var) = self.take_name(src)? else {
      return Ok(false);
    };

    self.put_name(dst, var).map(|()| true)
  }
  pub(crate) fn set_var(&mut self, var_name: &[u8], val: VarKind, flags: VarFlags) -> ShResult<()> {
    if flags.contains(VarFlags::LOCAL) || self.writes_are_local() {
      return self.set_var_local(var_name, val, flags);
    }
    // Dynamic scoping: walk scopes from innermost to outermost,
    // update the nearest scope that already has this variable
    for scope in self.bounded_scopes_rev_mut() {
      if scope.var_exists(var_name) {
        return scope.set_var(var_name, val, flags);
      }
    }
    // Not found in any scope - create in global scope
    self.create_var(var_name, val, flags)
  }
  /// Declare a name with no assigned value (`declare x` / `local x`).
  ///
  /// If the target scope already holds the variable, its value is preserved and
  /// only the attribute flags are merged in. A brand-new name is created as
  /// `VarKind::Unset` so `${x+}`/`${x-}` treat it as unset until it is assigned.
  pub(crate) fn declare_var_novalue(&mut self, var_name: &[u8], flags: VarFlags) -> ShResult<()> {
    if flags.contains(VarFlags::LOCAL) {
      let idx = self.local_scope_idx().unwrap_or(0);
      let Some(scope) = self.scopes.get_mut(idx) else {
        return Err(sherr!(ExecFail, "No local scope available to set variable '{}'", var_name.to_str_lossy()));
      };
      if scope.var_exists(var_name) {
        scope.merge_flags(var_name, flags);
        return Ok(());
      }
      return scope.set_var(var_name, VarKind::Unset, flags);
    }
    // Dynamic scoping: preserve the value in the nearest scope that has it.
    for scope in self.bounded_scopes_rev_mut() {
      if scope.var_exists(var_name) {
        scope.merge_flags(var_name, flags);
        return Ok(());
      }
    }

    // Not found anywhere - create in the global scope as declared-but-unset.
    self.create_var(var_name, VarKind::Unset, flags)
  }

  pub(crate) fn promote_var_global(&mut self, var_name: &[u8]) -> ShResult<()> {
    {
      let Some(ceiling) = self.ceiling_mut() else {
        return Ok(());
      };

      if ceiling
        .get_var_flags(var_name)
        .is_some_and(|flags| flags.contains(VarFlags::READONLY))
      {
        return Err(
          sherr!(ExecFail, "Cannot overwrite readonly global '{}'", var_name.to_str_lossy()),
        );
      }
    }

    let var = 'walk: {
      let mut scopes = self.bounded_scopes_rev_mut().peekable();
      while let Some(scope) = scopes.next() {
        if scope.var_exists(var_name) {
          if scopes.peek().is_none() {
            // this is the global scope, and we haven't
            // found any shadowing variable on the way up.
            // nothing to do
            return Ok(());
          }
          // we are inspecting a non-global scope
          break 'walk scope.try_take_var(var_name)?;
        }
      }
      None
    };

    let Some(ceiling) = self.ceiling_mut() else {
      return Ok(());
    };

    let mut var = var.unwrap_or_default();
    *var.flags_mut() &= !VarFlags::LOCAL;

    // clobbers any existing same-named variable
    ceiling.put_var(var_name, var);

    Ok(())
  }

  pub(crate) fn set_var_indexed(
    &mut self,
    var_name: &[u8],
    idx: ArrIndex,
    val: ShStr,
    flags: VarFlags,
  ) -> ShResult<()> {
    if flags.contains(VarFlags::LOCAL) || self.writes_are_local() {
      let idx_scope = self.local_scope_idx().unwrap_or(0);
      let Some(scope) = self.scopes.get_mut(idx_scope) else {
        return Ok(());
      };
      return scope.set_index(var_name, idx, val);
    }
    // Dynamic scoping: find nearest scope with this variable
    for scope in self.bounded_scopes_rev_mut() {
      if scope.var_exists(var_name) {
        return scope.set_index(var_name, idx, val);
      }
    }
    // Not found - create in global scope
    let Some(scope) = self.var_creation_scope_mut() else {
      return Ok(());
    };
    scope.set_index(var_name, idx, val)
  }
  /// Innermost ceiling scope, or `scopes[0]` if no ceiling exists.
  /// Used as the fallback target for "create new var" writes so subshell
  /// writes don't leak past the barrier.
  fn ceiling_mut(&mut self) -> Option<&mut VarTab> {
    let pos = self
      .scopes
      .iter()
      .rposition(VarTab::is_ceiling)
      .unwrap_or(0);
    self.scopes.get_mut(pos)
  }
  /// The scope that is targeted by bare var assignments
  ///
  fn var_creation_scope_mut(&mut self) -> Option<&mut VarTab> {
    if shopt!(set.localvars)
      && Shed::meta(MetaTab::in_func)
      && let Some(idx) = self.local_scope_idx()
    {
      return self.scopes.get_mut(idx);
    }
    self.ceiling_mut()
  }
  fn create_var(&mut self, var_name: &[u8], val: VarKind, flags: VarFlags) -> ShResult<()> {
    let Some(scope) = self.var_creation_scope_mut() else {
      return Ok(());
    };
    scope.set_var(var_name, val, flags)
  }
  fn set_var_local(&mut self, var_name: &[u8], val: VarKind, flags: VarFlags) -> ShResult<()> {
    let idx = self.local_scope_idx().unwrap_or(0);
    let Some(scope) = self.scopes.get_mut(idx) else {
      return Err(
        sherr!(ExecFail, "No local scope available to set variable '{}'", var_name.to_str_lossy()),
      );
    };
    scope.set_var(var_name, val, flags)
  }
  pub(crate) fn try_get_arr_elems(&self, var_name: &[u8]) -> ShResult<Vec<ShStr>> {
    for scope in self.scopes_rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars().get(var_name)
      {
        match var.kind() {
          VarKind::Arr(items) => {
            return Ok(items.iter().cloned().collect());
          }
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name.to_str_lossy()));
          }
        }
      }
    }
    Err(sherr!(ExecFail, "Variable '{}' not found", var_name.to_str_lossy()))
  }
  pub(crate) fn try_get_assoc_items(&self, var_name: &[u8]) -> ShResult<Vec<(ShStr, ShStr)>> {
    for scope in self.scopes_rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars().get(var_name)
      {
        match var.kind() {
          VarKind::AssocArr(items) => {
            return Ok(items.clone());
          }
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an associative array", var_name.to_str_lossy()));
          }
        }
      }
    }
    Err(sherr!(ExecFail, "Variable '{}' not found", var_name.to_str_lossy()))
  }
  pub(crate) fn get_arr_elems(&self, var_name: &[u8]) -> Vec<ShStr> {
    self.try_get_arr_elems(var_name).unwrap_or_default()
  }
  pub(crate) fn get_assoc_mut(&mut self, var_name: &[u8]) -> ShResult<&mut Vec<(ShStr, ShStr)>> {
    if self.try_get_assoc_mut(var_name).is_err() {
      self
        .set_var(var_name, VarKind::assoc_arr(vec![]), VarFlags::empty())
        .ok();
    }
    self.try_get_assoc_mut(var_name)
  }
  pub(crate) fn try_get_assoc_mut(
    &mut self,
    var_name: &[u8],
  ) -> ShResult<&mut Vec<(ShStr, ShStr)>> {
    for scope in self.scopes_rev_mut() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars_mut().get_mut(var_name)
      {
        if var.flags().contains(VarFlags::READONLY) {
          return Err(sherr!(ExecFail, "Variable '{}' is readonly", var_name.to_str_lossy()));
        }
        match var.kind_mut() {
          VarKind::AssocArr(items) => return Ok(items),
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an associative array", var_name.to_str_lossy()));
          }
        }
      }
    }
    Err(sherr!(ExecFail, "Variable '{}' not found", var_name.to_str_lossy()))
  }
  pub(crate) fn get_arr_mut(&mut self, var_name: &[u8]) -> ShResult<&mut VecDeque<ShStr>> {
    if !self.var_exists(var_name) {
      self
        .set_var(var_name, VarKind::arr(vec![]), VarFlags::empty())
        .ok();
    }
    self.try_get_arr_mut(var_name)
  }
  pub(crate) fn try_get_arr_mut(&mut self, var_name: &[u8]) -> ShResult<&mut VecDeque<ShStr>> {
    for scope in self.scopes_rev_mut() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars_mut().get_mut(var_name)
      {
        if var.flags().contains(VarFlags::READONLY) {
          return Err(sherr!(ExecFail, "Variable '{}' is readonly", var_name.to_str_lossy()));
        }
        match var.kind_mut() {
          VarKind::Arr(items) => return Ok(items),
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name.to_str_lossy()));
          }
        }
      }
    }
    Err(sherr!(ExecFail, "Variable '{}' not found", var_name.to_str_lossy()))
  }
  pub(crate) fn index_var(&self, var_name: &[u8], idx: &ArrIndex) -> ShResult<ShStr> {
    self.index_var_sliced(var_name, idx, None, None)
  }
  pub(crate) fn take_index_var(
    &mut self,
    var_name: &[u8],
    idx: &ArrIndex,
  ) -> ShResult<Option<ShStr>> {
    for scope in self.bounded_scopes_rev_mut() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars_mut().get_mut(var_name)
      {
        if var.flags().contains(VarFlags::READONLY) {
          return Err(sherr!(ExecFail, "Variable '{}' is readonly", var_name.to_str_lossy()));
        }

        let idx = idx.clone().resolve_for(var.kind().tag())?;
        match var.kind_mut() {
          VarKind::AssocArr(items) => {
            let ArrIndex::Key(key) = idx else {
              return Err(sherr!(
                ExecFail,
                "Cannot take all elements of associative array '{}'",
                var_name.to_str_lossy()
              ));
            };

            let Some(pos) = items.iter().position(|(k, _)| k == &key) else {
              return Ok(None);
            };
            return Ok(Some(items.remove(pos).1));
          }
          VarKind::Arr(items) => {
            match idx {
              ArrIndex::Literal(n) => {
                if n < items.len() {
                  return Ok(items.remove(n));
                }
                return Err(sherr!(ExecFail, "Index {} out of bounds for array '{}'", n, var_name.to_str_lossy()));
              }
              ArrIndex::FromBack(n) => {
                if n >= 1 && n <= items.len() {
                  let idx = items.len() - n;
                  return Ok(items.remove(idx));
                }
                return Err(sherr!(ExecFail, "Index {} out of bounds for array '{}'", n, var_name.to_str_lossy()));
              }
              _ => {
                return Err(sherr!(ExecFail, "Cannot take all elements of array '{}'", var_name.to_str_lossy()));
              }
            }
          }
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name.to_str_lossy()));
          }
        }
      }
    }
    Ok(None)
  }

  pub(crate) fn index_var_sliced(
    &self,
    var_name: &[u8],
    idx: &ArrIndex,
    slice_start: Option<usize>,
    slice_len: Option<usize>,
  ) -> ShResult<ShStr> {
    for scope in self.scopes_rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars().get(var_name)
      {
        let idx = idx.clone().resolve_for(var.kind().tag())?;
        match var.kind() {
          VarKind::Arr(items) => {
            match idx {
              ArrIndex::AllSplit => unreachable!(),
              ArrIndex::AllJoined => {
                let ifs = self
                  .try_get_var(b"IFS")
                  .unwrap_or_else(|| " \t\n".into())
                  .chars()
                  .next()
                  .unwrap_or(' ')
                  .to_string();

                let start = slice_start.unwrap_or(0);
                let end   = start + slice_len.unwrap_or(items.len().saturating_sub(start));

                let sliced = items
                  .iter()
                  .skip(start)
                  .take(end - start)
                  .cloned()
                  .collect::<Vec<_>>()
                  .join_with(&ifs);

                return Ok(sliced);
              }
              ArrIndex::ArgCount => {
                return Ok(items.len().to_string().into());
              }
              _ => {}
            }
            let idx = match idx {
              ArrIndex::Literal(n) => n,
              ArrIndex::FromBack(n) => {
                if items.len() >= n {
                  items.len() - n
                } else {
                  return Err(
                    sherr!(ExecFail, "Index {} out of bounds for array '{}'", n, var_name.to_str_lossy()),
                  );
                }
              }
              _ => {
                return Err(sherr!(ExecFail, "Cannot index all elements of array '{}'", var_name.to_str_lossy()));
              }
            };

            if let Some(item) = items.get(idx) {
              return Ok(item.clone());
            }
            return Err(
              sherr!(ExecFail, "Index {idx} out of bounds for array '{}'", var_name.to_str_lossy()),
            );
          }
          VarKind::AssocArr(items) => match idx {
            ArrIndex::AllSplit => unreachable!(),
            ArrIndex::AllJoined => {
              let ifs = self
                .try_get_var(b"IFS")
                .unwrap_or_else(|| " \t\n".into())
                .chars()
                .next()
                .unwrap_or(' ')
                .to_string();
              let values: Vec<&ShStr> = items.iter().map(|(_, v)| v).collect();
              return Ok(values.join_with(&ifs));
            }
            ArrIndex::ArgCount => {
              return Ok(items.len().to_string().into());
            }
            ArrIndex::Key(key) => {
              for (k, v) in items {
                if k == &key {
                  return Ok(v.clone());
                }
              }
              return Ok(ShStr::default());
            }
            _ => unreachable!("resolve_for guarantees Key/AllSplit/AllJoined/ArgCount on AssocArr"),
          },
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name.to_str_lossy()));
          }
        }
      }
    }
    Ok(ShStr::default())
  }

  pub(crate) fn get_array_keys(&self, var_name: &[u8]) -> ShResult<ShStr> {
    let get_sep = || {
      self
        .try_get_var(b"IFS")
        .unwrap_or_else(|| " \t\n".into())
        .chars()
        .next()
        .map(ShStr::from)
        .unwrap_or_else(|| ShStr::from(' '))
    };

    for scope in self.scopes_rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars().get(var_name)
      {
        match var.kind() {
          VarKind::Arr(items) => {
            let indices: Vec<ShStr> = (0..items.len()).map(|i| i.to_string().into()).collect();
            let sep                 = get_sep();

            return Ok(indices.join_with(&sep));
          }
          VarKind::AssocArr(items) => {
            let keys: Vec<&ShStr> = items.iter().map(|(k, _)| k).collect();
            let sep = self
              .try_get_var(b"IFS")
              .unwrap_or_else(|| " \t\n".into())
              .chars()
              .next()
              .unwrap_or(' ')
              .to_string();

            return Ok(keys.join_with(&sep));
          }
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name.to_str_lossy()));
          }
        }
      }
    }
    Ok(ShStr::default())
  }

  pub(crate) fn bounded_scopes_rev(&self) -> impl Iterator<Item = &VarTab> {
    let skip = self
      .scopes
      .iter()
      .rposition(VarTab::is_ceiling)
      .unwrap_or(0);
    self.scopes.iter().skip(skip).rev()
  }
  pub(crate) fn bounded_scopes_rev_mut(&mut self) -> impl Iterator<Item = &mut VarTab> {
    let skip = self
      .scopes
      .iter()
      .rposition(VarTab::is_ceiling)
      .unwrap_or(0);
    self.scopes.iter_mut().skip(skip).rev()
  }

  /// Reverse walk that ignores ceilings. Use for reads that should fall
  /// through subshell barriers (e.g. `$HOME` inside `$(...)`); for writes
  /// use `bounded_scopes_rev_mut` instead so subshell mutations stay local.
  pub(crate) fn scopes_rev(&self) -> impl Iterator<Item = &VarTab> {
    self.scopes.iter().rev()
  }

  pub(crate) fn scopes_rev_mut(&mut self) -> impl Iterator<Item = &mut VarTab> {
    self.scopes.iter_mut().rev()
  }

  /// Forward walk (outermost to innermost). Use when emitting all scope
  /// contents in shadowing order, e.g. building envp.
  pub(crate) fn scopes_iter(&self) -> impl Iterator<Item = &VarTab> {
    self.scopes.iter()
  }

  pub(crate) fn try_get_var(&self, var_name: &[u8]) -> Option<ShStr> {
    if ShellParam::is_param(var_name) && let Ok(param) = var_name.parse_bytes::<ShellParam>() {
      return self.try_get_param(param);
    }
    for scope in self.scopes_rev() {
      // Stop at the first scope that *declares* the name, even if it's a
      // declared-but-unset local: it shadows outer scopes, so an unset local
      // must read as unset rather than falling through to an outer value.
      if scope.var_exists(var_name) {
        return scope.try_get_local(var_name);
      }
    }
    None
  }
  pub(crate) fn try_take_var(&mut self, var_name: &[u8]) -> ShResult<Option<Var>> {
    if ShellParam::is_param(var_name) && let Ok(param) = var_name.parse_bytes::<ShellParam>() {
      return Ok(self.try_take_param(param)?.map(Var::from));
    }
    for scope in self.bounded_scopes_rev_mut() {
      if scope.var_exists(var_name) {
        return scope.try_take_local(var_name);
      }
    }

    Ok(None)
  }
  /// Resolve a pre-parsed `VarName`, handling array indexes and slicing if present.
  /// Whether an array/assoc element actually exists. Mirrors the
  /// `get_var`/`try_get_var` split for indexing: distinguishes a missing
  /// key/index from a present-but-empty one, which the `${x+y}`/`${x-y}`
  /// set-tests depend on.
  pub(crate) fn index_is_set(&self, var_name: &[u8], idx: &ArrIndex) -> bool {
    for scope in self.scopes_rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars().get(var_name)
      {
        let Ok(idx) = idx.clone().resolve_for(var.kind().tag()) else {
          return false;
        };
        return match var.kind() {
          VarKind::Arr(items) => match idx {
            ArrIndex::Literal(n)  => n < items.len(),
            ArrIndex::FromBack(n) => n >= 1 && n <= items.len(),
            ArrIndex::AllSplit | ArrIndex::AllJoined | ArrIndex::ArgCount => true,
            _ => false,
          },
          VarKind::AssocArr(items) => match idx {
            ArrIndex::Key(key) => items.iter().any(|(k, _)| k == &key),
            ArrIndex::AllSplit | ArrIndex::AllJoined | ArrIndex::ArgCount => true,
            _ => false,
          },
          _ => false,
        };
      }
    }
    false
  }
  fn resolve_var(&self, var: &VarName) -> Option<ShStr> {
    let name = var.name();

    if let Some(idx) = var.index() {
      if !self.index_is_set(&name, idx) {
        return None;
      }

      self
        .index_var_sliced(&name, idx, var.slice_start(), var.slice_len())
        .ok()
    } else {
      self.try_get_var(&name)
    }
  }

  pub(crate) fn resolve_words(&self, var: &VarName) -> Option<Words> {
    if var.is_argv_joined() || var.is_argv_split() {
      // "$@" or "$*"
      Some(self.sh_argv_scope().pos_args().cloned().collect())
    } else if var.is_all_joined() || var.is_all_split() {
      // "${array[@]}" or "${array[*]}"
      self.split_values(var)
    } else {
      // "$var"
      self.resolve_var(var).map(Words::from)
    }
  }

  pub(crate) fn split_keys(&self, var: &VarName) -> Option<Words> {
    self.split_words(var, SplitKind::Keys)
  }

  pub(crate) fn split_values(&self, var: &VarName) -> Option<Words> {
    self.split_words(var, SplitKind::Values)
  }

  /// The individual fields of an `[@]` split expansion, honoring an array
  /// slice. `None` when `var` is not a split form or names no existing array.
  fn split_words(&self, var: &VarName, split_kind: SplitKind) -> Option<Words> {
    if !matches!(var.index(), Some(ArrIndex::AllSplit | ArrIndex::AllJoined)) {
      return None;
    }
    let name = var.name();

    for scope in self.scopes_rev() {
      let Some(v) = scope.vars().get(&name) else {
        continue;
      };

      let fields = match (v.kind(), split_kind) {
        (VarKind::Arr(items), SplitKind::Values) => items.iter().cloned().collect_vec(),
        (VarKind::Arr(items), SplitKind::Keys) => items
          .iter()
          .enumerate()
          .map(|(i, _)| varstr!("{i}"))
          .collect_vec(),

        (VarKind::AssocArr(items), SplitKind::Values) => {
          items.iter().map(|(_, v)| v.clone()).collect_vec()
        }
        (VarKind::AssocArr(items), SplitKind::Keys) => {
          items.iter().map(|(k, _)| k.clone()).collect_vec()
        }

        (VarKind::Str(s), SplitKind::Values) => {
          vec![s.clone()]
        }
        (VarKind::Int(i), SplitKind::Values) => {
          vec![varstr!("{i}")]
        }
        (VarKind::Magic(m), SplitKind::Values) => match m() {
          Some(s) => vec![s],
          None    => vec![],
        },

        (VarKind::Int(_) | VarKind::Str(_) | VarKind::Magic(_), SplitKind::Keys) => {
          vec![varstr!("0")]
        }

        (VarKind::Unset, _) => vec![],
      };

      let skip   = var.slice_start().unwrap_or(0);
      let take   = var.slice_len().unwrap_or(fields.len().saturating_sub(skip));

      let sliced = fields.into_iter().skip(skip).take(take);

      return Some(sliced.collect());
    }

    Some(Words::default())
  }
  pub(crate) fn try_take_var_kind(&mut self, var_name: &[u8]) -> ShResult<Option<VarKind>> {
    Ok(self.try_take_var(var_name)?.map(Var::into_kind))
  }
  pub(crate) fn take_var(&mut self, var_name: &[u8]) -> ShResult<ShStr> {
    Ok(
      self
        .try_take_var(var_name)?
        .map(ShStr::from)
        .unwrap_or_default(),
    )
  }
  pub(crate) fn get_var(&self, var_name: &[u8]) -> ShStr {
    self.try_get_var(var_name).unwrap_or_default()
  }
  pub(crate) fn get_var_meta(&self, var_name: &[u8]) -> Var {
    self.try_get_var_meta(var_name).unwrap_or_default()
  }
  pub(crate) fn writes_are_local(&self) -> bool {
    shopt!(set.localvars) && Shed::meta(MetaTab::in_func)
  }
  pub(crate) fn try_get_var_meta_for_write(&self, var_name: &[u8]) -> Option<Var> {
    if self.writes_are_local() {
      let idx = self.local_scope_idx()?;
      return self.scopes.get(idx)?.try_get_var_meta(var_name);
    }
    self.try_get_var_meta(var_name)
  }
  pub(crate) fn try_get_var_meta(&self, var_name: &[u8]) -> Option<Var> {
    for scope in self.scopes_rev() {
      if scope.var_exists(var_name) {
        return scope.try_get_var_meta(var_name);
      }
    }
    None
  }

  #[cfg(test)]
  pub(crate) fn try_get_var_kind(&self, var_name: &[u8]) -> Option<VarKind> {
    for scope in self.scopes_rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars().get(var_name)
      {
        return Some(var.kind().clone());
      }
    }
    None
  }

  pub(crate) fn try_get_var_kind_tag(&self, var_name: &[u8]) -> Option<VarKindTag> {
    for scope in self.scopes_rev() {
      if let Some(tag) = scope.try_get_var_kind_tag(var_name) {
        return Some(tag);
      }
    }
    None
  }
  pub(crate) fn all_vars(&self) -> HashMap<ShStr, Var> {
    let mut vars = HashMap::default();
    for scope in &self.scopes {
      for (k, v) in scope.vars() {
        vars.insert(k.clone(), v.clone());
      }
    }
    vars
  }
  pub(crate) fn get_local_var_flags(&self, var_name: &[u8]) -> Option<VarFlags> {
    for scope in self.bounded_scopes_rev() {
      if scope.var_exists(var_name) {
        return scope.get_var_flags(var_name);
      }
    }
    None
  }
  pub(crate) fn get_var_flags(&self, var_name: &[u8]) -> Option<VarFlags> {
    for scope in self.scopes_rev() {
      if scope.var_exists(var_name) {
        return scope.get_var_flags(var_name);
      }
    }
    None
  }
  pub(crate) fn try_get_param(&self, param: ShellParam) -> Option<ShStr> {
    if param.is_global()
      && let Some(val) = self.global_params.get(&param)
    {
      return Some(val.clone());
    }
    // Positional params are scope-local; only check the current scope
    if matches!(
      param,
      ShellParam::Pos(_) | ShellParam::AllArgs | ShellParam::AllArgsStr | ShellParam::ArgCount
    ) {
      let scope = self.sh_argv_scope();
      return scope.try_get_param(param);
    }
    for scope in self.scopes_rev() {
      if let Some(val) = scope.try_get_param(param) {
        return Some(val);
      }
    }

    None
  }
  pub(crate) fn try_take_param(&mut self, param: ShellParam) -> ShResult<Option<ShStr>> {
    if param.is_global() && self.global_params.contains_key(&param) {
      return Err(sherr!(ExecFail, "Cannot take global shell parameter '{param}'"));
    }
    // Positional params are scope-local; only check the current scope
    if matches!(
      param,
      ShellParam::Pos(_) | ShellParam::AllArgs | ShellParam::AllArgsStr | ShellParam::ArgCount
    ) {
      let scope = self.sh_argv_scope_mut();
      return Ok(scope.try_take_param(param));
    }
    for scope in self.scopes_rev_mut() {
      if let Some(val) = scope.try_take_param(param) {
        return Ok(Some(val));
      }
    }

    Ok(None)
  }
  pub(crate) fn get_param(&self, param: ShellParam) -> ShStr {
    self.try_get_param(param).unwrap_or_default()
  }
  /// Set a shell parameter
  pub(crate) fn set_param(&mut self, param: ShellParam, val: ShStr) {
    match param {
      ShellParam::ShPid | ShellParam::LastJob | ShellParam::ShellName => {
        self.global_params.insert(param, val);
      }
      ShellParam::Pos(_) | ShellParam::AllArgs | ShellParam::AllArgsStr | ShellParam::ArgCount => {
        let scope = self.sh_argv_scope_mut();
        scope.set_param(param, &val);
      }
    }
  }
}

#[cfg(test)]
mod index_var_sliced_tests {
  use super::*;
  use crate::state::Shed;
  use crate::state::vars::{ArrIndex, VarFlags, VarKind};
  use crate::tests::testutil::TestGuard;
  use std::collections::VecDeque;

  fn set_arr(name: &str, items: &[&str]) {
    let vec: VecDeque<String> = items.iter().map(ToString::to_string).collect();
    Shed::vars_mut(|v| {
      v.set_var(
        name.as_bytes(),
        VarKind::arr(vec.into_iter().map(Into::into)),
        VarFlags::empty(),
      )
      .unwrap();
    });
  }

  fn set_assoc(name: &str, pairs: &[(&str, &str)]) {
    let vec: Vec<(ShStr, ShStr)> = pairs
      .iter()
      .map(|(k, v)| ((*k).into(), (*v).into()))
      .collect();
    Shed::vars_mut(|v| {
      v.set_var(name.as_bytes(), VarKind::assoc_arr(vec), VarFlags::empty())
        .unwrap();
    });
  }

  fn set_str(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name.as_bytes(), VarKind::Str(val.into()), VarFlags::empty())
        .unwrap();
    });
  }

  fn index(
    name: &str,
    idx: &ArrIndex,
    slice_start: Option<usize>,
    slice_len: Option<usize>,
  ) -> ShResult<ShStr> {
    Shed::vars(|v| v.index_var_sliced(name.as_bytes(), idx, slice_start, slice_len))
  }

  // ─── Arr: positional index ────────────────────────────────────────

  #[test]
  fn arr_literal_index() {
    let _g = TestGuard::new();
    set_arr("arr", &["a", "b", "c"]);
    assert_eq!(
      index("arr", &ArrIndex::Literal(1), None, None).unwrap(),
      "b"
    );
  }

  #[test]
  fn arr_literal_out_of_bounds_errors() {
    let _g = TestGuard::new();
    set_arr("arr", &["a", "b"]);
    assert!(index("arr", &ArrIndex::Literal(99), None, None).is_err());
  }

  #[test]
  fn arr_from_back_index() {
    let _g = TestGuard::new();
    set_arr("arr", &["a", "b", "c", "d"]);
    // FromBack(1) → items.len() - 1 = 3 → last element "d"
    assert_eq!(
      index("arr", &ArrIndex::FromBack(1), None, None).unwrap(),
      "d"
    );
  }

  #[test]
  fn arr_from_back_overflows_errors() {
    let _g = TestGuard::new();
    set_arr("arr", &["a", "b"]);
    // FromBack(99) — items.len() (2) < 99 → ExecFail.
    assert!(index("arr", &ArrIndex::FromBack(99), None, None).is_err());
  }

  // ─── Arr: ArgCount ────────────────────────────────────────────────

  #[test]
  fn arr_arg_count_returns_length() {
    let _g = TestGuard::new();
    set_arr("arr", &["x", "y", "z", "w"]);
    assert_eq!(index("arr", &ArrIndex::ArgCount, None, None).unwrap(), "4");
  }

  #[test]
  fn arr_arg_count_on_empty_array() {
    let _g = TestGuard::new();
    set_arr("arr", &[]);
    assert_eq!(index("arr", &ArrIndex::ArgCount, None, None).unwrap(), "0");
  }

  // ─── Arr: AllSplit / AllJoined ────────────────────────────────────

  #[test]
  fn arr_all_joined_uses_first_char_of_ifs() {
    let _g = TestGuard::new();
    set_str("IFS", ",xy");
    set_arr("arr", &["a", "b", "c"]);
    // First char of IFS is ',', so values join with ','.
    assert_eq!(
      index("arr", &ArrIndex::AllJoined, None, None).unwrap(),
      "a,b,c"
    );
  }

  #[test]
  fn arr_all_joined_defaults_to_space_when_ifs_empty() {
    let _g = TestGuard::new();
    set_str("IFS", "");
    set_arr("arr", &["a", "b", "c"]);
    // Empty IFS → first-char is '\0'?  Actually IFS=""  → next().unwrap_or(' ').
    // Looking at code: `.chars().next().unwrap_or(' ')`. An empty string
    // yields None from next() so we get ' '.
    assert_eq!(
      index("arr", &ArrIndex::AllJoined, None, None).unwrap(),
      "a b c"
    );
  }

  // ─── Arr: slicing ────────────────────────────────────────────────

  #[test]
  fn arr_all_joined_with_slice() {
    let _g = TestGuard::new();
    set_str("IFS", "-");
    set_arr("arr", &["a", "b", "c", "d", "e"]);
    assert_eq!(
      index("arr", &ArrIndex::AllJoined, Some(2), Some(2)).unwrap(),
      "c-d"
    );
  }

  #[test]
  fn arr_all_joined_with_slice_start_only() {
    let _g = TestGuard::new();
    set_str("IFS", "-");
    set_arr("arr", &["a", "b", "c", "d"]);
    // start=1, no len → take rest
    assert_eq!(
      index("arr", &ArrIndex::AllJoined, Some(1), None).unwrap(),
      "b-c-d"
    );
  }

  // ─── AssocArr ────────────────────────────────────────────────────

  #[test]
  fn assoc_arr_key_lookup() {
    let _g = TestGuard::new();
    set_assoc("amap", &[("apple", "red"), ("banana", "yellow")]);
    assert_eq!(
      index("amap", &ArrIndex::Key("apple".into()), None, None).unwrap(),
      "red"
    );
  }

  #[test]
  fn assoc_arr_missing_key_returns_empty_string() {
    let _g = TestGuard::new();
    set_assoc("amap", &[("a", "1")]);
    assert_eq!(
      index("amap", &ArrIndex::Key("missing".into()), None, None).unwrap(),
      ""
    );
  }

  #[test]
  fn assoc_arr_arg_count() {
    let _g = TestGuard::new();
    set_assoc("amap", &[("a", "1"), ("b", "2"), ("c", "3")]);
    assert_eq!(index("amap", &ArrIndex::ArgCount, None, None).unwrap(), "3");
  }

  #[test]
  fn assoc_arr_all_joined_with_ifs() {
    let _g = TestGuard::new();
    set_str("IFS", "+");
    set_assoc("amap", &[("a", "1"), ("b", "2")]);
    let result = index("amap", &ArrIndex::AllJoined, None, None).unwrap();
    // Iteration order is preserved, values joined by '+'.
    assert_eq!(result, "1+2");
  }

  // ─── Scalar var errors ───────────────────────────────────────────

  #[test]
  fn scalar_var_is_not_indexable_errors() {
    let _g = TestGuard::new();
    set_str("scalar", "hello");
    assert!(index("scalar", &ArrIndex::Literal(0), None, None).is_err());
  }

  // ─── Missing var returns empty string ────────────────────────────

  #[test]
  fn missing_var_returns_empty_string() {
    let _g = TestGuard::new();
    assert_eq!(
      index("no_such_var_xyz_qqq", &ArrIndex::Literal(0), None, None).unwrap(),
      ""
    );
  }
}

#[cfg(test)]
mod get_array_keys_tests {
  use super::*;
  use crate::state::Shed;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::tests::testutil::TestGuard;
  use std::collections::VecDeque;

  fn set_arr(name: &str, items: &[&str]) {
    let vec: VecDeque<String> = items.iter().map(ToString::to_string).collect();
    Shed::vars_mut(|v| {
      v.set_var(
        name.as_bytes(),
        VarKind::arr(vec.into_iter().map(Into::into)),
        VarFlags::empty(),
      )
      .unwrap();
    });
  }

  fn set_assoc(name: &str, pairs: &[(&str, &str)]) {
    let vec: Vec<(ShStr, ShStr)> = pairs
      .iter()
      .map(|(k, v)| ((*k).into(), (*v).into()))
      .collect();
    Shed::vars_mut(|v| {
      v.set_var(name.as_bytes(), VarKind::assoc_arr(vec), VarFlags::empty())
        .unwrap();
    });
  }

  fn set_str(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name.as_bytes(), VarKind::Str(val.into()), VarFlags::empty())
        .unwrap();
    });
  }

  fn get_keys(name: &str, _joined: bool) -> ShResult<ShStr> {
    Shed::vars(|v| v.get_array_keys(name.as_bytes()))
  }

  #[test]
  fn arr_keys_are_sequential_indices() {
    let _g = TestGuard::new();
    set_arr("arr", &["a", "b", "c", "d"]);
    // joined=true → first char of IFS (default ' ').
    let out = get_keys("arr", true).unwrap();
    assert_eq!(out, "0 1 2 3");
  }

  #[test]
  fn empty_arr_returns_empty_string() {
    let _g = TestGuard::new();
    set_arr("arr_empty", &[]);
    assert_eq!(get_keys("arr_empty", true).unwrap(), "");
  }

  #[test]
  fn assoc_keys_returned_in_insertion_order() {
    let _g = TestGuard::new();
    set_assoc("h", &[("foo", "1"), ("bar", "2"), ("baz", "3")]);
    let out = get_keys("h", true).unwrap();
    // Joined with space (default IFS first char).
    assert_eq!(out, "foo bar baz");
  }

  #[test]
  fn scalar_var_errors() {
    let _g = TestGuard::new();
    set_str("scalar", "value");
    assert!(get_keys("scalar", true).is_err());
  }

  #[test]
  fn missing_var_returns_empty_string() {
    let _g = TestGuard::new();
    assert_eq!(
      get_keys("no_such_var_for_get_array_keys", true).unwrap(),
      ""
    );
  }

  #[test]
  fn custom_ifs_first_char_is_used_when_joined() {
    let _g = TestGuard::new();
    set_arr("arr", &["a", "b", "c"]);
    Shed::vars_mut(|v| {
      v.set_var(b"IFS", VarKind::Str(":/".into()), VarFlags::empty())
        .unwrap();
    });
    // First char of IFS is ':'.
    assert_eq!(get_keys("arr", true).unwrap(), "0:1:2");
  }
}
