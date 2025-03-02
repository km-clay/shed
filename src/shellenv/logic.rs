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
	pub fn get_function(&self,name: &str) -> Option<&str> {
		self.functions.get(name).map(|a| a.as_str())
	}
}
