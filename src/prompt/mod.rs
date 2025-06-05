pub mod readline;
pub mod highlight;

use std::path::Path;

use readline::{FernVi, Readline};

use crate::{expand::expand_prompt, libsh::error::ShResult, prelude::*, shopt::FernEditMode, state::read_shopts};

/// Initialize the line editor
fn get_prompt() -> ShResult<String> {
	let Ok(prompt) = env::var("PS1") else {
		// prompt expands to:
		//
		// username@hostname
		// short/path/to/pwd/
		// $ _
		let default = "\\n\\e[1;0m\\u\\e[1;36m@\\e[1;31m\\h\\n\\e[1;36m\\W\\e[1;32m/\\n\\e[1;32m\\$\\e[0m ";
		return expand_prompt(default)
	};

	expand_prompt(&prompt)
}

pub fn readline(edit_mode: FernEditMode) -> ShResult<String> {
	let prompt = get_prompt()?;
	let mut reader: Box<dyn Readline> = match edit_mode {
		FernEditMode::Vi => Box::new(FernVi::new(Some(prompt))),
		FernEditMode::Emacs => todo!()
	};
	reader.readline()
}
