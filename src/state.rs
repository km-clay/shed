use std::{collections::{HashMap, VecDeque}, ops::Deref, sync::{LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard}, time::Duration};

use nix::unistd::{gethostname, getppid, User};

use crate::{exec_input, jobs::JobTab, libsh::{error::{ShErr, ShErrKind, ShResult}, utils::VecDequeExt}, parse::{lex::get_char, ConjunctNode, NdRule, Node, ParsedSrc}, prelude::*};

pub static JOB_TABLE: LazyLock<RwLock<JobTab>> = LazyLock::new(|| RwLock::new(JobTab::new()));

pub static VAR_TABLE: LazyLock<RwLock<VarTab>> = LazyLock::new(|| RwLock::new(VarTab::new()));

pub static META_TABLE: LazyLock<RwLock<MetaTab>> = LazyLock::new(|| RwLock::new(MetaTab::new()));

pub static LOGIC_TABLE: LazyLock<RwLock<LogTab>> = LazyLock::new(|| RwLock::new(LogTab::new()));

/// A shell function
///
/// Consists of the BraceGrp Node and the stored ParsedSrc that the node refers to
/// The Node must be stored with the ParsedSrc because the tokens of the node contain an Arc<String>
/// Which refers to the String held in ParsedSrc
///
/// Can be dereferenced to pull out the wrapped Node
#[derive(Clone,Debug)]
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
}

impl Deref for ShFunc {
	type Target = Node;
	fn deref(&self) -> &Self::Target {
		&self.0
	}
}

/// The logic table for the shell
///
/// Contains aliases and functions
pub struct LogTab {
	functions: HashMap<String,ShFunc>,
	aliases: HashMap<String,String>
}

impl LogTab {
	pub fn new() -> Self {
		Self { functions: HashMap::new(), aliases: HashMap::new() }
	}
	pub fn insert_func(&mut self, name: &str, src: ShFunc) {
		self.functions.insert(name.into(), src);
	}
	pub fn get_func(&self, name: &str) -> Option<ShFunc> {
		self.functions.get(name).cloned()
	}
	pub fn funcs(&self) -> &HashMap<String,ShFunc> {
		&self.functions
	}
	pub fn aliases(&self) -> &HashMap<String,String> {
		&self.aliases
	}
	pub fn insert_alias(&mut self, name: &str, body: &str) {
		self.aliases.insert(name.into(), body.into());
	}
	pub fn get_alias(&self, name: &str) -> Option<String> {
		self.aliases.get(name).cloned()
	}
}

#[derive(Clone)]
pub struct VarTab {
	vars: HashMap<String,String>,
	params: HashMap<char,String>,
	sh_argv: VecDeque<String>, // Using a VecDeque makes the implementation of `shift` straightforward
}

