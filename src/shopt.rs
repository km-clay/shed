use std::str::FromStr;

use rustyline::EditMode;

use crate::{libsh::error::{ShErr, ShErrKind, ShResult}, prelude::*, state::LogTab};

#[derive(Clone, Debug)]
pub enum BellStyle {
	Audible,
	Visible,
	Disable,
}


impl FromStr for BellStyle {
	type Err = ShErr;
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s.to_ascii_uppercase().as_str() {
			"audible" => Ok(Self::Audible),
			"visible" => Ok(Self::Visible),
			"disable" => Ok(Self::Disable),
			_ => return Err(
				ShErr::simple(
					ShErrKind::SyntaxErr,
					format!("Invalid bell style '{s}'")
				)
			)
		}
	}
}

#[derive(Clone, Debug)]
pub enum FernEditMode {
	Vi,
	Emacs
}

impl Into<EditMode> for FernEditMode {
	fn into(self) -> EditMode {
		match self {
			Self::Vi => EditMode::Vi,
			Self::Emacs => EditMode::Emacs
		}
	}
}

impl FromStr for FernEditMode {
	type Err = ShErr;
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s.to_ascii_lowercase().as_str() {
			"vi" => Ok(Self::Vi),
			"emacs" => Ok(Self::Emacs),
			_ => return Err(
				ShErr::simple(
					ShErrKind::SyntaxErr,
					format!("Invalid edit mode '{s}'")
				)
			)
		}
	}
}

#[derive(Clone, Debug)]
pub struct ShOpts {
	core: ShOptCore,
	prompt: ShOptPrompt
}

impl Default for ShOpts {
	fn default() -> Self {
		let core = ShOptCore {
			dotglob: false,
			autocd: false,
			hist_ignore_dupes: true,
			max_hist: 1000,
			int_comments: true,
			auto_hist: true,
			bell_style: BellStyle::Audible,
			max_recurse_depth: 1000,
		};

		let prompt = ShOptPrompt {
			trunc_prompt_path: 3,
			edit_mode: FernEditMode::Vi,
			comp_limit: 100,
			prompt_highlight: true,
			tab_stop: 4,
			custom: LogTab::new()
		};

		Self { core, prompt }
	}
}

impl ShOpts {
	pub fn get(query: &str) -> ShResult<String> {
		todo!();
		// TODO: handle escapes?
		let mut query = query.split('.');
		//let Some(key) = query.next() else {

		//};
	}
}

#[derive(Clone, Debug)]
pub struct ShOptCore {
	pub dotglob: bool,
	pub autocd: bool,
	pub hist_ignore_dupes: bool,
	pub max_hist: usize,
	pub int_comments: bool,
	pub auto_hist: bool,
	pub bell_style: BellStyle,
	pub max_recurse_depth: usize,
}

#[derive(Clone, Debug)]
pub struct ShOptPrompt {
	pub trunc_prompt_path: usize,
	pub edit_mode: FernEditMode,
	pub comp_limit: usize,
	pub prompt_highlight: bool,
	pub tab_stop: usize,
	pub custom: LogTab // Contains functions for prompt modules
}
