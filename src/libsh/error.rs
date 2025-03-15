use std::{fmt::Display, str::FromStr};

use crate::{parse::lex::Span, prelude::*};

pub type ShResult<'s,T> = Result<T,ShErr<'s>>;

#[derive(Debug)]
pub enum ShErr<'s> {
	Simple { kind: ShErrKind, msg: String },
	Full { kind: ShErrKind, msg: String, span: Span<'s> }
}

impl<'s> ShErr<'s> {
	pub fn simple(kind: ShErrKind, msg: impl Into<String>) -> Self {
		let msg = msg.into();
		Self::Simple { kind, msg }
	}
	pub fn full(kind: ShErrKind, msg: impl Into<String>, span: Span<'s>) -> Self {
		let msg = msg.into();
		Self::Full { kind, msg, span }
	}
	pub fn unpack(self) -> (ShErrKind,String,Option<Span<'s>>) {
		match self {
			ShErr::Simple { kind, msg } => (kind,msg,None),
			ShErr::Full { kind, msg, span } => (kind,msg,Some(span))
		}
	}
	pub fn with_span(sherr: ShErr, span: Span<'s>) -> Self {
		let (kind,msg,_) = sherr.unpack();
		Self::Full { kind, msg, span }
	}
}

impl<'s> Display for ShErr<'s> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Simple { msg, kind: _ } => writeln!(f, "{}", msg),
			Self::Full { msg, kind: _, span: _ } => writeln!(f, "{}", msg)
		}
	}
}

impl<'s> From<std::io::Error> for ShErr<'s> {
	fn from(_: std::io::Error) -> Self {
		let msg = std::io::Error::last_os_error();
		ShErr::simple(ShErrKind::IoErr, msg.to_string())
	}
}

impl<'s> From<std::env::VarError> for ShErr<'s> {
	fn from(value: std::env::VarError) -> Self {
		ShErr::simple(ShErrKind::InternalErr, &value.to_string())
	}
}

impl<'s> From<rustyline::error::ReadlineError> for ShErr<'s> {
	fn from(value: rustyline::error::ReadlineError) -> Self {
		ShErr::simple(ShErrKind::ParseErr, &value.to_string())
	}
}

impl<'s> From<Errno> for ShErr<'s> {
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
