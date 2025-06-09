use std::sync::Arc;

use super::*;
use crate::libsh::error::{
	Note, ShErr, ShErrKind
};
use crate::parse::{
	node_operation, Node, NdRule, ParseStream,
	lex::{
		Tk, TkRule, LexFlags, LexStream
	}
};
use crate::expand::{
	expand_aliases, unescape_str
};
use crate::state::{
	write_logic, write_vars
};


pub mod lexer;
pub mod parser;
pub mod expand;
pub mod term;
pub mod error;
pub mod getopt;
pub mod script;
pub mod highlight;
pub mod readline;

/// Unsafe to use outside of tests
pub fn get_nodes<F1>(input: &str, filter: F1) -> Vec<Node>
	where
		F1: Fn(&Node) -> bool
{
	let mut nodes = vec![];
	let tokens = LexStream::new(Arc::new(input.into()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect::<Vec<_>>();
	let mut parsed_nodes = ParseStream::new(tokens)
		.map(|nd| nd.unwrap())
		.collect::<Vec<_>>();

	for node in parsed_nodes.iter_mut() {
		node_operation(node,
			&filter,
			&mut |node: &mut Node| nodes.push(node.clone())
		);
	}
	nodes
}
