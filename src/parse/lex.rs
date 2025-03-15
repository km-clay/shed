use std::{fmt::Display, ops::{Bound, Deref, Range, RangeBounds}};

use bitflags::bitflags;

use crate::{libsh::error::{ShErr, ShErrKind}, prelude::*};

pub const KEYWORDS: [&'static str;14] = [
	"if",
	"then",
	"elif",
	"else",
	"fi",
	"while",
	"until",
	"select",
	"for",
	"in",
	"do",
	"done",
	"case",
	"esac",
];

pub const OPENERS: [&'static str;6] = [
	"if",
	"while",
	"until",
	"for",
	"select",
	"case"
];

#[derive(Clone,PartialEq,Default,Debug)]
pub struct Span<'s> {
	range: Range<usize>,
	source: &'s str
}

impl<'s> Span<'s> {
	/// New `Span`. Wraps a range and a string slice that it refers to.
	pub fn new(range: Range<usize>, source: &'s str) -> Self {
		Span {
			range,
			source,
		}
	}
	/// Slice the source string at the wrapped range
	pub fn as_str(&self) -> &str {
		&self.source[self.start..self.end]
	}
}

/// Allows simple access to the underlying range wrapped by the span
impl<'s> Deref for Span<'s> {
	type Target = Range<usize>;
	fn deref(&self) -> &Self::Target {
		&self.range
	}
}

#[derive(Clone,PartialEq,Debug)]
pub enum TkRule {
	Null,
	SOI, // Start-of-Input
	Str,
	Pipe,
	ErrPipe,
	And,
	Or,
	Bg,
	Sep,
	Redir,
	Expanded { exp: Vec<String> },
	Comment,
	EOI, // End-of-Input
}

impl Default for TkRule {
	fn default() -> Self {
		TkRule::Null
	}
}

#[derive(Clone,Copy,PartialEq,Debug)]
pub enum TkErr {
	Null,
	UntermQuote,
	UntermSubsh,
	UntermEscape,
	UntermBrace,
	BadRedir,
	BadPipe,
	HangingDelim,
}

impl Default for TkErr {
	fn default() -> Self {
		TkErr::Null
	}
}

pub enum TkState {
	Raw,
}

#[derive(Clone,Debug,PartialEq,Default)]
pub struct Tk<'s> {
	pub class: TkRule,
	pub err_span: Option<Span<'s>>,
	pub err: TkErr,
	pub span: Span<'s>,
	pub flags: TkFlags
}

// There's one impl here and then another in expand.rs which has the expansion logic
impl<'s> Tk<'s> {
	pub fn new(class: TkRule, span: Span<'s>) -> Self {
		Self { class, err_span: None, err: TkErr::Null, span, flags: TkFlags::empty() }
	}
	pub fn to_string(&self) -> String {
		match &self.class {
			TkRule::Expanded { exp } => exp.join(" "),
			_ => self.span.as_str().to_string()
		}
	}
	pub fn set_err(&mut self, range: Range<usize>, slice: &'s str, err: TkErr) {
		self.err_span = Some(Span::new(range, slice));
		self.err = err
	}
	pub fn is_err(&self) -> bool {
		self.err_span.is_some()
	}
}

impl<'s> Display for Tk<'s> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.class {
			TkRule::Expanded { exp } => write!(f,"{}",exp.join(" ")),
			_ => write!(f,"{}",self.span.as_str())
		}
	}
}


bitflags! {
	#[derive(Debug,Clone,Copy,PartialEq,Default)]
	pub struct TkFlags: u32 {
		const KEYWORD = 0b0000000000000001;
		/// This is a keyword that opens a new block statement, like 'if' and 'while'
		const OPENER  = 0b0000000000000010;
		const IS_CMD  = 0b0000000000000100;
		const IS_OP   = 0b0000000000001000;
	}
}

pub struct LexStream<'t> {
	source: &'t str,
	pub cursor: usize,
	in_quote: bool,
	flags: LexFlags,
}

bitflags! {
	pub struct LexFlags: u32 {
		/// Return comment tokens
		const LEX_COMMENTS   = 0b00000001;
		/// Allow unfinished input
		const LEX_UNFINISHED = 0b00000010;
		/// The next string-type token is a command name
		const NEXT_IS_CMD    = 0b00000100;
		/// We are in a quotation, so quoting rules apply
		const IN_QUOTE       = 0b00001000;
		/// Only lex strings; used in expansions
		const RAW            = 0b00010000;
		/// The lexer has not produced any tokens yet
		const FRESH          = 0b00010000;
		/// The lexer has no more tokens to produce
		const STALE          = 0b00100000;
	}
}

