use core::fmt::Display;
use std::str::FromStr;

use crate::prelude::*;

use super::lex::{TkRule, Span, Token};

bitflags! {
	#[derive(Debug,Clone,Copy,PartialEq,Eq)]
	pub struct NdFlag: u32 {
		const BACKGROUND = 0b00000000000000000000000000000001;
		const FUNCTION   = 0b00000000000000000000000000000010;
		const BUILTIN    = 0b00000000000000000000000000000100;
	}
}


pub trait ParseRule {
	/// Used for cases where a rule is optional
	fn try_match(input: &[Token]) -> ShResult<Option<Node>>;
	/// Used for cases where a rule is assumed based on context
	/// For instance, if the "for" keyword is encountered, then it *must* be a for loop
	/// And if it isn't, return a parse error
	fn assert_match(input: &[Token]) -> ShResult<Node> {
		Self::try_match(input)?.ok_or_else(||
			ShErr::simple(ShErrKind::ParseErr, "Parse Error")
		)
	}
}

#[derive(Debug,Clone)]
pub enum CmdGuard {
	And,
	Or
}



#[derive(Debug,Clone)]
pub struct Node {
	node_rule: NdRule,
	tokens: Vec<Token>,
	span: Span,
	flags: NdFlag,
}

impl Node {
	pub fn len(&self) -> usize {
		self.tokens.len()
	}
	pub fn tokens(&self) -> &Vec<Token> {
		&self.tokens
	}
	pub fn rule(&self) -> &NdRule {
		&self.node_rule
	}
	pub fn rule_mut(&mut self) -> &mut NdRule {
		&mut self.node_rule
	}
	pub fn into_rule(self) -> NdRule {
		self.node_rule
	}
	pub fn span(&self) -> Span {
		self.span.clone()
	}
	pub fn flags(&self) -> NdFlag {
		self.flags
	}
	pub fn flags_mut(&mut self) -> &mut NdFlag {
		&mut self.flags
	}
}

impl Display for Node {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		let raw = self.span().get_slice();
		write!(f, "{}", raw)
	}
}

#[derive(Clone,Debug)]
pub enum NdRule {
	Main { cmd_lists: Vec<Node> },
	Command { argv: Vec<Token>, redirs: Vec<Redir> },
	Assignment { assignments: Vec<Token>, cmd: Option<Box<Node>> },
	FuncDef { name: Token, body: Token },
	Subshell { body: Token, argv: Vec<Token>, redirs: Vec<Redir> },
	CmdList { cmds: Vec<(Option<CmdGuard>,Node)> },
	Pipeline { cmds: Vec<Node> }
}

/// Define a Node rule. The body of this macro becomes the implementation for the try_match() method for the rule.
macro_rules! ndrule_def {
	($name:ident,$try:expr) => {
		#[derive(Debug)]
		pub struct $name;
		impl ParseRule for $name {
			fn try_match(input: &[Token]) -> ShResult<Option<Node>> {
				$try(input)
			}
		}
	};
}

/// This macro attempts to match all of the given Rules. It returns upon finding the first match, so the order matters
/// Place the most specialized/specific rules first, and the most general rules last
macro_rules! try_rules {
    ($tokens:expr, $($name:ident),+) => {
			$(
				let result = $name::try_match($tokens)?;
				if let Some(node) = result {
					return Ok(Some(node))
				}
			)+
			return Ok(None)
    };
}

#[derive(Debug)]
pub struct SynTree {
	tree: VecDeque<Node>
}

impl SynTree {
	pub fn new() -> Self {
		Self { tree: VecDeque::new() }
	}
	pub fn push_node(&mut self, node: Node) {
		self.tree.bpush(node)
	}
	pub fn next_node(&mut self) -> Option<Node> {
		self.tree.fpop()
	}
}

pub struct Parser {
	token_stream: Vec<Token>,
	ast: SynTree
}

impl Parser {
	pub fn new(mut token_stream: Vec<Token>) -> Self {
		log!(TRACE, "New parser");
		token_stream.retain(|tk| !matches!(tk.rule(), TkRule::Whitespace | TkRule::Comment));
		Self { token_stream, ast: SynTree::new() }
	}

