use std::{
  cell::RefCell,
  collections::{HashMap, HashSet, VecDeque, hash_map::Entry},
  fmt::Display,
  ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign},
  os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
  },
  str::FromStr,
  time::Duration,
};

use itertools::Itertools;
use nix::unistd::{User, gethostname, getppid, getuid};
use regex::Regex;

use crate::{
  builtin::{
    BUILTINS,
    keymap::{KeyMap, KeyMapFlags, KeyMapMatch},
    map::MapNode,
    trap::TrapTarget,
  },
  exec_input,
  expand::expand_keymap,
  jobs::{Job, JobTab},
  libsh::{
    error::{ShErr, ShErrKind, ShResult},
    utils::VecDequeExt,
  },
  parse::{
    ConjunctNode, NdRule, Node, ParsedSrc,
    lex::{LexFlags, LexStream, Span, Tk},
  },
  prelude::*,
  readline::{
    complete::{BashCompSpec, Candidate, CompSpec},
    keys::KeyEvent,
    markers,
  },
  shopt::ShOpts,
};

thread_local! {
  pub static SHED: Shed = Shed::new();
}

#[derive(Clone, Debug)]
pub struct Shed {
  pub jobs: RefCell<JobTab>,
  pub var_scopes: RefCell<ScopeStack>,
  pub meta: RefCell<MetaTab>,
  pub logic: RefCell<LogTab>,
  pub shopts: RefCell<ShOpts>,

  #[cfg(test)]
  saved: RefCell<Option<Box<Self>>>,
}

impl Shed {
  pub fn new() -> Self {
    Self {
      jobs: RefCell::new(JobTab::new()),
      var_scopes: RefCell::new(ScopeStack::new()),
      meta: RefCell::new(MetaTab::new()),
      logic: RefCell::new(LogTab::new()),
      shopts: RefCell::new(ShOpts::default()),

      #[cfg(test)]
      saved: RefCell::new(None),
    }
  }
}

impl Default for Shed {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
impl Shed {
  pub fn save(&self) {
    let saved = Self {
      jobs: RefCell::new(self.jobs.borrow().clone()),
      var_scopes: RefCell::new(self.var_scopes.borrow().clone()),
      meta: RefCell::new(self.meta.borrow().clone()),
      logic: RefCell::new(self.logic.borrow().clone()),
      shopts: RefCell::new(self.shopts.borrow().clone()),
      saved: RefCell::new(None),
    };
    *self.saved.borrow_mut() = Some(Box::new(saved));
  }

  pub fn restore(&self) {
    if let Some(saved) = self.saved.take() {
      *self.jobs.borrow_mut() = saved.jobs.into_inner();
      *self.var_scopes.borrow_mut() = saved.var_scopes.into_inner();
      *self.meta.borrow_mut() = saved.meta.into_inner();
      *self.logic.borrow_mut() = saved.logic.into_inner();
      *self.shopts.borrow_mut() = saved.shopts.into_inner();
    }
  }
}

#[derive(Hash, Eq, PartialEq, Debug, Clone, Copy)]
pub enum ShellParam {
  // Global
  Status,
  ShPid,
  LastJob,
  ShellName,

  // Local
  Pos(usize),
  AllArgs,
  AllArgsStr,
  ArgCount,
}

impl ShellParam {
  pub fn is_global(&self) -> bool {
    matches!(
      self,
      Self::Status | Self::ShPid | Self::LastJob | Self::ShellName
    )
  }

  pub fn from_char(c: &char) -> Option<Self> {
    match c {
      '?' => Some(Self::Status),
      '$' => Some(Self::ShPid),
      '!' => Some(Self::LastJob),
      '0' => Some(Self::ShellName),
      '@' => Some(Self::AllArgs),
      '*' => Some(Self::AllArgsStr),
      '#' => Some(Self::ArgCount),
      _ => None,
    }
  }
}

impl Display for ShellParam {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Status => write!(f, "?"),
      Self::ShPid => write!(f, "$"),
      Self::LastJob => write!(f, "!"),
      Self::ShellName => write!(f, "0"),
      Self::Pos(n) => write!(f, "{}", n),
      Self::AllArgs => write!(f, "@"),
      Self::AllArgsStr => write!(f, "*"),
      Self::ArgCount => write!(f, "#"),
    }
  }
}

impl FromStr for ShellParam {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "?" => Ok(Self::Status),
      "$" => Ok(Self::ShPid),
      "!" => Ok(Self::LastJob),
      "0" => Ok(Self::ShellName),
      "@" => Ok(Self::AllArgs),
      "*" => Ok(Self::AllArgsStr),
      "#" => Ok(Self::ArgCount),
      n if n.parse::<usize>().is_ok() => {
        let idx = n.parse::<usize>().unwrap();
        Ok(Self::Pos(idx))
      }
      _ => Err(ShErr::simple(
        ShErrKind::InternalErr,
        format!("Invalid shell parameter: {}", s),
      )),
    }
  }
}

#[derive(Clone, Default, Debug)]
pub struct ScopeStack {
  // ALWAYS keep one scope.
  // The bottom scope is the global variable space.
  // Scopes that come after that are pushed in functions,
  // and only contain variables that are defined using `local`.
  scopes: Vec<VarTab>,
  depth: u32,

  // Global parameters such as $?, $!, $$, etc
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
    Err(ShErr::simple(
      ShErrKind::ExecFail,
      format!("Variable '{}' not found", var_name),
    ))
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
    // Not found in any scope — create in global scope
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
    // Not found — create in global scope
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
      _ => None,
    }
  }
  pub fn get_arr_elems(&self, var_name: &str) -> ShResult<Vec<String>> {
    for scope in self.scopes.iter().rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars().get(var_name)
      {
        match var.kind() {
          VarKind::Arr(items) => {
            return Ok(items.iter().cloned().collect());
          }
          _ => {
            return Err(ShErr::simple(
              ShErrKind::ExecFail,
              format!("Variable '{}' is not an array", var_name),
            ));
          }
        }
      }
    }
    Err(ShErr::simple(
      ShErrKind::ExecFail,
      format!("Variable '{}' not found", var_name),
    ))
  }
  pub fn get_arr_mut(&mut self, var_name: &str) -> ShResult<&mut VecDeque<String>> {
    for scope in self.scopes.iter_mut().rev() {
      if scope.var_exists(var_name)
        && let Some(var) = scope.vars_mut().get_mut(var_name)
      {
        match var.kind_mut() {
          VarKind::Arr(items) => return Ok(items),
          _ => {
            return Err(ShErr::simple(
              ShErrKind::ExecFail,
              format!("Variable '{}' is not an array", var_name),
            ));
          }
        }
      }
    }
    Err(ShErr::simple(
      ShErrKind::ExecFail,
      format!("Variable '{}' not found", var_name),
    ))
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
                  return Err(ShErr::simple(
                    ShErrKind::ExecFail,
                    format!("Index {} out of bounds for array '{}'", n, var_name),
                  ));
                }
              }
              _ => {
                return Err(ShErr::simple(
                  ShErrKind::ExecFail,
                  format!("Cannot index all elements of array '{}'", var_name),
                ));
              }
            };

            if let Some(item) = items.get(idx) {
              return Ok(item.clone());
            } else {
              return Err(ShErr::simple(
                ShErrKind::ExecFail,
                format!("Index {} out of bounds for array '{}'", idx, var_name),
              ));
            }
          }
          _ => {
            return Err(ShErr::simple(
              ShErrKind::ExecFail,
              format!("Variable '{}' is not an array", var_name),
            ));
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
      return Some(magic);
    } else if let Ok(param) = var_name.parse::<ShellParam>() {
      let val = self.get_param(param);
      if !val.is_empty() {
        return Some(val);
      } else {
        return None;
      }
    }

    for scope in self.scopes.iter().rev() {
      if scope.var_exists(var_name) {
        return Some(scope.get_var(var_name));
      }
    }

    None
  }
  pub fn take_var(&mut self, var_name: &str) -> String {
    let var = self.get_var(var_name);
    self.unset_var(var_name).ok();
    var
  }
  pub fn get_var(&self, var_name: &str) -> String {
    if let Some(magic) = self.get_magic_var(var_name) {
      return magic;
    }
    if let Ok(param) = var_name.parse::<ShellParam>() {
      return self.get_param(param);
    }
    for scope in self.scopes.iter().rev() {
      if scope.var_exists(var_name) {
        return scope.get_var(var_name);
      }
    }
    // Fallback to env var
    std::env::var(var_name).unwrap_or_default()
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

#[derive(Clone, Debug)]
pub struct ShAlias {
  pub body: String,
  pub source: Span,
}

impl Display for ShAlias {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.body)
  }
}

