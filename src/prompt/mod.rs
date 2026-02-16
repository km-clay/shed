pub mod highlight;
pub mod readline;
pub mod statusline;


use crate::{expand::expand_prompt, libsh::error::ShResult, prelude::*};

/// Initialize the line editor
pub fn get_prompt() -> ShResult<String> {
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