impl<'t> LexStream<'t> {
	pub fn new(source: &'t str, flags: LexFlags) -> Self {
		let flags = flags | LexFlags::FRESH | LexFlags::NEXT_IS_CMD;
		Self { source, cursor: 0, in_quote: false, flags }
	}
	/// Returns a slice of the source input using the given range
	/// Returns None if the range is out of the bounds of the string slice
	///
	/// Works with any kind of range
	/// examples:
	/// `LexStream.slice(1..10)`
	/// `LexStream.slice(1..=10)`
	/// `LexStream.slice(..10)`
	/// `LexStream.slice(1..)`
	///
	pub fn slice<R: RangeBounds<usize>>(&self, range: R) -> Option<&'t str> {
		// Sketchy downcast
		let start = match range.start_bound() {
			Bound::Included(&start) => start,
			Bound::Excluded(&start) => start + 1,
			Bound::Unbounded => 0
		};
		let end = match range.end_bound() {
			Bound::Included(&end) => end,
			Bound::Excluded(&end) => end + 1,
			Bound::Unbounded => self.source.len()
		};
		self.source.get(start..end)
	}
	pub fn slice_from_cursor(&self) -> Option<&'t str> {
		self.slice(self.cursor..)
	}
	/// The next string token is a command name
	pub fn next_is_cmd(&mut self) {
		self.flags |= LexFlags::NEXT_IS_CMD;
	}
	/// The next string token is not a command name
	pub fn next_is_not_cmd(&mut self) {
		self.flags &= !LexFlags::NEXT_IS_CMD;
	}
	pub fn read_redir(&mut self) -> Option<Tk<'t>> {
		assert!(self.cursor <= self.source.len());
		let slice = self.slice(self.cursor..)?;
		let mut pos = self.cursor;
		let mut chars = slice.chars().peekable();
		let mut tk = Tk::default();

		while let Some(ch) = chars.next() {
			match ch {
				'>' => {
					pos += 1;
					if let Some('>') = chars.peek() {
						chars.next();
						pos += 1;
					}
					if let Some('&') = chars.peek() {
						chars.next();
						pos += 1;

						let mut found_fd = false;
						while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
							chars.next();
							found_fd = true;
							pos += 1;
						}


						if !found_fd {
							let err = TkErr::BadRedir;
							tk = self.get_token(self.cursor..pos, TkRule::Redir);
							tk.set_err(self.cursor..pos, self.source, err);
							break
						} else {
							tk = self.get_token(self.cursor..pos, TkRule::Redir);
							break
						}
					} else {
						tk = self.get_token(self.cursor..pos, TkRule::Redir);
						break
					}
				}
				'<' => {
					pos += 1;

					for _ in 0..2 {
						if let Some('<') = chars.peek() {
							chars.next();
							pos += 1;
						} else {
							break
						}
					}
					tk = self.get_token(self.cursor..pos, TkRule::Redir);
					break
				}
				'0'..='9' => {
					pos += 1;
					while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
						chars.next();
						pos += 1;
					}
				}
				_ => {
					return None;
				}
			}
		}

		assert!(tk != Tk::default());

		self.cursor = pos;
		Some(tk)
	}
	pub fn read_string(&mut self) -> Tk<'t> {
		assert!(self.cursor <= self.source.len());
		let slice = self.slice_from_cursor().unwrap();
		let mut pos = self.cursor;
		let mut chars = slice.chars();
		let mut quote_pos = None;
		while let Some(ch) = chars.next() {
			match ch {
				'"' | '\'' => {
					self.in_quote = true;
					quote_pos = Some(pos);
					pos += 1;
					while let Some(q_ch) = chars.next() {
						match q_ch {
							'\\' => {
								pos += 2;
								chars.next();
							}
							_ if q_ch == ch => {
								pos += 1;
								self.in_quote = false;
								break
							}
							// Any time an ambiguous character is found
							// we must push the cursor by the length of the character
							// instead of just assuming a length of 1.
							// Allows spans to work for wide characters
							_ => pos += q_ch.len_utf8()
						}
					}
				}
				_ if self.flags.contains(LexFlags::RAW) => {
					if ch.is_whitespace() {
						break;
					} else {
						pos += ch.len_utf8()
					}
				}
				_ if !self.in_quote && is_op(ch) => break,
				_ if is_hard_sep(ch) => break,
				_ => pos += ch.len_utf8()
			}
		}
		let mut new_tk = self.get_token(self.cursor..pos, TkRule::Str);
		if self.in_quote && !self.flags.contains(LexFlags::LEX_UNFINISHED) {
			new_tk.set_err(
				quote_pos.unwrap()..pos,
				self.source,
				TkErr::UntermQuote
			);
		}
		if self.flags.contains(LexFlags::NEXT_IS_CMD) {
			if KEYWORDS.contains(&new_tk.span.as_str()) {
				new_tk.flags |= TkFlags::KEYWORD;
			} else {
				new_tk.flags |= TkFlags::IS_CMD;
				self.next_is_not_cmd();
			}
		}
		self.cursor = pos;
		new_tk
	}
	pub fn get_token(&self, range: Range<usize>, class: TkRule) -> Tk<'t> {
		let span = Span::new(range, self.source);
		Tk::new(class, span)
	}
}

