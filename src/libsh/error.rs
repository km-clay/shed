use std::fmt::Display;

use crate::{
	libsh::term::{Style, Styled},
	parse::lex::Span,
	prelude::*
};

pub type ShResult<T> = Result<T,ShErr>;

pub trait ShResultExt {
	fn blame(self, span: Span) -> Self;
	fn try_blame(self, span: Span) -> Self;
}

impl<T> ShResultExt for Result<T,ShErr> {
	/// Blame a span for an error
	fn blame(self, new_span: Span) -> Self {
		let Err(e) = self else {
			return self
		};
		match e {
			ShErr::Simple { kind, msg } |
			ShErr::Full { kind, msg, span: _ } => Err(ShErr::full(kind, msg, new_span)),
		}
	}
	/// Blame a span if no blame has been assigned yet
	fn try_blame(self, new_span: Span) -> Self {
		let Err(e) = &self else {
			return self
		};
		match e {
			ShErr::Simple { kind, msg } => Err(ShErr::full(*kind, msg, new_span)),
			ShErr::Full { kind: _, msg: _, span: _ } => self
		}
	}
}

#[derive(Debug)]
pub enum ShErr {
	Simple { kind: ShErrKind, msg: String },
	Full { kind: ShErrKind, msg: String, span: Span }
}

impl ShErr {
	pub fn simple(kind: ShErrKind, msg: impl Into<String>) -> Self {
		let msg = msg.into();
		Self::Simple { kind, msg }
	}
	pub fn full(kind: ShErrKind, msg: impl Into<String>, span: Span) -> Self {
		let msg = msg.into();
		Self::Full { kind, msg, span }
	}
	pub fn unpack(self) -> (ShErrKind,String,Option<Span>) {
		match self {
			ShErr::Simple { kind, msg } => (kind,msg,None),
			ShErr::Full { kind, msg, span } => (kind,msg,Some(span))
		}
	}
	pub fn with_span(sherr: ShErr, span: Span) -> Self {
		let (kind,msg,_) = sherr.unpack();
		let span = span.into();
		Self::Full { kind, msg, span }
	}
	pub fn kind(&self) -> &ShErrKind {
		match self {
			ShErr::Simple { kind, msg: _ } |
			ShErr::Full { kind, msg: _, span: _ } => kind
		}
	}
	pub fn get_window(&self) -> Vec<(usize,String)> {
		let ShErr::Full { kind: _, msg: _, span } = self else {
			unreachable!()
		};
		let mut total_len: usize = 0;
		let mut total_lines: usize = 1;
		let mut lines = vec![];
		let mut cur_line = String::new();

		let src = span.get_source();
		let mut chars = src.chars();

		while let Some(ch) = chars.next() {
			total_len += ch.len_utf8();
			cur_line.push(ch);
			if ch == '\n' {
				total_lines += 1;

				if total_len >= span.start {
					let line = (
						total_lines,
						mem::take(&mut cur_line)
					);
					lines.push(line);
				}
				if total_len >= span.end {
					break
				}
			}
		}

		if !cur_line.is_empty() {
			let line = (
				total_lines,
				mem::take(&mut cur_line)
			);
			lines.push(line);
		}

		lines
	}
	pub fn get_line_col(&self) -> (usize,usize) {
		let ShErr::Full { kind: _, msg: _, span } = self else {
			unreachable!()
		};

		let mut lineno = 1;
		let mut colno = 1;
		let src = span.get_source();
		let mut chars = src.chars().enumerate();
		while let Some((pos,ch)) = chars.next() {
			if pos >= span.start {
				break
			}
			if ch == '\n' {
				lineno += 1;
				colno = 1;
			} else {
				colno += 1;
			}
		}
		(lineno,colno)
	}
}

impl Display for ShErr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Simple { msg, kind: _ } => writeln!(f, "{}", msg),
			Self::Full { msg, kind, span: _ } => {
				let window = self.get_window();
				let mut lineno_pad_count = 0;
				for (lineno,_) in window.clone() {
					if lineno.to_string().len() > lineno_pad_count {
						lineno_pad_count = lineno.to_string().len() + 1
					}
				}
				let (line,col) = self.get_line_col();
				let line = line.styled(Style::Cyan | Style::Bold);
				let col = col.styled(Style::Cyan | Style::Bold);
				let kind = kind.styled(Style::Red | Style::Bold);
				let padding = " ".repeat(lineno_pad_count);
				let arrow = "->".styled(Style::Cyan | Style::Bold);
				writeln!(f,
					"{padding}{arrow} [{line};{col}] - {kind}",
				)?;

				let mut bar = format!("{padding}|");
				bar = bar.styled(Style::Cyan | Style::Bold);
				writeln!(f,"{bar}")?;

				for (lineno,line) in window {
					let lineno = lineno.to_string();
					let mut prefix = format!("{padding}|");
					prefix.replace_range(0..lineno.len(), &lineno);
					prefix = prefix.styled(Style::Cyan | Style::Bold);
					writeln!(f,"{prefix} {line}")?;
				}

				writeln!(f,"{bar}")?;

				let bar_break = "-".styled(Style::Cyan | Style::Bold);
				writeln!(f,
					"{padding}{bar_break} {msg}",
				)
			}
		}
	}
}

impl From<std::io::Error> for ShErr {
	fn from(_: std::io::Error) -> Self {
		let msg = std::io::Error::last_os_error();
		ShErr::simple(ShErrKind::IoErr, msg.to_string())
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

#[derive(Debug,Clone,Copy)]
pub enum ShErrKind {
	IoErr,
	SyntaxErr,
	ParseErr,
	InternalErr,
	ExecFail,
	ResourceLimitExceeded,
	BadPermission,
	Errno,
	FileNotFound,
	CmdNotFound,
	CleanExit,
	FuncReturn,
	LoopContinue,
	LoopBreak,
	Null
}

impl Display for ShErrKind {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let output = match self {
			ShErrKind::IoErr => "I/O Error",
			ShErrKind::SyntaxErr => "Syntax Error",
			ShErrKind::ParseErr => "Parse Error",
			ShErrKind::InternalErr => "Internal Error",
			ShErrKind::ExecFail => "Execution Failed",
			ShErrKind::ResourceLimitExceeded => "Resource Limit Exceeded",
			ShErrKind::BadPermission => "Bad Permissions",
			ShErrKind::Errno => "ERRNO",
			ShErrKind::FileNotFound => "File Not Found",
			ShErrKind::CmdNotFound => "Command Not Found",
			ShErrKind::CleanExit => "",
			ShErrKind::FuncReturn => "",
			ShErrKind::LoopContinue => "",
			ShErrKind::LoopBreak => "",
			ShErrKind::Null => "",
		};
		write!(f,"{output}")
	}
}
