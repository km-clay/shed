pub mod highlight;
pub mod readline;

use std::path::Path;

use readline::{FernVi, Readline};

use crate::{
  expand::expand_prompt, libsh::error::ShResult, prelude::*, shopt::FernEditMode,
  state::read_shopts,
};

/// Initialize the line editor
fn get_prompt() -> ShResult<String> {
  let Ok(prompt) = env::var("PS1") else {
    // prompt expands to:
    //
    // username@hostname
    // short/path/to/pwd/
    // $ _
    let default =
      "\\n\\e[1;0m\\u\\e[1;36m@\\e[1;31m\\h\\n\\e[1;36m\\W\\e[1;32m/\\n\\e[1;32m\\$\\e[0m ";
    return expand_prompt(default);
  };

  expand_prompt(&prompt)
}

pub fn readline(edit_mode: FernEditMode, initial: Option<&str>) -> ShResult<String> {
  let prompt = get_prompt()?;
  let mut reader: Box<dyn Readline> = match edit_mode {
    FernEditMode::Vi => {
			let mut fern_vi = FernVi::new(Some(prompt))?;
			if let Some(input) = initial {
				fern_vi = fern_vi.with_initial(&input)
			}
			Box::new(fern_vi) as Box<dyn Readline>
		}
    FernEditMode::Emacs => todo!(), // idk if I'm ever gonna do this one actually, I don't use emacs
  };
  reader.readline()
}