impl<'t> Iterator for LexStream<'t> {
	type Item = Tk<'t>;
	fn next(&mut self) -> Option<Self::Item> {
		assert!(self.cursor <= self.source.len());
		// We are at the end of the input
		if self.cursor == self.source.len() {
			if self.flags.contains(LexFlags::STALE) {
				// We've already returned an EOI token, nothing left to do
				return None
			} else {
				// Return the EOI token
				let token = self.get_token(self.cursor..self.cursor, TkRule::EOI);
				self.flags |= LexFlags::STALE;
				return Some(token)
			}
		}
		// Return the SOI token
		if self.flags.contains(LexFlags::FRESH) {
			self.flags &= !LexFlags::FRESH;
			let token = self.get_token(self.cursor..self.cursor, TkRule::SOI);
			return Some(token)
		}

		// If we are just reading raw words, short circuit here
		// Used for word splitting variable values
		if self.flags.contains(LexFlags::RAW) {
			return Some(self.read_string())
		}

		loop {
			let pos = self.cursor;
			if self.slice(pos..pos+2) == Some("\\\n") {
				self.cursor += 2;
			} else if pos < self.source.len() && is_field_sep(get_char(self.source, pos).unwrap()) {
				self.cursor += 1;
			} else {
				break
			}
		}

		if self.cursor == self.source.len() {
			return None
		}

		let token = match get_char(self.source, self.cursor).unwrap() {
			'\r' | '\n' | ';' => {
				let ch_idx = self.cursor;
				self.cursor += 1;
				self.next_is_cmd();

				while let Some(ch) = get_char(self.source, self.cursor) {
					if is_hard_sep(ch) { // Combine consecutive separators into one, including whitespace
						self.cursor += 1;
					} else {
						break
					}
				}
				self.get_token(ch_idx..self.cursor, TkRule::Sep)
			}
			'#' => {
				let ch_idx = self.cursor;
				self.cursor += 1;

				while let Some(ch) = get_char(self.source, self.cursor) {
					self.cursor += 1;
					if ch == '\n' {
						break
					}
				}

				self.get_token(ch_idx..self.cursor, TkRule::Comment)
			}
			'|' => {
				let ch_idx = self.cursor;
				self.cursor += 1;
				self.next_is_cmd();

				let tk_type = if let Some('|') = get_char(self.source, self.cursor) {
					self.cursor += 1;
					TkRule::Or
				} else if let Some('&') = get_char(self.source, self.cursor) {
					self.cursor += 1;
					TkRule::ErrPipe
				} else {
					TkRule::Pipe
				};

				self.get_token(ch_idx..self.cursor, tk_type)
			}
			'&' => {
				let ch_idx = self.cursor;
				self.cursor += 1;
				self.next_is_cmd();

				let tk_type = if let Some('&') = get_char(self.source, self.cursor) {
					self.cursor += 1;
					TkRule::And
				} else {
					TkRule::Bg
				};
				self.get_token(ch_idx..self.cursor, tk_type)
			}
			_ => {
				if let Some(tk) = self.read_redir() {
					self.next_is_not_cmd();
					tk
				} else {
					self.read_string()
				}
			}
		};
		Some(token)
	}
}


pub fn get_char(src: &str, idx: usize) -> Option<char> {
	src.get(idx..)?.chars().next()
}

/// Is '|', '&', '>', or '<'
pub fn is_op(ch: char) -> bool {
	matches!(ch, '|' | '&' | '>' | '<')
}

/// Is whitespace or a semicolon
pub fn is_hard_sep(ch: char) -> bool {
	matches!(ch, ' ' | '\t' | '\n' | ';')
}

/// Is whitespace, but not a newline
pub fn is_field_sep(ch: char) -> bool {
	matches!(ch, ' ' | '\t')
}