	pub fn parse(mut self) -> ShResult<SynTree> {
		log!(TRACE, "Starting parse");
		let mut lists = VecDeque::new();
		let token_slice = &*self.token_stream;
		// Get the Main rule
		if let Some(mut node) = Main::try_match(token_slice)? {
			// Extract the inner lists
			if let NdRule::Main { ref mut cmd_lists } = node.rule_mut() {
				while let Some(node) = cmd_lists.pop() {
					log!(DEBUG, node);
					lists.bpush(node)
				}
			}
		}
		while let Some(node) = lists.bpop() {
			// Push inner command lists to self.ast
			self.ast.push_node(node);
		}
		Ok(self.ast)
	}
}

fn get_span(toks: &Vec<Token>) -> ShResult<Span> {
	if toks.is_empty() {
		Err(ShErr::simple(ShErrKind::InternalErr, "Get_span was given an empty token list"))
	} else {
		let start = toks.first().unwrap().span().start();
		let end = toks.iter().last().unwrap().span().end();
		let input = toks.iter().last().unwrap().span().get_input();
		Ok(Span::new(input,start,end))
	}
}

// TODO: Redirs with FD sources appear to be looping endlessly for some reason

ndrule_def!(Main, |tokens: &[Token]| {
	log!(TRACE, "Parsing main");
	let mut cmd_lists = vec![];
	let mut node_toks = vec![];
	let mut token_slice = &*tokens;

	while let Some(node) = CmdList::try_match(token_slice)? {
		node_toks.extend(node.tokens().clone());
		token_slice = &token_slice[node.len()..];
		cmd_lists.push(node);
	}

	if cmd_lists.is_empty() {
		return Ok(None)
	}
	let span = get_span(&node_toks)?;
	let node = Node {
		node_rule: NdRule::Main { cmd_lists },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};
	Ok(Some(node))
});

ndrule_def!(CmdList, |tokens: &[Token]| {
	log!(TRACE, "Parsing cmdlist");
	let mut commands: Vec<(Option<CmdGuard>,Node)> = vec![];
	let mut node_toks = vec![];
	let mut token_slice = &*tokens;
	let mut cmd_guard = None; // Operators like '&&' and '||'

	while let Some(mut node) = Expr::try_match(token_slice)? {
		// Add sub-node tokens to our tokens
		node_toks.extend(node.tokens().clone());
		// Reflect changes in the token slice
		log!(DEBUG, token_slice);
		token_slice = &token_slice[node.len()..];
		log!(DEBUG, token_slice);
		// Push sub-node
		if let NdRule::Command { argv, redirs: _ } = node.rule() {
			if argv.first().is_some_and(|arg| BUILTINS.contains(&arg.to_string().as_str())) {
				*node.flags_mut() |= NdFlag::BUILTIN;
			}
		}
		commands.push((cmd_guard.take(),node));

		// If the next token is '&&' or '||' then we set cmd_guard and go again
		if token_slice.first().is_some_and(|tk| matches!(tk.rule(),TkRule::AndOp | TkRule::OrOp)) {
			let token = token_slice.first().unwrap();
			node_toks.push(token.clone());
			match token.rule() {
				TkRule::AndOp => cmd_guard = Some(CmdGuard::And),
				TkRule::OrOp => cmd_guard = Some(CmdGuard::Or),
				_ => unreachable!()
			}
			token_slice = &token_slice[1..];
		} else {
			break
		}
	}
	if node_toks.is_empty() {
		return Ok(None)
	}
	let span = get_span(&node_toks)?;
	let node = Node {
		node_rule: NdRule::CmdList { cmds: commands },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};
	Ok(Some(node))
});

ndrule_def!(Expr, |tokens: &[Token]| {
	try_rules!(tokens,
		ShellCmd,
		Pipeline,
		Subshell,
		Assignment,
		Command
	);
});
// Used in pipelines to avoid recursion
ndrule_def!(ExprNoPipeline, |tokens: &[Token]| {
	try_rules!(tokens,
		ShellCmd,
		Subshell,
		Assignment,
		Command
	);
});

ndrule_def!(ShellCmd, |tokens: &[Token]| {
	try_rules!(tokens,
		FuncDef
	);
});

ndrule_def!(FuncDef, |tokens: &[Token]| {
	let mut tokens_iter = tokens.iter();
	let mut node_toks = vec![];
	let name: Token;
	let body: Token;

	if let Some(token) = tokens_iter.next() {
		if let TkRule::FuncName = token.rule() {
			node_toks.push(token.clone());
			name = token.clone();
		} else {
			return Ok(None)
		}
	} else {
		return Ok(None)
	}

	if let Some(token) = tokens_iter.next() {
		if let TkRule::BraceGrp = token.rule() {
			node_toks.push(token.clone());
			body = token.clone();
		} else {
			return Ok(None)
		}
	} else {
		return Ok(None)
	}

	let span = get_span(&node_toks)?;
	let node = Node {
		node_rule: NdRule::FuncDef { name, body },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};
	Ok(Some(node))
});

