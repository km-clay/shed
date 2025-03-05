use std::fmt::Debug;

use crate::prelude::*;

pub const KEYWORDS: [TkRule;14] = [
	TkRule::If,
	TkRule::Then,
	TkRule::Elif,
	TkRule::Else,
	TkRule::Fi,
	TkRule::While,
	TkRule::Until,
	TkRule::For,
	TkRule::In,
	TkRule::Select,
	TkRule::Do,
	TkRule::Done,
	TkRule::Case,
	TkRule::Esac
];

pub const SEPARATORS: [TkRule; 6] = [
	TkRule::Sep,
	TkRule::AndOp,
	TkRule::OrOp,
	TkRule::PipeOp,
	TkRule::ErrPipeOp,
	TkRule::BgOp,
];

pub trait LexRule {
	fn try_match(input: &str) -> Option<usize>;
}

pub struct Lexer<'a> {
	input: String,
	tokens: Vec<Token>,
	is_command: bool,
	shenv: &'a mut ShEnv,
	consumed: usize
}

impl<'a> Lexer<'a> {
	pub fn new(input: String, shenv: &'a mut ShEnv) -> Self {
		Self { input, tokens: vec![], is_command: true, shenv, consumed: 0  }
	}
	pub fn lex(mut self) -> Vec<Token> {
		unsafe {
			let mut input = self.input.as_str();
			while let Some((mut rule,len)) = TkRule::try_match(input) {
				// If we see a keyword in an argument position, it's actually an ident
				if !self.is_command && KEYWORDS.contains(&rule) {
					rule = TkRule::Ident

				// If we are in a command right now, after this we are in arguments
				} else if self.is_command && rule != TkRule::Whitespace && !KEYWORDS.contains(&rule) {
					self.is_command = false;
				}
				// If we see a separator like && or ;, we are now in a command again
				if SEPARATORS.contains(&rule) {
					self.is_command = true;
				}
				let span = self.shenv.inputman_mut().new_span(self.consumed, self.consumed + len);
				let token = Token::new(rule, span);
				self.consumed += len;
				input = &input[len..];
				self.tokens.push(token);
			}
			if !input.is_empty() {
				log!(WARN, "unconsumed input: {}", input)
			}
			self.tokens
		}
	}
}

#[derive(Clone)]
pub struct Token {
	rule: TkRule,
	span: Rc<RefCell<Span>>
}

impl Token {
	pub fn new(rule: TkRule, span: Rc<RefCell<Span>>) -> Self {
		Self { rule, span }
	}

	pub fn span(&self) -> Rc<RefCell<Span>> {
		self.span.clone()
	}

	pub fn rule(&self) -> TkRule {
		self.rule
	}

	pub fn as_raw(&self, shenv: &mut ShEnv) -> String {
		shenv.input_slice(self.span()).to_string()
	}
}

impl Debug for Token {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let info = (self.rule(),self.span.borrow().start,self.span.borrow().end);
		write!(f,"{:?}",info)
	}
}

#[derive(Debug,Clone)]
pub struct Span {
	start: usize,
	end: usize
}

impl Span {
	pub fn new(start: usize, end: usize) -> Self {
		Self { start, end }
	}
	pub fn start(&self) -> usize {
		self.start
	}
	pub fn end(&self) -> usize {
		self.end
	}
	pub fn clamp_start(&mut self, start: usize) {
		if self.start > start {
			self.start = start
		}
	}
	pub fn clamp_end(&mut self, end: usize) {
		if self.end > end {
			self.end = end
		}
	}
	pub fn shift(&mut self, delta: isize) {
		self.start = self.start.saturating_add_signed(delta);
		self.end = self.end.saturating_add_signed(delta);
	}
}

macro_rules! try_match {
	($rule:ident,$input:expr) => {
		if let Some(len) = $rule::try_match($input) {
			return Some((TkRule::$rule,len))
		}
	};
}

/// For matching on sub-rules
macro_rules! try_match_inner {
	($rule:ident,$input:expr) => {
		if let Some(len) = $rule::try_match($input) {
			return Some(len)
		}
	};
}