/// A shell function
///
/// Wraps the BraceGrp Node that forms the body of the function, and provides some helper methods to extract it from the parse tree
#[derive(Clone, Debug)]
pub struct ShFunc {
  pub body: Node,
  pub source: Span,
}

impl ShFunc {
  pub fn new(mut src: ParsedSrc, source: Span) -> Self {
    let body = Self::extract_brc_grp_hack(src.extract_nodes());
    Self { body, source }
  }
  fn extract_brc_grp_hack(mut tree: Vec<Node>) -> Node {
    // FIXME: find a better way to do this
    let conjunction = tree.pop().unwrap();
    let NdRule::Conjunction { mut elements } = conjunction.class else {
      unreachable!()
    };
    let conjunct_node = elements.pop().unwrap();
    let ConjunctNode { cmd, operator: _ } = conjunct_node;
    *cmd
  }
  pub fn body(&self) -> &Node {
    &self.body
  }
  pub fn body_mut(&mut self) -> &mut Node {
    &mut self.body
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AutoCmdKind {
  PreCmd,
  PostCmd,
  PreChangeDir,
  PostChangeDir,
  OnJobFinish,
  PrePrompt,
  PostPrompt,
  PreModeChange,
  PostModeChange,
  OnHistoryOpen,
  OnHistoryClose,
  OnHistorySelect,
  OnCompletionStart,
  OnCompletionCancel,
  OnCompletionSelect,
  OnScreensaverExec,
  OnScreensaverReturn,
  OnExit,
}

crate::two_way_display!(AutoCmdKind,
  PreCmd              <=> "pre-cmd";
  PostCmd             <=> "post-cmd";
  PreChangeDir        <=> "pre-change-dir";
  PostChangeDir       <=> "post-change-dir";
  OnJobFinish         <=> "on-job-finish";
  PrePrompt           <=> "pre-prompt";
  PostPrompt          <=> "post-prompt";
  PreModeChange       <=> "pre-mode-change";
  PostModeChange      <=> "post-mode-change";
  OnHistoryOpen       <=> "on-history-open";
  OnHistoryClose      <=> "on-history-close";
  OnHistorySelect     <=> "on-history-select";
  OnCompletionStart   <=> "on-completion-start";
  OnCompletionCancel  <=> "on-completion-cancel";
  OnCompletionSelect  <=> "on-completion-select";
  OnScreensaverExec   <=> "on-screensaver-exec";
  OnScreensaverReturn <=> "on-screensaver-return";
  OnExit              <=> "on-exit";
);

#[derive(Clone, Debug)]
pub struct AutoCmd {
  pub pattern: Option<Regex>,
  pub kind: AutoCmdKind,
  pub command: String,
}

/// The logic table for the shell
///
/// Contains aliases and functions
#[derive(Default, Clone, Debug)]
pub struct LogTab {
  functions: HashMap<String, ShFunc>,
  aliases: HashMap<String, ShAlias>,
  traps: HashMap<TrapTarget, String>,
  keymaps: Vec<KeyMap>,
  autocmds: HashMap<AutoCmdKind, Vec<AutoCmd>>,
}

impl LogTab {
  pub fn new() -> Self {
    Self::default()
  }
  pub fn autocmds(&self) -> &HashMap<AutoCmdKind, Vec<AutoCmd>> {
    &self.autocmds
  }
  pub fn autocmds_mut(&mut self) -> &mut HashMap<AutoCmdKind, Vec<AutoCmd>> {
    &mut self.autocmds
  }
  pub fn insert_autocmd(&mut self, cmd: AutoCmd) {
    self.autocmds.entry(cmd.kind).or_default().push(cmd);
  }
  pub fn get_autocmds(&self, kind: AutoCmdKind) -> Vec<AutoCmd> {
    write_meta(|m| m.notify_autocmd(kind)).ok();
    self.autocmds.get(&kind).cloned().unwrap_or_default()
  }
  pub fn clear_autocmds(&mut self, kind: AutoCmdKind) {
    self.autocmds.remove(&kind);
  }
  pub fn keymaps(&self) -> &Vec<KeyMap> {
    &self.keymaps
  }
  pub fn keymaps_mut(&mut self) -> &mut Vec<KeyMap> {
    &mut self.keymaps
  }
  pub fn insert_keymap(&mut self, keymap: KeyMap) {
    let mut found_dup = false;
    for map in self.keymaps.iter_mut() {
      if map.keys == keymap.keys {
        *map = keymap.clone();
        found_dup = true;
        break;
      }
    }
    if !found_dup {
      self.keymaps.push(keymap);
    }
  }
  pub fn remove_keymap(&mut self, keys: &str) {
    self.keymaps.retain(|km| km.keys != keys);
  }
  pub fn keymaps_filtered(&self, flags: KeyMapFlags, pending: &[KeyEvent]) -> Vec<KeyMap> {
    self
      .keymaps
      .iter()
      .filter(|km| km.flags.intersects(flags) && km.compare(pending) != KeyMapMatch::NoMatch)
      .cloned()
      .collect()
  }
  pub fn insert_func(&mut self, name: &str, src: ShFunc) {
    self.functions.insert(name.into(), src);
  }
  pub fn insert_trap(&mut self, target: TrapTarget, command: String) {
    self.traps.insert(target, command);
  }
  pub fn get_trap(&self, target: TrapTarget) -> Option<String> {
    self.traps.get(&target).cloned()
  }
  pub fn remove_trap(&mut self, target: TrapTarget) {
    self.traps.remove(&target);
  }
  pub fn traps(&self) -> &HashMap<TrapTarget, String> {
    &self.traps
  }
  pub fn get_func(&self, name: &str) -> Option<ShFunc> {
    self.functions.get(name).cloned()
  }
  pub fn funcs(&self) -> &HashMap<String, ShFunc> {
    &self.functions
  }
  pub fn aliases(&self) -> &HashMap<String, ShAlias> {
    &self.aliases
  }
  pub fn insert_alias(&mut self, name: &str, body: &str, source: Span) {
    self.aliases.insert(
      name.into(),
      ShAlias {
        body: body.into(),
        source,
      },
    );
  }
  pub fn get_alias(&self, name: &str) -> Option<ShAlias> {
    self.aliases.get(name).cloned()
  }
  pub fn remove_alias(&mut self, name: &str) {
    self.aliases.remove(name);
  }
  pub fn clear_aliases(&mut self) {
    self.aliases.clear()
  }
  pub fn clear_functions(&mut self) {
    self.functions.clear()
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct VarFlags(u8);

impl VarFlags {
  pub const NONE: Self = Self(0);
  pub const EXPORT: Self = Self(1 << 0);
  pub const LOCAL: Self = Self(1 << 1);
  pub const READONLY: Self = Self(1 << 2);
}

impl BitOr for VarFlags {
  type Output = Self;
  fn bitor(self, rhs: Self) -> Self::Output {
    Self(self.0 | rhs.0)
  }
}

impl BitOrAssign for VarFlags {
  fn bitor_assign(&mut self, rhs: Self) {
    self.0 |= rhs.0;
  }
}

impl BitAnd for VarFlags {
  type Output = Self;
  fn bitand(self, rhs: Self) -> Self::Output {
    Self(self.0 & rhs.0)
  }
}

impl BitAndAssign for VarFlags {
  fn bitand_assign(&mut self, rhs: Self) {
    self.0 &= rhs.0;
  }
}

impl VarFlags {
  pub fn contains(&self, flag: Self) -> bool {
    (self.0 & flag.0) == flag.0
  }
  pub fn intersects(&self, flag: Self) -> bool {
    (self.0 & flag.0) != 0
  }
  pub fn is_empty(&self) -> bool {
    self.0 == 0
  }

  pub fn insert(&mut self, flag: Self) {
    self.0 |= flag.0;
  }
  pub fn remove(&mut self, flag: Self) {
    self.0 &= !flag.0;
  }
  pub fn toggle(&mut self, flag: Self) {
    self.0 ^= flag.0;
  }
  pub fn set(&mut self, flag: Self, value: bool) {
    if value {
      self.insert(flag);
    } else {
      self.remove(flag);
    }
  }
}

#[derive(Clone, Debug)]
pub enum ArrIndex {
  Literal(usize),
  FromBack(usize),
	Slice(usize, Option<usize>),
  ArgCount,
  AllJoined,
  AllSplit,
}

impl FromStr for ArrIndex {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "@" => Ok(Self::AllSplit),
      "*" => Ok(Self::AllJoined),
      "#" => Ok(Self::ArgCount),
      _ if s.starts_with('-') && s[1..].chars().all(|c| c.is_digit(1)) => {
        let idx = s[1..].parse::<usize>().unwrap();
        Ok(Self::FromBack(idx))
      }
      _ if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => {
        let idx = s.parse::<usize>().unwrap();
        Ok(Self::Literal(idx))
      }
      _ => Err(ShErr::simple(
        ShErrKind::ParseErr,
        format!("Invalid array index: {}", s),
      )),
    }
  }
}

#[derive(Clone, Debug)]
pub enum VarKind {
  Str(String),
  Int(i32),
  Arr(VecDeque<String>),
  AssocArr(Vec<(String, String)>),
}

impl VarKind {
  pub fn arr_from_tk(tk: Tk) -> ShResult<Self> {
    let raw = tk.as_str();
    if !raw.starts_with('(') || !raw.ends_with(')') {
      return Err(ShErr::simple(
        ShErrKind::ParseErr,
        format!("Invalid array syntax: {}", raw),
      ));
    }
    let raw = raw[1..raw.len() - 1].to_string();

    let tokens: VecDeque<String> = LexStream::new(Arc::new(raw), LexFlags::empty())
      .map(|tk| tk.and_then(|tk| tk.expand()).map(|tk| tk.get_words()))
      .try_fold(vec![], |mut acc, wrds| {
        match wrds {
          Ok(wrds) => acc.extend(wrds),
          Err(e) => return Err(e),
        }
        Ok(acc)
      })?
      .into_iter()
      .collect();

    Ok(Self::Arr(tokens))
  }

  pub fn arr_from_vec(vec: Vec<String>) -> Self {
    Self::Arr(VecDeque::from(vec))
  }
}

impl Display for VarKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      VarKind::Str(s) => write!(f, "{s}"),
      VarKind::Int(i) => write!(f, "{i}"),
      VarKind::Arr(items) => {
        let mut item_iter = items.iter().peekable();
        while let Some(item) = item_iter.next() {
          write!(f, "{item}")?;
          if item_iter.peek().is_some() {
            write!(f, " ")?;
          }
        }
        Ok(())
      }
      VarKind::AssocArr(items) => {
        let mut item_iter = items.iter().peekable();
        while let Some(item) = item_iter.next() {
          let (k, v) = item;
          write!(f, "{k}={v}")?;
          if item_iter.peek().is_some() {
            write!(f, " ")?;
          }
        }
        Ok(())
      }
    }
  }
}

