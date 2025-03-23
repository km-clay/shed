use std::rc::Arc;

use crate::parse::{lex::{LexFlags, LexStream}, node_operation, Node, ParseStream};

pub mod lexer;
pub mod parser;
pub mod expand;
pub mod term;
pub mod error;
pub mod getopt;

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
