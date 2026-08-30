//! Parsing, lexing, and execution

use super::{
  Shed, builtin, errln, expand, match_loop, procio, sherr, shopt, signal, state, state::jobs,
  try_var, two_way_display, util, var,
};

pub(super) mod execute;
pub(super) mod lex;

pub(super) mod parse;
pub(super) use parse::{
  AssignKind, CaseNode, CondNode, ConjunctNode, ConjunctOp, LoopKind, NdFlags, NdRule, Node,
  ParseFlags, ParsedSrc,
};

#[cfg(test)]
pub(super) use parse::NdKind;
