pub mod history;
pub mod readline;

use std::path::Path;

use history::FernHist;
use readline::FernReadline;
use rustyline::{error::ReadlineError, history::{FileHistory, History}, Config, Editor};

use crate::{libsh::{error::ShResult, term::{Style, Styled}}, prelude::*};

fn init_rl<'s>() -> ShResult<Editor<FernReadline,FileHistory>> {
	let rl = FernReadline::new();
	let mut editor = Editor::new()?;
	editor.set_helper(Some(rl));
	editor.load_history(&Path::new("/home/pagedmov/.fernhist"))?;
	Ok(editor)
}

pub fn read_line<'s>() -> ShResult<String> {
	assert!(isatty(STDIN_FILENO).unwrap());
	let mut editor = init_rl()?;
	let prompt = "$ ".styled(Style::Green | Style::Bold);
	match editor.readline(&prompt) {
		Ok(line) => {
			if !line.is_empty() {
				editor.add_history_entry(&line)?;
				editor.save_history(&Path::new("/home/pagedmov/.fernhist"))?;
			}
			Ok(line)
		}
		Err(ReadlineError::Eof) => std::process::exit(0),
		Err(ReadlineError::Interrupted) => Ok(String::new()),
		Err(e) => {
			return Err(e.into())
		}
	}
}
