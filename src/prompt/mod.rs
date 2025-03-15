pub mod history;
pub mod readline;

use std::path::Path;

use history::FernHist;
use readline::FernReadline;
use rustyline::{error::ReadlineError, history::{FileHistory, History}, Config, Editor};

use crate::{libsh::{error::ShResult, term::{Style, Styled}}, prelude::*};

fn init_rl<'s>() -> ShResult<'s,Editor<FernReadline,FernHist>> {
	let hist = FernHist::default();
	let rl = FernReadline::new();
	let config = Config::default();
	let mut editor = Editor::with_history(config,hist)?;
	editor.set_helper(Some(rl));
	Ok(editor)
}

pub fn read_line<'s>() -> ShResult<'s,String> {
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