macro_rules! tkrule_def {
	($rule:ident, $logic:expr) => {
		#[derive(Debug)]
		pub struct $rule;
		impl LexRule for $rule {
			fn try_match(input: &str) -> Option<usize> {
				$logic(input)
			}
		}
	};
}

#[derive(Debug,Clone,PartialEq,Copy)]
pub enum TkRule {
	Whitespace,
	Comment,
	PipeOp,
	ErrPipeOp,
	AndOp,
	OrOp,
	BgOp,
	RedirOp,
	FuncName,
	BraceGrp,
	ProcSub,
	VarSub,
	TildeSub,
	Subshell,
	CmdSub,
	DQuote,
	SQuote,
	If,
	Then,
	Elif,
	Else,
	Fi,
	While,
	Until,
	For,
	In,
	Select,
	Do,
	Done,
	Case,
	Esac,
	Assign,
	Ident,
	Sep,
}

impl TkRule {
	fn try_match(input: &str) -> Option<(TkRule,usize)> {
		// Specialized rules come first,
		// Generalized rules come last
		try_match!(Whitespace,input);
		try_match!(Comment,input);
		try_match!(VarSub,input);
		try_match!(ProcSub,input);
		try_match!(CmdSub,input);
		try_match!(AndOp,input);
		try_match!(OrOp,input);
		try_match!(PipeOp,input);
		try_match!(ErrPipeOp,input);
		try_match!(BgOp,input);
		try_match!(RedirOp,input);
		try_match!(SQuote,input);
		try_match!(DQuote,input);
		try_match!(FuncName,input);
		try_match!(BraceGrp,input);
		try_match!(TildeSub,input);
		try_match!(Subshell,input);
		try_match!(Sep,input);
		try_match!(Assign,input);
		try_match!(If,input);
		try_match!(Then,input);
		try_match!(Elif,input);
		try_match!(Else,input);
		try_match!(Fi,input);
		try_match!(While,input);
		try_match!(Until,input);
		try_match!(For,input);
		try_match!(In,input);
		try_match!(Select,input);
		try_match!(Do,input);
		try_match!(Done,input);
		try_match!(Case,input);
		try_match!(Esac,input);
		try_match!(Ident,input);
		None
	}
}

tkrule_def!(Comment, |input: &str| {
	let mut chars = input.chars();
	let mut len = 0;

	if let Some('#') = chars.next() {
		len += 1;
		while let Some(ch) = chars.next() {
			len += 1;
			if ch == '\n' {
				break
			}
		}
		Some(len)
	} else {
		None
	}
});

tkrule_def!(Whitespace, |input: &str| {
	let mut chars = input.chars();
	let mut len = 0;
	while let Some(ch) = chars.next() {
		match ch {
			' ' | '\t' => len += 1,
			_ => {
				match len {
					0 => return None,
					_ => return Some(len),
				}
			}
		}
	}
	match len {
		0 => return None,
		_ => return Some(len),
	}
});

tkrule_def!(TildeSub, |input: &str| {
	let mut chars = input.chars();
	let mut len = 0;
	if let Some('~') = chars.next() {
		len += 1;
		while let Some(ch) = chars.next() {
			match ch {
				' ' | '\t' | '\n' | ';' => {
					return Some(len)
				}
				_ => len += 1
			}
		}
	}
	match len {
		0 => None,
		_ => Some(len)
	}
});

tkrule_def!(Subshell, |input: &str| {
	let mut chars = input.chars();
	let mut len = 0;
	let mut paren_count = 0;

	if let Some('(') = chars.next() {
		len += 1;
		paren_count += 1;
		while let Some(ch) = chars.next() {
			match ch {
				'\\' => {
					len += 2;
					chars.next();
				}
				'(' => {
					len += 1;
					paren_count += 1;
				}
				')' => {
					len += 1;
					paren_count -= 1;
					if paren_count == 0 {
						return Some(len);
					}
				}
				_ => len += 1
			}
		}
		None
	} else {
		None
	}
});

