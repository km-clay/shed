use std::str::FromStr;

use bitflags::bitflags;
use lex::{is_hard_sep, Span, Tk, TkFlags, TkRule};

use crate::{prelude::*, libsh::error::{ShErr, ShErrKind, ShResult}, procio::{IoFd, IoFile, IoInfo}};

pub mod lex;
pub mod execute;


#[derive(Debug)]
pub struct Node<'t> {
	pub class: NdRule<'t>,
	pub flags: NdFlags,
	pub redirs: Vec<Redir>,
	pub tokens: Vec<Tk<'t>>,
}

impl<'t> Node<'t> {
	pub fn get_command(&'t self) -> Option<&'t Tk<'t>> {
		let NdRule::Command { assignments: _, argv } = &self.class else {
			return None
		};
		let command = argv.iter().find(|tk| tk.flags.contains(TkFlags::IS_CMD))?;
		Some(command)
	}
}

bitflags! {
#[derive(Debug)]
	pub struct NdFlags: u32 {
		const BACKGROUND = 0b000001;
	}
}

#[derive(Debug)]
pub struct Redir {
	pub io_info: Box<dyn IoInfo>,
	pub class: RedirType
}

impl Redir {
	pub fn new(io_info: Box<dyn IoInfo>, class: RedirType) -> Self {
		Self { io_info, class }
	}
}

#[derive(Default,Debug)]
pub struct RedirBldr {
	pub io_info: Option<Box<dyn IoInfo>>,
	pub class: Option<RedirType>,
	pub tgt_fd: Option<RawFd>,
}

impl RedirBldr {
	pub fn new() -> Self {
		Default::default()
	}
	pub fn with_io_info(self, io_info: Box<dyn IoInfo>) -> Self {
		let Self { io_info: _, class, tgt_fd } = self;
		Self { io_info: Some(io_info), class, tgt_fd }
	}
	pub fn with_class(self, class: RedirType) -> Self {
		let Self { io_info, class: _, tgt_fd } = self;
		Self { io_info, class: Some(class), tgt_fd }
	}
	pub fn with_tgt(self, tgt_fd: RawFd) -> Self {
		let Self { io_info, class, tgt_fd: _ } = self;
		Self { io_info, class, tgt_fd: Some(tgt_fd) }
	}
	pub fn build(self) -> Redir {
		Redir::new(self.io_info.unwrap(), self.class.unwrap())
	}
}

impl FromStr for RedirBldr {
	type Err = ();
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let mut chars = s.chars().peekable();
		let mut src_fd = String::new();
		let mut tgt_fd = String::new();
		let mut redir = RedirBldr::new();

		while let Some(ch) = chars.next() {
			match ch {
				'>' => {
					redir = redir.with_class(RedirType::Output);
					if let Some('>') = chars.peek() {
						chars.next();
						redir = redir.with_class(RedirType::Append);
					}
				}
				'<' => {
					redir = redir.with_class(RedirType::Input);
					let mut count = 0;

					while count < 2 && matches!(chars.peek(), Some('<')) {
						chars.next();
						count += 1;
					}

					redir = match count {
						1 => redir.with_class(RedirType::HereDoc),
						2 => redir.with_class(RedirType::HereString),
						_ => redir, // Default case remains RedirType::Input
					};
				}
				'&' => {
					while let Some(next_ch) = chars.next() {
						if next_ch.is_ascii_digit() {
							src_fd.push(next_ch)
						} else {
							break
						}
					}
					if src_fd.is_empty() {
						return Err(())
					}
				}
				_ if ch.is_ascii_digit() && tgt_fd.is_empty() => {
					tgt_fd.push(ch);
					while let Some(next_ch) = chars.peek() {
						if next_ch.is_ascii_digit() {
							let next_ch = chars.next().unwrap();
							tgt_fd.push(next_ch);
						} else {
							break
						}
					}
				}
				_ => return Err(())
			}
		}

		// FIXME: I am 99.999999999% sure that tgt_fd and src_fd are backwards here
		let tgt_fd = tgt_fd.parse::<i32>().unwrap_or_else(|_| {
			match redir.class.unwrap() {
				RedirType::Input |
					RedirType::HereDoc |
					RedirType::HereString => 0,
				_ => 1
			}
		});
		redir = redir.with_tgt(tgt_fd);
		if let Ok(src_fd) = src_fd.parse::<i32>() {
			let io_info = IoFd::new(tgt_fd, src_fd);
			redir = redir.with_io_info(Box::new(io_info));
		}
		Ok(redir)
	}
}

