use crate::prelude::*;
use readline::SynHelper;
use rustyline::{config::Configurer, history::{DefaultHistory, History}, ColorMode, CompletionType, Config, DefaultEditor, EditMode, Editor};

pub mod readline;
pub mod highlight;
pub mod validate;

fn init_rl<'a>(shenv: &'a mut ShEnv) -> Editor<SynHelper<'a>, DefaultHistory> {
	let hist_path = std::env::var("FERN_HIST").unwrap_or_default();
	let mut config = Config::builder()
		.max_history_size(1000).unwrap()
		.history_ignore_dups(true).unwrap()
		.completion_prompt_limit(100)
		.edit_mode(EditMode::Vi)
		.color_mode(ColorMode::Enabled)
		.tab_stop(2)
		.build();

	let mut editor = Editor::with_config(config).unwrap();
	editor.set_completion_type(CompletionType::List);
	editor.set_helper(Some(SynHelper::new(shenv)));
	if !hist_path.is_empty() {
		editor.load_history(&PathBuf::from(hist_path)).unwrap();
	}
	editor
}

pub fn read_line(shenv: &mut ShEnv) -> ShResult<String> {
	log!(TRACE, "Entering prompt");
	let prompt = "$ ".styled(Style::Green | Style::Bold);
	let mut editor = init_rl(shenv);
	match editor.readline(&prompt) {
		Ok(line) => {
			if !line.is_empty() {
				let hist_path = std::env::var("FERN_HIST").ok();
				editor.history_mut().add(&line).unwrap();
				if let Some(path) = hist_path {
					editor.history_mut().save(&PathBuf::from(path)).unwrap();
				}
			}
			Ok(line)
		},
		Err(rustyline::error::ReadlineError::Eof) => {
			kill(Pid::this(), Signal::SIGQUIT)?;
			Ok(String::new())
		}
		Err(rustyline::error::ReadlineError::Interrupted) => {
			Ok(String::new())
		}
		Err(e) => {
			log!(ERROR, e);
			Err(e.into())
		}
	}
}