impl VarTab {
	pub fn new() -> Self {
		let vars = HashMap::new();
		let params = Self::init_params();
		Self::init_env();
		let mut var_tab = Self { vars, params, sh_argv: VecDeque::new() };
		var_tab.init_sh_argv();
		var_tab
	}
	fn init_params() -> HashMap<char, String> {
		let mut params = HashMap::new();
		params.insert('?', "0".into());  // Last command exit status
		params.insert('#', "0".into());  // Number of positional parameters
		params.insert('0', std::env::current_exe().unwrap().to_str().unwrap().to_string()); // Name of the shell
		params.insert('$', Pid::this().to_string()); // PID of the shell
		params.insert('!', "".into()); // PID of the last background job (if any)
		params
	}
	fn init_env() {
		let pathbuf_to_string = |pb: Result<PathBuf, std::io::Error>| pb.unwrap_or_default().to_string_lossy().to_string();
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
		let hostname = gethostname().map(|hname| hname.to_string_lossy().to_string()).unwrap_or_default();

		env::set_var("IFS", " \t\n");
		env::set_var("HOSTNAME", hostname);
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
		env::set_var("FERN_HIST",format!("{}/.fernhist",home));
		env::set_var("FERN_RC",format!("{}/.fernrc",home));
	}
	pub fn init_sh_argv(&mut self) {
		for arg in env::args() {
			self.bpush_arg(arg);
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
		self.set_param('@', &self.sh_argv.clone().to_vec()[1..].join(" "));
		self.set_param('#', &(self.sh_argv.len() - 1).to_string());
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
	pub fn vars(&self) -> &HashMap<String,String> {
		&self.vars
	}
	pub fn vars_mut(&mut self) -> &mut HashMap<String,String> {
		&mut self.vars
	}
	pub fn params(&self) -> &HashMap<char,String> {
		&self.params
	}
	pub fn params_mut(&mut self) -> &mut HashMap<char,String> {
		&mut self.params
	}
	pub fn get_var(&self, var: &str) -> String {
		if var.chars().count() == 1 {
			let param = self.get_param(get_char(var, 0).unwrap());
			if !param.is_empty() {
				return param
			}
		}
		if let Some(var) = self.vars.get(var).map(|s| s.to_string()) {
			var
		} else {
			std::env::var(var).unwrap_or_default()
		}
	}
	pub fn new_var(&mut self, var: &str, val: &str) {
		self.vars.insert(var.to_string(), val.to_string());
	}
	pub fn set_param(&mut self, param: char, val: &str) {
		self.params.insert(param,val.to_string());
	}
	pub fn get_param(&self, param: char) -> String {
		if param.is_ascii_digit() {
			let argv_idx = param
				.to_string()
				.parse::<usize>()
				.unwrap();
			let arg = self.sh_argv.get(argv_idx).map(|s| s.to_string()).unwrap_or_default();
			arg
		} else if param == '?' {
			self.params.get(&param).map(|s| s.to_string()).unwrap_or("0".into())
		} else {
			self.params.get(&param).map(|s| s.to_string()).unwrap_or_default()
		}
	}
}

/// A table of metadata for the shell
pub struct MetaTab {
	runtime_start: Option<Instant>
}

impl MetaTab {
	pub fn new() -> Self {
		Self { runtime_start: None }
	}
	pub fn start_timer(&mut self) {
		self.runtime_start = Some(Instant::now());
	}
	pub fn stop_timer(&mut self) -> Option<Duration> {
		self.runtime_start
			.take() // runtime_start returns to None
			.map(|start| start.elapsed()) // return the duration, if any
	}
}

/// Read from the job table
pub fn read_jobs<T, F: FnOnce(RwLockReadGuard<JobTab>) -> T>(f: F) -> T {
	let lock = JOB_TABLE.read().unwrap();
	f(lock)
}

/// Write to the job table
pub fn write_jobs<T, F: FnOnce(&mut RwLockWriteGuard<JobTab>) -> T>(f: F) -> T {
	let lock = &mut JOB_TABLE.write().unwrap();
	f(lock)
}

/// Read from the variable table
pub fn read_vars<T, F: FnOnce(RwLockReadGuard<VarTab>) -> T>(f: F) -> T {
	let lock = VAR_TABLE.read().unwrap();
	f(lock)
}

/// Write to the variable table
pub fn write_vars<T, F: FnOnce(&mut RwLockWriteGuard<VarTab>) -> T>(f: F) -> T {
	let lock = &mut VAR_TABLE.write().unwrap();
	f(lock)
}

pub fn read_meta<T, F: FnOnce(RwLockReadGuard<MetaTab>) -> T>(f: F) -> T {
	let lock = META_TABLE.read().unwrap();
	f(lock)
}

/// Write to the variable table
pub fn write_meta<T, F: FnOnce(&mut RwLockWriteGuard<MetaTab>) -> T>(f: F) -> T {
	let lock = &mut META_TABLE.write().unwrap();
	f(lock)
}

/// Read from the logic table
pub fn read_logic<T, F: FnOnce(RwLockReadGuard<LogTab>) -> T>(f: F) -> T {
	let lock = LOGIC_TABLE.read().unwrap();
	f(lock)
}

/// Write to the logic table
pub fn write_logic<T, F: FnOnce(&mut RwLockWriteGuard<LogTab>) -> T>(f: F) -> T {
	let lock = &mut LOGIC_TABLE.write().unwrap();
	f(lock)
}

pub fn get_status() -> i32 {
	read_vars(|v| v.get_param('?')).parse::<i32>().unwrap()
}
#[track_caller]
pub fn set_status(code: i32) {
	write_vars(|v| v.set_param('?', &code.to_string()))
}

pub fn source_rc() -> ShResult<()> {
	let path = if let Ok(path) = env::var("FERN_RC") {
		PathBuf::from(&path)
	} else {
		let home = env::var("HOME").unwrap();
		PathBuf::from(format!("{home}/.fernrc"))
	};
	if !path.exists() {
		return Err(
			ShErr::simple(ShErrKind::InternalErr, ".fernrc not found")
		)
	}
	source_file(path)
}

pub fn source_file(path: PathBuf) -> ShResult<()> {
	let mut file = OpenOptions::new()
		.read(true)
		.open(path)?;

	let mut buf = String::new();
	file.read_to_string(&mut buf)?;
	exec_input(buf)?;
	Ok(())
}
