use crate::prelude::*;

#[derive(Clone,Debug)]
pub struct LogTab {
	aliases: HashMap<String,String>,
	functions: HashMap<String,String>
}

impl LogTab {
	pub fn new() -> Self {
		Self {
			aliases: HashMap::new(),
			functions: HashMap::new()
		}
	}
	pub fn get_alias(&self,name: &str) -> Option<&str> {
		self.aliases.get(name).map(|a| a.as_str())
	}
	pub fn set_alias(&mut self, name: &str, body: &str) {
		self.aliases.insert(name.to_string(),body.trim().to_string());
	}
	pub fn get_function(&self,name: &str) -> Option<&str> {
		self.functions.get(name).map(|a| a.as_str())
	}
	pub fn set_function(&mut self, name: &str, body: &str) {
		self.functions.insert(name.to_string(),body.trim().to_string());
	}
}
