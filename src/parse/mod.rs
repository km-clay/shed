pub mod lex;

use std::{iter::Peekable, str::FromStr};

use crate::prelude::*;

use lex::{Span, TkRule, Token, KEYWORDS};

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
	fn try_match(input: &[Token], shenv: &mut ShEnv) -> ShResult<Option<Node>>;
	/// Used for cases where a rule is assumed based on context
	/// For instance, if the "for" keyword is encountered, then it *must* be a for loop
	/// And if it isn't, return a parse error
	fn assert_match(input: &[Token], shenv: &mut ShEnv) -> ShResult<Node> {
		Self::try_match(input,shenv)?.ok_or_else(||
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
	span: Rc<RefCell<Span>>,
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
	pub fn span(&self) -> Rc<RefCell<Span>> {
		self.span.clone()
	}
	pub fn as_raw(&self, shenv: &mut ShEnv) -> String {
		shenv.input_slice(self.span()).to_string()
	}
	pub fn flags(&self) -> NdFlag {
		self.flags
	}
	pub fn flags_mut(&mut self) -> &mut NdFlag {
		&mut self.flags
	}
}

#[derive(Clone,Debug)]
pub enum LoopKind {
	While,
	Until
}

#[derive(Clone,Debug)]
pub enum NdRule {
	Main { cmd_lists: Vec<Node> },
	Command { argv: Vec<Token>, redirs: Vec<Redir> },
	Assignment { assignments: Vec<Token>, cmd: Option<Box<Node>> },
	FuncDef { name: Token, body: Token },
	Case { pat: Token, blocks: Vec<(Token,Vec<Node>)>, redirs: Vec<Redir> },
	IfThen { cond_blocks: Vec<(Vec<Node>,Vec<Node>)>, else_block: Option<Vec<Node>>, redirs: Vec<Redir> },
	Loop { kind: LoopKind, cond: Vec<Node>, body: Vec<Node>, redirs: Vec<Redir> },
	ForLoop { vars: Vec<Token>, arr: Vec<Token>, body: Vec<Node>, redirs: Vec<Redir> },
	Subshell { body: Token, argv: Vec<Token>, redirs: Vec<Redir> },
	CmdList { cmds: Vec<(Option<CmdGuard>,Node)> },
	Pipeline { cmds: Vec<Node> }
}

/// Define a Node rule. The body of this macro becomes the implementation for the try_match() method for the rule.
macro_rules! ndrule_def {
	($name:ident,$shenv:ident,$try:expr) => {
		#[derive(Debug)]
		pub struct $name;
		impl ParseRule for $name {
			fn try_match(input: &[Token],shenv: &mut ShEnv) -> ShResult<Option<Node>> {
				$try(input,shenv)
			}
		}
	};
}

/// This macro attempts to match all of the given Rules. It returns upon finding the first match, so the order matters
/// Place the most specialized/specific rules first, and the most general rules last
macro_rules! try_rules {
    ($tokens:expr,$shenv:expr,$($name:ident),+) => {
			$(
				let result = $name::try_match($tokens,$shenv)?;
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
	pub fn from_vec(nodes: Vec<Node>) -> Self {
		Self { tree: VecDeque::from(nodes) }
	}
	pub fn push_node(&mut self, node: Node) {
		self.tree.bpush(node)
	}
	pub fn next_node(&mut self) -> Option<Node> {
		self.tree.fpop()
	}
}

pub struct Parser<'a> {
	token_stream: Vec<Token>,
	shenv: &'a mut ShEnv,
	ast: SynTree
}

impl<'a> Parser<'a> {
	pub fn new(mut token_stream: Vec<Token>, shenv: &'a mut ShEnv) -> Self {
		log!(TRACE, "New parser");
		token_stream.retain(|tk| !matches!(tk.rule(), TkRule::Whitespace | TkRule::Comment));
		Self { token_stream, shenv, ast: SynTree::new() }
	}

	pub fn parse(mut self) -> ShResult<SynTree> {
		log!(TRACE, "Starting parse");
		let mut lists = VecDeque::new();
		let token_slice = &*self.token_stream;
		// Get the Main rule
		if let Some(mut node) = Main::try_match(token_slice,self.shenv)? {
			// Extract the inner lists
			if let NdRule::Main { ref mut cmd_lists } = node.rule_mut() {
				while let Some(node) = cmd_lists.pop() {
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

pub fn get_span(toks: &Vec<Token>, shenv: &mut ShEnv) -> ShResult<Rc<RefCell<Span>>> {
	if toks.is_empty() {
		Err(ShErr::simple(ShErrKind::InternalErr, "Get_span was given an empty token list"))
	} else {
		let start = toks.first().unwrap().span().borrow().start();
		let end = toks.iter().last().unwrap().span().borrow().end();
		let span = shenv.inputman_mut().new_span(start, end);
		Ok(span)
	}
}

fn get_lists(mut tokens: &[Token], shenv: &mut ShEnv) -> (usize,Vec<Node>) {
	let mut lists = vec![];
	let mut tokens_eaten = 0;
	while !tokens.is_empty() {
		match CmdList::try_match(tokens, shenv) {
			Ok(Some(list)) => {
				tokens_eaten += list.len();
				tokens = &tokens[list.len()..];
				lists.push(list);
			}
			Ok(None) | Err(_) => break
		}
	}
	(tokens_eaten,lists)
}

fn get_redir(token: Token, token_slice: &[Token], shenv: &mut ShEnv) -> ShResult<(usize,Redir)> {
	let mut tokens_eaten = 0;
	let mut tokens_iter = token_slice.into_iter();
	let redir_raw = shenv.input_slice(token.span());
	let mut redir_bldr = RedirBldr::from_str(&redir_raw).unwrap();
	// If there isn't an FD target, get the next token and use it as the filename
	if redir_bldr.tgt().is_none() {
		if let Some(filename) = tokens_iter.next() {
			// Make sure it's a word and not an operator or something
			if !matches!(filename.rule(), TkRule::SQuote | TkRule::DQuote | TkRule::Ident) || KEYWORDS.contains(&filename.rule()) {
				let mut err = ShErr::simple(ShErrKind::ParseErr, "Did not find a target for this redirection");
				let input = shenv.input_slice(token.span()).to_string();
				err.blame(input, token.span());
				return Err(err)
			}
			tokens_eaten += 1;
			// Construct the Path object
			let filename_raw = shenv.input_slice(filename.span()).to_string();
			let filename_path = PathBuf::from(filename_raw);
			let tgt = RedirTarget::File(filename_path);
			// Update the builder
			redir_bldr = redir_bldr.with_tgt(tgt);
		} else {
			let mut err = ShErr::simple(ShErrKind::ParseErr, "Did not find a target for this redirection");
			let input = shenv.input_slice(token.span()).to_string();
			err.blame(input, token.span());
			return Err(err)
		}
	}
	Ok((tokens_eaten,redir_bldr.build()))
}

// TODO: Redirs with FD sources appear to be looping endlessly for some reason

ndrule_def!(Main, shenv, |tokens: &[Token], shenv: &mut ShEnv| {
	log!(TRACE, "Parsing main");
	let mut cmd_lists = vec![];
	let mut node_toks = vec![];
	let mut token_slice = &*tokens;

	while let Some(node) = CmdList::try_match(token_slice,shenv)? {
		node_toks.extend(node.tokens().clone());
		token_slice = &token_slice[node.len()..];
		cmd_lists.push(node);
	}

	if cmd_lists.is_empty() {
		return Ok(None)
	}
	let span = get_span(&node_toks,shenv)?;
	let node = Node {
		node_rule: NdRule::Main { cmd_lists },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};
	Ok(Some(node))
});

ndrule_def!(CmdList, shenv, |tokens: &[Token], shenv: &mut ShEnv| {
	log!(TRACE, "Parsing cmdlist");
	let mut commands: Vec<(Option<CmdGuard>,Node)> = vec![];
	let mut node_toks = vec![];
	let mut token_slice = &*tokens;
	let mut cmd_guard = None; // Operators like '&&' and '||'

	while let Some(mut node) = Expr::try_match(token_slice,shenv)? {
		// Add sub-node tokens to our tokens
		node_toks.extend(node.tokens().clone());
		// Reflect changes in the token slice
		token_slice = &token_slice[node.len()..];
		// Push sub-node
		if let NdRule::Command { argv, redirs: _ } = node.rule() {
			if let Some(arg) = argv.first() {
				let slice = shenv.input_slice(arg.span().clone());
				if BUILTINS.contains(&slice) {
					*node.flags_mut() |= NdFlag::BUILTIN;
				}
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
	let span = get_span(&node_toks,shenv)?;
	let node = Node {
		node_rule: NdRule::CmdList { cmds: commands },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};
	Ok(Some(node))
});

ndrule_def!(Expr, shenv, |tokens: &[Token], shenv: &mut ShEnv| {
	try_rules!(tokens, shenv,
		ShellCmd,
		Pipeline,
		Subshell,
		Assignment,
		Command
	);
});
// Used in pipelines to avoid recursion
ndrule_def!(ExprNoPipeline, shenv, |tokens: &[Token], shenv: &mut ShEnv| {
	try_rules!(tokens, shenv,
		ShellCmd,
		Subshell,
		Assignment,
		Command
	);
});

ndrule_def!(ShellCmd, shenv, |tokens: &[Token], shenv: &mut ShEnv| {
	try_rules!(tokens, shenv,
		Case,
		ForLoop,
		IfThen,
		Loop,
		FuncDef
	);
});

ndrule_def!(Case, shenv, |mut tokens: &[Token], shenv: &mut ShEnv| {
	log!(DEBUG, tokens);
	let err = |msg: &str, span: Rc<RefCell<Span>>, shenv: &mut ShEnv | {
		ShErr::full(ShErrKind::ParseErr, msg, shenv.get_input(), span)
	};
	let mut tokens_iter = tokens.iter().peekable();
	let mut node_toks = vec![];
	let mut pat: Option<Token> = None;
	let mut blocks = vec![];
	let mut redirs = vec![];

	if let Some(token) = tokens_iter.next() {
		if let TkRule::Case = token.rule() {
			node_toks.push(token.clone());
			tokens = &tokens[1..];
		} else { return Ok(None) }
	} else { return Ok(None) }

	while let Some(token) = tokens_iter.next() {
		node_toks.push(token.clone());
		tokens = &tokens[1..];
		match token.rule() {
			TkRule::Whitespace => continue,
			TkRule::Ident => {
				pat = Some(token.clone());
				break
			}
			_ => return Err(err("Expected an ident in case statement", token.span(), shenv))
		}
	}

	if pat.is_none() {
		return Err(err("Expected an ident in case statement", node_toks.last().unwrap().span(), shenv))
	}
	let pat = pat.unwrap();

	let mut closed = false;
	while let Some(token) = tokens_iter.next() {
		node_toks.push(token.clone());
		tokens = &tokens[1..];
		match token.rule() {
			TkRule::Whitespace => continue,
			TkRule::Ident => {
				if token.as_raw(shenv) != "in" {
					return Err(err("Expected `in` after case statement pattern", token.span(), shenv))
				} else {
					closed = true;
				}
			}
			TkRule::Sep => {
				if closed {
					break
				}
			}
			_ => return Err(err("Expected `in` after case statement pattern", token.span(), shenv))
		}
	}

	if tokens_iter.peek().is_none() {
		return Err(err("Expected `in` after case statement pattern", node_toks.last().unwrap().span(), shenv))
	}

	let mut closed = false;
	loop {
		if tokens.is_empty() {
			break
		}
		if let Some(token) = tokens_iter.next() {
			match token.rule() {
				TkRule::CasePat => {
					node_toks.push(token.clone());
					tokens = &tokens[1..];
					let block_pat = token.clone();
					let (used,lists) = get_lists(tokens, shenv);
					let mut lists_iter = lists.iter().peekable();
					log!(DEBUG, used);
					log!(DEBUG, lists);
					while let Some(list) = lists_iter.next() {
						node_toks.extend(list.tokens.clone());
						if lists_iter.peek().is_none() {
							log!(DEBUG, list);
							for token in list.tokens() {
								log!(DEBUG, "{}", token.as_raw(shenv));
							}
						}
						if let Some(token) = list.tokens().last() {
							if lists_iter.peek().is_none() && (token.rule() != TkRule::Sep || token.as_raw(shenv).trim() != ";;") {
								log!(ERROR, "{:?}",list.tokens());
								log!(ERROR, token);
								log!(ERROR, "{}",token.as_raw(shenv).trim());
								return Err(err("Expected `;;` after case block", token.span(), shenv))
							}
						}
					}
					tokens = &tokens[used..];
					tokens_iter = tokens.iter().peekable();
					blocks.push((block_pat,lists));
				}
				TkRule::Esac => {
					node_toks.push(token.clone());
					tokens = &tokens[1..];
					closed = true;
				}
				TkRule::Sep => {
					node_toks.push(token.clone());
					tokens = &tokens[1..];
					if closed {
						break
					}
				}
				TkRule::RedirOp if closed => {
					node_toks.push(token.clone());
					tokens = &tokens[1..];
					let (used,redir) = get_redir(token.clone(), tokens, shenv)?;
					for _ in 0..used {
						if let Some(token) = tokens_iter.next() {
							node_toks.push(token.clone());
						}
					}
					tokens = &tokens[used..];
					redirs.push(redir);
				}
				_ => {
					log!(DEBUG, token);
					return Err(err("Expected `esac` or a case block here", node_toks.last().unwrap().span(), shenv))
				}
			}
		}
	}

	if !closed {
		return Err(err("Expected `esac` after case statement", node_toks.last().unwrap().span(), shenv))
	}

	let span = get_span(&node_toks,shenv)?;
	let node = Node {
		node_rule: NdRule::Case { pat, blocks, redirs },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};

	Ok(Some(node))
});

ndrule_def!(ForLoop, shenv, |mut tokens: &[Token], shenv: &mut ShEnv| {
	let err = |msg: &str, span: Rc<RefCell<Span>>, shenv: &mut ShEnv | {
		ShErr::full(ShErrKind::ParseErr, msg, shenv.get_input(), span)
	};
	let mut tokens_iter = tokens.iter().peekable();
	let mut node_toks = vec![];
	let mut vars = vec![];
	let mut arr = vec![];
	let mut redirs = vec![];
	let body: Vec<Node>;

	if let Some(token) = tokens_iter.next() {
		if let TkRule::For = token.rule() {
			node_toks.push(token.clone());
			tokens = &tokens[1..];
		} else { return Ok(None) }
	} else { return Ok(None) }

	while let Some(token) = tokens_iter.next() {
		node_toks.push(token.clone());
		tokens = &tokens[1..];
		if let TkRule::Ident = token.rule() {
			if token.as_raw(shenv) == "in" { break }
			vars.push(token.clone());
		} else {
			let span = get_span(&node_toks, shenv)?;
			return Err(err("Expected an ident in for loop vars",span,shenv))
		}
	}
	if vars.is_empty() {
		let span = get_span(&node_toks, shenv)?;
		return Err(err("Expected an ident in for loop vars",span,shenv))
	}
	while let Some(token) = tokens_iter.next() {
		node_toks.push(token.clone());
		tokens = &tokens[1..];
		if token.rule() == TkRule::Sep { break }
		if let TkRule::Ident = token.rule() {
			arr.push(token.clone());
		} else {
			let span = get_span(&node_toks, shenv)?;
			return Err(err("Expected an ident in for loop array",span,shenv))
		}
	}
	if arr.is_empty() {
		let span = get_span(&node_toks, shenv)?;
		return Err(err("Expected an ident in for loop array",span,shenv))
	}

	let mut closed = false;
	while let Some(token) = tokens_iter.next() {
		match token.rule() {
			TkRule::Sep | TkRule::Whitespace => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				if closed { break }
			}
			TkRule::Do => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				closed = true;
			}
			_ => {
				if closed { break }
				let span = get_span(&node_toks,shenv)?;
				return Err(err("Expected `do` after loop condition",span,shenv))
			}
		}
	}

	let (used,lists) = get_lists(tokens, shenv);
	for list in &lists {
		node_toks.extend(list.tokens().clone());
	}
	tokens = &tokens[used..];
	body = lists;
	tokens_iter = tokens.iter().peekable();

	let mut closed = false;
	while let Some(token) = tokens_iter.next() {
		match token.rule() {
			TkRule::Done => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				closed = true;
			}
			TkRule::Sep => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				if closed { break }
			}
			TkRule::RedirOp if closed => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				let (used,redir) = get_redir(token.clone(), tokens, shenv)?;
				for _ in 0..used {
					if let Some(token) = tokens_iter.next() {
						node_toks.push(token.clone());
					}
				}
				tokens = &tokens[used..];
				redirs.push(redir);
			}
			_ => {
				let span = get_span(&node_toks, shenv)?;
				return Err(err("Expected `done` after for loop",span,shenv))
			}
		}
	}

	if !closed {
		let span = get_span(&node_toks, shenv)?;
		return Err(err("Expected `done` after for loop",span,shenv))
	}

	let span = get_span(&node_toks, shenv)?;
	let node = Node {
		node_rule: NdRule::ForLoop { vars, arr, body, redirs },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};

	Ok(Some(node))
});

ndrule_def!(IfThen, shenv, |mut tokens: &[Token], shenv: &mut ShEnv| {
	log!(DEBUG,tokens);
	let err = |msg: &str, span: Rc<RefCell<Span>>, shenv: &mut ShEnv | {
		ShErr::full(ShErrKind::ParseErr, msg, shenv.get_input(), span)
	};
	let mut tokens_iter = tokens.iter().peekable();
	let mut node_toks = vec![];
	let mut cond_blocks = vec![];
	let mut redirs = vec![];
	let mut else_block: Option<Vec<Node>> = None;

	log!(DEBUG,tokens);
	if let Some(token) = tokens_iter.next() {
		if let TkRule::If = token.rule() {
			node_toks.push(token.clone());
			tokens = &tokens[1..];
		} else { return Ok(None) }
	} else { return Ok(None) }

	let (used,lists) = get_lists(tokens, shenv);
	for list in &lists {
		node_toks.extend(list.tokens().clone());
	}
	tokens = &tokens[used..];
	let cond = lists;
	tokens_iter = tokens.iter().peekable();

	let mut closed = false;
	while let Some(token) = tokens_iter.next() {
		match token.rule() {
			TkRule::Then => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				closed = true;
			}
			TkRule::Sep | TkRule::Whitespace => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				if closed { break }
			}
			_ => {
				if closed { break }
				log!(ERROR, token);
				let span = get_span(&node_toks,shenv)?;
				return Err(err("Expected `then` after if statement condition",span,shenv))
			}
		}
	}

	if tokens_iter.peek().is_none() {
		let span = get_span(&node_toks,shenv)?;
		return Err(err("Failed to parse this if statement",span,shenv))
	}

	let (used,lists) = get_lists(tokens, shenv);
	for list in &lists {
		node_toks.extend(list.tokens().clone());
	}
	tokens = &tokens[used..];
	let body = lists;
	tokens_iter = tokens.iter().peekable();
	cond_blocks.push((cond,body));


	let mut closed = false;
	while let Some(token) = tokens_iter.next() {
		match token.rule() {
			TkRule::Elif => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];

				let (used,lists) = get_lists(tokens, shenv);
				for list in &lists {
					node_toks.extend(list.tokens().clone());
				}
				tokens = &tokens[used..];
				let cond = lists;
				tokens_iter = tokens.iter().peekable();

				let mut closed = false;
				while let Some(token) = tokens_iter.next() {
					match token.rule() {
						TkRule::Then => {
							node_toks.push(token.clone());
							tokens = &tokens[1..];
							closed = true;
						}
						TkRule::Sep | TkRule::Whitespace => {
							node_toks.push(token.clone());
							tokens = &tokens[1..];
							if closed { break }
						}
						_ => {
							let span = get_span(&node_toks,shenv)?;
							return Err(err("Expected `then` after if statement condition",span,shenv))
						}
					}
				}

				if tokens_iter.peek().is_none() {
					let span = get_span(&node_toks,shenv)?;
					return Err(err("Failed to parse this if statement",span,shenv))
				}

				let (used,lists) = get_lists(tokens, shenv);
				for list in &lists {
					node_toks.extend(list.tokens().clone());
				}
				tokens = &tokens[used..];
				let body = lists;
				tokens_iter = tokens.iter().peekable();
				cond_blocks.push((cond,body));
			}
			TkRule::Else => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				let (used,lists) = get_lists(tokens, shenv);
				for list in &lists {
					node_toks.extend(list.tokens().clone());
				}
				tokens = &tokens[used..];
				else_block = Some(lists);
				tokens_iter = tokens.iter().peekable();
			}
			TkRule::Fi => {
				closed = true;
				node_toks.push(token.clone());
				tokens = &tokens[1..];
			}
			TkRule::Whitespace => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
			}
			TkRule::Sep => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				if closed { break }
			}
			TkRule::RedirOp if closed => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				let (used,redir) = get_redir(token.clone(), tokens, shenv)?;
				for _ in 0..used {
					if let Some(token) = tokens_iter.next() {
						node_toks.push(token.clone());
					}
				}
				tokens = &tokens[used..];
				redirs.push(redir);
			}
			_ => {
				let span = get_span(&node_toks, shenv)?;
				return Err(err(&format!("Unexpected token in if statement: {:?}",token.rule()),span,shenv))
			}
		}
	}

	if !closed {
		let span = get_span(&node_toks, shenv)?;
		return Err(err("Expected `fi` to close if statement",span,shenv))
	}

	let span = get_span(&node_toks, shenv)?;
	let node = Node {
		node_rule: NdRule::IfThen { cond_blocks, else_block, redirs },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};

	Ok(Some(node))
});

