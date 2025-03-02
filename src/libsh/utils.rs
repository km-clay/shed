use core::{arch::asm, fmt::{self, Debug, Display, Write}, ops::Deref};
use std::{os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd}, str::FromStr};

use nix::libc::getpgrp;

use crate::{expand::{expand_vars::{expand_dquote, expand_var}, tilde::expand_tilde}, prelude::*};

pub trait StrOps {
	fn trim_quotes(&self) -> String;
}

pub trait ArgVec {
	fn as_strings(self, shenv: &mut ShEnv) -> Vec<String>;
	fn drop_first(self) -> Vec<Token>;
}

impl ArgVec for Vec<Token> {
	/// Converts the contained tokens into strings.
	/// This function also performs token expansion.
	fn as_strings(self, shenv: &mut ShEnv) -> Vec<String> {
		let mut argv_iter = self.into_iter();
		let mut argv_processed = vec![];
		while let Some(arg) = argv_iter.next() {
			match arg.rule() {
				TkRule::VarSub => {
					let mut tokens = expand_var(arg, shenv).into_iter();
					while let Some(token) = tokens.next() {
						argv_processed.push(token.to_string())
					}
				}
				TkRule::TildeSub => {
					let expanded = expand_tilde(arg);
					argv_processed.push(expanded);
				}
				TkRule::DQuote => {
					let expanded = expand_dquote(arg, shenv);
					argv_processed.push(expanded)
				}
				_ => {
					argv_processed.push(arg.to_string())
				}
			}
		}
		argv_processed
	}
	/// This is used to ignore the first argument
	/// Most commonly used in builtins where execvpe is not used
	fn drop_first(self) -> Vec<Token> {
		self[1..].to_vec()
	}
}

#[macro_export]
macro_rules! test {
	($test:block) => {
		$test
			exit(1)
	};
}

#[derive(Clone, Copy, PartialEq, PartialOrd, Ord, Eq , Debug)]
#[repr(i32)]
pub enum LogLevel {
	ERROR = 1,
	WARN = 2,
	INFO = 3,
	DEBUG = 4,
	TRACE = 5,
	NULL = 0
}

impl Display for LogLevel {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			ERROR => write!(f,"{}",style_text("ERROR", Style::Red | Style::Bold)),
			WARN => write!(f,"{}",style_text("WARN", Style::Yellow | Style::Bold)),
			INFO => write!(f,"{}",style_text("INFO", Style::Green | Style::Bold)),
			DEBUG => write!(f,"{}",style_text("DEBUG", Style::Magenta | Style::Bold)),
			TRACE => write!(f,"{}",style_text("TRACE", Style::Blue | Style::Bold)),
			NULL => write!(f,"")
		}
	}
}

#[macro_export]
macro_rules! log {
	($level:expr, $($var:ident),+) => {{
		$(
			let var_name = stringify!($var);
			if $level <= log_level() {
				let file = file!();
				let file_styled = style_text(file,Style::Cyan);
				let line = line!();
				let line_styled = style_text(line,Style::Cyan);
				let logged = format!("[{}][{}:{}] {} = {:#?}",$level, file_styled,line_styled,var_name, &$var);

				write(borrow_fd(2),format!("{}\n",logged).as_bytes()).unwrap();
			}
		)+
	}};

	($level:expr, $lit:literal) => {{
		if $level <= log_level() {
			let file = file!();
			let file_styled = style_text(file, Style::Cyan);
			let line = line!();
			let line_styled = style_text(line, Style::Cyan);
			let logged = format!("[{}][{}:{}] {}", $level, file_styled, line_styled, $lit);
			write(borrow_fd(2), format!("{}\n", logged).as_bytes()).unwrap();
		}
	}};

	($level:expr, $($arg:tt)*) => {{
		if $level <= log_level() {
			let formatted = format!($($arg)*);
			let file = file!();
			let file_styled = style_text(file, Style::Cyan);
			let line = line!();
			let line_styled = style_text(line, Style::Cyan);
			let logged = format!("[{}][{}:{}] {}", $level, file_styled, line_styled, formatted);
			write(borrow_fd(2), format!("{}\n", logged).as_bytes()).unwrap();
		}
	}};
}

#[macro_export]
macro_rules! bp {
	($var:expr) => {
		log!($var);
		let mut buf = String::new();
		readln!("Press enter to continue", buf);
	};
	($($arg:tt)*) => {
		log!($(arg)*);
		let mut buf = String::new();
		readln!("Press enter to continue", buf);
	};
}

pub fn borrow_fd<'a>(fd: i32) -> BorrowedFd<'a> {
	unsafe { BorrowedFd::borrow_raw(fd) }
}

// TODO: add more of these
#[derive(Debug,Clone,Copy)]
pub enum RedirType {
	Input,
	Output,
	Append,
	HereDoc,
	HereString
}

#[derive(Debug,Clone)]
pub enum RedirTarget {
	Fd(i32),
	File(PathBuf),
}

pub struct RedirBldr {
	src: Option<i32>,
	op: Option<RedirType>,
	tgt: Option<RedirTarget>,
}