#[derive(Clone, Debug)]
pub struct Var {
  flags: VarFlags,
  kind: VarKind,
}

impl Var {
  pub fn new(kind: VarKind, flags: VarFlags) -> Self {
    Self { flags, kind }
  }
  pub fn kind(&self) -> &VarKind {
    &self.kind
  }
  pub fn kind_mut(&mut self) -> &mut VarKind {
    &mut self.kind
  }
  pub fn mark_for_export(&mut self) {
    self.flags.set(VarFlags::EXPORT, true);
  }
  pub fn flags(&self) -> VarFlags {
    self.flags
  }
  pub fn as_shell_arg(&self) -> String {
    match &self.kind {
      VarKind::Arr(_) => format!("( {} )", self),
      _ => self.to_string(),
    }
  }
}

impl Display for Var {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.kind.fmt(f)
  }
}

impl From<Vec<String>> for Var {
  fn from(value: Vec<String>) -> Self {
    Self::new(VarKind::Arr(value.into()), VarFlags::NONE)
  }
}

impl From<Vec<Candidate>> for Var {
  fn from(value: Vec<Candidate>) -> Self {
    let as_strs = value
      .into_iter()
      .map(|c| c.content().to_string())
      .collect::<Vec<_>>();
    Self::new(VarKind::Arr(as_strs.into()), VarFlags::NONE)
  }
}

impl From<&[String]> for Var {
  fn from(value: &[String]) -> Self {
    let mut new = VecDeque::new();
    new.extend(value.iter().cloned());
    Self::new(VarKind::Arr(new), VarFlags::NONE)
  }
}

macro_rules! impl_var_from {
    ($($t:ty),*) => {
			$(impl From<$t> for Var {
				fn from(value: $t) -> Self {
					Self::new(VarKind::Str(value.to_string()), VarFlags::NONE)
				}
			})*
    };
}

impl_var_from!(
  i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, String, &str, bool
);

#[derive(Default, Clone, Debug)]
pub struct VarTab {
  vars: HashMap<String, Var>,
  params: HashMap<ShellParam, String>,
  sh_argv: VecDeque<String>, /* Using a VecDeque makes the implementation of `shift` straightforward */

  maps: HashMap<String, MapNode>,
}

