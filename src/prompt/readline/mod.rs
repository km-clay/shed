use linebuf::LineBuf;
use term::TermReader;

use crate::libsh::error::ShResult;

pub mod term;
pub mod linebuf;
pub mod layout;

pub trait Readline {
	fn readline(&mut self, prompt: Option<String>) -> ShResult<String>;
}

pub struct FernVi {
	reader: TermReader,
	writer: TermWriter,
	editor: LineBuf
}

impl Readline for FernVi {
	fn readline(&mut self, prompt: Option<String>) -> ShResult<String> {
		todo!()
	}
}

impl FernVi {
	pub fn new() -> Self {
		Self {
		}
	}
}

