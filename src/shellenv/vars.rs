use std::env;

use nix::unistd::{gethostname, User};

use crate::prelude::*;

#[derive(Clone,Debug)]
pub struct VarTab {
	env: HashMap<String,String>,
	params: HashMap<String,String>,
	pos_params: VecDeque<String>,
	vars: HashMap<String,String>
}

impl VarTab {
	pub fn new() -> Self {
		let (params,pos_params) = Self::init_params();
		Self {
			env: Self::init_env(),
			params,
			pos_params,
			vars: HashMap::new(),
		}
	}
	pub fn init_params() -> (HashMap<String,String>, VecDeque<String>) {
		let mut args = std::env::args().collect::<Vec<String>>();
		let mut params = HashMap::new();
		let mut pos_params = VecDeque::new();

		params.insert("@".to_string(), args.join(" "));
		params.insert("#".to_string(), args.len().to_string());

		while let Some(arg) = args.pop() {
			pos_params.fpush(arg);
		}

		(params,pos_params)
	}
	pub fn init_env() -> HashMap<String,String> {
		let pathbuf_to_string = |pb: Result<PathBuf, std::io::Error>| pb.unwrap_or_default().to_string_lossy().to_string();
		// First, inherit any env vars from the parent process
		let mut env_vars = std::env::vars().collect::<HashMap<String,String>>();
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

		env_vars.insert("IFS".into(), " \t\n".into());
		env::set_var("IFS", " \t\n");
		env_vars.insert("HOSTNAME".into(), hostname.clone());
		env::set_var("HOSTNAME", hostname);
		env_vars.insert("UID".into(), uid.to_string());
		env::set_var("UID", uid.to_string());
		env_vars.insert("PPID".into(), getppid().to_string());
		env::set_var("PPID", getppid().to_string());
		env_vars.insert("TMPDIR".into(), "/tmp".into());
		env::set_var("TMPDIR", "/tmp");
		env_vars.insert("TERM".into(), term.clone());
		env::set_var("TERM", term);
		env_vars.insert("LANG".into(), "en_US.UTF-8".into());
		env::set_var("LANG", "en_US.UTF-8");
		env_vars.insert("USER".into(), username.clone());
		env::set_var("USER", username.clone());
		env_vars.insert("LOGNAME".into(), username.clone());
		env::set_var("LOGNAME", username);
		env_vars.insert("PWD".into(), pathbuf_to_string(std::env::current_dir()));
		env::set_var("PWD", pathbuf_to_string(std::env::current_dir()));
		env_vars.insert("OLDPWD".into(), pathbuf_to_string(std::env::current_dir()));
		env::set_var("OLDPWD", pathbuf_to_string(std::env::current_dir()));
		env_vars.insert("HOME".into(), home.clone());
		env::set_var("HOME", home.clone());
		env_vars.insert("SHELL".into(), pathbuf_to_string(std::env::current_exe()));
		env::set_var("SHELL", pathbuf_to_string(std::env::current_exe()));
		env_vars.insert("FERN_HIST".into(),format!("{}/.fern_hist",home));
		env::set_var("FERN_HIST",format!("{}/.fern_hist",home));

		env_vars
	}
	pub fn env(&self) -> &HashMap<String,String> {
		&self.env
	}
	pub fn env_mut(&mut self) -> &mut HashMap<String,String> {
		&mut self.env
	}
	pub fn reset_params(&mut self) {
		self.params.clear();
	}
	pub fn unset_param(&mut self, key: &str) {
		self.params.remove(key);
	}
	pub fn set_param(&mut self, key: &str, val: &str) {
		self.params.insert(key.to_string(), val.to_string());
	}
	pub fn get_param(&self, key: &str) -> &str {
		self.params.get(key).map(|s| s.as_str()).unwrap_or_default()
	}
	/// Push an arg to the back of the positional parameter deque
	pub fn bpush_arg(&mut self, arg: &str) {
		self.pos_params.bpush(arg.to_string());
		self.set_param("@", &self.pos_params.clone().to_vec().join(" "));
		self.set_param("#", &self.pos_params.len().to_string());
	}
	/// Pop an arg from the back of the positional parameter deque
	pub fn bpop_arg(&mut self) -> Option<String> {
		let item = self.pos_params.bpop();
		self.set_param("@", &self.pos_params.clone().to_vec().join(" "));
		self.set_param("#", &self.pos_params.len().to_string());
		item
	}
	/// Push an arg to the front of the positional parameter deque
	pub fn fpush_arg(&mut self, arg: &str) {
		self.pos_params.fpush(arg.to_string());
		self.set_param("@", &self.pos_params.clone().to_vec().join(" "));
		self.set_param("#", &self.pos_params.len().to_string());
	}
	/// Pop an arg from the front of the positional parameter deque
	pub fn fpop_arg(&mut self) -> Option<String> {
		let item = self.pos_params.fpop();
		self.set_param("@", &self.pos_params.clone().to_vec().join(" "));
		self.set_param("#", &self.pos_params.len().to_string());
		item
	}
	pub fn get_var(&self, var: &str) -> &str {
		if let Ok(idx) = var.parse::<usize>() {
			self.pos_params.get(idx).map(|p| p.as_str()).unwrap_or_default()
		} else if let Some(var) = self.env.get(var) {
			var.as_str()
		} else if let Some(param) = self.params.get(var) {
			param.as_str()
		} else {
			self.vars.get(var).map(|v| v.as_str()).unwrap_or_default()
		}
	}
	pub fn set_var(&mut self, var: &str, val: &str) {
		self.vars.insert(var.to_string(), val.to_string());
	}
	pub fn export(&mut self, var: &str, val: &str) {
		self.env.insert(var.to_string(),val.to_string());
		std::env::set_var(var, val);
	}
}