impl VarTab {
  pub fn bare() -> Self {
    Self {
      vars: HashMap::new(),
      params: HashMap::new(),
      sh_argv: VecDeque::new(),
      maps: HashMap::new(),
    }
  }
  pub fn new() -> Self {
    let vars = Self::init_sh_vars();
    let params = Self::init_params();
    Self::init_env();
    let mut var_tab = Self {
      vars,
      params,
      sh_argv: VecDeque::new(),
      maps: HashMap::new(),
    };
    var_tab.init_sh_argv();
    var_tab
  }
  fn init_params() -> HashMap<ShellParam, String> {
    let mut params = HashMap::new();
    params.insert(ShellParam::ArgCount, "0".into()); // Number of positional parameters
    params.insert(ShellParam::ShPid, Pid::this().to_string()); // PID of the shell
    params.insert(ShellParam::LastJob, "".into()); // PID of the last background job (if any)
    params
  }
  fn init_sh_vars() -> HashMap<String, Var> {
    let mut vars = HashMap::new();
    vars.insert("COMP_WORDBREAKS".into(), " \t\n\"'@><=;|&(".into());
    vars
  }
  fn init_env() {
    let pathbuf_to_string =
      |pb: Result<PathBuf, std::io::Error>| pb.unwrap_or_default().to_string_lossy().to_string();
    // First, inherit any env vars from the parent process
    let term = {
      if isatty(1).unwrap() {
        if let Ok(term) = std::env::var("TERM") {
          term
        } else {
          "linux".to_string()
        }
      } else {
        "xterm-256color".to_string()
      }
    };
    let home;
    let username;
    let uid;
    if let Some(user) = User::from_uid(nix::unistd::Uid::current()).ok().flatten() {
      home = user.dir;
      username = user.name;
      uid = user.uid;
    } else {
      home = PathBuf::new();
      username = "unknown".into();
      uid = 0.into();
    }
    let home = pathbuf_to_string(Ok(home));
    let hostname = gethostname()
      .map(|hname| hname.to_string_lossy().to_string())
      .unwrap_or_default();

    let help_paths = format!("/usr/share/shed/doc:{home}/.local/share/shed/doc");

    unsafe {
      env::set_var("IFS", " \t\n");
      env::set_var("HOST", hostname.clone());
      env::set_var("UID", uid.to_string());
      env::set_var("PPID", getppid().to_string());
      env::set_var("TMPDIR", "/tmp");
      env::set_var("TERM", term);
      env::set_var("LANG", "en_US.UTF-8");
      env::set_var("USER", username.clone());
      env::set_var("LOGNAME", username);
      env::set_var("PWD", pathbuf_to_string(std::env::current_dir()));
      env::set_var("OLDPWD", pathbuf_to_string(std::env::current_dir()));
      env::set_var("HOME", home.clone());
      env::set_var("SHELL", pathbuf_to_string(std::env::current_exe()));
      env::set_var("SHED_HIST", format!("{}/.shedhist", home));
      env::set_var("SHED_RC", format!("{}/.shedrc", home));
      env::set_var("SHED_HPATH", help_paths);
    }
  }
  pub fn init_sh_argv(&mut self) {
    for arg in env::args() {
      self.bpush_arg(arg);
    }
  }
  pub fn update_exports(&mut self) {
    for var_name in self.vars.keys() {
      let var = self.vars.get(var_name).unwrap();
      if var.flags.contains(VarFlags::EXPORT) {
        unsafe { env::set_var(var_name, var.to_string()) };
      } else {
        unsafe { env::set_var(var_name, "") };
      }
    }
  }
  pub fn sh_argv(&self) -> &VecDeque<String> {
    &self.sh_argv
  }
  pub fn sh_argv_mut(&mut self) -> &mut VecDeque<String> {
    &mut self.sh_argv
  }
  pub fn clear_args(&mut self) {
    let first = self.sh_argv.pop_front();
    self.sh_argv.clear();

    // preserve the first arg, which is conventionally the name of the shell, script, or function
    if let Some(arg) = first {
      self.bpush_arg(arg);
    }
  }
  fn update_arg_params(&mut self) {
    self.set_param(
      ShellParam::AllArgs,
      &self.sh_argv.clone().to_vec()[1..].join(&markers::ARG_SEP.to_string()),
    );
    self.set_param(ShellParam::ArgCount, &(self.sh_argv.len() - 1).to_string());
  }
  /// Push an arg to the front of the arg deque
  pub fn fpush_arg(&mut self, arg: String) {
    self.sh_argv.push_front(arg);
    self.update_arg_params();
  }
  /// Push an arg to the back of the arg deque
  pub fn bpush_arg(&mut self, arg: String) {
    self.sh_argv.push_back(arg);
    self.update_arg_params();
  }
  /// Pop an arg from the front of the arg deque
  pub fn fpop_arg(&mut self) -> Option<String> {
    let arg = self.sh_argv.pop_front();
    self.update_arg_params();
    arg
  }
  /// Pop an arg from the back of the arg deque
  pub fn bpop_arg(&mut self) -> Option<String> {
    let arg = self.sh_argv.pop_back();
    self.update_arg_params();
    arg
  }
  pub fn set_map(&mut self, map_name: &str, map: MapNode) {
    self.maps.insert(map_name.to_string(), map);
  }
  pub fn remove_map(&mut self, map_name: &str) -> Option<MapNode> {
    self.maps.remove(map_name)
  }
  pub fn get_map(&self, map_name: &str) -> Option<&MapNode> {
    self.maps.get(map_name)
  }
  pub fn get_map_mut(&mut self, map_name: &str) -> Option<&mut MapNode> {
    self.maps.get_mut(map_name)
  }
  pub fn maps(&self) -> &HashMap<String, MapNode> {
    &self.maps
  }
  pub fn maps_mut(&mut self) -> &mut HashMap<String, MapNode> {
    &mut self.maps
  }
  pub fn vars(&self) -> &HashMap<String, Var> {
    &self.vars
  }
  pub fn vars_mut(&mut self) -> &mut HashMap<String, Var> {
    &mut self.vars
  }
  pub fn params(&self) -> &HashMap<ShellParam, String> {
    &self.params
  }
  pub fn params_mut(&mut self) -> &mut HashMap<ShellParam, String> {
    &mut self.params
  }
  pub fn export_var(&mut self, var_name: &str) {
    if let Some(var) = self.vars.get_mut(var_name) {
      var.mark_for_export();
      unsafe { env::set_var(var_name, var.to_string()) };
    }
  }
  pub fn get_var(&self, var: &str) -> String {
    if let Ok(param) = var.parse::<ShellParam>() {
      let param = self.get_param(param);
      if !param.is_empty() {
        return param;
      }
    }
    if let Some(var) = self.vars.get(var).map(|s| s.to_string()) {
      var
    } else {
      std::env::var(var).unwrap_or_default()
    }
  }
  pub fn get_var_flags(&self, var_name: &str) -> Option<VarFlags> {
    self.vars.get(var_name).map(|var| var.flags)
  }
  pub fn unset_var(&mut self, var_name: &str) -> ShResult<()> {
    if let Some(var) = self.vars.get(var_name)
      && var.flags.contains(VarFlags::READONLY)
    {
      return Err(ShErr::simple(
        ShErrKind::ExecFail,
        format!("cannot unset readonly variable '{}'", var_name),
      ));
    }
    self.vars.remove(var_name);
    unsafe { env::remove_var(var_name) };
    Ok(())
  }
  pub fn set_index(&mut self, var_name: &str, idx: ArrIndex, val: String) -> ShResult<()> {
    if self.var_exists(var_name)
      && let Some(var) = self.vars_mut().get_mut(var_name)
    {
      match var.kind_mut() {
        VarKind::Arr(items) => {
          let idx = match idx {
            ArrIndex::Literal(n) => n,
            ArrIndex::FromBack(n) => {
              if items.len() >= n {
                items.len() - n
              } else {
                return Err(ShErr::simple(
                  ShErrKind::ExecFail,
                  format!("Index {} out of bounds for array '{}'", n, var_name),
                ));
              }
            }
            _ => {
              return Err(ShErr::simple(
                ShErrKind::ExecFail,
                format!("Cannot index all elements of array '{}'", var_name),
              ));
            }
          };

          if idx >= items.len() {
            items.resize(idx + 1, String::new());
          }
          items[idx] = val;
          return Ok(());
        }
        _ => {
          return Err(ShErr::simple(
            ShErrKind::ExecFail,
            format!("Variable '{}' is not an array", var_name),
          ));
        }
      }
    }
    Ok(())
  }
  pub fn set_var(&mut self, var_name: &str, val: VarKind, flags: VarFlags) -> ShResult<()> {
    if let Some(var) = self.vars.get_mut(var_name) {
      if var.flags.contains(VarFlags::READONLY) && !flags.contains(VarFlags::READONLY) {
        return Err(ShErr::simple(
          ShErrKind::ExecFail,
          format!("Variable '{}' is readonly", var_name),
        ));
      }
      var.kind = val;
      var.flags |= flags;
      if var.flags.contains(VarFlags::EXPORT) || flags.contains(VarFlags::EXPORT) {
        if flags.contains(VarFlags::EXPORT) && !var.flags.contains(VarFlags::EXPORT) {
          var.mark_for_export();
        }
        unsafe { env::set_var(var_name, var.kind.to_string()) };
      }
    } else {
      let mut var = Var::new(val, flags);
      if flags.contains(VarFlags::EXPORT) {
        var.mark_for_export();
        unsafe { env::set_var(var_name, var.to_string()) };
      }
      self.vars.insert(var_name.to_string(), var);
    }
    Ok(())
  }
  pub fn map_exists(&self, map_name: &str) -> bool {
    self.maps.contains_key(map_name)
  }
  pub fn var_exists(&self, var_name: &str) -> bool {
    if let Ok(param) = var_name.parse::<ShellParam>() {
      return self.params.contains_key(&param);
    }
    self.vars.contains_key(var_name)
  }
  pub fn set_param(&mut self, param: ShellParam, val: &str) {
    self.params.insert(param, val.to_string());
  }
  pub fn get_param(&self, param: ShellParam) -> String {
    match param {
      ShellParam::Pos(n) => self
        .sh_argv()
        .get(n)
        .map(|s| s.to_string())
        .unwrap_or_default(),
      ShellParam::Status => self
        .params
        .get(&ShellParam::Status)
        .map(|s| s.to_string())
        .unwrap_or("0".into()),
      ShellParam::AllArgsStr => {
        let ifs = get_separator();
        self
          .params
          .get(&ShellParam::AllArgs)
          .map(|s| s.replace(markers::ARG_SEP, &ifs).to_string())
          .unwrap_or_default()
      }

      _ => self
        .params
        .get(&param)
        .map(|s| s.to_string())
        .unwrap_or_default(),
    }
  }
}