ndrule_def!(Loop, shenv, |mut tokens: &[Token], shenv: &mut ShEnv| {
	let err = |msg: &str, span: Rc<RefCell<Span>>, shenv: &mut ShEnv | {
		ShErr::full(ShErrKind::ParseErr, msg, shenv.get_input(), span)
	};
	let mut tokens_iter = tokens.iter().peekable();
	let mut node_toks = vec![];
	let mut redirs = vec![];
	let kind: LoopKind;
	let cond: Vec<Node>;
	let body: Vec<Node>;

	if let Some(token) = tokens_iter.next() {
		node_toks.push(token.clone());
		match token.rule() {
			TkRule::While => {
				kind = LoopKind::While
			}
			TkRule::Until => {
				kind = LoopKind::Until
			}
			_ => return Ok(None)
		}
	} else { return Ok(None) }
	tokens = &tokens[1..];

	let (used,lists) = get_lists(tokens, shenv);
	for list in &lists {
		node_toks.extend(list.tokens().clone());
	}
	tokens = &tokens[used..];
	cond = lists;
	tokens_iter = tokens.iter().peekable();

	let mut closed = false;
	while let Some(token) = tokens_iter.next() {
		match token.rule() {
			TkRule::Sep | TkRule::Whitespace => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				if closed { break }
			}
			TkRule::Do => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				closed = true;
			}
			_ => {
				if closed { break }
				let span = get_span(&node_toks,shenv)?;
				return Err(err("Expected `do` after loop condition",span,shenv))
			}
		}
	}

	if tokens_iter.peek().is_none() {
		return Ok(None)
	}

	let (used,lists) = get_lists(tokens, shenv);
	for list in &lists {
		node_toks.extend(list.tokens().clone());
	}
	tokens = &tokens[used..];
	body = lists;
	tokens_iter = tokens.iter().peekable();

	let mut closed = false;
	while let Some(token) = tokens_iter.next() {
		match token.rule() {
			TkRule::Sep | TkRule::Whitespace => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				if closed { break }
			}
			TkRule::Done => {
				closed = true;
				node_toks.push(token.clone());
				tokens = &tokens[1..];
			}
			TkRule::RedirOp if closed => {
				node_toks.push(token.clone());
				tokens = &tokens[1..];
				let (used,redir) = get_redir(token.clone(), tokens, shenv)?;
				for _ in 0..used {
					if let Some(token) = tokens_iter.next() {
						node_toks.push(token.clone());
					}
				}
				tokens = &tokens[used..];
				redirs.push(redir);
			}
			_ => {
				let span = get_span(&node_toks,shenv)?;
				return Err(err("Unexpected token in loop",span,shenv))
			}
		}
	}

	if !closed {
		let span = get_span(&node_toks,shenv)?;
		return Err(err("Expected `done` to close loop",span,shenv))
	}

	let span = get_span(&node_toks, shenv)?;
	let node = Node {
		node_rule: NdRule::Loop { kind, cond, body, redirs },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};

	Ok(Some(node))
});

