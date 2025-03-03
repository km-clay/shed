use std::fmt::Display;

use crate::parse::lex::Span;
use crate::prelude::*;

pub type ShResult<T> = Result<T,ShErr>;


pub trait Blame {
	/// Blame a span for a propagated error. This will convert a ShErr::Simple into a ShErr::Full
	/// This will also set the span on a ShErr::Builder
	fn blame(self, span: Span) -> Self;

	/// If an error is propagated to this point, then attempt to blame a span.
	/// If the error in question has already blamed a span, don't overwrite it.
	/// Used as a last resort in higher level contexts in case an error somehow goes unblamed
	fn try_blame(self, span: Span) -> Self;
}

impl From<std::io::Error> for ShErr {
	fn from(_: std::io::Error) -> Self {
		ShErr::io()
	}
}

impl From<std::env::VarError> for ShErr {
	fn from(value: std::env::VarError) -> Self {
		ShErr::simple(ShErrKind::InternalErr, &value.to_string())
	}
}

impl From<rustyline::error::ReadlineError> for ShErr {
	fn from(value: rustyline::error::ReadlineError) -> Self {
		ShErr::simple(ShErrKind::ParseErr, &value.to_string())
	}
}

impl From<Errno> for ShErr {
	fn from(value: Errno) -> Self {
		ShErr::simple(ShErrKind::Errno, &value.to_string())
	}
}

impl<T> Blame for Result<T,ShErr> {
	fn blame(self, span: Span) -> Self {
		if let Err(mut e) = self {
			e.blame(span);
			Err(e)
		} else {
			self
		}
	}
	fn try_blame(self, span: Span) -> Self {
		if let Err(mut e) = self {
			e.try_blame(span);
			Err(e)
		} else {
			self
		}
	}
}

#[derive(Debug,Clone)]
pub enum ShErrKind {
	IoErr,
	SyntaxErr,
	ParseErr,
	InternalErr,
	ExecFail,
	Errno,
	CmdNotFound,
	CleanExit,
	FuncReturn,
	LoopContinue,
	LoopBreak,
	Null
}

impl Default for ShErrKind {
	fn default() -> Self {
		Self::Null
	}
}

#[derive(Clone,Debug)]
pub enum ShErr {
	Simple { kind: ShErrKind, message: String },
	Full { kind: ShErrKind, message: String, span: Span },
}

impl ShErr {
	pub fn simple<S: Into<String>>(kind: ShErrKind, message: S) -> Self {
		Self::Simple { kind, message: message.into() }
	}
	pub fn io() -> Self {
		io::Error::last_os_error().into()
	}
	pub fn full<S: Into<String>>(kind: ShErrKind, message: S, span: Span) -> Self {
		Self::Full { kind, message: message.into(), span }
	}
	pub fn try_blame(&mut self, blame: Span) {
		match self {
			Self::Full {..} => {
				/* Do not overwrite */
			}
			Self::Simple { kind, message } => {
				*self = Self::Full { kind: core::mem::take(kind), message: core::mem::take(message), span: blame }
			}
		}
	}
	pub fn blame(&mut self, blame: Span) {
		match self {
			Self::Full { kind: _, message: _, span } => {
				*span = blame;
			}
			Self::Simple { kind, message } => {
				*self = Self::Full { kind: core::mem::take(kind), message: core::mem::take(message), span: blame }
			}
		}
	}
	pub fn with_msg(&mut self, new_message: String) {
		match self {
			Self::Full { kind: _, message, span: _ } => {
				*message = new_message
			}
			Self::Simple { kind: _, message } => {
				*message = new_message
			}
		}
	}
	pub fn with_kind(&mut self, new_kind: ShErrKind) {
		match self {
			Self::Full { kind, message: _, span: _ } => {
				*kind = new_kind
			}
			Self::Simple { kind, message: _ } => {
				*kind = new_kind
			}
		}
	}
	pub fn display_kind(&self) -> String {
		match self {
			ShErr::Simple { kind, message: _ } |
			ShErr::Full { kind, message: _, span: _ } => {
				match kind {
						ShErrKind::IoErr => "I/O Error: ".into(),
						ShErrKind::SyntaxErr => "Syntax Error: ".into(),
						ShErrKind::ParseErr => "Parse Error: ".into(),
						ShErrKind::InternalErr => "Internal Error: ".into(),
						ShErrKind::ExecFail => "Execution Failed: ".into(),
						ShErrKind::Errno => "ERRNO: ".into(),
						ShErrKind::CmdNotFound => "Command not found: ".into(),
						ShErrKind::CleanExit |
						ShErrKind::FuncReturn |
						ShErrKind::LoopContinue |
						ShErrKind::LoopBreak |
						ShErrKind::Null => "".into()
				}
			}
		}
	}
	pub fn get_window(&self) -> String {
		if let ShErr::Full { kind: _, message: _, span } = self {
			let window = span.get_slice();
			window.split_once('\n').unwrap_or((&window,"")).0.to_string()
		} else {
			String::new()
		}
	}
}

impl Display for ShErr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let error_display = match self {
			ShErr::Simple { kind: _, message } => format!("{}{}",self.display_kind(),message),
			ShErr::Full { kind: _, message, span } => {
				let (offset,line_no,line_text) = span.get_line();
				let dist = span.end() - span.start();
				let padding = " ".repeat(offset);
				let line_inner = "~".repeat(dist.saturating_sub(2));
				let err_kind = &self.display_kind().styled(Style::Red | Style::Bold);
				let stat_line = format!("[{}:{}] - {}{}",line_no,offset,err_kind,message);
				let indicator_line = if dist == 1 {
					format!("{}^",padding)
				} else {
					format!("{}^{}^",padding,line_inner)
				};
				let error_full = format!("\n{}\n{}\n{}\n",stat_line,line_text,indicator_line);

				error_full
			}
		};
		write!(f,"{}",error_display)
	}
}
