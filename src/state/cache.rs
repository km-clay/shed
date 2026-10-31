use std::{
  ffi::CString,
  os::unix::ffi::OsStrExt,
  path::{Path, PathBuf},
  rc::Rc,
  sync::Arc,
};

use regex::Regex;

use crate::{
  HashMap,
  eval::parse::{
    ast::{Ast, NodeId},
    node::ForkReport,
  },
  expand::glob::{GlobOpts, Pattern},
  state::vars::ShStr,
  var,
};

use super::{
  Shed, paths,
  vars::{VarFlags, VarKind},
};

#[derive(Debug, Clone, Default)]
pub(crate) struct PathTable {
  index: HashMap<ShStr, PathBuf>,
}

impl PathTable {
  pub(crate) fn new() -> Self {
    Self::default()
  }
  pub(crate) fn hash_path_list(&mut self, path_list: &[u8]) {
    self.index.clear();
    for entry in paths::path_list_entries(path_list) {
      if !paths::is_executable_file(&entry) {
        continue;
      }
      let name = entry.file_name();

      self
        .index
        .entry(name.as_bytes().into())
        .or_insert_with(|| entry.path());
    }
  }
  pub(crate) fn lookup(&self, cmd: &[u8]) -> Option<&Path> {
    self.index.get(cmd).map(PathBuf::as_path)
  }
  pub(crate) fn insert(&mut self, name: &[u8], path: PathBuf) {
    self.index.insert(name.into(), path);
  }
  pub(crate) fn entries(&self) -> impl Iterator<Item = (&ShStr, &PathBuf)> {
    self.index.iter()
  }
  pub(crate) fn clear(&mut self) {
    self.index.clear();
  }
}

#[derive(Debug, Clone, Default)]
struct ForkMemo {
  hot : Option<(u32, Vec<Option<ForkReport>>)>,
  cold: HashMap<u32, Vec<Option<ForkReport>>>,
}

impl ForkMemo {
  fn get(&self, node: NodeId) -> Option<&ForkReport> {
    let tree = self
      .get_hot(node.ast_id())
      .or_else(|| self.cold.get(&node.ast_id()))?;

    tree.get(node.slot())?.as_ref()
  }

  fn get_hot(&self, ast_id: u32) -> Option<&Vec<Option<ForkReport>>> {
    self
      .hot
      .as_ref()
      .and_then(|(id, cache)| (*id == ast_id).then_some(cache))
  }

  fn focus(&mut self, ast_id: u32, slots: usize) {
    if self.hot.as_ref().is_some_and(|(id, _)| *id == ast_id) {
      return;
    }

    if let Some((old_id, old)) = self.hot.take() {
      self.cold.insert(old_id, old);
    }

    let tree = self
      .cold
      .remove(&ast_id)
      .unwrap_or_else(|| vec![None; slots]);
    self.hot = Some((ast_id, tree));
  }

  fn insert(&mut self, node_id: NodeId, slots: usize, report: ForkReport) {
    self.focus(node_id.ast_id(), slots);

    let Some((_, tree)) = self.hot.as_mut() else {
      return;
    };

    if tree.len() < slots {
      tree.resize(slots, None);
    }

    let Some(slot) = tree.get_mut(node_id.slot()) else {
      return;
    };

    match slot {
      None | Some(ForkReport::Simple(_)) => *slot = Some(report),
      Some(ForkReport::Detailed(_))      => {}
    }
  }

  fn clear(&mut self) {
    self.hot = None;
    self.cold.clear();
  }

  fn drop_tree(&mut self, ast_id: u32) {
    if self.hot.as_ref().is_some_and(|(id, _)| *id == ast_id) {
      self.hot = None;
    }

    self.cold.remove(&ast_id);
  }
}

#[derive(Debug, Clone, Default)]
struct RegexCache {
  regexes: HashMap<String, Rc<Regex>>,
  globs  : HashMap<Rc<[u8]>, Rc<Pattern>>,
}

impl RegexCache {
  pub(crate) fn new() -> Self {
    Self::default()
  }
  pub(crate) fn get_regex(&mut self, pat: &str) -> Result<Rc<Regex>, String> {
    if let Some(rx) = self.regexes.get(pat) {
      return Ok(Rc::clone(rx));
    }
    let rx = Rc::new(Regex::new(pat).map_err(|e| e.to_string())?);
    self.regexes.insert(pat.to_string(), Rc::clone(&rx));
    Ok(rx)
  }
  pub(crate) fn get_glob(&mut self, pat: &[u8]) -> Rc<Pattern> {
    if let Some(p) = self.globs.get(pat) {
      return p.clone();
    }
    // `case`/`[[ == ]]` matching is case-sensitive (no `nocasematch` shopt).
    let p = Rc::from(Pattern::compile(pat, GlobOpts::new()));
    self.globs.insert(p.orig().clone(), p.clone());
    p
  }
}

#[derive(Debug, Clone, Default)]
struct AstCache {
  trees: HashMap<ShStr, Arc<Ast>>,
}

impl AstCache {
  const CAP: usize = 512;

  fn len(&self) -> usize {
    self.trees.len()
  }

  fn trees(&self) -> impl Iterator<Item = &Arc<Ast>> {
    self.trees.values()
  }

  fn insert(&mut self, src: ShStr, ast: Arc<Ast>) {
    self.trees.insert(src, ast);
  }