#[derive(PartialEq,Clone,Copy,Debug)]
pub enum RedirType {
	Null, // Default
	Pipe, // |
	PipeAnd, // |&, redirs stderr and stdout
	Input, // <
	Output, // >
	Append, // >>
	HereDoc, // <<
	HereString, // <<<
}

#[derive(Debug)]
pub struct CondNode<'t> {
	pub cond: Vec<Node<'t>>,
	pub body: Vec<Node<'t>>
}

#[derive(Debug)]
pub struct CaseNode<'t> {
	pub pattern: Tk<'t>,
	pub body: Vec<Node<'t>>
}

#[derive(Clone,Copy,PartialEq,Debug)]
pub enum ConjunctOp {
	And,
	Or,
	Null
}

#[derive(Debug)]
pub struct ConjunctNode<'t> {
	pub cmd: Box<Node<'t>>,
	pub operator: ConjunctOp
}

#[derive(Debug)]
pub enum LoopKind {
	While,
	Until
}

#[derive(Debug)]
pub enum AssignKind {
	Eq,
	PlusEq,
	MinusEq,
	MultEq,
	DivEq,
}

#[derive(Debug)]
pub enum NdRule<'t> {
	IfNode { cond_blocks: Vec<CondNode<'t>>, else_block: Vec<Node<'t>> },
	LoopNode { kind: LoopKind, cond_block: CondNode<'t> },
	ForNode { vars: Vec<Tk<'t>>, arr: Vec<Tk<'t>>, body: Vec<Node<'t>> },
	CaseNode { pattern: Tk<'t>, case_blocks: Vec<CaseNode<'t>> },
	Command { assignments: Vec<Node<'t>>, argv: Vec<Tk<'t>> },
	Pipeline { cmds: Vec<Node<'t>>, pipe_err: bool },
	CmdList { elements: Vec<ConjunctNode<'t>> },
	Assignment { kind: AssignKind, var: Tk<'t>, val: Tk<'t> },
}

#[derive(Debug)]
pub struct ParseStream<'t> {
	pub tokens: Vec<Tk<'t>>,
	pub flags: ParseFlags
}

bitflags! {
	#[derive(Debug)]
	pub struct ParseFlags: u32 {
		const ERROR = 0b0000001;
	}
}

impl<'t> ParseStream<'t> {
	pub fn new(tokens: Vec<Tk<'t>>) -> Self {
		Self { tokens, flags: ParseFlags::empty() }
	}
	fn next_tk_class(&self) -> &TkRule {
		if let Some(tk) = self.tokens.first() {
			&tk.class
		} else {
			&TkRule::Null
		}
	}
	fn next_tk(&mut self) -> Option<Tk<'t>> {
		if !self.tokens.is_empty() {
			if *self.next_tk_class() == TkRule::EOI {
				return None
			}
			Some(self.tokens.remove(0))
		} else {
			None
		}
	}
	/// Slice off consumed tokens
	fn commit(&mut self, num_consumed: usize) {
		assert!(num_consumed <= self.tokens.len());
		self.tokens = self.tokens[num_consumed..].to_vec();
	}
	fn parse_cmd_list(&mut self) -> ShResult<Option<Node<'t>>> {
		let mut elements = vec![];
		let mut node_tks = vec![];

		while let Some(block) = self.parse_block(true)? {
			node_tks.append(&mut block.tokens.clone());
			let conjunct_op = match self.next_tk_class() {
				TkRule::And => ConjunctOp::And,
				TkRule::Or => ConjunctOp::Or,
				_ => ConjunctOp::Null
			};
			let conjunction = ConjunctNode { cmd: Box::new(block), operator: conjunct_op };
			elements.push(conjunction);
			let Some(tk) = self.next_tk() else {
				break
			};
			node_tks.push(tk);
			if conjunct_op == ConjunctOp::Null {
				break
			}
		}
		if elements.is_empty() {
			Ok(None)
		} else {
			Ok(Some(Node {
				class: NdRule::CmdList { elements },
				flags: NdFlags::empty(),
				redirs: vec![],
				tokens: node_tks
			}))
		}
	}
	/// This tries to match on different stuff that can appear in a command position
	/// Matches shell commands like if-then-fi, pipelines, etc.
	/// Ordered from specialized to general, with more generally matchable stuff appearing at the bottom
	/// The check_pipelines parameter is used to prevent infinite recursion in parse_pipeline
	fn parse_block(&mut self, check_pipelines: bool) -> ShResult<Option<Node<'t>>> {
		if check_pipelines {
			if let Some(node) = self.parse_pipeline()? {
				return Ok(Some(node))
			}
		} else {
			if let Some(node) = self.parse_cmd()? {
				return Ok(Some(node))
			}
		}
		Ok(None)
	}
	fn parse_pipeline(&mut self) -> ShResult<Option<Node<'t>>> {
		let mut cmds = vec![];
		let mut node_tks = vec![];
		while let Some(cmd) = self.parse_block(false)? {
			let is_punctuated = node_is_punctuated(&cmd.tokens);
			node_tks.append(&mut cmd.tokens.clone());
			cmds.push(cmd);
			if *self.next_tk_class() != TkRule::Pipe || is_punctuated {
				break
			} else {
				if let Some(pipe) = self.next_tk() {
					node_tks.push(pipe)
				} else {
					break
				}
			}
		}
		if cmds.is_empty() {
			Ok(None)
		} else {
			Ok(Some(Node {
				// TODO: implement pipe_err support
				class: NdRule::Pipeline { cmds, pipe_err: false },
				flags: NdFlags::empty(),
				redirs: vec![],
				tokens: node_tks
			}))
		}
	}
	fn parse_cmd(&mut self) -> ShResult<Option<Node<'t>>> {
		let tk_slice = self.tokens.as_slice();
		let mut tk_iter = tk_slice.iter();
		let mut node_tks = vec![];
		let mut redirs = vec![];
		let mut argv = vec![];
		let mut assignments = vec![];

		while let Some(prefix_tk) = tk_iter.next() {
			if prefix_tk.flags.contains(TkFlags::IS_CMD) {
				node_tks.push(prefix_tk.clone());
				argv.push(prefix_tk.clone());
				break
			} else if prefix_tk.flags.contains(TkFlags::ASSIGN) {
				let Some(assign) = self.parse_assignment(&prefix_tk) else {
					break
				};
				node_tks.push(prefix_tk.clone());
				assignments.push(assign)
			}
		}

		if argv.is_empty() && assignments.is_empty() {
			return Ok(None)
		}

		while let Some(tk) = tk_iter.next() {
			match tk.class {
				TkRule::EOI |
					TkRule::Pipe |
					TkRule::And |
					TkRule::Or => {
						break
					}
				TkRule::Sep => {
					node_tks.push(tk.clone());
					break
				}
				TkRule::Str => {
					argv.push(tk.clone());
					node_tks.push(tk.clone());
				}
				TkRule::Redir => {
					node_tks.push(tk.clone());
					let redir_bldr = tk.span.as_str().parse::<RedirBldr>().unwrap();
					if redir_bldr.io_info.is_none() {
						let path_tk = tk_iter.next();

						if path_tk.is_none_or(|tk| tk.class == TkRule::EOI) {
							self.flags |= ParseFlags::ERROR;
							return Err(
								ShErr::full(
									ShErrKind::ParseErr,
									"Expected a filename after this redirection",
									tk.span.clone()
								)
							)
						};

						let path_tk = path_tk.unwrap();
						node_tks.push(path_tk.clone());

						let Ok(file) = (match redir_bldr.class.unwrap() {
							RedirType::Input => {
								OpenOptions::new()
									.read(true)
									.open(Path::new(path_tk.span.as_str()))
							}
							RedirType::Output => {
								OpenOptions::new()
									.write(true)
									.create(true)
									.truncate(true)
									.open(Path::new(path_tk.span.as_str()))
							}
							RedirType::Append => {
								OpenOptions::new()
									.write(true)
									.create(true)
									.append(true)
									.open(Path::new(path_tk.span.as_str()))
							}
							_ => unreachable!()
						}) else {
							self.flags |= ParseFlags::ERROR;
							return Err(
								ShErr::full(
									ShErrKind::InternalErr,
									"Error opening file for redirection",
									path_tk.span.clone()
								)
							)
						};

						let io_info = IoFile::new(redir_bldr.tgt_fd.unwrap(), file);
						let redir_bldr = redir_bldr.with_io_info(Box::new(io_info));
						let redir = redir_bldr.build();
						redirs.push(redir);
					}
				}
				_ => unimplemented!("Unexpected token rule `{:?}` in parse_cmd()",tk.class)
			}
		}
		self.commit(node_tks.len());

		Ok(Some(Node {
			class: NdRule::Command { assignments, argv },
			tokens: node_tks,
			flags: NdFlags::empty(),
			redirs,
		}))
	}
	fn parse_assignment(&self, token: &Tk<'t>) -> Option<Node<'t>> {
		let mut chars = token.span.as_str().chars();
		let mut var_name = String::new();
		let mut name_range = token.span.start..token.span.start;
		let mut var_val = String::new();
		let mut val_range = token.span.end..token.span.end;
		let mut assign_kind = None;
		let mut pos = token.span.start;

		while let Some(ch) = chars.next() {
			if !assign_kind.is_none() {
				match ch {
					'\\' => {
						pos += ch.len_utf8();
						var_val.push(ch);
						if let Some(esc_ch) = chars.next() {
							pos += esc_ch.len_utf8();
							var_val.push(esc_ch);
						}
					}
					_ => {
						pos += ch.len_utf8();
						var_val.push(ch);
					}
				}
			} else {
				match ch {
					'=' => {
						name_range.end = pos;
						pos += ch.len_utf8();
						val_range.start = pos;
						assign_kind = Some(AssignKind::Eq);
					}
					'-' => {
						name_range.end = pos;
						pos += ch.len_utf8();
						let Some('=') = chars.next() else {
							return None
						};
						pos += '='.len_utf8();
						val_range.start = pos;
						assign_kind = Some(AssignKind::MinusEq);
					}
					'+' => {
						name_range.end = pos;
						pos += ch.len_utf8();
						let Some('=') = chars.next() else {
							return None
						};
						pos += '='.len_utf8();
						val_range.start = pos;
						assign_kind = Some(AssignKind::PlusEq);
					}
					'/' => {
						name_range.end = pos;
						pos += ch.len_utf8();
						let Some('=') = chars.next() else {
							return None
						};
						pos += '='.len_utf8();
						val_range.start = pos;
						assign_kind = Some(AssignKind::DivEq);
					}
					'*' => {
						name_range.end = pos;
						pos += ch.len_utf8();
						let Some('=') = chars.next() else {
							return None
						};
						pos += '='.len_utf8();
						val_range.start = pos;
						assign_kind = Some(AssignKind::MultEq);
					}
					'\\' => {
						pos += ch.len_utf8();
						var_name.push(ch);
						if let Some(esc_ch) = chars.next() {
							pos += esc_ch.len_utf8();
							var_name.push(esc_ch);
						}
					}
					_ => {
						pos += ch.len_utf8();
						var_name.push(ch)
					}
				}
			}
		}
		if assign_kind.is_none() || var_name.is_empty() {
			None
		} else {
			let var = Tk::new(TkRule::Str, Span::new(name_range, token.source()));
			let val = Tk::new(TkRule::Str, Span::new(val_range, token.source()));
			Some(Node {
				class: NdRule::Assignment { kind: assign_kind.unwrap(), var, val },
				tokens: vec![token.clone()],
				flags: NdFlags::empty(),
				redirs: vec![]
			})
		}
	}
}

impl<'t> Iterator for ParseStream<'t> {
	type Item = ShResult<Node<'t>>;
	fn next(&mut self) -> Option<Self::Item> {
		// Empty token vector or only SOI/EOI tokens, nothing to do
		if self.tokens.is_empty() || self.tokens.len() == 2 {
			return None
		}
		if self.flags.contains(ParseFlags::ERROR) {
			return None
		}
		if let Some(tk) = self.tokens.first() {
			if tk.class == TkRule::EOI {
				return None
			}
			if tk.class == TkRule::SOI {
				self.next_tk();
			}
		}
		match self.parse_cmd_list() {
			Ok(Some(node)) => return Some(Ok(node)),
			Ok(None) => return None,
			Err(e) => return Some(Err(e))
		}
	}
}

fn node_is_punctuated<'t>(tokens: &Vec<Tk>) -> bool {
	tokens.last().is_some_and(|tk| {
		matches!(tk.class, TkRule::Sep)
	})
}
