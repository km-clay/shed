use std::{
  cell::RefCell, cmp::Ordering, collections::{HashMap, HashSet, VecDeque, hash_map::Entry}, fmt::Display, ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Deref}, os::unix::fs::PermissionsExt, str::FromStr, time::Duration
};

use nix::unistd::{User, gethostname, getppid};

use crate::{
  builtin::{BUILTINS, trap::TrapTarget}, exec_input, jobs::JobTab, libsh::{
    error::{ShErr, ShErrKind, ShResult},
    utils::VecDequeExt,
  }, parse::{ConjunctNode, NdRule, Node, ParsedSrc, lex::{LexFlags, LexStream, Tk}}, prelude::*, readline::{complete::{BashCompSpec, CompSpec}, markers}, shopt::ShOpts
};

pub struct Shed {
  pub jobs: RefCell<JobTab>,
  pub var_scopes: RefCell<ScopeStack>,
  pub meta: RefCell<MetaTab>,
  pub logic: RefCell<LogTab>,
  pub shopts: RefCell<ShOpts>,
}

impl Shed {
  pub fn new() -> Self {
    Self {
      jobs: RefCell::new(JobTab::new()),
      var_scopes: RefCell::new(ScopeStack::new()),
      meta: RefCell::new(MetaTab::new()),
      logic: RefCell::new(LogTab::new()),
      shopts: RefCell::new(ShOpts::default()),
    }
  }
}

