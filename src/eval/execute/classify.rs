//! This module contains functions for classifying commands
//!
//! Used to dispatch commands to the proper execution functions, and for stuff like figuring out of a given path
//! can be autocd'd to

use std::{os::unix::fs::PermissionsExt, path::Path};

use bstr::ByteSlice;

use crate::{builtin, shopt, state::Shed, try_var, var};

use super::{
  lex::{Tk, TkFlags},
  parse::{
    ast::{Ast, NodeId},
    node::{NdRule, Node},
  },
};
pub(crate) fn in_cd_path(name: &Tk) -> bool {
  let Ok(expanded) = name.expand_no_side_effects() else {
    return false;
  };
  let Some(name) = expanded.get_first_word() else {
    return false;
  };
  if Path::new(&name).is_dir() {
    return true;
  }
  let cd_path = var!("CDPATH");
  let cd_path = cd_path.to_str_lossy();
  let entries = cd_path.split(':');
  for entry in entries {
    let full_path = Path::new(entry).join(&name);
    if full_path.is_dir() {
      return true;
    }
  }
  false
}

pub(crate) fn is_in_path(name: &Tk) -> bool {
  let Ok(expanded) = name.expand_no_side_effects() else {
    return false;
  };
  let Some(name) = expanded.get_first_word() else {
    return false;
  };
  if name.starts_with_str("./") || name.starts_with_str("../") || name.starts_with_str("/") {
    let path = Path::new(&name);
    if path.exists() && path.is_file() && !path.is_dir() {
      let Ok(meta) = path.metadata() else {
        return false;
      };

      if meta.permissions().mode() & 0o111 != 0 {
        return true;
      }
    }
    false
  } else {
    let Some(path) = try_var!("PATH") else {
      return false;
    };
    let path = path.to_str_lossy();
    let paths = path.split(':');
    for path in paths {
      let full_path = Path::new(path).join(&name);
      if full_path.exists() && full_path.is_file() && !full_path.is_dir() {
        let Ok(meta) = full_path.metadata() else {
          continue;
        };

        if meta.permissions().mode() & 0o111 != 0 {
          return true;
        }
      }
    }
    false
  }
}

pub(crate) fn is_func_node(cmd: NodeId, tree: &Ast) -> bool {
  tree
    .command_for(cmd)
    .is_some_and(|cmd_word| is_func(&cmd_word.to_str_lossy()))
}

pub(super) fn is_func(name: &str) -> bool {
  Shed::logic(|l| l.has_command_func(name))
}

pub(super) fn is_arith(tk: &Tk) -> bool {
  tk.flags.contains(TkFlags::IS_ARITH)
}

pub(super) fn can_autocd(cmd: &Tk) -> bool {
  shopt!(core.autocd) && in_cd_path(cmd) && !is_in_path(cmd)
}

pub(crate) fn is_builtin(cmd: NodeId, tree: &Ast) -> bool {
  let Some(cmd_word) = tree.command_for(cmd) else {
    return false;
  };

  !is_func(&cmd_word.to_str_lossy())
    && builtin::lookup_builtin(cmd_word.as_bytes()).is_some_and(|b| !b.always_forks())
    && cmd_word.flags.contains(TkFlags::BUILTIN)
}

/// Checks if a command will fork on its own or not
pub(super) fn runs_inline(cmd: &Node, tree: &Ast) -> bool {
  match &cmd.class {
    NdRule::Command { argv, .. } => {
      if argv.is_empty() {
        // assignment-only command, will never fork
        return true;
      }
      let cmd_id = cmd.get_command().unwrap();
      let cmd_word = &tree[cmd_id];
      is_func(&cmd_word.to_str_lossy()) || cmd_word.flags.contains(TkFlags::BUILTIN)
    }
    NdRule::List { .. }
    | NdRule::Conjunction { .. }
    | NdRule::IfNode { .. }
    | NdRule::LoopNode { .. }
    | NdRule::ForNode { .. }
    | NdRule::ForArith { .. }
    | NdRule::CaseNode { .. }
    | NdRule::BraceGrp { .. }
    | NdRule::TryNode { .. }
    | NdRule::DeferNode { .. }
    | NdRule::Negate { .. }
    | NdRule::Timed { .. }
    | NdRule::Arithmetic { .. }
    | NdRule::FuncDef { .. } => true,
    NdRule::Subshell { .. } | NdRule::Pipeline { .. } | NdRule::Assignment { .. } => false,
  }
}

pub(super) fn will_fork(cmd: &Node, tree: &Ast) -> bool {
  match &cmd.class {
    NdRule::Subshell { .. } => true,
    NdRule::Command { argv, .. } if !argv.is_empty() => {
      let cmd_id = cmd.get_command().unwrap();
      let cmd_word = &tree[cmd_id];
      !(is_func(&cmd_word.to_str_lossy()) || cmd_word.flags.contains(TkFlags::BUILTIN))
    }
    _ => false,
  }
}
