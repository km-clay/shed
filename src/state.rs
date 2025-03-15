use std::{collections::HashMap, sync::{LazyLock, RwLock, RwLockReadGuard, RwLockWriteGuard}};

use crate::prelude::*;

pub static JOB_TABLE: LazyLock<RwLock<JobTab>> = LazyLock::new(|| RwLock::new(JobTab::new()));

pub static VAR_TABLE: LazyLock<RwLock<VarTab>> = LazyLock::new(|| RwLock::new(VarTab::new()));

pub static ENV_TABLE: LazyLock<RwLock<EnvTab>> = LazyLock::new(|| RwLock::new(EnvTab::new()));

pub struct JobTab {

}

impl JobTab {
	pub fn new() -> Self {
		Self {}
	}
}

pub struct VarTab {
	vars: HashMap<String,String>,
	params: HashMap<char,String>,
}

impl VarTab {
	pub fn new() -> Self {
		let vars = HashMap::new();
		let params = Self::init_params();
		Self { vars, params }
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
		self.vars.get(var).map(|s| s.to_string()).unwrap_or_default()
	}
	pub fn new_var(&mut self, var: &str, val: &str) {
		self.vars.insert(var.to_string(), val.to_string());
	}
	pub fn set_param(&mut self, param: char, val: &str) {
		self.params.insert(param,val.to_string());
	}
	pub fn get_param(&self, param: char) -> String {
		self.params.get(&param).map(|s| s.to_string()).unwrap_or("0".to_string())
	}
}

pub struct EnvTab {

}

impl EnvTab {
	pub fn new() -> Self {
		Self {}
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

/// Read from the environment table
pub fn read_env<T, F: FnOnce(RwLockReadGuard<EnvTab>) -> T>(f: F) -> T {
	let lock = ENV_TABLE.read().unwrap();
	f(lock)
}

/// Write to the environment table
pub fn write_env<T, F: FnOnce(&mut RwLockWriteGuard<EnvTab>) -> T>(f: F) -> T {
	let lock = &mut ENV_TABLE.write().unwrap();
	f(lock)
}

pub fn get_status() -> i32 {
	read_vars(|v| v.get_param('?')).parse::<i32>().unwrap()
}
pub fn set_status(code: i32) {
	write_vars(|v| v.set_param('?', &code.to_string()))
}