#[derive(Debug)]
pub enum StatusHeader {
  ExitCode,
  CommandName,
  Runtime,
  Pid,
  Pgid,
}

#[derive(Debug)]
pub enum QueryHeader {
  Cwd,
  Var(String),
  Status(Vec<StatusHeader>),
  Jobs,
}

#[derive(Debug)]
pub enum SocketRequest {
  /// Posts a system message. System messages appear above the prompt, the same way that job status notifications do.
  /// Useful for important information.
  PostSystemMessage(String),
  /// Posts a status message. Status messages appear under the prompt, and are short lived. Will only survive redraws for a few seconds.
  /// Useful for quick notifications.
  PostStatusMessage(String),

  /// Requests information from the shell. The shell will respond with a SocketResponse containing the requested information, or an error if the query was invalid.
  Query(QueryHeader),

  /// Opens a subscription to the shell's event stream. The shell will send a SocketResponse for each event that occurs, until the socket or connnection is closed.
  Subscribe,

  /// Requests the shell to redraw the prompt. The shell will respond by redrawing the prompt, and sending a SocketResponse confirming the redraw.
  RefreshPrompt,
}

impl FromStr for SocketRequest {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let request_kind = s
      .chars()
      .peeking_take_while(|c| c.is_ascii_alphabetic())
      .collect::<String>()
      .to_lowercase();

    // take care of no-argument requests
    match request_kind.trim() {
      "subscribe" => return Ok(Self::Subscribe),
      "redraw" => return Ok(Self::RefreshPrompt),
      _ => {}
    }

    let rest = s[request_kind.len()..].trim();
    let mut sep = String::new();
    let mut rest_chars = rest.chars().peekable();

    // collect the separator
    while let Some(ch) = rest_chars.peek() {
      if !ch.is_ascii_alphanumeric() && ch.is_ascii_graphic() {
        sep.push(*ch);
        rest_chars.next();
      } else {
        break;
      }
    }
    let rest = rest_chars.collect::<String>();
    let mut args = rest.split(&sep);

    match request_kind.trim() {
      "msg" => {
        let Some(msg_kind) = args.next() else {
          return Err(ShErr::simple(
            ShErrKind::ParseErr,
            "Missing message kind in 'msg' request",
          ));
        };
        match msg_kind.to_lowercase().as_str() {
          "system" => {
            let Some(msg) = args.next() else {
              return Err(ShErr::simple(
                ShErrKind::ParseErr,
                "Missing message in system msg request",
              ));
            };
            Ok(Self::PostSystemMessage(msg.to_string()))
          }
          "status" => {
            let Some(msg) = args.next() else {
              return Err(ShErr::simple(
                ShErrKind::ParseErr,
                "Missing message in status msg request",
              ));
            };
            Ok(Self::PostStatusMessage(msg.to_string()))
          }
          _ => Err(ShErr::simple(
            ShErrKind::ParseErr,
            format!("Unknown message kind in 'msg' request: {}", msg_kind),
          )),
        }
      }

      "query" => {
        let Some(query_kind) = args.next() else {
          return Err(ShErr::simple(
            ShErrKind::ParseErr,
            "Missing query kind in 'query' request",
          ));
        };
        match query_kind.to_lowercase().as_str() {
          "cwd" => Ok(Self::Query(QueryHeader::Cwd)),
          "jobs" => Ok(Self::Query(QueryHeader::Jobs)),
          "status" => {
            let mut headers = vec![];
            while let Some(header) = args.next() {
              let status_header = match header.to_lowercase().as_str() {
                "code" => StatusHeader::ExitCode,
                "command" => StatusHeader::CommandName,
                "runtime" => StatusHeader::Runtime,
                "pid" => StatusHeader::Pid,
                "pgid" => StatusHeader::Pgid,
                _ => {
                  return Err(ShErr::simple(
                    ShErrKind::ParseErr,
                    format!(
                      "Unknown status header in 'query status' request: {}",
                      header
                    ),
                  ));
                }
              };
              headers.push(status_header);
            }
            if headers.is_empty() {
              headers = vec![
                StatusHeader::ExitCode,
                StatusHeader::CommandName,
                StatusHeader::Runtime,
                StatusHeader::Pid,
                StatusHeader::Pgid,
              ];
            }
            Ok(Self::Query(QueryHeader::Status(headers)))
          }
          "var" => {
            let Some(var_name) = args.next() else {
              return Err(ShErr::simple(
                ShErrKind::ParseErr,
                "Missing variable name in 'query var' request",
              ));
            };
            Ok(Self::Query(QueryHeader::Var(var_name.to_string())))
          }
          _ => Err(ShErr::simple(
            ShErrKind::ParseErr,
            format!("Unknown query kind in 'query' request: {}", query_kind),
          )),
        }
      }
      _ => Err(ShErr::simple(
        ShErrKind::ParseErr,
        format!("Unknown socket request kind: {}", request_kind),
      )),
    }
  }
}

/// The socket used to expose the system/status message interface
#[derive(Debug)]
pub struct ShedSocket {
  listener: UnixListener,
  pid: Pid,
  path: PathBuf,
}

impl ShedSocket {
  pub fn new() -> ShResult<Self> {
    let pid = Pid::this();
    let runtime_dir = env::var("XDG_RUNTIME_DIR")
      .unwrap_or_else(|_| format!("/tmp/shed-{}", nix::unistd::getuid()));

    std::fs::create_dir_all(format!("{runtime_dir}/shed"))?;
    let sock_path = format!("{runtime_dir}/shed/{pid}.sock");
    std::fs::remove_file(&sock_path).ok();
    let listener = UnixListener::bind(&sock_path)?;

    let raw_fd = listener.into_raw_fd();
    let high_fd = fcntl(raw_fd, FcntlArg::F_DUPFD_CLOEXEC(10))?;
    close(raw_fd)?;

    let listener = unsafe { UnixListener::from_raw_fd(high_fd) };
    listener.set_nonblocking(true).ok();

    write_vars(|v| {
      v.set_var(
        "SHED_SOCK",
        VarKind::Str(sock_path.clone()),
        VarFlags::EXPORT,
      )
    })
    .ok();
    Ok(Self {
      listener,
      pid,
      path: PathBuf::from(sock_path),
    })
  }
  pub fn listener(&self) -> &UnixListener {
    &self.listener
  }
  pub fn as_raw_fd(&self) -> RawFd {
    self.listener.as_raw_fd()
  }
}