impl Default for Shed {
  fn default() -> Self {
    Self::new()
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
    let mut new_vars = VarTab::new();
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
  pub fn cur_scope(&self) -> &VarTab {
    self.scopes.last().unwrap()
  }
  pub fn cur_scope_mut(&mut self) -> &mut VarTab {
    self.scopes.last_mut().unwrap()
  }
  pub fn unset_var(&mut self, var_name: &str) -> ShResult<()> {
    for scope in self.scopes.iter_mut().rev() {
      if scope.var_exists(var_name) {
        return scope.unset_var(var_name);
      }
    }
		Err(ShErr::simple(
			ShErrKind::ExecFail,
			format!("Variable '{}' not found", var_name)
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
    let is_local = self.is_local_var(var_name);
    if flags.contains(VarFlags::LOCAL) || is_local {
      self.set_var_local(var_name, val, flags)
		} else {
			self.set_var_global(var_name, val, flags)
    }
  }
  pub fn set_var_indexed(&mut self, var_name: &str, idx: ArrIndex, val: String, flags: VarFlags) -> ShResult<()> {
    let is_local = self.is_local_var(var_name);
    if flags.contains(VarFlags::LOCAL) || is_local {
      let Some(scope) = self.scopes.last_mut() else { return Ok(()) };
      scope.set_index(var_name, idx, val)
    } else {
      let Some(scope) = self.scopes.first_mut() else { return Ok(()) };
      scope.set_index(var_name, idx, val)
    }
  }
  fn set_var_global(&mut self, var_name: &str, val: VarKind, flags: VarFlags) -> ShResult<()> {
    let Some(scope) = self.scopes.first_mut() else {
      return Ok(())
    };
    scope.set_var(var_name, val, flags)
  }
  fn set_var_local(&mut self, var_name: &str, val: VarKind, flags: VarFlags) -> ShResult<()> {
    let Some(scope) = self.scopes.last_mut() else {
      return Ok(())
    };
    scope.set_var(var_name, val, flags)
  }
	pub fn get_arr_elems(&self, var_name: &str) -> ShResult<Vec<String>> {
		for scope in self.scopes.iter().rev() {
			if scope.var_exists(var_name)
			&& let Some(var) = scope.vars().get(var_name) {
				match var.kind() {
					VarKind::Arr(items) => {
						let mut item_vec = items.clone()
							.into_iter()
							.collect::<Vec<(usize, String)>>();

						item_vec.sort_by_key(|(idx, _)| *idx); // sort by index

						return Ok(item_vec.into_iter()
							.map(|(_,s)| s)
							.collect())
					}
					_ => {
						return Err(ShErr::simple(
							ShErrKind::ExecFail,
							format!("Variable '{}' is not an array", var_name)
						));
					}
				}
			}
		}
		Err(ShErr::simple(
			ShErrKind::ExecFail,
			format!("Variable '{}' not found", var_name)
		))
	}
	pub fn index_var(&self, var_name: &str, idx: ArrIndex) -> ShResult<String> {
		for scope in self.scopes.iter().rev() {
			if scope.var_exists(var_name)
			&& let Some(var) = scope.vars().get(var_name) {
				match var.kind() {
					VarKind::Arr(items) => {
						let idx = match idx {
							ArrIndex::Literal(n) => {
								n
							}
							ArrIndex::FromBack(n) => {
								if items.len() >= n {
									items.len() - n
								} else {
									return Err(ShErr::simple(
										ShErrKind::ExecFail,
										format!("Index {} out of bounds for array '{}'", n, var_name)
									));
								}
							}
							_ => return Err(ShErr::simple(
								ShErrKind::ExecFail,
								format!("Cannot index all elements of array '{}'", var_name)
							)),
						};

						if let Some(item) = items.get(&idx) {
							return Ok(item.clone());
						} else {
							return Err(ShErr::simple(
								ShErrKind::ExecFail,
								format!("Index {} out of bounds for array '{}'", idx, var_name)
							));
						}
					}
					_ => {
						return Err(ShErr::simple(
							ShErrKind::ExecFail,
							format!("Variable '{}' is not an array", var_name)
						));
					}
				}
			}
		}
		Ok("".into())
	}
	pub fn try_get_var(&self, var_name: &str) -> Option<String> {
		// This version of get_var() is mainly used internally
		// so that we have access to Option methods
		if let Ok(param) = var_name.parse::<ShellParam>() {
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
  pub fn get_var(&self, var_name: &str) -> String {
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
	pub fn is_local_var(&self, var_name: &str) -> bool {
		self.scopes
			.last()
			.is_some_and(|s|
				s.get_var_flags(var_name).is_some_and(|flags| flags.contains(VarFlags::LOCAL))
			)
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

thread_local! {
  pub static SHED: Shed = Shed::new();
}

/// A shell function
///
/// Consists of the BraceGrp Node and the stored ParsedSrc that the node refers to.
/// The Node must be stored with the ParsedSrc because the tokens of the node
/// contain an Arc<String> Which refers to the String held in ParsedSrc
#[derive(Clone, Debug)]
pub struct ShFunc(Node);

impl ShFunc {
  pub fn new(mut src: ParsedSrc) -> Self {
    let body = Self::extract_brc_grp_hack(src.extract_nodes());
    Self(body)
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
		&self.0
	}
	pub fn body_mut(&mut self) -> &mut Node {
		&mut self.0
	}
}

/// The logic table for the shell
///
/// Contains aliases and functions
#[derive(Default, Clone, Debug)]
pub struct LogTab {
  functions: HashMap<String, ShFunc>,
  aliases: HashMap<String, String>,
  traps: HashMap<TrapTarget, String>,
}

impl LogTab {
  pub fn new() -> Self {
    Self::default()
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
  pub fn aliases(&self) -> &HashMap<String, String> {
    &self.aliases
  }
  pub fn insert_alias(&mut self, name: &str, body: &str) {
    self.aliases.insert(name.into(), body.into());
  }
  pub fn get_alias(&self, name: &str) -> Option<String> {
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
	AllJoined,
	AllSplit
}

impl FromStr for ArrIndex {
	type Err = ShErr;
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"@" => Ok(Self::AllSplit),
			"*" => Ok(Self::AllJoined),
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
					format!("Invalid array index: {}", s)
			))
		}
	}
}

pub fn hashmap_to_vec(map: HashMap<usize, String>) -> Vec<String> {
	let mut items = map.into_iter()
		.collect::<Vec<(usize, String)>>();
	items.sort_by_key(|(idx, _)| *idx);

	items.into_iter()
		.map(|(_,i)| i)
		.collect()
}

#[derive(Clone, Debug)]
pub enum VarKind {
	Str(String),
	Int(i32),
	Arr(HashMap<usize,String>),
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

		let tokens: HashMap<usize,String> = LexStream::new(Arc::new(raw), LexFlags::empty())
			.map(|tk| tk.and_then(|tk| tk.expand()).map(|tk| tk.get_words()))
			.try_fold(vec![], |mut acc, wrds| {
				match wrds {
					Ok(wrds) => acc.extend(wrds),
					Err(e) => return Err(e),
				}
				Ok(acc)
			})?
			.into_iter()
			.enumerate()
			.collect();

		Ok(Self::Arr(tokens))
	}

	pub fn arr_from_vec(vec: Vec<String>) -> Self {
		let tokens: HashMap<usize,String> = vec.into_iter()
			.enumerate()
			.collect();

		Self::Arr(tokens)
	}
}

impl Display for VarKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      VarKind::Str(s) => write!(f, "{s}"),
      VarKind::Int(i) => write!(f, "{i}"),
      VarKind::Arr(items) => {
				let items = hashmap_to_vec(items.clone());
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
}

impl Display for Var {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.kind.fmt(f)
  }
}

#[derive(Default, Clone, Debug)]
pub struct VarTab {
  vars: HashMap<String, Var>,
  params: HashMap<ShellParam, String>,
  sh_argv: VecDeque<String>, /* Using a VecDeque makes the implementation of `shift`
                              * straightforward */
}

impl VarTab {
  pub fn new() -> Self {
    let vars = HashMap::new();
    let params = Self::init_params();
    Self::init_env();
    let mut var_tab = Self {
      vars,
      params,
      sh_argv: VecDeque::new(),
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
    self.sh_argv.clear();
    // Push the current exe again
    // This makes sure that $0 is always the current shell, no matter what
    // It also updates the arg parameters '@' and '#' as well
    self.bpush_arg(env::current_exe().unwrap().to_str().unwrap().to_string());
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
		if let Some(var) = self.vars.get(var_name) && var.flags.contains(VarFlags::READONLY) {
			return Err(ShErr::simple(
				ShErrKind::ExecFail,
				format!("cannot unset readonly variable '{}'", var_name)
			));
		}
    self.vars.remove(var_name);
    unsafe { env::remove_var(var_name) };
		Ok(())
  }
	pub fn set_index(&mut self, var_name: &str, idx: ArrIndex, val: String) -> ShResult<()> {
		if self.var_exists(var_name)
			&& let Some(var) = self.vars_mut().get_mut(var_name) {
				match var.kind_mut() {
					VarKind::Arr(items) => {
						let idx = match idx {
							ArrIndex::Literal(n) => {
								n
							}
							ArrIndex::FromBack(n) => {
								if items.len() >= n {
									items.len() - n
								} else {
									return Err(ShErr::simple(
											ShErrKind::ExecFail,
											format!("Index {} out of bounds for array '{}'", n, var_name)
									));
								}
							}
							_ => return Err(ShErr::simple(
									ShErrKind::ExecFail,
									format!("Cannot index all elements of array '{}'", var_name)
							)),
						};

						items.insert(idx, val);
						return Ok(());
					}
					_ => {
						return Err(ShErr::simple(
								ShErrKind::ExecFail,
								format!("Variable '{}' is not an array", var_name)
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
					format!("Variable '{}' is readonly", var_name)
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
      _ => self
        .params
        .get(&param)
        .map(|s| s.to_string())
        .unwrap_or_default(),
    }
  }
}

/// A table of metadata for the shell
#[derive(Default, Debug)]
pub struct MetaTab {
  // command running duration
  runtime_start: Option<Instant>,
  runtime_stop: Option<Instant>,

  // pending system messages
  system_msg: Vec<String>,

	// pushd/popd stack
	dir_stack: VecDeque<PathBuf>,

	old_path: Option<String>,
	old_pwd: Option<String>,
	// valid command cache
	path_cache: HashSet<String>,
	cwd_cache: HashSet<String>,
	// programmable completion specs
	comp_specs: HashMap<String, Box<dyn CompSpec>>,
}

impl MetaTab {
  pub fn new() -> Self {
    Self::default()
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
		self.comp_specs.get(cmd).map(|spec| spec.clone())
	}
	pub fn set_comp_spec(&mut self, cmd: String, spec: Box<dyn CompSpec>) {
		self.comp_specs.insert(cmd, spec);
	}
	pub fn remove_comp_spec(&mut self, cmd: &str) -> bool {
		self.comp_specs.remove(cmd).is_some()
	}
	pub fn try_rehash_commands(&mut self) {
		let path = env::var("PATH").unwrap_or_default();
		let cwd = env::var("PWD").unwrap_or_default();
		if self.old_path.as_ref().is_some_and(|old| *old == path)
		&& self.old_pwd.as_ref().is_some_and(|old| *old == cwd) {
			log::trace!("PATH and PWD unchanged, skipping rehash");
			return;
		}

		log::trace!("Rehashing commands for PATH: '{}' and PWD: '{}'", path, cwd);

		self.path_cache.clear();
		self.old_path = Some(path.clone());
		self.old_pwd = Some(cwd.clone());
		let paths = path.split(":")
			.map(PathBuf::from);

		for path in paths {
			if let Ok(entries) = path.read_dir() {
				for entry in entries.flatten() {
          let Ok(meta) = std::fs::metadata(entry.path()) else { continue };
          let is_exec = meta.permissions().mode() & 0o111 != 0;

					if meta.is_file() && is_exec
					&& let Some(name) = entry.file_name().to_str() {
						self.path_cache.insert(name.to_string());
					}
				}
			}
		}
		if let Ok(entries) = Path::new(&cwd).read_dir() {
			for entry in entries.flatten() {
				let Ok(meta) = std::fs::metadata(entry.path()) else { continue };
				let is_exec = meta.permissions().mode() & 0o111 != 0;

				if meta.is_file() && is_exec
				&& let Some(name) = entry.file_name().to_str() {
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
	pub fn try_rehash_cwd_listing(&mut self) {
		let cwd = env::var("PWD").unwrap_or_default();
		if self.old_pwd.as_ref().is_some_and(|old| *old == cwd) {
			log::trace!("PWD unchanged, skipping rehash of cwd listing");
			return;
		}

		log::debug!("Rehashing cwd listing for PWD: '{}'", cwd);

		if let Ok(entries) = Path::new(&cwd).read_dir() {
			for entry in entries.flatten() {
				let Ok(meta) = std::fs::metadata(entry.path()) else { continue };
				let is_exec = meta.permissions().mode() & 0o111 != 0;

				if meta.is_file() && is_exec
				&& let Some(name) = entry.file_name().to_str() {
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
    self.system_msg.push(message);
  }
  pub fn pop_system_message(&mut self) -> Option<String> {
    self.system_msg.pop()
  }
  pub fn system_msg_pending(&self) -> bool {
    !self.system_msg.is_empty()
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
			'\\' => { chars.next(); }
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

	expanded.parse::<ArrIndex>().map_err(|_| ShErr::simple(
		ShErrKind::ParseErr,
		format!("Invalid array index: {}", expanded)
	))
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

pub fn get_status() -> i32 {
  read_vars(|v| v.get_param(ShellParam::Status))
    .parse::<i32>()
    .unwrap()
}
pub fn set_status(code: i32) {
  write_vars(|v| v.set_param(ShellParam::Status, &code.to_string()))
}

pub fn source_rc() -> ShResult<()> {
  let path = if let Ok(path) = env::var("SHED_RC") {
    PathBuf::from(&path)
  } else {
    let home = env::var("HOME").unwrap();
    PathBuf::from(format!("{home}/.shedrc"))
  };
  if !path.exists() {
    return Err(ShErr::simple(ShErrKind::InternalErr, ".shedrc not found"));
  }
  source_file(path)
}

pub fn source_file(path: PathBuf) -> ShResult<()> {
  let mut file = OpenOptions::new().read(true).open(path)?;

  let mut buf = String::new();
  file.read_to_string(&mut buf)?;
  exec_input(buf, None, false)?;
  Ok(())
}
