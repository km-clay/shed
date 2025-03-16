use std::{cell::RefCell, collections::{HashMap, VecDeque}, ops::Range, sync::{LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard}};

use crate::{exec_input, jobs::JobTab, libsh::{error::ShResult, utils::VecDequeExt}, parse::{lex::{get_char, Tk}, Node}, prelude::*};

pub static JOB_TABLE: LazyLock<RwLock<JobTab>> = LazyLock::new(|| RwLock::new(JobTab::new()));

pub static VAR_TABLE: LazyLock<RwLock<VarTab>> = LazyLock::new(|| RwLock::new(VarTab::new()));

pub static LOGIC_TABLE: LazyLock<RwLock<LogTab>> = LazyLock::new(|| RwLock::new(LogTab::new()));

thread_local! {
	pub static LAST_INPUT: RefCell<String> = RefCell::new(String::new());
}

/// The logic table for the shell
///
/// Contains aliases and functions
pub struct LogTab {
	// TODO: Find a way to store actual owned nodes instead of strings that must be re-parsed
	functions: HashMap<String,String>,
	aliases: HashMap<String,String>
}

impl LogTab {
	pub fn new() -> Self {
		Self { functions: HashMap::new(), aliases: HashMap::new() }
	}
	pub fn insert_func(&mut self, name: &str, body: &str) {
		self.functions.insert(name.into(), body.into());
	}
	pub fn get_func(&self, name: &str) -> Option<String> {
		self.functions.get(name).cloned()
	}
	pub fn insert_alias(&mut self, name: &str, body: &str) {
		self.aliases.insert(name.into(), body.into());
	}
	pub fn get_alias(&self, name: &str) -> Option<String> {
		self.aliases.get(name).cloned()
	}
}

pub struct VarTab {
	vars: HashMap<String,String>,
	params: HashMap<char,String>,
	sh_argv: VecDeque<String>, // Using a VecDeque makes the implementation of `shift` straightforward
}

impl VarTab {
	pub fn new() -> Self {
		let vars = HashMap::new();
		let params = Self::init_params();
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
		self.sh_argv.clear()
	}
	fn update_arg_params(&mut self) {
		self.set_param('@', &self.sh_argv.clone().to_vec().join(" "));
		self.set_param('#', &self.sh_argv.len().to_string());
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
			return self.sh_argv.get(argv_idx).map(|s| s.to_string()).unwrap_or_default()
		} else if param == '?' {
			self.params.get(&param).map(|s| s.to_string()).unwrap_or("0".into())
		} else {
			self.params.get(&param).map(|s| s.to_string()).unwrap_or_default()
		}
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

pub fn set_last_input(input: &str) {
	LAST_INPUT.with(|input_ref| {
		let mut last_input = input_ref.borrow_mut();
		last_input.clear();
		last_input.push_str(input);
	})
}

pub fn slice_last_input(range: Range<usize>) -> String {
	LAST_INPUT.with(|input_ref| {
		let input = input_ref.borrow();
		input[range].to_string()
	})
}

pub fn get_status() -> i32 {
	read_vars(|v| v.get_param('?')).parse::<i32>().unwrap()
}
pub fn set_status(code: i32) {
	write_vars(|v| v.set_param('?', &code.to_string()))
}

pub fn source_file(path: PathBuf) -> ShResult<()> {
	let mut file = OpenOptions::new()
		.read(true)
		.open(path)?;

	let mut buf = String::new();
	file.read_to_string(&mut buf)?;
	exec_input(&buf, None)?;
	Ok(())
}