impl RedirBldr {
	pub fn new() -> Self {
		Self { src: None, op: None, tgt: None }
	}
	pub fn with_src(self, src: i32) -> Self {
		Self { src: Some(src), op: self.op, tgt: self.tgt }
	}
	pub fn with_op(self, op: RedirType) -> Self {
		Self { src: self.src, op: Some(op), tgt: self.tgt }
	}
	pub fn with_tgt(self, tgt: RedirTarget) -> Self {
		Self { src: self.src, op: self.op, tgt: Some(tgt) }
	}
	pub fn src(&self) -> Option<i32> {
		self.src
	}
	pub fn op(&self) -> Option<RedirType> {
		self.op
	}
	pub fn tgt(&self) -> Option<&RedirTarget> {
		self.tgt.as_ref()
	}
	pub fn build(self) -> Redir {
		Redir::new(self.src.unwrap(), self.op.unwrap(), self.tgt.unwrap())
	}
}

impl FromStr for RedirBldr {
	type Err = ShErr;
	fn from_str(raw: &str) -> ShResult<Self> {
		let mut redir_bldr = RedirBldr::new().with_src(1);
		let mut chars = raw.chars().peekable();

		let mut raw_src = String::new();
		while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
			raw_src.push(chars.next().unwrap())
		}
		if !raw_src.is_empty() {
			let src = raw_src.parse::<i32>().unwrap();
			redir_bldr = redir_bldr.with_src(src);
		}

		while let Some(ch) = chars.next() {
			match ch {
				'<' => {
					redir_bldr = redir_bldr.with_src(0);
					if chars.peek() == Some(&'<') {
						chars.next();
						if chars.peek() == Some(&'<') {
							chars.next();
							redir_bldr = redir_bldr.with_op(RedirType::HereString);
						} else {
							redir_bldr = redir_bldr.with_op(RedirType::HereDoc);
						}
					} else {
						redir_bldr = redir_bldr.with_op(RedirType::Input);
					}
				}
				'>' => {
					if chars.peek() == Some(&'>') {
						chars.next();
						redir_bldr = redir_bldr.with_op(RedirType::Append);
					} else {
						redir_bldr = redir_bldr.with_op(RedirType::Output);
					}
				}
				'&' => {
					let mut raw_tgt = String::new();
					while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
						raw_tgt.push(chars.next().unwrap())
					}
					let redir_target = RedirTarget::Fd(raw_tgt.parse::<i32>().unwrap());
					redir_bldr = redir_bldr.with_tgt(redir_target);
				}
				_ => unreachable!()
			}
		}
		Ok(redir_bldr)
	}
}

#[derive(Debug,Clone)]
pub struct Redir {
	pub src: i32,
	pub op: RedirType,
	pub tgt: RedirTarget
}

impl Redir {
	pub fn new(src: i32, op: RedirType, tgt: RedirTarget) -> Self {
		Self { src, op, tgt }
	}
}

#[derive(Debug,Clone)]
pub struct CmdRedirs {
	open: Vec<RawFd>,
	targets_fd: Vec<Redir>,
	targets_file: Vec<Redir>
}

impl CmdRedirs {
	pub fn new(mut redirs: Vec<Redir>) -> Self {
		let mut targets_fd = vec![];
		let mut targets_file = vec![];
		while let Some(redir) = redirs.pop() {
			let Redir { src: _, op: _, tgt } = &redir;
			match tgt {
				RedirTarget::Fd(_) => targets_fd.push(redir),
				RedirTarget::File(_) => targets_file.push(redir)
			}
		}
		Self { open: vec![], targets_fd, targets_file }
	}
	pub fn close_all(&mut self) -> ShResult<()> {
		while let Some(fd) = self.open.pop() {
			if let Err(e) = close(fd) {
				self.open.push(fd);
				return Err(e.into())
			}
		}
		Ok(())
	}
	pub fn activate(&mut self) -> ShResult<()> {
		self.open_file_tgts()?;
		self.open_fd_tgts()?;
		Ok(())
	}
	pub fn open_file_tgts(&mut self) -> ShResult<()> {
		while let Some(redir) = self.targets_file.pop() {
			let Redir { src, op, tgt } = redir;
			let src = borrow_fd(src);

			let mut file_fd = if let RedirTarget::File(path) = tgt {
				let flags = match op {
					RedirType::Input => OFlag::O_RDONLY,
					RedirType::Output => OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
					RedirType::Append => OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_APPEND,
					_ => unimplemented!()
				};
				let mode = Mode::from_bits(0o644).unwrap();
				open(&path,flags,mode)?
			} else { unreachable!() };

			dup2(file_fd.as_raw_fd(),src.as_raw_fd())?;
			close(file_fd.as_raw_fd())?;
			self.open.push(src.as_raw_fd());
		}
		Ok(())
	}
	pub fn open_fd_tgts(&mut self) -> ShResult<()> {
		while let Some(redir) = self.targets_fd.pop() {
			let Redir { src, op: _, tgt } = redir;
			let mut tgt = if let RedirTarget::Fd(fd) = tgt {
				borrow_fd(fd)
			} else { unreachable!() };
			let src = borrow_fd(src);
			dup2(tgt.as_raw_fd(), src.as_raw_fd())?;
			close(tgt.as_raw_fd())?;
			self.open.push(src.as_raw_fd());
		}
		Ok(())
	}
}

pub fn trim_quotes(s: impl ToString) -> String {
	let s = s.to_string();
	if s.starts_with('"') && s.ends_with('"') {
		s.trim_matches('"').to_string()
	} else if s.starts_with('\'') && s.ends_with('\'') {
		s.trim_matches('\'').to_string()
	} else {
		s
	}
}
