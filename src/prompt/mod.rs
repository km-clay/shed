pub mod readline;
pub mod highlight;

use std::path::Path;

use readline::FernReadline;
use rustyline::{error::ReadlineError, history::FileHistory, ColorMode, Config, Editor};

use crate::{expand::expand_prompt, libsh::error::ShResult, prelude::*, state::read_shopts};

/// Initialize the line editor
fn init_rl() -> ShResult<Editor<FernReadline,FileHistory>> {
	let rl = FernReadline::new();

	let tab_stop = read_shopts(|s| s.prompt.tab_stop);
	let edit_mode = read_shopts(|s| s.prompt.edit_mode).into();
	let bell_style = read_shopts(|s| s.core.bell_style).into();
	let ignore_dups = read_shopts(|s| s.core.hist_ignore_dupes);
	let comp_limit = read_shopts(|s| s.prompt.comp_limit);
	let auto_hist = read_shopts(|s| s.core.auto_hist);
	let max_hist = read_shopts(|s| s.core.max_hist);
	let color_mode = match read_shopts(|s| s.prompt.prompt_highlight) {
		true => ColorMode::Enabled,
		false => ColorMode::Disabled,
	};

	let config = Config::builder()
		.tab_stop(tab_stop)
		.indent_size(1)
		.edit_mode(edit_mode)
		.bell_style(bell_style)
		.color_mode(color_mode)
		.history_ignore_dups(ignore_dups).unwrap()
		.completion_prompt_limit(comp_limit)
		.auto_add_history(auto_hist)
		.max_history_size(max_hist).unwrap()
		.build();

	let mut editor = Editor::with_config(config).unwrap();

	editor.set_helper(Some(rl));
	editor.load_history(&Path::new("/home/pagedmov/.fernhist"))?;
	Ok(editor)
}

fn get_prompt() -> ShResult<String> {
	let Ok(prompt) = env::var("PS1") else {
		// username@hostname
		// short/path/to/pwd/
		// $
		let default = "\\e[1;0m\\u\\e[1;36m@\\e[1;31m\\h\\n\\e[1;36m\\W\\e[1;32m/\\n\\e[1;32m\\$ ";
		return Ok(format!("\n{}",expand_prompt(default)?))
	};

	Ok(format!("\n{}",expand_prompt(&prompt)?))
}

fn get_hist_path() -> ShResult<PathBuf> {
	if let Ok(path) = env::var("FERN_HIST") {
		Ok(PathBuf::from(path))
	} else {
		let home = env::var("HOME")?;
		let path = PathBuf::from(format!("{home}/.fernhist"));
		Ok(path)
	}

}

pub fn read_line() -> ShResult<String> {
	assert!(isatty(STDIN_FILENO).unwrap());
	let mut editor = init_rl()?;
	let prompt = get_prompt()?;
	match editor.readline(&prompt) {
		Ok(line) => {
			if !line.is_empty() {
				let hist_path = get_hist_path()?;
				editor.add_history_entry(&line)?;
				editor.save_history(&hist_path)?;
			}
			Ok(line)
		}
		Err(ReadlineError::Eof) => {
			kill(Pid::this(), Signal::SIGQUIT)?;
			Ok(String::new())
		}
		Err(ReadlineError::Interrupted) => Ok(String::new()),
		Err(e) => {
			Err(e.into())
		}
	}
}