  fn has_tree_for(&self, src: &[u8]) -> bool {
    self.trees.contains_key(src)
  }
}

/// Derived state that may be discarded at any time.
///
/// Invalidation is expressed as *events* (`funcs_changed`, `env_changed`) rather
/// than per-cache clears, so that adding a cache which depends on, say, the
/// function table is one edit here instead of a hunt through call sites.
#[derive(Debug, Clone, Default)]
pub(crate) struct CacheTab {
  fork    : ForkMemo,
  envp    : Option<Rc<[CString]>>,
  regex   : RegexCache,
  ast     : AstCache,
  path    : PathTable,
  old_path: Option<ShStr>,
}

impl CacheTab {
  pub(crate) fn new() -> Self {
    Self {
      fork    : ForkMemo::default(),
      envp    : None,
      regex   : RegexCache::new(),
      ast     : AstCache::default(),
      path    : PathTable::new(),
      old_path: None,
    }
  }
  /// A function was defined, removed, or the table cleared wholesale.
  pub(crate) fn funcs_changed(&mut self) {
    self.fork.clear();
  }

  /// An exported variable changed, or a scope holding exports was popped.
  pub(crate) fn env_changed(&mut self) {
    self.envp = None;
  }

  pub(crate) fn get_ast(&self, src: &[u8]) -> Option<Arc<Ast>> {
    self.ast.trees.get(src).cloned()
  }
  pub(crate) fn cache_ast(&mut self, src: ShStr, ast: Arc<Ast>) {
    if self.ast.len() >= AstCache::CAP {
      for tree in self.ast.trees() {
        // Purge the fork memo cache here
        // AST's drop impl needs a cache borrow, but cant get it since we
        // are already in one. therefore we just nuke the fork memo cache
        self.fork.drop_tree(tree.id());
      }
      self.ast.trees.clear()
    }
    self.ast.insert(src, ast);
  }
  pub(crate) fn has_ast_for(&self, src: &[u8]) -> bool {
    self.ast.has_tree_for(src)
  }

  pub(crate) fn path_cache(&self) -> &PathTable {
    &self.path
  }

  /// Look up an external command in the PATH cache. Returns `None` for a
  /// cache miss; callers that want to populate the cache on miss should
  /// call [`try_rehash_path_cache`](Self::try_rehash_path_cache) first.
  pub(crate) fn lookup_cached_cmd(&self, cmd: &[u8]) -> Option<&Path> {
    self.path.lookup(cmd)
  }

  pub(crate) fn rehash_path_cache(&mut self) {
    let path = var!(b"PATH");
    self.old_path = Some(path.clone());
    self.path.hash_path_list(&path);
  }

  pub(crate) fn clear_path_cache(&mut self) {
    self.old_path = None;
    self.path.clear();
  }

  pub(crate) fn try_rehash_path_cache(&mut self) {
    let path = var!(b"PATH");
    if self.old_path.as_ref().is_none_or(|old| *old != path) {
      self.old_path = Some(path.clone());
      self.path.hash_path_list(&path);
    }
  }

  pub(crate) fn invalidate_path_cache_if_stale(&mut self) {
    let path = var!(b"PATH");
    if self.old_path.as_ref().is_none_or(|old| *old != path) {
      self.old_path = Some(path);
      self.path.clear();
    }
  }

  pub(crate) fn cache_cmd(&mut self, name: &[u8], path: PathBuf) {
    self.path.insert(name, path);
  }

  pub(crate) fn fork_memo_for(&self, node: NodeId) -> Option<&ForkReport> {
    self.fork.get(node)
  }

  pub(crate) fn drop_fork_memos_for(&mut self, ast_id: u32) {
    self.fork.drop_tree(ast_id);
  }

  pub(crate) fn memoize_fork_behavior(&mut self, node: NodeId, slots: usize, behavior: ForkReport) {
    self.fork.insert(node, slots, behavior);
  }

  pub(crate) fn get_regex(&mut self, pat: &str) -> Result<Rc<Regex>, String> {
    self.regex.get_regex(pat)
  }
  pub(crate) fn get_glob(&mut self, pat: &[u8]) -> Rc<Pattern> {
    self.regex.get_glob(pat)
  }

  pub(crate) fn get_envp(&mut self) -> Rc<[CString]> {
    if let Some(envp) = &self.envp {
      return Rc::clone(envp);
    }

    // Walk scopes outermost-to-innermost so inner bindings shadow outer
    // ones in the flat map. Libc env is not consulted, so shell writes
    // outside this builder can't desync the env children inherit.
    let mut flat: HashMap<ShStr, ShStr> = HashMap::default();
    Shed::vars(|v| {
      for scope in v.scopes_iter() {
        for (name, var) in scope.vars() {
          if var.flags().contains(VarFlags::EXPORT)
            && let VarKind::Str(s) = var.kind()
          {
            flat.insert(name.clone(), s.clone());
          }
        }
      }
    });

    let envp: Vec<CString> = flat
      .into_iter()
      .map(|(k, v)| {
        let mut bytes = ShStr::with_capacity(k.len() + v.len() + 2);
        bytes.push_slice(k.as_bytes());
        bytes.push(b'=');
        bytes.push_slice(v.as_bytes());

        bytes.to_cstring_lossy()
      })
      .collect();

    self.envp = Some(Rc::from(envp.as_slice()));

    self.get_envp()
  }
}
