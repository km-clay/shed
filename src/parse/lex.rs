use std::fmt::Display;

use crate::prelude::*;

pub const KEYWORDS: [&str;14] = [
	"if",
	"then",
	"elif",
	"else",
	"fi",
	"while",
	"until",
	"for",
	"in",
	"select",
	"do",
	"done",
	"case",
	"esac"
];

pub const SEPARATORS: [TkRule; 7] = [
	TkRule::Sep,
	TkRule::AndOp,
	TkRule::OrOp,
	TkRule::PipeOp,
	TkRule::ErrPipeOp,
	TkRule::BgOp,
	TkRule::Keyword // Keywords don't count as command names
];

pub trait LexRule {
	fn try_match(input: &str) -> Option<usize>;
}

pub struct Lexer {
	input: Rc<String>,
	tokens: Vec<Token>,
	is_command: bool,
	consumed: usize
}

impl Lexer {
	pub fn new(input: Rc<String>) -> Self {
		Self { input, tokens: vec![], is_command: true, consumed: 0  }
	}
	pub fn lex(mut self) -> Vec<Token> {
		unsafe {
			let mut input = self.input.as_str();
			while let Some((mut rule,len)) = TkRule::try_match(input) {
				// If we see a keyword in an argument position, it's actually an ident
				if !self.is_command && rule == TkRule::Keyword {
					rule = TkRule::Ident

				// If we are in a command right now, after this we are in arguments
				} else if self.is_command && !matches!(rule,TkRule::Keyword | TkRule::Whitespace) {
					self.is_command = false;
				}
				// If we see a separator like && or ;, we are now in a command again
				if SEPARATORS.contains(&rule) {
					self.is_command = true;
				}
				let span = Span::new(self.input.clone(),self.consumed,self.consumed + len);
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

#[derive(Debug,Clone)]
pub struct Token {
	rule: TkRule,
	span: Span
}

impl Token {
	pub fn new(rule: TkRule, span: Span) -> Self {
		Self { rule, span }
	}

	pub fn span(&self) -> &Span {
		&self.span
	}

	pub fn span_mut(&mut self) -> &mut Span {
		&mut self.span
	}

	pub fn rule(&self) -> TkRule {
		self.rule
	}
}

impl Display for Token {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		let slice = self.span.get_slice();
		write!(f,"{}",slice)
	}
}

#[derive(Debug,Clone)]
pub struct Span {
	input: Rc<String>,
	start: usize,
	end: usize
}

impl Span {
	pub fn new(input: Rc<String>, start: usize, end: usize) -> Self {
		Self { input, start, end } }
	pub fn get_slice(&self) -> String {
		unsafe {
			let slice = &self.input[self.start..self.end];
			slice.to_string()
		}
	}
	pub fn get_line(&self) -> (usize,usize,String) {
		unsafe {
			let mut dist = 0;
			let mut line_no = 0;
			let mut lines = self.input.lines();
			while let Some(line) = lines.next() {
				line_no += 1;
				dist += line.len();
				if dist > self.start {
					dist -= line.len();
					let offset = self.start - dist;
					return (offset,line_no,line.to_string())
				}
			}
		}
		(0,0,String::new())
	}
	pub fn start(&self) -> usize {
		self.start
	}
	pub fn end(&self) -> usize {
		self.end
	}
	pub fn get_input(&self) -> Rc<String> {
		self.input.clone()
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
	Keyword,
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
		try_match!(Keyword,input);
		try_match!(Assign,input);
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


tkrule_def!(Keyword, |input: &str| {
	for &kw in KEYWORDS.iter() {
		if input.starts_with(kw) {
			let len = kw.len();
			if input.chars().nth(len).map_or(true, |ch| ch.is_whitespace() || ch == ';') {
				return Some(len);
			}
		}
	}
	None
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
	let mut quoted = false;

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				chars.next();
				len += 2;
			}
			'"' if !quoted => {
				len += 1;
				quoted = true;
			}
			'"' if quoted => {
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
