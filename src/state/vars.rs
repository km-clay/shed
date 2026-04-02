use super::*;

use std::{
  collections::{HashMap, VecDeque},
  fmt::{self, Display},
  ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign},
  str::FromStr,
};

use nix::unistd::{User, gethostname, getppid};

use crate::{
  builtin::map::MapNode,
  libsh::{
    error::{ShErr, ShResult},
    utils::VecDequeExt,
  },
  parse::lex::{LexFlags, LexStream, Tk},
  prelude::*,
  readline::{complete::Candidate, markers},
  sherr,
};

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
      _ => Err(sherr!(InternalErr, "Invalid shell parameter: {}", s,)),
    }
  }
}

#[derive(Clone, Default, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
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
      _ => Err(sherr!(ParseErr, "Invalid array index: {}", s,)),
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
      return Err(sherr!(ParseErr, "Invalid array syntax: {}", raw,));
    }
    let raw = raw[1..raw.len() - 1].to_string();

    let tokens: VecDeque<String> = LexStream::new(raw.into(), LexFlags::empty())
      .map(|tk| tk.and_then(|tk| tk.expand()).map(|tk| tk.get_words()))
      .try_fold(String::new(), |mut acc, wrds| {
        match wrds {
          Ok(wrds) => {
            let wrds_joined = wrds.join(" ");
            acc = [acc, wrds_joined].join(&markers::ARG_SEP.to_string());
          }
          Err(e) => return Err(e),
        }
        Ok(acc)
      })?
      .split(markers::ARG_SEP)
      .filter(|s| !s.is_empty())
      .map(|s| s.to_string())
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

impl Default for Var {
	fn default() -> Self {
	  Self {
			flags: VarFlags::default(),
			kind: VarKind::Str(String::new()),
		}
	}
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
		vars.insert("OPTIND".into(), "1".into());
		let env_vars = Self::init_env();
		vars.extend(env_vars);
    vars
  }
  fn init_env() -> Vec<(String, Var)> {
		let mut vars = vec![];
		let mut set_var = |var: &str, val: &str| {
			unsafe { env::set_var(var, val) };
			vars.push((var.to_string(), val.into()))
		};

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

    let mut data_dir =
      dirs::data_dir().unwrap_or_else(|| PathBuf::from(format!("{home}/.local/share")));
    data_dir.push("shed");
    let shed_docs = data_dir.join("doc");
    let shed_db = data_dir.join("shed_hist.db");

    let help_paths = format!("/usr/share/shed/doc:{}", shed_docs.display());

		set_var("IFS", " \t\n");
		set_var("HOST", &hostname.clone());
		set_var("UID", &uid.to_string());
		set_var("PPID", &getppid().to_string());
		set_var("TMPDIR", "/tmp");
		set_var("TERM", &term);
		set_var("LANG", "en_US.UTF-8");
		set_var("USER", &username.clone());
		set_var("LOGNAME", &username);
		set_var("PWD", &pathbuf_to_string(std::env::current_dir()));
		set_var("OLDPWD", &pathbuf_to_string(std::env::current_dir()));
		set_var("HOME", &home.clone());
		set_var("SHELL", &pathbuf_to_string(std::env::current_exe()));
		set_var("SHED_HIST", &format!("{}/.shed_history", home));
		set_var("SHED_HISTDB", &format!("{}", shed_db.display()));
		set_var("SHED_RC", &format!("{}/.shedrc", home));
		set_var("SHED_HPATH", &help_paths);

		vars
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
	pub fn get_var_meta(&self, var: &str) -> Var {
		self.try_get_var_meta(var).unwrap_or_default()
	}
	pub fn try_get_var_meta(&self, var: &str) -> Option<Var> {
		self.vars.get(var).cloned()
	}
  pub fn get_var_flags(&self, var_name: &str) -> Option<VarFlags> {
    self.vars.get(var_name).map(|var| var.flags)
  }
  pub fn unset_var(&mut self, var_name: &str) -> ShResult<()> {
    if let Some(var) = self.vars.get(var_name)
      && var.flags.contains(VarFlags::READONLY)
    {
      return Err(sherr!(
        ExecFail,
        "cannot unset readonly variable '{}'",
        var_name,
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

          if idx >= items.len() {
            items.resize(idx + 1, String::new());
          }
          items[idx] = val;
          return Ok(());
        }
        _ => {
          return Err(sherr!(ExecFail, "Variable '{}' is not an array", var_name,));
        }
      }
    }
    Ok(())
  }
  pub fn set_var(&mut self, var_name: &str, val: VarKind, flags: VarFlags) -> ShResult<()> {
    if let Some(var) = self.vars.get_mut(var_name) {
      if var.flags.contains(VarFlags::READONLY) && !flags.contains(VarFlags::READONLY) {
        return Err(sherr!(ExecFail, "Variable '{}' is readonly", var_name,));
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
