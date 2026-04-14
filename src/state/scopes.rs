use super::*;

use std::{
  collections::{HashMap, VecDeque, hash_map::Entry},
  sync::atomic::Ordering,
  time::Duration,
};

use crate::{builtin::map::MapNode, libsh::error::ShResult, prelude::*, sherr};

#[derive(Clone, Default, Debug)]
pub struct ScopeStack {
  // ALWAYS keep one scope.
  // The bottom scope is the global variable space.
  // Scopes that come after that are pushed in functions,
  // and only contain variables that are defined using `local`.
  scopes: Vec<VarTab>,
  depth: u32,

  // Global parameters such as $!, $$, etc
  global_params: HashMap<String, String>,
}

impl ScopeStack {
  pub fn new() -> Self {
    let mut new = Self::default();
    new.scopes.push(VarTab::new());
    let shell_name = std::env::args()
      .next()
      .unwrap_or_else(|| "shed".to_string());
    new
      .global_params
      .insert(ShellParam::ShellName.to_string(), shell_name);
    new
  }
  pub fn descend(&mut self, argv: Option<Vec<String>>) {
    let mut new_vars = VarTab::bare();
    if let Some(argv) = argv {
      for arg in argv {
        new_vars.bpush_arg(arg);
      }
    }
    self.scopes.push(new_vars);
    self.depth += 1;
  }
  pub fn ascend(&mut self) {
    if self.depth >= 1 {
      self.scopes.pop();
      self.depth -= 1;
    }
  }
  pub fn depth(&self) -> u32 {
    self.depth
  }
  pub fn cur_scope(&self) -> &VarTab {
    self.scopes.last().unwrap()
  }
  pub fn cur_scope_mut(&mut self) -> &mut VarTab {
    self.scopes.last_mut().unwrap()
  }
  pub fn sh_argv(&self) -> &VecDeque<String> {
    self.cur_scope().sh_argv()
  }
  pub fn unset_var(&mut self, var_name: &str) -> ShResult<()> {
    for scope in self.scopes.iter_mut().rev() {
      if scope.var_exists(var_name) {
        return scope.unset_var(var_name);
      }
    }
    Err(sherr!(ExecFail, "Variable '{}' not found", var_name,))
  }
  pub fn export_var(&mut self, var_name: &str) {
    for scope in self.scopes.iter_mut().rev() {
      if scope.var_exists(var_name) {
        scope.export_var(var_name);
        return;
      }
    }
  }
  pub fn var_exists(&self, var_name: &str) -> bool {
    for scope in self.scopes.iter().rev() {
      if scope.var_exists(var_name) {
        return true;
      }
    }
    if let Ok(param) = var_name.parse::<ShellParam>() {
      return self.global_params.contains_key(&param.to_string());
    }
    false
  }
  pub fn flatten_vars(&self) -> HashMap<String, Var> {
    let mut flat_vars = HashMap::new();
    for scope in self.scopes.iter() {
      for (var_name, var) in scope.vars() {
        flat_vars.insert(var_name.clone(), var.clone());
      }
    }
    for var in env::vars() {
      if let Entry::Vacant(e) = flat_vars.entry(var.0) {
        e.insert(Var::new(VarKind::Str(var.1), VarFlags::EXPORT));
      }
    }

    flat_vars
  }
  pub fn set_var(&mut self, var_name: &str, val: VarKind, flags: VarFlags) -> ShResult<()> {
    if flags.contains(VarFlags::LOCAL) {
      return self.set_var_local(var_name, val, flags);
    }
    // Dynamic scoping: walk scopes from innermost to outermost,
    // update the nearest scope that already has this variable
    for scope in self.scopes.iter_mut().rev() {
      if scope.var_exists(var_name) {
        return scope.set_var(var_name, val, flags);
      }
    }
    // Not found in any scope - create in global scope
    self.set_var_global(var_name, val, flags)
  }
  pub fn set_var_indexed(
    &mut self,
    var_name: &str,
    idx: ArrIndex,
    val: String,
    flags: VarFlags,
  ) -> ShResult<()> {
    if flags.contains(VarFlags::LOCAL) {
      let Some(scope) = self.scopes.last_mut() else {
        return Ok(());
      };
      return scope.set_index(var_name, idx, val);
    }
    // Dynamic scoping: find nearest scope with this variable
    for scope in self.scopes.iter_mut().rev() {
      if scope.var_exists(var_name) {
        return scope.set_index(var_name, idx, val);
      }
    }
    // Not found - create in global scope
    let Some(scope) = self.scopes.first_mut() else {
      return Ok(());
    };
    scope.set_index(var_name, idx, val)
  }
  fn set_var_global(&mut self, var_name: &str, val: VarKind, flags: VarFlags) -> ShResult<()> {
    let Some(scope) = self.scopes.first_mut() else {
      return Ok(());
    };
    scope.set_var(var_name, val, flags)
  }
  fn set_var_local(&mut self, var_name: &str, val: VarKind, flags: VarFlags) -> ShResult<()> {
    let Some(scope) = self.scopes.last_mut() else {
      return Ok(());
    };
    scope.set_var(var_name, val, flags)
  }
  pub fn get_magic_var(&self, var_name: &str) -> Option<String> {
    match var_name {
      "SECONDS" => {
        let shell_time = read_meta(|m| m.shell_time());
        let secs = Instant::now().duration_since(shell_time).as_secs();
        Some(secs.to_string())
      }
      "EPOCHREALTIME" => {
        let epoch = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap_or(Duration::from_secs(0))
          .as_secs_f64();
        Some(epoch.to_string())
      }
      "EPOCHSECONDS" => {
        let epoch = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap_or(Duration::from_secs(0))
          .as_secs();
        Some(epoch.to_string())
      }
      "RANDOM" => {
        let random = rand::random_range(0..32768);
        Some(random.to_string())
      }
      "?" => Some(get_status().to_string()),
      "-" => {
        let mut set_string = String::new();
        read_shopts(|o| {
          if o.set.allexport {
            set_string.push('a');
          }
          if o.set.notify {
            set_string.push('b');
          }
          if o.set.noclobber {
            set_string.push('C');
          }
          if o.set.errexit {
            set_string.push('e');
          }
          if o.set.noglob {
            set_string.push('f');
          }
          if o.set.hashall {
            set_string.push('h');
          }
          if INTERACTIVE.load(Ordering::SeqCst) {
            set_string.push('i');
          }
          if o.set.monitor {
            set_string.push('m');
          }
          if o.set.noexec {
            set_string.push('n');
          }
          if o.set.nounset {
            set_string.push('u');
          }
          if o.set.verbose {
            set_string.push('v');
          }
          if o.set.xtrace {
            set_string.push('x');
          }
        });
        (!set_string.is_empty()).then_some(set_string)
      }
      _ => None,
    }
  }
  pub fn try_get_arr_elems(&self, var_name: &str) -> ShResult<Vec<String>> {
    for scope in self.scopes.iter().rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars().get(var_name)
      {
        match var.kind() {
          VarKind::Arr(items) => {
            return Ok(items.iter().cloned().collect());
          }
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name,));
          }
        }
      }
    }
    Err(sherr!(ExecFail, "Variable '{}' not found", var_name,))
  }
  pub fn get_arr_elems(&self, var_name: &str) -> Vec<String> {
    self.try_get_arr_elems(var_name).unwrap_or_default()
  }
  pub fn get_arr_mut(&mut self, var_name: &str) -> ShResult<&mut VecDeque<String>> {
    for scope in self.scopes.iter_mut().rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars_mut().get_mut(var_name)
      {
        match var.kind_mut() {
          VarKind::Arr(items) => return Ok(items),
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name,));
          }
        }
      }
    }
    Err(sherr!(ExecFail, "Variable '{var_name}' not found"))
  }
  pub fn index_var(&self, var_name: &str, idx: ArrIndex) -> ShResult<String> {
    for scope in self.scopes.iter().rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars().get(var_name)
      {
        match var.kind() {
          VarKind::Arr(items) => {
            let idx = match idx {
              ArrIndex::Literal(n) => n,
              ArrIndex::FromBack(n) => {
                if items.len() >= n {
                  items.len() - n
                } else {
                  return Err(sherr!(
                    ExecFail,
                    "Index {} out of bounds for array '{}'",
                    n,
                    var_name,
                  ));
                }
              }
              _ => {
                return Err(sherr!(
                  ExecFail,
                  "Cannot index all elements of array '{}'",
                  var_name,
                ));
              }
            };

            if let Some(item) = items.get(idx) {
              return Ok(item.clone());
            } else {
              return Err(sherr!(
                ExecFail,
                "Index {} out of bounds for array '{}'",
                idx,
                var_name,
              ));
            }
          }
          _ => {
            return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name,));
          }
        }
      }
    }
    Ok("".into())
  }
  pub fn remove_map(&mut self, map_name: &str) -> Option<MapNode> {
    for scope in self.scopes.iter_mut().rev() {
      if scope.get_map(map_name).is_some() {
        return scope.remove_map(map_name);
      }
    }
    None
  }
  pub fn get_map(&self, map_name: &str) -> Option<&MapNode> {
    for scope in self.scopes.iter().rev() {
      if let Some(map) = scope.get_map(map_name) {
        return Some(map);
      }
    }
    None
  }
  pub fn get_map_mut(&mut self, map_name: &str) -> Option<&mut MapNode> {
    for scope in self.scopes.iter_mut().rev() {
      if let Some(map) = scope.get_map_mut(map_name) {
        return Some(map);
      }
    }
    None
  }
  pub fn set_map(&mut self, map_name: &str, map: MapNode, local: bool) {
    if local && let Some(scope) = self.scopes.last_mut() {
      scope.set_map(map_name, map);
    } else if let Some(scope) = self.scopes.first_mut() {
      scope.set_map(map_name, map);
    }
  }
  pub fn try_get_var(&self, var_name: &str) -> Option<String> {
    // This version of get_var() is mainly used internally
    // so that we have access to Option methods
    if let Some(magic) = self.get_magic_var(var_name) {
      Some(magic)
    } else if let Ok(param) = var_name.parse::<ShellParam>() {
      let val = self.get_param(param);
      (!val.is_empty()).then_some(val)
    } else {
      for scope in self.scopes.iter().rev() {
        if scope.var_exists(var_name) {
          return Some(scope.get_var(var_name));
        }
      }

      None
    }
  }
  pub fn take_var(&mut self, var_name: &str) -> String {
    let var = self.get_var(var_name);
    self.unset_var(var_name).ok();
    var
  }
  pub fn get_var(&self, var_name: &str) -> String {
    self.try_get_var(var_name).unwrap_or_default()
  }
  pub fn get_var_meta(&self, var_name: &str) -> Var {
    self.try_get_var_meta(var_name).unwrap_or_default()
  }
  pub fn try_get_var_meta(&self, var_name: &str) -> Option<Var> {
    for scope in self.scopes.iter().rev() {
      if scope.var_exists(var_name) {
        return scope.try_get_var_meta(var_name);
      }
    }
    None
  }
  pub fn all_vars(&self) -> HashMap<String, Var> {
    let mut vars = HashMap::new();
    for scope in self.scopes.iter() {
      for (k, v) in scope.vars() {
        vars.insert(k.to_string(), v.clone());
      }
    }
    vars
  }
  pub fn is_local_var(&self, var_name: &str) -> bool {
    self.scopes.last().is_some_and(|s| {
      s.get_var_flags(var_name)
        .is_some_and(|flags| flags.contains(VarFlags::LOCAL))
    })
  }
  pub fn get_var_flags(&self, var_name: &str) -> Option<VarFlags> {
    for scope in self.scopes.iter().rev() {
      if scope.var_exists(var_name) {
        return scope.get_var_flags(var_name);
      }
    }
    None
  }
  pub fn get_param(&self, param: ShellParam) -> String {
    if param.is_global()
      && let Some(val) = self.global_params.get(&param.to_string())
    {
      return val.clone();
    }
    // Positional params are scope-local; only check the current scope
    if matches!(
      param,
      ShellParam::Pos(_) | ShellParam::AllArgs | ShellParam::AllArgsStr | ShellParam::ArgCount
    ) {
      if let Some(scope) = self.scopes.last() {
        return scope.get_param(param);
      }
      return "".into();
    }
    for scope in self.scopes.iter().rev() {
      let val = scope.get_param(param);
      if !val.is_empty() {
        return val;
      }
    }
    // Fallback to empty string
    "".into()
  }
  /// Set a shell parameter
  /// Therefore, these are global state and we use the global scope
  pub fn set_param(&mut self, param: ShellParam, val: &str) {
    match param {
      ShellParam::ShPid | ShellParam::Status | ShellParam::LastJob | ShellParam::ShellName => {
        self
          .global_params
          .insert(param.to_string(), val.to_string());
      }
      ShellParam::Pos(_) | ShellParam::AllArgs | ShellParam::AllArgsStr | ShellParam::ArgCount => {
        if let Some(scope) = self.scopes.first_mut() {
          scope.set_param(param, val);
        }
      }
    }
  }
}
