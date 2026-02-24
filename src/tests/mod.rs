use std::sync::Arc;

use super::*;
use crate::expand::{expand_aliases, unescape_str};
use crate::libsh::error::{Note, ShErr, ShErrKind};
use crate::parse::{
  lex::{LexFlags, LexStream, Tk, TkRule},
  node_operation, NdRule, Node, ParseStream,
};
use crate::state::{write_logic, write_vars};

pub mod complete;
pub mod error;
pub mod expand;
pub mod getopt;
pub mod highlight;
pub mod lexer;
pub mod parser;
pub mod readline;
pub mod redir;
pub mod script;
pub mod state;
pub mod term;

/// Unsafe to use outside of tests
pub fn get_nodes<F1>(input: &str, filter: F1) -> Vec<Node>
where
  F1: Fn(&Node) -> bool,
{
  let mut nodes = vec![];
  let tokens = LexStream::new(Arc::new(input.into()), LexFlags::empty())
    .map(|tk| tk.unwrap())
    .collect::<Vec<_>>();
  let mut parsed_nodes = ParseStream::new(tokens)
    .map(|nd| nd.unwrap())
    .collect::<Vec<_>>();

  for node in parsed_nodes.iter_mut() {
    node_operation(node, &filter, &mut |node: &mut Node| {
      nodes.push(node.clone())
    });
  }
  nodes
}