tkrule_def!(PipeOp, |input: &str| {
	if input.starts_with('|') {
		Some(1)
	} else {
		None
	}
});

tkrule_def!(BgOp, |input: &str| {
	if input.starts_with('&') {
		Some(1)
	} else {
		None
	}
});

tkrule_def!(ErrPipeOp, |input: &str| {
	if input.starts_with("|&") {
		Some(2)
	} else {
		None
	}
});

tkrule_def!(AndOp, |input: &str| {
	if input.starts_with("&&") {
		Some(2)
	} else {
		None
	}
});

tkrule_def!(OrOp, |input: &str| {
	if input.starts_with("||") {
		Some(2)
	} else {
		None
	}
});

tkrule_def!(If, |input: &str| {
	if input.starts_with("if") {
		match input.chars().nth(2) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(2),
			Some(_) => None,
			None => Some(2), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Then, |input: &str| {
	if input.starts_with("then") {
		match input.chars().nth(4) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(4),
			Some(_) => None,
			None => Some(4), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Elif, |input: &str| {
	if input.starts_with("elif") {
		match input.chars().nth(4) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(4),
			Some(_) => None,
			None => Some(4), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Else, |input: &str| {
	if input.starts_with("else") {
		match input.chars().nth(4) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(4),
			Some(_) => None,
			None => Some(4), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Fi, |input: &str| {
	if input.starts_with("fi") {
		match input.chars().nth(2) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(2),
			Some(_) => None,
			None => Some(2), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(While, |input: &str| {
	if input.starts_with("while") {
		match input.chars().nth(5) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(5),
			Some(_) => None,
			None => Some(5), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Until, |input: &str| {
	if input.starts_with("until") {
		match input.chars().nth(5) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(5),
			Some(_) => None,
			None => Some(5), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(For, |input: &str| {
	if input.starts_with("for") {
		match input.chars().nth(3) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(3),
			Some(_) => None,
			None => Some(3), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(In, |input: &str| {
	if input.starts_with("in") {
		match input.chars().nth(2) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(2),
			Some(_) => None,
			None => Some(2), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Select, |input: &str| {
	if input.starts_with("select") {
		match input.chars().nth(6) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(6),
			Some(_) => None,
			None => Some(6), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Do, |input: &str| {
	if input.starts_with("do") {
		match input.chars().nth(2) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(2),
			Some(_) => None,
			None => Some(2), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Done, |input: &str| {
	if input.starts_with("done") {
		match input.chars().nth(4) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(4),
			Some(_) => None,
			None => Some(4), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Case, |input: &str| {
	if input.starts_with("case") {
		match input.chars().nth(4) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(4),
			Some(_) => None,
			None => Some(4), // "if" is the entire input
		}
	} else {
		None
	}
});
tkrule_def!(Esac, |input: &str| {
	if input.starts_with("esac") {
		match input.chars().nth(4) {
			Some(ch) if ch.is_whitespace() || ch == ';' => Some(4),
			Some(_) => None,
			None => Some(4), // "if" is the entire input
		}
	} else {
		None
	}
});

tkrule_def!(Ident, |input: &str| {
	// An ident is any span of text that is not a space, tab, newline, or semicolon
	let mut chars = input.chars();
	let mut len = 0;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				chars.next();
				len += 2;
			}
			'>' | '<' | '$' | ' ' | '\t' | '\n' | ';' => {
				match len {
					0 => return None,
					_ => return Some(len),
				}
			}
			_ => len += 1
		}
	}
	match len {
		0 => return None,
		_ => return Some(len),
	}
});

tkrule_def!(Sep, |input: &str| {
	// Command separator; newline or semicolon
	let mut chars = input.chars();
	let mut len = 0;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				chars.next();
				len += 2;
			}
			';' | '\n' => len += 1,
			_ => {
				match len {
					0 => return None,
					_ => return Some(len),
				}
			}
		}
	}
	match len {
		0 => return None,
		_ => return Some(len),
	}
});

tkrule_def!(SQuote, |input: &str| {
	// Double quoted strings
	let mut chars = input.chars();
	let mut len = 0;
	let mut quoted = false;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				chars.next();
				len += 2;
			}
			'\'' if !quoted => {
				len += 1;
				quoted = true;
			}
			'\'' if quoted => {
				len += 1;
				return Some(len)
			}
			_ if !quoted => {
				return None
			}
			_ => len += 1
		}
	}
	None
});

tkrule_def!(DQuote, |input: &str| {
	// Double quoted strings
	let mut chars = input.chars();
	let mut len = 0;
	let mut quote_count = 0;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				chars.next();
				len += 2;
			}
			'"' => {
				len += 1;
				quote_count += 1;
			}
			' ' | '\t' | ';' | '\n' if quote_count % 2 == 0 => {
				if quote_count > 0 {
					if quote_count % 2 == 0 {
						return Some(len)
					} else {
						return None
					}
				} else {
					return None
				}
			}
			_ => len += 1
		}
	}
	match len {
		0 => None,
		_ => {
			if quote_count > 0 {
				if quote_count % 2 == 0 {
					return Some(len)
				} else {
					return None
				}
			} else {
				return None
			}
		}
	}
});

tkrule_def!(ProcSub, |input: &str| {
	let mut chars = input.chars();
	let mut len = 0;

	match chars.next() {
		Some('<') | Some('>') => {
			len += 1;
			match chars.next() {
				Some('(') => {
					len += 1;
					while let Some(ch) = chars.next() {
						match ch {
							'\\' => {
								len += 2;
								chars.next();
							}
							')' => {
								len += 1;
								return Some(len)
							}
							_ => len += 1
						}
					}
					None
				}
				_ => None
			}
		}
		_ => None
	}
});

tkrule_def!(CmdSub, |input: &str| {
	let mut chars = input.chars();
	let mut len = 0;

	if let Some('$') = chars.next() {
		len += 1;
		if let Some('(') = chars.next() {
			len += 1;
			while let Some(ch) = chars.next() {
				match ch {
					'\\' => {
						len += 2;
						chars.next();
					}
					')' => {
						len += 1;
						return Some(len)
					}
					_ => len += 1
				}
			}
			return None
		}
		return None
	} else {
		return None
	}
});

tkrule_def!(VarSub, |input: &str| {
	// Variable substitutions
	let mut chars = input.chars();
	let mut len = 0;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				chars.next();
				len += 2;
			}
			'{' => {
				match len {
					0 => return None,
					_ => {
						while let Some(ch) = chars.next() {
							match ch {
								'\\' => {
									chars.next();
									len += 2;
								}
								'}' => {
									len += 1;
									return Some(len)
								}
								_ => len += 1
							}
						}
					}
				}
			}
			'$' => {
				match len {
					0 => len += 1,
					_ => return None
				}
			}
			' ' | '\t' | '\n' | ';' => {
				match len {
					0 => return None,
					_ => return Some(len),
				}
			}
			_ => {
				match len {
					0 => return None,
					_ => len += 1
				}
			}
		}
	}
	match len {
		0 => return None,
		_ => return Some(len)
	}
});

tkrule_def!(FuncName, |input: &str| {
	// Function names; foo() for instance
	let mut chars = input.chars();
	let mut len = 0;

	while let Some(ch) = chars.next() {
		match ch {
			'(' => {
				len += 1;
				if let Some(')') = chars.next() {
					len += 1;
					return Some(len)
				}
			}
			_ if ch.is_ascii_alphanumeric() => len += 1,
			_ => return None
		}
	}
	None
});

tkrule_def!(Assign, |input: &str| {
	let mut chars = input.chars();
	let mut len = 0;
	let mut found_equals = false;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				len += 2;
				chars.next();
			}
			'=' if len == 0 => return None,
			'=' => {
				len += 1;
				found_equals = true;
			}
			' ' | '\t' | ';' | '\n' => {
				match len {
					_ if found_equals && len > 1 => return Some(len),
					_ => return None
				}
			}
			_ => len += 1
		}
	}
	match len {
		_ if found_equals && len > 1 => return Some(len),
		_ => return None
	}
});

tkrule_def!(BraceGrp, |input: &str| {
	// A group of commands inside of braces
	// Currently just holds a raw string to be re-parsed later
	let mut chars = input.chars();
	let mut len = 0;
	let mut brace_depth = 0;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				len += 2;
				chars.next();
			}
			'{' => {
				len += 1;
				brace_depth += 1;
			}
			'}' => {
				len += 1;
				brace_depth -= 1;
				if brace_depth == 0 {
					return Some(len)
				}
			}
			_ => {
				match brace_depth {
					0 => return None,
					_ => len += 1
				}
			}
		}
	}
	None
});

tkrule_def!(RedirOp, |input: &str| {
	if let Some(ch) = input.chars().next() {
		match ch {
			'>' |
			'<' |
			'&' => { /* Continue */ }
			_ => return None
		}
	}
	// Order matters here
	// For instance, if '>' is checked before '>>', '>' will always match first, and '>>' will never be checked
	try_match_inner!(RedirCombineAppend,input); // Ex: &>>
	try_match_inner!(RedirCombineOut,input); // Ex: &>
	try_match_inner!(RedirOutFd,input); // Ex: >&2, >&-
	try_match_inner!(RedirInFd,input); // Ex: <&2
	try_match_inner!(RedirClobber,input); // >|
	try_match_inner!(RedirSimpleAppend,input); // >>
	try_match_inner!(RedirSimpleOut,input); // >
	try_match_inner!(RedirInOut,input); // <>
	try_match_inner!(RedirSimpleHerestring,input); // <<<
	try_match_inner!(RedirSimpleHeredoc,input); // <<
	try_match_inner!(RedirSimpleIn,input); // <
	try_match_inner!(RedirFdOutFd,input); // Ex: 2>&1
	try_match_inner!(RedirFdInFd,input); // Ex: 2<&1
	try_match_inner!(RedirFdClobber,input); // 2>|
	try_match_inner!(RedirFdAppend,input); // Ex: 2>>
	try_match_inner!(RedirFdOut,input); // Ex: 2>
	try_match_inner!(RedirFdHeredoc,input); // Ex: 2<<
	try_match_inner!(RedirFdIn,input); // Ex: 2<

	None
});

tkrule_def!(RedirClobber, |input: &str| {
	if input.starts_with(">|") {
		Some(2)
	} else {
		None
	}
});

tkrule_def!(RedirInOut, |input: &str| {
	if input.starts_with("<>") {
		Some(2)
	} else {
		None
	}
});

tkrule_def!(RedirSimpleHeredoc, |input: &str| {
	if input.starts_with("<<") {
		Some(2)
	} else {
		None
	}
});

tkrule_def!(RedirSimpleHerestring, |input: &str| {
	if input.starts_with("<<<") {
		Some(3)
	} else {
		None
	}
});

tkrule_def!(RedirSimpleOut, |input: &str| {
	if input.starts_with('>') {
		Some(1)
	} else {
		None
	}
});

tkrule_def!(RedirCombineOut, |input: &str| {
	if input.starts_with("&>") {
		Some(2)
	} else {
		None
	}
});

tkrule_def!(RedirCombineAppend, |input: &str| {
	if input.starts_with("&>>") {
		Some(3)
	} else {
		None
	}
});

tkrule_def!(RedirSimpleAppend, |input: &str| {
	if input.starts_with(">>") {
		Some(2)
	} else {
		None
	}
});

tkrule_def!(RedirSimpleIn, |input: &str| {
	if input.starts_with('<') {
		Some(1)
	} else {
		None
	}
});

tkrule_def!(RedirInFd, |input: &str| {
	// Ex: <&2
	let mut chars = input.chars();
	let mut len = 0;

	if input.starts_with("<&") {
		len += 2;
		chars.next();
		chars.next();
	}
	while let Some(ch) = chars.next() {
		if !ch.is_ascii_digit() {
			break
		}
		len += 1;
	}
	if len <= 2 {
		None
	} else {
		Some(len)
	}
});

tkrule_def!(RedirOutFd, |input: &str| {
	// Ex: >&2
	let mut chars = input.chars().peekable();
	let mut len = 0;

	if input.starts_with(">&") {
		len += 2;
		chars.next();
		chars.next();
	}
	if let Some(&'-') = chars.peek() {
		len += 1;
		return Some(len);
	}
	while let Some(ch) = chars.next() {
		if !ch.is_ascii_digit() {
			break
		}
		len += 1;
	}
	if len <= 2 {
		None
	} else {
		Some(len)
	}
});

tkrule_def!(RedirFdOut, |input: &str| {
	// Ex: 2>
	let mut chars = input.chars().peekable();
	let mut len = 0;

	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == 0 {
				break
			} else if char == '>' {
				len += 1;
				return Some(len)
			}
		}
		len += 1;
	}
	None
});

tkrule_def!(RedirFdClobber, |input: &str| {
	// Ex: 2>|
	let mut chars = input.chars().peekable();
	let mut len = 0;

	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == 0 {
				break
			} else if char == '>' {
				len += 1;
				if chars.next() == Some('|') {
					len += 1;
					return Some(len)
				} else {
					return None
				}
			}
		}
		len += 1;
	}
	None
});

tkrule_def!(RedirFdInOut, |input: &str| {
	// Ex: 2<>
	let mut chars = input.chars().peekable();
	let mut len = 0;

	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == 0 {
				break
			} else if char == '<' {
				len += 1;
				if chars.next() == Some('>') {
					len += 1;
					return Some(len)
				} else {
					return None
				}
			}
		}
		len += 1;
	}
	None
});

