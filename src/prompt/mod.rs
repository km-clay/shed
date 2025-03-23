pub mod readline;

use std::path::Path;

use readline::FernReadline;
use rustyline::{error::ReadlineError, history::FileHistory, Editor};

use crate::{expand::expand_prompt, libsh::{error::ShResult, term::{Style, Styled}}, prelude::*};

fn init_rl() -> ShResult<Editor<FernReadline,FileHistory>> {
	let rl = FernReadline::new();
	let mut editor = Editor::new()?;
	editor.set_helper(Some(rl));
	editor.load_history(&Path::new("/home/pagedmov/.fernhist"))?;
	Ok(editor)
}

fn get_prompt() -> ShResult<String> {
	let Ok(prompt) = env::var("PS1") else {
		return Ok("$ ".styled(Style::Green | Style::Bold))
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
			return Err(e.into())
		}
	}
}
