pub mod readline;
pub mod highlight;

use std::path::Path;

use readline::FernReader;

use crate::{expand::expand_prompt, libsh::error::ShResult, prelude::*, state::read_shopts};

/// Initialize the line editor
fn get_prompt() -> ShResult<String> {
	let Ok(prompt) = env::var("PS1") else {
		// username@hostname
		// short/path/to/pwd/
		// $
		let default = "\\e[1;0m\\u\\e[1;36m@\\e[1;31m\\h\\n\\e[1;36m\\W\\e[1;32m/\\n\\e[1;32m\\$\\e[0m ";
		return Ok(format!("\n{}",expand_prompt(default)?))
	};

	Ok(format!("\n{}",expand_prompt(&prompt)?))
}

pub fn read_line() -> ShResult<String> {
	let prompt = get_prompt()?;
	let mut reader = FernReader::new(prompt);
	reader.readline()
}