impl Drop for ShedSocket {
  fn drop(&mut self) {
    if Pid::this() == self.pid {
      std::fs::remove_file(&self.path).ok();
    }
  }
}

/// A table of metadata for the shell
#[derive(Clone, Debug)]
pub struct MetaTab {
  // Time when the shell was started, used for calculating shell uptime
  shell_time: Instant,

  // command running duration
  runtime_start: Option<Instant>,
  runtime_stop: Option<Instant>,

  socket: Option<Arc<ShedSocket>>,
  subscribers: Vec<Arc<UnixStream>>,
  last_job: Option<Job>,

  // pending system messages
  // are drawn above the prompt and survive redraws
  system_msg: VecDeque<String>,

  // same as system messages,
  // but they appear under the prompt and are erased on redraw
  status_msg: VecDeque<String>,

  // pushd/popd stack
  dir_stack: VecDeque<PathBuf>,
  // getopts char offset for opts like -abc
  getopts_offset: usize,

  old_path: Option<String>,
  old_pwd: Option<String>,
  // valid command cache
  path_cache: HashSet<String>,
  cwd_cache: HashSet<String>,
  // programmable completion specs
  comp_specs: HashMap<String, Box<dyn CompSpec>>,

  // pending keys from widget function
  pending_widget_keys: Vec<KeyEvent>,
}

impl Default for MetaTab {
  fn default() -> Self {
    Self {
      shell_time: Instant::now(),
      runtime_start: None,
      runtime_stop: None,
      socket: None,
      subscribers: vec![],
      last_job: None,
      system_msg: VecDeque::new(),
      status_msg: VecDeque::new(),
      dir_stack: VecDeque::new(),
      getopts_offset: 0,
      old_path: None,
      old_pwd: None,
      path_cache: HashSet::new(),
      cwd_cache: HashSet::new(),
      comp_specs: HashMap::new(),
      pending_widget_keys: vec![],
    }
  }
}

