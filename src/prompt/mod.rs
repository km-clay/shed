pub mod highlight;
pub mod readline;
pub mod statusline;


use readline::{FernVi, Readline};

use crate::{
  expand::expand_prompt, libsh::error::ShResult, prelude::*, shopt::FernEditMode,
};

/// Initialize the line editor
fn get_prompt() -> ShResult<String> {
  let Ok(prompt) = env::var("PS1") else {
    // default prompt expands to:
    //
    // username@hostname
    // short/path/to/pwd/
    // $ _
    let default =
      "\\e[0m\\n\\e[1;0m\\u\\e[1;36m@\\e[1;31m\\h\\n\\e[1;36m\\W\\e[1;32m/\\n\\e[1;32m\\$\\e[0m ";
    return expand_prompt(default);
  };
	let sanitized = format!("\\e[0m{prompt}");
	flog!(DEBUG, "Using prompt: {}", sanitized.replace("\n", "\\n"));

  expand_prompt(&sanitized)
}

pub fn readline(edit_mode: FernEditMode, initial: Option<&str>) -> ShResult<String> {
  let prompt = get_prompt()?;
  let mut reader: Box<dyn Readline> = match edit_mode {
    FernEditMode::Vi => {
			let mut fern_vi = FernVi::new(Some(prompt))?;
			if let Some(input) = initial {
				fern_vi = fern_vi.with_initial(input)
			}
			Box::new(fern_vi) as Box<dyn Readline>
		}
    FernEditMode::Emacs => todo!(), // idk if I'm ever gonna do this one actually, I don't use emacs
  };
  reader.readline()
}