ndrule_def!(FuncDef, shenv, |tokens: &[Token], shenv: &mut ShEnv| {
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

	let span = get_span(&node_toks,shenv)?;
	let node = Node {
		node_rule: NdRule::FuncDef { name, body },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};
	Ok(Some(node))
});

ndrule_def!(Subshell, shenv, |tokens: &[Token], shenv: &mut ShEnv| {
	let mut tokens_iter = tokens.into_iter();
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
						let slice = &tokens_iter.clone().map(|tk| tk.clone()).collect::<Vec<_>>();
						let (used,redir) = get_redir(token.clone(), slice, shenv)?;
						for _ in 0..used {
							if let Some(token) = tokens_iter.next() {
								node_toks.push(token.clone());
							}
						}
						redirs.push(redir);
					}
					_ => break
				}
			}
			let span = get_span(&node_toks,shenv)?;
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

ndrule_def!(Pipeline, shenv, |mut tokens: &[Token], shenv: &mut ShEnv| {
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
		if let Some(mut cmd) = ExprNoPipeline::try_match(tokens,shenv)? {
			// Add sub-node's tokens to our tokens
			node_toks.extend(cmd.tokens().clone());

			// Reflect changes in tokens and tokens_iter
			tokens = &tokens[cmd.len()..];
			for _ in 0..cmd.len() {
				tokens_iter.next();
			}

			if let NdRule::Command { argv, redirs: _ } = cmd.rule() {
				if let Some(arg) = argv.first() {
					let slice = shenv.input_slice(arg.span().clone());
					if BUILTINS.contains(&slice) {
						*cmd.flags_mut() |= NdFlag::BUILTIN;
					}
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
	let span = get_span(&node_toks,shenv)?;
	let node = Node {
		node_rule: NdRule::Pipeline { cmds },
		tokens: node_toks,
		span,
		flags: NdFlag::empty()
	};
	Ok(Some(node))
});

ndrule_def!(Command, shenv, |tokens: &[Token], shenv: &mut ShEnv| {
	log!(TRACE, "Parsing command");
	let mut tokens_iter = tokens.iter().peekable();
	let mut node_toks = vec![];
	let mut argv = vec![];
	let mut redirs = vec![];

	while let Some(token) = tokens_iter.peek() {
		match token.rule() {
			TkRule::AndOp | TkRule::OrOp | TkRule::PipeOp | TkRule::ErrPipeOp => {
				break
			}
			_ => { /* Keep going */ }
		}
		let token = tokens_iter.next().unwrap();
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
				let slice = &tokens_iter.clone().map(|tk| tk.clone()).collect::<Vec<_>>();
				let (used,redir) = get_redir(token.clone(), slice, shenv)?;
				for _ in 0..used {
					if let Some(token) = tokens_iter.next() {
						node_toks.push(token.clone());
					}
				}
				redirs.push(redir);
			}
			TkRule::Sep => break,
			_ => return Err(
				ShErr::full(
					ShErrKind::ParseErr,
					format!("Unexpected token in command rule: {:?}", token.rule()),
					shenv.get_input(),
					get_span(&node_toks,shenv)?
				)
			)
		}
	}
	if node_toks.is_empty() {
		return Ok(None)
	}
	let span = get_span(&node_toks,shenv)?;
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

ndrule_def!(Assignment, shenv, |tokens: &[Token], shenv: &mut ShEnv| {
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
		let cmd = Command::try_match(tokens_slice,shenv)?.map(|cmd| Box::new(cmd));
		if let Some(ref cmd) = cmd {
			node_toks.extend(cmd.tokens().clone());
		}
		let span = get_span(&node_toks,shenv)?;
		let node = Node {
			node_rule: NdRule::Assignment { assignments, cmd },
			tokens: node_toks,
			span,
			flags: NdFlag::empty()
		};
		return Ok(Some(node))
	} else {
		let span = get_span(&node_toks,shenv)?;
		let node = Node {
			node_rule: NdRule::Assignment { assignments, cmd: None },
			tokens: node_toks,
			span,
			flags: NdFlag::empty()
		};
		Ok(Some(node))
	}
});