impl MetaTab {
  pub fn new() -> Self {
    Self {
      comp_specs: Self::get_builtin_comp_specs(),
      ..Default::default()
    }
  }
  pub fn shell_time(&self) -> Instant {
    self.shell_time
  }
  pub fn set_pending_widget_keys(&mut self, keys: &str) {
    let exp = expand_keymap(keys);
    self.pending_widget_keys = exp;
  }
  pub fn take_pending_widget_keys(&mut self) -> Option<Vec<KeyEvent>> {
    if self.pending_widget_keys.is_empty() {
      None
    } else {
      Some(std::mem::take(&mut self.pending_widget_keys))
    }
  }
  pub fn set_last_job(&mut self, job: Option<Job>) {
    self.last_job = job;
  }
  pub fn last_job(&self) -> Option<&Job> {
    self.last_job.as_ref()
  }
  pub fn getopts_char_offset(&self) -> usize {
    self.getopts_offset
  }
  pub fn inc_getopts_char_offset(&mut self) -> usize {
    let offset = self.getopts_offset;
    self.getopts_offset += 1;
    offset
  }
  pub fn reset_getopts_char_offset(&mut self) {
    self.getopts_offset = 0;
  }
  pub fn get_builtin_comp_specs() -> HashMap<String, Box<dyn CompSpec>> {
    let mut map = HashMap::new();

    map.insert(
      "cd".into(),
      Box::new(BashCompSpec::new().dirs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "pushd".into(),
      Box::new(BashCompSpec::new().dirs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "popd".into(),
      Box::new(BashCompSpec::new().dirs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "source".into(),
      Box::new(BashCompSpec::new().files(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "bg".into(),
      Box::new(BashCompSpec::new().jobs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "fg".into(),
      Box::new(BashCompSpec::new().jobs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "disown".into(),
      Box::new(BashCompSpec::new().jobs(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "alias".into(),
      Box::new(BashCompSpec::new().aliases(true)) as Box<dyn CompSpec>,
    );
    map.insert(
      "trap".into(),
      Box::new(BashCompSpec::new().signals(true)) as Box<dyn CompSpec>,
    );

    map
  }
  pub fn cached_cmds(&self) -> &HashSet<String> {
    &self.path_cache
  }
  pub fn cwd_cache(&self) -> &HashSet<String> {
    &self.cwd_cache
  }
  pub fn comp_specs(&self) -> &HashMap<String, Box<dyn CompSpec>> {
    &self.comp_specs
  }
  pub fn comp_specs_mut(&mut self) -> &mut HashMap<String, Box<dyn CompSpec>> {
    &mut self.comp_specs
  }
  pub fn get_comp_spec(&self, cmd: &str) -> Option<Box<dyn CompSpec>> {
    self.comp_specs.get(cmd).cloned()
  }
  pub fn set_comp_spec(&mut self, cmd: String, spec: Box<dyn CompSpec>) {
    self.comp_specs.insert(cmd, spec);
  }
  pub fn remove_comp_spec(&mut self, cmd: &str) -> bool {
    self.comp_specs.remove(cmd).is_some()
  }
  pub fn get_cmds_in_path() -> Vec<String> {
    let path = env::var("PATH").unwrap_or_default();
    let paths = path.split(":").map(PathBuf::from);
    let mut cmds = vec![];
    for path in paths {
      if let Ok(entries) = path.read_dir() {
        for entry in entries.flatten() {
          let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
          };
          let is_exec = meta.permissions().mode() & 0o111 != 0;

          if meta.is_file()
            && is_exec
            && let Some(name) = entry.file_name().to_str()
          {
            cmds.push(name.to_string());
          }
        }
      }
    }
    cmds
  }
  pub fn create_socket(&mut self) -> ShResult<()> {
    let sock = ShedSocket::new()?;
    self.socket = Some(sock.into());
    Ok(())
  }
  pub fn get_socket(&self) -> Option<Arc<ShedSocket>> {
    self.socket.as_ref().cloned()
  }
  pub fn read_socket(&mut self) -> ShResult<()> {
    if let Some(sock) = &self.socket
      && let Ok((conn, _)) = sock.listener().accept()
    {
      conn.set_nonblocking(false).ok();
      let mut bytes = vec![];
      loop {
        let mut buffer = [0u8; 1024];
        match read(conn.as_raw_fd(), &mut buffer) {
          Ok(0) => break,
          Ok(n) => {
            if let Some(pos) = buffer[..n].iter().position(|&b| b == b'\n') {
              bytes.extend_from_slice(&buffer[..pos]);
              break;
            }
            bytes.extend_from_slice(&buffer[..n]);
          }
          Err(Errno::EINTR) => continue,
          Err(e) => {
            eprintln!("error reading from message socket: {e}");
            break;
          }
        }
      }
      let input = String::from_utf8_lossy(&bytes).to_string();
      let request = match SocketRequest::from_str(&input) {
        Ok(req) => req,
        Err(e) => {
          write(&conn, format!("error parsing request: {e}\n").as_bytes()).ok();
          return Ok(());
        }
      };

      self.handle_socket_request(conn, request)?;
    }

    Ok(())
  }
  pub fn handle_socket_request(
    &mut self,
    conn: UnixStream,
    request: SocketRequest,
  ) -> ShResult<()> {
    match request {
      SocketRequest::PostSystemMessage(msg) => {
        log::debug!("Posting system message: {}", msg);
        self.post_system_message(msg);
        write(&conn, b"ok\n").ok();
      }
      SocketRequest::PostStatusMessage(msg) => {
        log::debug!("Posting status message: {}", msg);
        self.post_status_message(msg);
        write(&conn, b"ok\n").ok();
      }
      SocketRequest::Subscribe => {
        log::debug!("New subscriber to event stream");
        let conn = Arc::new(conn);
        self.subscribers.push(conn.clone());
      }
      SocketRequest::Query(query_header) => {
        log::debug!("Received query: {:?}", query_header);
        match query_header {
          QueryHeader::Cwd => {
            let cwd = env::current_dir()?.to_string_lossy().to_string();
            write(&conn, cwd.as_bytes()).ok();
            write(&conn, b"\n").ok();
          }
          QueryHeader::Var(var) => {
            let var = read_vars(|v| v.get_var(&var));
            write(&conn, var.as_bytes()).ok();
            write(&conn, b"\n").ok();
          }
          QueryHeader::Status(headers) => {
            let mut responses = vec![];
            for header in headers {
              match header {
                StatusHeader::ExitCode => responses.push(get_status().to_string()),
                StatusHeader::CommandName => {
                  if let Some(job) = self.last_job()
                    && let Some(cmd) = job.name()
                  {
                    responses.push(cmd.to_string());
                  } else {
                    responses.push("".to_string());
                  }
                }
                StatusHeader::Runtime => {
                  let Some(dur) = self.get_time() else {
                    responses.push("".to_string());
                    continue;
                  };
                  responses.push(format!("{}", dur.as_millis()));
                }
                StatusHeader::Pid => {
                  let Some(job) = self.last_job() else {
                    responses.push("".to_string());
                    continue;
                  };
                  responses.push(
                    job
                      .get_pids()
                      .first()
                      .map(|p| p.to_string())
                      .unwrap_or_default(),
                  );
                }
                StatusHeader::Pgid => {
                  let Some(job) = self.last_job() else {
                    responses.push("".to_string());
                    continue;
                  };
                  responses.push(job.pgid().to_string());
                }
              }
            }
            let output = responses.join(" ");
            write(&conn, output.as_bytes()).ok();
            write(&conn, b"\n").ok();
          }
          QueryHeader::Jobs => todo!(),
        }
      }
      SocketRequest::RefreshPrompt => {
        log::debug!("Received prompt refresh request");
        kill(Pid::this(), Signal::SIGUSR1)?;
        write(&conn, b"ok\n").ok();
      }
    }
    Ok(())
  }
  pub fn notify_autocmd(&self, kind: AutoCmdKind) -> ShResult<()> {
    for subscriber in &self.subscribers {
      write(subscriber, format!("autocmd_event>> {kind}\n").as_bytes()).ok();
    }

    Ok(())
  }
  pub fn rehash_commands(&mut self) {
    let path = env::var("PATH").unwrap_or_default();
    let cwd = env::var("PWD").unwrap_or_default();
    log::trace!("Rehashing commands for PATH: '{}' and PWD: '{}'", path, cwd);

    self.path_cache.clear();
    self.old_path = Some(path.clone());
    self.old_pwd = Some(cwd.clone());
    let cmds_in_path = Self::get_cmds_in_path();
    for cmd in cmds_in_path {
      self.path_cache.insert(cmd);
    }
    if let Ok(entries) = Path::new(&cwd).read_dir() {
      for entry in entries.flatten() {
        let Ok(meta) = std::fs::metadata(entry.path()) else {
          continue;
        };
        let is_exec = meta.permissions().mode() & 0o111 != 0;

        if meta.is_file()
          && is_exec
          && let Some(name) = entry.file_name().to_str()
        {
          self.path_cache.insert(format!("./{}", name));
        }
      }
    }

    read_logic(|l| {
      let funcs = l.funcs();
      let aliases = l.aliases();
      for func in funcs.keys() {
        self.path_cache.insert(func.clone());
      }
      for alias in aliases.keys() {
        self.path_cache.insert(alias.clone());
      }
    });

    for cmd in BUILTINS {
      self.path_cache.insert(cmd.to_string());
    }
  }
  pub fn try_rehash_commands(&mut self) {
    let path = env::var("PATH").unwrap_or_default();
    let cwd = env::var("PWD").unwrap_or_default();
    if self.old_path.as_ref().is_some_and(|old| *old == path)
      && self.old_pwd.as_ref().is_some_and(|old| *old == cwd)
    {
      log::trace!("PATH and PWD unchanged, skipping rehash");
      return;
    }

    self.rehash_commands();
  }
  pub fn try_rehash_cwd_listing(&mut self) {
    let cwd = env::var("PWD").unwrap_or_default();
    if self.old_pwd.as_ref().is_some_and(|old| *old == cwd) {
      log::trace!("PWD unchanged, skipping rehash of cwd listing");
      return;
    }

    log::debug!("Rehashing cwd listing for PWD: '{}'", cwd);

    if let Ok(entries) = Path::new(&cwd).read_dir() {
      for entry in entries.flatten() {
        let Ok(meta) = std::fs::metadata(entry.path()) else {
          continue;
        };
        let is_exec = meta.permissions().mode() & 0o111 != 0;

        if meta.is_file()
          && is_exec
          && let Some(name) = entry.file_name().to_str()
        {
          self.cwd_cache.insert(name.to_string());
        }
      }
    }
  }
  pub fn start_timer(&mut self) {
    self.runtime_start = Some(Instant::now());
  }
  pub fn stop_timer(&mut self) {
    self.runtime_stop = Some(Instant::now());
  }
  pub fn get_time(&self) -> Option<Duration> {
    if let (Some(start), Some(stop)) = (self.runtime_start, self.runtime_stop) {
      Some(stop.duration_since(start))
    } else {
      None
    }
  }
  pub fn post_system_message(&mut self, message: String) {
    self.system_msg.push_back(message);
  }
  pub fn pop_system_message(&mut self) -> Option<String> {
    self.system_msg.pop_front()
  }
  pub fn system_msg_pending(&self) -> bool {
    !self.system_msg.is_empty()
  }
  pub fn post_status_message(&mut self, message: String) {
    self.status_msg.push_back(message);
  }
  pub fn pop_status_message(&mut self) -> Option<String> {
    self.status_msg.pop_front()
  }
  pub fn status_msg_pending(&self) -> bool {
    !self.status_msg.is_empty()
  }
  pub fn dir_stack_top(&self) -> Option<&PathBuf> {
    self.dir_stack.front()
  }
  pub fn push_dir(&mut self, path: PathBuf) {
    self.dir_stack.push_front(path);
  }
  pub fn pop_dir(&mut self) -> Option<PathBuf> {
    self.dir_stack.pop_front()
  }
  pub fn remove_dir(&mut self, idx: i32) -> Option<PathBuf> {
    if idx < 0 {
      let neg_idx = (self.dir_stack.len() - 1).saturating_sub((-idx) as usize);
      self.dir_stack.remove(neg_idx)
    } else {
      self.dir_stack.remove((idx - 1) as usize)
    }
  }
  pub fn rotate_dirs_fwd(&mut self, steps: usize) {
    self.dir_stack.rotate_left(steps);
  }
  pub fn rotate_dirs_bkwd(&mut self, steps: usize) {
    self.dir_stack.rotate_right(steps);
  }
  pub fn dirs(&self) -> &VecDeque<PathBuf> {
    &self.dir_stack
  }
  pub fn dirs_mut(&mut self) -> &mut VecDeque<PathBuf> {
    &mut self.dir_stack
  }
}

/// Read from the job table
pub fn read_jobs<T, F: FnOnce(&JobTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.jobs.borrow()))
}

/// Write to the job table
pub fn write_jobs<T, F: FnOnce(&mut JobTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.jobs.borrow_mut()))
}

/// Read from the var scope stack
pub fn read_vars<T, F: FnOnce(&ScopeStack) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.var_scopes.borrow()))
}

/// Write to the variable table
pub fn write_vars<T, F: FnOnce(&mut ScopeStack) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.var_scopes.borrow_mut()))
}

