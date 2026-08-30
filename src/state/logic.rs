use nix::sys::signal::Signal;

use std::fmt::{self, Display};
use std::rc::Rc;

use crate::util::error::LabelBuilder;
use crate::{
  HashMap, ShResult,
  autoload::{self, AutoloadSrc, Autoloader},
  eval::parse::ast::Ast,
  sherr,
  state::vars::VarStr,
};

use super::{
  eval::lex::Span,
  expand::shell_quote,
  keys::{KeyEvent, KeyMap, KeyMapFlags, KeyMapMatch},
  signal::parse_signal,
};

#[derive(Clone, Debug)]
pub(crate) struct ShAlias {
  body: VarStr,
  source: Span,
}

impl ShAlias {
  pub fn new(body: VarStr, source: Span) -> Self {
    Self { body, source }
  }
  pub fn body(&self) -> VarStr {
    self.body.clone()
  }
  pub fn source(&self) -> &Span {
    &self.source
  }
}

impl Display for ShAlias {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.body)
  }
}

impl super::vars::ValueBytes for ShAlias {
  fn value_bytes(&self) -> Vec<u8> {
    self.body.as_bytes().to_vec()
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IsInternal {
  Yes,
  No,
  Checking,
}

/// A shell function
///
/// Comes in two flavors:
/// * `Defined`, which holds the actual parsed AST and source
/// * `Autoload`, which holds info for parsing and loading the function lazily
#[derive(Clone, Debug)]
pub enum ShFunc {
  Defined {
    logic: Rc<Ast>, // immutable
    source: Span,
    ctx: Option<LabelBuilder>,
    is_internal: Option<IsInternal>,
  },
  Autoload(AutoloadSrc),
}

impl ShFunc {
  pub fn defined(logic: Ast, source: Span) -> Self {
    Self::Defined {
      logic: Rc::new(logic),
      source,
      ctx: None,
      is_internal: None,
    }
  }
  pub fn with_ctx(mut self, ctx: LabelBuilder) -> Self {
    match &mut self {
      Self::Defined { ctx: c, .. } => {
        *c = Some(ctx);
      }
      Self::Autoload(_) => {}
    }
    self
  }
  #[allow(dead_code)]
  pub fn autoload_src(&self) -> Option<&AutoloadSrc> {
    match self {
      Self::Autoload(src) => Some(src),
      Self::Defined { .. } => None,
    }
  }
  #[allow(dead_code)]
  pub fn source(&self) -> Option<&Span> {
    match self {
      Self::Defined { source, .. } => Some(source),
      Self::Autoload(_) => None,
    }
  }
  #[allow(dead_code)]
  pub fn logic(&self) -> Option<&Ast> {
    match self {
      Self::Defined { logic, .. } => Some(&**logic),
      Self::Autoload(_) => None,
    }
  }
  #[allow(dead_code)]
  pub fn is_defined(&self) -> bool {
    matches!(self, Self::Defined { .. })
  }
  pub fn set_is_internal(&mut self, is_internal: IsInternal) -> ShResult<()> {
    match self {
      Self::Defined { is_internal: i, .. } => {
        *i = Some(is_internal);
        Ok(())
      }
      Self::Autoload(_) => Err(sherr!(
        InternalErr,
        "Cannot set is_internal on autoload function"
      )),
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum AutoCmdKind {
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
  OnIdleTimeout,
  OnTimeReport,
  OnExit,
  OnCommandNotFound,
}

impl AutoCmdKind {
  pub fn iter() -> impl Iterator<Item = Self> {
    [
      Self::PreCmd,
      Self::PostCmd,
      Self::PreChangeDir,
      Self::PostChangeDir,
      Self::OnJobFinish,
      Self::PrePrompt,
      Self::PostPrompt,
      Self::PreModeChange,
      Self::PostModeChange,
      Self::OnHistoryOpen,
      Self::OnHistoryClose,
      Self::OnHistorySelect,
      Self::OnCompletionStart,
      Self::OnCompletionCancel,
      Self::OnCompletionSelect,
      Self::OnIdleTimeout,
      Self::OnTimeReport,
      Self::OnExit,
      Self::OnCommandNotFound,
    ]
    .into_iter()
  }
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
  OnIdleTimeout       <=> "on-idle-timeout";
  OnTimeReport        <=> "on-time-report";
  OnExit              <=> "on-exit";
  OnCommandNotFound   <=> "on-command-not-found";
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AutoCmd {
  kind: AutoCmdKind,
  command: VarStr,
}

impl AutoCmd {
  pub fn new(kind: AutoCmdKind, command: VarStr) -> Self {
    Self { kind, command }
  }
  pub fn command(&self) -> VarStr {
    self.command.clone()
  }
}

impl Display for AutoCmd {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let kind = self.kind.to_string();
    let command = shell_quote(&self.command.to_str_lossy());
    write!(f, "autocmd {kind} {command}")
  }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub(crate) enum TrapTarget {
  Exit,
  Error,
  Return,
  Signal(Signal),
}

impl TrapTarget {
  pub(crate) fn parse(s: &VarStr) -> ShResult<Self> {
    match s.as_bytes() {
      b"0" | b"EXIT" => Ok(TrapTarget::Exit),
      b"RETURN" => Ok(TrapTarget::Return),
      b"ERR" => Ok(TrapTarget::Error),
      _ => Ok(TrapTarget::Signal(parse_signal(&s.to_str_lossy())?)),
    }
  }
}

impl Display for TrapTarget {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      TrapTarget::Exit => write!(f, "EXIT"),
      TrapTarget::Return => write!(f, "RETURN"),
      TrapTarget::Error => write!(f, "ERR"),
      TrapTarget::Signal(s) => {
        let name = s.to_string();
        write!(f, "{}", name.strip_prefix("SIG").unwrap_or(&name))
      }
    }
  }
}

/// The logic table for the shell
///
/// Contains aliases and functions
#[derive(Default, Clone, Debug)]
pub(crate) struct LogTab {
  functions: HashMap<String, ShFunc>,
  comp_autoloads: HashMap<String, AutoloadSrc>,
  aliases: HashMap<String, ShAlias>,
  ex_aliases: HashMap<String, ShAlias>,

  traps: HashMap<TrapTarget, VarStr>,
  keymaps: Vec<KeyMap>,
  autocmds: HashMap<AutoCmdKind, Vec<AutoCmd>>,
}

impl LogTab {
  pub fn new() -> Self {
    let mut new = Self::default();
    new.register_autoload_funcs();
    new
  }
  fn register_autoload_funcs(&mut self) {
    for (name, src) in autoload::FuncLoader.bundled() {
      self.functions.insert(name, ShFunc::Autoload(src));
    }
  }
  pub fn register_autoload_comps(&mut self) {
    self.comp_autoloads = autoload::CompLoader.bundled();
  }
  pub fn get_autoload_func_names(&self) -> Vec<String> {
    let mut names: Vec<String> = self
      .functions
      .iter()
      .filter_map(|(name, func)| match func {
        ShFunc::Autoload(AutoloadSrc::Path(_)) => Some(name.clone()),
        _ => None,
      })
      .collect();
    names.sort();
    names
  }
  pub fn get_autoload_comp_names(&self) -> Vec<String> {
    let mut names: Vec<String> = self
      .comp_autoloads
      .iter()
      .filter_map(|(name, src)| match src {
        AutoloadSrc::Path(_) => Some(name.clone()),
        AutoloadSrc::Embedded { .. } => None,
      })
      .collect();
    names.sort();
    names
  }
  pub fn insert_comp_autoload(&mut self, name: &str, src: AutoloadSrc) {
    self.comp_autoloads.insert(name.into(), src);
  }
  pub fn insert_autocmd(&mut self, cmd: AutoCmd) {
    let entry = self.autocmds.entry(cmd.kind).or_default();
    if entry.contains(&cmd) {
      return;
    }
    entry.push(cmd);
  }
  pub fn get_autocmds(&self, kind: AutoCmdKind) -> Vec<AutoCmd> {
    self.autocmds.get(&kind).cloned().unwrap_or_default()
  }
  /// Iterate every registered autocmd in `(kind, command)` order. Skips
  /// the `notify_autocmd` side effect that `get_autocmds` performs, since
  /// dumping for `genrc` shouldn't mark autocmds as fired.
  pub fn iter_autocmds(&self) -> impl Iterator<Item = &AutoCmd> {
    let mut kinds: Vec<&AutoCmdKind> = self.autocmds.keys().collect();
    kinds.sort_by_key(ToString::to_string);
    kinds
      .into_iter()
      .flat_map(move |k| self.autocmds.get(k).map(|v| v.iter()).into_iter().flatten())
  }
  pub fn keymaps(&self) -> &[KeyMap] {
    &self.keymaps
  }
  pub fn clear_autocmds(&mut self, kind: AutoCmdKind) -> Option<Vec<AutoCmd>> {
    self.autocmds.remove(&kind)
  }
  pub fn insert_keymap(&mut self, keymap: KeyMap) {
    for map in &mut self.keymaps {
      if map.keys == keymap.keys {
        map.flags.remove(keymap.flags);
      }
    }
    self.keymaps.retain(|km| !km.flags.is_empty());
    self.keymaps.push(keymap);
  }
  pub fn remove_keymap(&mut self, keys: &str, flags: KeyMapFlags) {
    for km in &mut self.keymaps {
      if km.keys == keys {
        km.flags.remove(flags);
      }
    }
    self.keymaps.retain(|km| !km.flags.is_empty());
  }
  pub fn keymaps_filtered(&self, flags: KeyMapFlags, pending: &[KeyEvent]) -> Vec<KeyMap> {
    self
      .keymaps
      .iter()
      .filter(|km| km.flags.intersects(flags) && km.compare(pending) != KeyMapMatch::NoMatch)
      .cloned()
      .collect()
  }
  pub fn invalidate_internal_cache(&mut self) {
    for func in self.functions.values_mut() {
      if let ShFunc::Defined { is_internal, .. } = func {
        *is_internal = None;
      }
    }
  }
  pub fn insert_func(&mut self, name: &str, src: ShFunc) {
    self.functions.insert(name.into(), src);
    self.invalidate_internal_cache();
  }
  pub fn insert_trap(&mut self, target: TrapTarget, command: VarStr) {
    self.traps.insert(target, command);
  }
  pub fn get_trap(&self, target: TrapTarget) -> Option<VarStr> {
    self.traps.get(&target).cloned()
  }
  pub fn remove_trap(&mut self, target: TrapTarget) -> Option<VarStr> {
    self.traps.remove(&target)
  }
  pub fn reset_caught_traps(&mut self) {
    self.traps.retain(|_, command| command.is_empty());
  }
  pub fn traps(&self) -> &HashMap<TrapTarget, VarStr> {
    &self.traps
  }
  pub fn has_command_func(&self, name: &str) -> bool {
    self.functions.contains_key(name)
  }
  pub fn get_func(&self, name: &str) -> Option<ShFunc> {
    self.functions.get(name).cloned()
  }
  pub fn get_func_ref(&self, name: &str) -> Option<&ShFunc> {
    self.functions.get(name)
  }
  pub fn get_func_mut(&mut self, name: &str) -> Option<&mut ShFunc> {
    self.functions.get_mut(name)
  }
  pub fn funcs(&self) -> &HashMap<String, ShFunc> {
    &self.functions
  }
  pub fn remove_func(&mut self, name: &str) -> Option<ShFunc> {
    let func = self.functions.remove(name);
    self.invalidate_internal_cache();
    func
  }
  pub fn take_comp_autoload(&mut self, name: &str) -> Option<AutoloadSrc> {
    self.comp_autoloads.remove(name)
  }
  pub fn aliases(&self) -> &HashMap<String, ShAlias> {
    &self.aliases
  }
  pub fn insert_alias(&mut self, name: &str, body: &VarStr, source: Span) {
    self
      .aliases
      .insert(name.into(), ShAlias::new(body.clone(), source));
  }
  pub fn get_alias(&self, name: &str) -> Option<ShAlias> {
    self.aliases.get(name).cloned()
  }
  pub fn remove_alias(&mut self, name: &str) {
    self.aliases.remove(name);
  }

  pub fn insert_ex_alias(&mut self, name: &str, body: &VarStr, source: Span) {
    self
      .ex_aliases
      .insert(name.into(), ShAlias::new(body.clone(), source));
  }
  pub fn remove_ex_alias(&mut self, name: &str) {
    self.ex_aliases.remove(name);
  }
  pub fn get_ex_alias(&self, name: &str) -> Option<ShAlias> {
    self.ex_aliases.get(name).cloned()
  }
  pub fn ex_aliases(&self) -> &HashMap<String, ShAlias> {
    &self.ex_aliases
  }
}