ndrule_def!(Subshell, |tokens: &[Token]| {
	let mut tokens_iter = tokens.iter();
	let mut node_toks = vec![];
	let mut argv = vec![];
	let mut redirs = vec![];
	if let Some(token) = tokens_iter.next() {
		if let TkRule::Subshell = token.rule() {
			node_toks.push(token.clone());
			let body = token.clone();
			while let Some(token) = tokens_iter.next() {
				match token.rule() {
					TkRule::AndOp |
					TkRule::OrOp |
					TkRule::PipeOp |
					TkRule::ErrPipeOp => {
						break
					}
					TkRule::Sep => {
						node_toks.push(token.clone());
						break;
					}
					TkRule::Ident |
					TkRule::SQuote |
					TkRule::DQuote |
					TkRule::Assign |
					TkRule::TildeSub |
					TkRule::VarSub => {
						node_toks.push(token.clone());
						argv.push(token.clone());
					}
					TkRule::RedirOp => {
						node_toks.push(token.clone());
						// Get the raw redirection text, e.g. "1>&2" or "2>" or ">>" or something
						let redir_raw = token.span().get_slice();
						let mut redir_bldr = RedirBldr::from_str(&redir_raw).unwrap();
						// If there isn't an FD target, get the next token and use it as the filename
						if redir_bldr.tgt().is_none() {
							if let Some(filename) = tokens_iter.next() {
								// Make sure it's a word and not an operator or something
								if !matches!(filename.rule(), TkRule::SQuote | TkRule::DQuote | TkRule::Ident | TkRule::Keyword) {
									let mut err = ShErr::simple(ShErrKind::ParseErr, "Did not find a target for this redirection");
									err.blame(token.span().clone());
									return Err(err)
								}
								node_toks.push(filename.clone());
								// Construct the Path object
								let filename_raw = filename.span().get_slice();
								let filename_path = PathBuf::from(filename_raw);
								let tgt = RedirTarget::File(filename_path);
								// Update the builder
								redir_bldr = redir_bldr.with_tgt(tgt);
							} else {
								let mut err = ShErr::simple(ShErrKind::ParseErr, "Did not find a target for this redirection");
								err.blame(token.span().clone());
								return Err(err)
							}
						}
						redirs.push(redir_bldr.build());
					}
					_ => break
				}
			}
			let span = get_span(&node_toks)?;
			let node = Node {
				node_rule: NdRule::Subshell { body, argv, redirs },
				tokens: node_toks,
				span,
				flags: NdFlag::empty()
			};
			return Ok(Some(node))
		} else {
			return Ok(None)
		}
	}
	Ok(None)
});

ndrule_def!(Pipeline, |mut tokens: &[Token]| {
	log!(TRACE, "Parsing pipeline");
	let mut tokens_iter = tokens.iter().peekable();
	let mut node_toks = vec![];
	let mut cmds = vec![];

	while let Some(token) = tokens_iter.peek() {
		match token.rule() {
			TkRule::AndOp | TkRule::OrOp => {
				// If there are no commands or only one, this isn't a pipeline
				match cmds.len() {
					0 | 1 => return Ok(None),
					_ => break
				}
			}
			_ => { /* Keep going */ }
		}
		if let Some(mut cmd) = ExprNoPipeline::try_match(tokens)? {
			// Add sub-node's tokens to our tokens
			node_toks.extend(cmd.tokens().clone());

			// Reflect changes in tokens and tokens_iter
			tokens = &tokens[cmd.len()..];
			for _ in 0..cmd.len() {
				tokens_iter.next();
			}

			if let NdRule::Command { argv, redirs: _ } = cmd.rule() {
				if argv.first().is_some_and(|arg| BUILTINS.contains(&arg.to_string().as_str())) {
					*cmd.flags_mut() |= NdFlag::BUILTIN;
				}
			}
			// Push sub-node
			cmds.push(cmd);

			if tokens_iter.peek().is_none_or(|tk| !matches!(tk.rule(),TkRule::PipeOp | TkRule::ErrPipeOp)) {
				match cmds.len() {
					0 | 1 => {
						return Ok(None)
					}
					_ => break
				}
			} else {
				if tokens_iter.peek().is_some() {
					node_toks.push(tokens_iter.next().unwrap().clone());
					tokens = &tokens[1..];
					continue
				} else {
					match cmds.len() {
						0 | 1 => return Ok(None),
						_ => break
					}
				}
			}
		} else {
			match cmds.len() {
				0 | 1 => return Ok(None),
				_ => break
			}
		}
	}
	if node_toks.is_empty() {
		return Ok(None)
	}
	let span = get_span(&node_toks)?;
	let node = Node {
		node_rule: NdRule::Pipeline { cmds },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};
	Ok(Some(node))
});