tkrule_def!(RedirFdIn, |input: &str| {
	// Ex: 2<
	let mut chars = input.chars().peekable();
	let mut len = 0;

	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == 0 {
				break
			} else if char == '<' {
				len += 1;
				return Some(len)
			}
		}
		len += 1;
	}
	None
});

tkrule_def!(RedirFdHeredoc, |input: &str| {
	// Ex: 2<<
	let mut chars = input.chars().peekable();
	let mut len = 0;

	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == 0 {
				break
			} else if char == '<' {
				len += 1;
				if chars.next() == Some('<') {
					len += 1;
					return Some(len)
				} else {
					return None
				}
			}
		}
		len += 1;
	}
	None
});

tkrule_def!(RedirFdAppend, |input: &str| {
	// Ex: 2>>
	let mut chars = input.chars().peekable();
	let mut len = 0;

	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == 0 {
				break
			} else if char == '>' {
				len += 1;
				if chars.next() == Some('>') {
					len += 1;
					return Some(len)
				} else {
					return None
				}
			}
		}
		len += 1;
	}
	None
});

tkrule_def!(RedirFdOutFd, |input: &str| {
	// Ex: 2>&1
	let mut chars = input.chars().peekable();
	let mut len = 0;
	let mut base_len = 0;

	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == 0 {
				break
			} else if char == '>' {
				len += 1;
				if chars.next() == Some('&') {
					len += 1;
					base_len = len;
					break
				} else {
					return None
				}
			} else {
				return None
			}
		}
		len += 1;
	}
	if chars.peek().is_none() {
		return None
	}
	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == base_len {
				return None
			} else {
				return Some(len)
			}
		}
		len += 1;
	}
	if len == 0 || len == base_len {
		None
	} else {
		Some(len)
	}
});

tkrule_def!(RedirFdInFd, |input: &str| {
	// Ex: 2<&1
	let mut chars = input.chars().peekable();
	let mut len = 0;
	let mut base_len = 0;

	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == 0 {
				break
			} else if char == '<' {
				len += 1;
				if chars.next() == Some('&') {
					len += 1;
					base_len = len;
					break
				} else {
					return None
				}
			} else {
				return None
			}
		}
		len += 1;
	}
	if chars.peek().is_none() {
		return None
	}
	while let Some(char) = chars.next() {
		if !char.is_ascii_digit() {
			if len == base_len {
				return None
			} else {
				return Some(len)
			}
		}
		len += 1;
	}
	if len == 0 || len == base_len {
		None
	} else {
		Some(len)
	}
});
