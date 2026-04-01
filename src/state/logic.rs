use super::*;

use std::{
  collections::HashMap,
  fmt::{self, Display},
  str::FromStr,
};

use regex::Regex;

use crate::{
  builtin::{
    keymap::{KeyMap, KeyMapFlags, KeyMapMatch},
    trap::TrapTarget,
  },
  parse::{ConjunctNode, NdRule, Node, ParsedSrc, lex::Span},
  readline::keys::KeyEvent,
};

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