ndrule_def!(Command, |tokens: &[Token]| {
	log!(TRACE, "Parsing command");
	let mut tokens = tokens.iter().peekable();
	let mut node_toks = vec![];
	let mut argv = vec![];
	let mut redirs = vec![];

	while let Some(token) = tokens.peek() {
		match token.rule() {
			TkRule::AndOp | TkRule::OrOp | TkRule::PipeOp | TkRule::ErrPipeOp => {
				break
			}
			_ => { /* Keep going */ }
		}
		let token = tokens.next().unwrap();
		node_toks.push(token.clone());
		match token.rule() {
			TkRule::Ident |
			TkRule::SQuote |
			TkRule::DQuote |
			TkRule::Assign |
			TkRule::TildeSub |
			TkRule::VarSub => {
				argv.push(token.clone());
			}
			TkRule::RedirOp => {
				// Get the raw redirection text, e.g. "1>&2" or "2>" or ">>" or something
				let redir_raw = token.span().get_slice();
				let mut redir_bldr = RedirBldr::from_str(&redir_raw).unwrap();
				// If there isn't an FD target, get the next token and use it as the filename
				if redir_bldr.tgt().is_none() {
					if let Some(filename) = tokens.next() {
						// Make sure it's a word and not an operator or something
						if !matches!(filename.rule(), TkRule::SQuote | TkRule::DQuote | TkRule::Ident | TkRule::Keyword) {
							let mut err = ShErr::simple(ShErrKind::ParseErr, "Did not find a target for this redirection");
							err.blame(token.span().clone());
							return Err(err)
						}
						node_toks.push(filename.clone());
						// Construct the Path object
						let filename_raw = filename.span().get_slice();
						let filename_path = PathBuf::from(filename_raw);
						let tgt = RedirTarget::File(filename_path);
						// Update the builder
						redir_bldr = redir_bldr.with_tgt(tgt);
					} else {
						let mut err = ShErr::simple(ShErrKind::ParseErr, "Did not find a target for this redirection");
						err.blame(token.span().clone());
						return Err(err)
					}
				}
				redirs.push(redir_bldr.build());
			}
			TkRule::Sep => break,
			_ => unreachable!("Found this rule: {:?}", token.rule())
		}
	}
	if node_toks.is_empty() {
		return Ok(None)
	}
	let span = get_span(&node_toks)?;
	if !argv.is_empty() {
		let node = Node {
			node_rule: NdRule::Command { argv, redirs },
			tokens: node_toks,
			span,
			flags: NdFlag::empty()
		};
		Ok(Some(node))
	} else {
		Ok(None)
	}
});

ndrule_def!(Assignment, |tokens: &[Token]| {
	log!(TRACE, "Parsing assignment");
	let mut tokens = tokens.into_iter().peekable();
	let mut node_toks = vec![];
	let mut assignments = vec![];
	while tokens.peek().is_some_and(|tk| tk.rule() == TkRule::Assign) {
		let token = tokens.next().unwrap();
		node_toks.push(token.clone());
		assignments.push(token.clone());
	}
	if assignments.is_empty() {
		return Ok(None)
	}

	if tokens.peek().is_some() {
		let tokens_vec: Vec<Token> = tokens.into_iter().map(|token| token.clone()).collect();
		let tokens_slice = &tokens_vec;
		let cmd = Command::try_match(tokens_slice)?.map(|cmd| Box::new(cmd));
		if let Some(ref cmd) = cmd {
			node_toks.extend(cmd.tokens().clone());
		}
		let span = get_span(&node_toks)?;
		let node = Node {
			node_rule: NdRule::Assignment { assignments, cmd },
			tokens: node_toks,
			span,
			flags: NdFlag::empty()
		};
		return Ok(Some(node))
	} else {
		let span = get_span(&node_toks)?;
		let node = Node {
			node_rule: NdRule::Assignment { assignments, cmd: None },
			tokens: node_toks,
			span,
			flags: NdFlag::empty()
		};
		Ok(Some(node))
	}
});
