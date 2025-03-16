use std::{collections::{HashMap, VecDeque}, sync::{LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard}};

use crate::{exec_input, jobs::JobTab, libsh::{error::ShResult, utils::VecDequeExt}, parse::lex::get_char, prelude::*};

pub static JOB_TABLE: LazyLock<RwLock<JobTab>> = LazyLock::new(|| RwLock::new(JobTab::new()));

pub static VAR_TABLE: LazyLock<RwLock<VarTab>> = LazyLock::new(|| RwLock::new(VarTab::new()));


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
	exec_input(&buf)?;
	Ok(())
}