/// Parse `arr[idx]` into (name, raw_index_expr). Pure parsing, no expansion.
pub fn parse_arr_bracket(var_name: &str) -> Option<(String, String)> {
  let mut chars = var_name.chars();
  let mut name = String::new();
  let mut idx_raw = String::new();
  let mut bracket_depth = 0;

  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        chars.next();
      }
      '[' => {
        bracket_depth += 1;
        if bracket_depth > 1 {
          idx_raw.push(ch);
        }
      }
      ']' => {
        if bracket_depth > 0 {
          bracket_depth -= 1;
          if bracket_depth == 0 {
            if idx_raw.is_empty() {
              return None;
            }
            break;
          }
        }
        idx_raw.push(ch);
      }
      _ if bracket_depth > 0 => idx_raw.push(ch),
      _ => name.push(ch),
    }
  }

  if name.is_empty() || idx_raw.is_empty() {
    None
  } else {
    Some((name, idx_raw))
  }
}

/// Expand the raw index expression and parse it into an ArrIndex.
pub fn expand_arr_index(idx_raw: &str) -> ShResult<ArrIndex> {
  let expanded = LexStream::new(Arc::new(idx_raw.to_string()), LexFlags::empty())
    .map(|tk| tk.and_then(|tk| tk.expand()).map(|tk| tk.get_words()))
    .try_fold(vec![], |mut acc, wrds| {
      match wrds {
        Ok(wrds) => acc.extend(wrds),
        Err(e) => return Err(e),
      }
      Ok(acc)
    })?
    .into_iter()
    .next()
    .ok_or_else(|| ShErr::simple(ShErrKind::ParseErr, "Empty array index"))?;

  expanded.parse::<ArrIndex>().map_err(|_| {
    ShErr::simple(
      ShErrKind::ParseErr,
      format!("Invalid array index: {}", expanded),
    )
  })
}

pub fn read_meta<T, F: FnOnce(&MetaTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.meta.borrow()))
}

/// Write to the meta table
pub fn write_meta<T, F: FnOnce(&mut MetaTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.meta.borrow_mut()))
}

/// Read from the logic table
pub fn read_logic<T, F: FnOnce(&LogTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.logic.borrow()))
}

/// Write to the logic table
pub fn write_logic<T, F: FnOnce(&mut LogTab) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.logic.borrow_mut()))
}

pub fn read_shopts<T, F: FnOnce(&ShOpts) -> T>(f: F) -> T {
  SHED.with(|shed| f(&shed.shopts.borrow()))
}

pub fn write_shopts<T, F: FnOnce(&mut ShOpts) -> T>(f: F) -> T {
  SHED.with(|shed| f(&mut shed.shopts.borrow_mut()))
}

pub fn descend_scope(argv: Option<Vec<String>>) {
  write_vars(|v| v.descend(argv));
}
pub fn ascend_scope() {
  write_vars(|v| v.ascend());
}

/// This function is used internally and ideally never sees user input
///
/// It will panic if you give it an invalid path.
pub fn get_shopt(path: &str) -> String {
  read_shopts(|s| s.get(path)).unwrap().unwrap()
}

pub fn with_vars<F, H, V, T>(vars: H, f: F) -> T
where
  F: FnOnce() -> T,
  H: Into<HashMap<String, V>>,
  V: Into<Var>,
{
  let snapshot = read_vars(|v| v.clone());
  let vars = vars.into();
  for (name, val) in vars {
    let val = val.into();
    write_vars(|v| v.set_var(&name, val.kind, val.flags).unwrap());
  }
  let _guard = scopeguard::guard(snapshot, |snap| {
    write_vars(|v| *v = snap);
  });
  f()
}

pub fn change_dir<P: AsRef<Path>>(dir: P) -> ShResult<()> {
  let dir = dir.as_ref();
  let dir_raw = &dir.display().to_string();
  let pre_cd = read_logic(|l| l.get_autocmds(AutoCmdKind::PreChangeDir));
  let post_cd = read_logic(|l| l.get_autocmds(AutoCmdKind::PostChangeDir));

  let current_dir = env::current_dir()?.display().to_string();
  with_vars(
    [
      ("_NEW_DIR".into(), dir_raw.as_str()),
      ("_OLD_DIR".into(), current_dir.as_str()),
    ],
    || {
      for cmd in pre_cd {
        let AutoCmd {
          command,
          kind: _,
          pattern,
        } = cmd;
        if let Some(pat) = pattern
          && !pat.is_match(dir_raw)
        {
          continue;
        }

        if let Err(e) = exec_input(
          command.clone(),
          None,
          false,
          Some("autocmd (pre-changedir)".to_string()),
        ) {
          e.print_error();
        };
      }
    },
  );

  env::set_current_dir(dir)?;

  with_vars(
    [
      ("_NEW_DIR".into(), dir_raw.as_str()),
      ("_OLD_DIR".into(), current_dir.as_str()),
    ],
    || {
      for cmd in post_cd {
        let AutoCmd {
          command,
          kind: _,
          pattern,
        } = cmd;
        if let Some(pat) = pattern
          && !pat.is_match(dir_raw)
        {
          continue;
        }

        if let Err(e) = exec_input(
          command.clone(),
          None,
          false,
          Some("autocmd (post-changedir)".to_string()),
        ) {
          e.print_error();
        };
      }
    },
  );

  Ok(())
}

pub fn get_separator() -> String {
  env::var("IFS")
    .unwrap_or(String::from(" "))
    .chars()
    .next()
    .unwrap()
    .to_string()
}

pub fn get_status() -> i32 {
  read_vars(|v| v.get_param(ShellParam::Status))
    .parse::<i32>()
    .unwrap()
}
pub fn set_status(code: i32) {
  write_vars(|v| v.set_param(ShellParam::Status, &code.to_string()))
}

pub fn source_runtime_file(name: &str, env_var_name: Option<&str>) -> ShResult<()> {
  let etc_path = PathBuf::from(format!("/etc/shed/{name}"));
  if etc_path.is_file()
    && let Err(e) = source_file(etc_path)
  {
    e.print_error();
  }

  let path = if let Some(name) = env_var_name
    && let Ok(path) = env::var(name)
  {
    PathBuf::from(&path)
  } else if let Some(home) = get_home() {
    home.join(format!(".{name}"))
  } else {
    return Err(ShErr::simple(
      ShErrKind::InternalErr,
      "could not determine home path",
    ));
  };
  if !path.is_file() {
    return Ok(());
  }
  source_file(path)
}

pub fn source_rc() -> ShResult<()> {
  source_runtime_file("shedrc", Some("SHED_RC"))
}

pub fn source_login() -> ShResult<()> {
  source_runtime_file("shed_profile", Some("SHED_PROFILE"))
}

pub fn source_env() -> ShResult<()> {
  source_runtime_file("shedenv", Some("SHED_ENV"))
}

pub fn source_file(path: PathBuf) -> ShResult<()> {
  let source_name = path.to_string_lossy().to_string();
  let mut file = OpenOptions::new().read(true).open(path)?;

  let mut buf = String::new();
  file.read_to_string(&mut buf)?;
  exec_input(buf, None, false, Some(source_name))?;
  Ok(())
}

#[track_caller]
pub fn get_home_unchecked() -> PathBuf {
  if let Some(home) = get_home() {
    home
  } else {
    let caller = std::panic::Location::caller();
    panic!(
      "get_home_unchecked: could not determine home directory (called from {}:{})",
      caller.file(),
      caller.line()
    )
  }
}

#[track_caller]
pub fn get_home_str_unchecked() -> String {
  if let Some(home) = get_home() {
    home.to_string_lossy().to_string()
  } else {
    let caller = std::panic::Location::caller();
    panic!(
      "get_home_str_unchecked: could not determine home directory (called from {}:{})",
      caller.file(),
      caller.line()
    )
  }
}

pub fn get_home() -> Option<PathBuf> {
  env::var("HOME")
    .ok()
    .map(PathBuf::from)
    .or_else(|| User::from_uid(getuid()).ok().flatten().map(|u| u.dir))
}

pub fn get_home_str() -> Option<String> {
  get_home().map(|h| h.to_string_lossy().to_string())
}
