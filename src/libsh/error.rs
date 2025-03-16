use std::{fmt::Display, ops::Range};

use crate::{parse::lex::Span, prelude::*};

pub type ShResult<T> = Result<T,ShErr>;

#[derive(Debug)]
pub struct ErrSpan {
	range: Range<usize>,
	source: String
}

impl<'s> From<Span<'s>> for ErrSpan {
	fn from(value: Span<'s>) -> Self {
		let range = value.range();
		let source = value.get_source().to_string();
		Self { range, source }
	}
}

#[derive(Debug)]
pub enum ShErr {
	Simple { kind: ShErrKind, msg: String },
	Full { kind: ShErrKind, msg: String, span: ErrSpan }
}

impl<'s> ShErr {
	pub fn simple(kind: ShErrKind, msg: impl Into<String>) -> Self {
		let msg = msg.into();
		Self::Simple { kind, msg }
	}
	pub fn full(kind: ShErrKind, msg: impl Into<String>, span: ErrSpan) -> Self {
		let msg = msg.into();
		Self::Full { kind, msg, span }
	}
	pub fn unpack(self) -> (ShErrKind,String,Option<ErrSpan>) {
		match self {
			ShErr::Simple { kind, msg } => (kind,msg,None),
			ShErr::Full { kind, msg, span } => (kind,msg,Some(span))
		}
	}
	pub fn with_span(sherr: ShErr, span: Span<'s>) -> Self {
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
}

impl Display for ShErr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Simple { msg, kind: _ } => writeln!(f, "{}", msg),
			Self::Full { msg, kind: _, span: _ } => writeln!(f, "{}", msg)
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

#[derive(Debug)]
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
