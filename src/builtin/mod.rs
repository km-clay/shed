use ariadne::Span as ASpan;

use crate::{libsh::error::ShResult, parse::lex::Span, state};

pub mod alias;
pub mod arrops;
pub mod autocmd;
pub mod cd;
pub mod complete;
pub mod dirstack;
pub mod echo;
pub mod eval;
pub mod exec;
pub mod fixcmd;
pub mod flowctl;
pub mod getopts;
pub mod help;
pub mod hist;
pub mod intro;
pub mod jobctl;
pub mod keymap;
pub mod map;
pub mod msg;
pub mod pwd;
pub mod read;
pub mod resource;
pub mod seek;
pub mod set;
pub mod shift;
pub mod shopt;
pub mod source;
pub mod test; // [[ ]] thing
pub mod trap;
pub mod varcmds;
pub mod hash;

pub const BUILTINS: [&str; 56] = [
  "echo", "cd", "read", "export", "local", "pwd", "source", ".", "shift", "jobs", "fg", "bg",
  "disown", "alias", "unalias", "return", "break", "continue", "exit", "shopt", "builtin",
  "command", "trap", "pushd", "popd", "dirs", "exec", "eval", "true", "false", ":", "readonly",
  "unset", "complete", "compgen", "map", "pop", "fpop", "push", "fpush", "rotate", "wait", "type",
  "getopts", "keymap", "read_key", "autocmd", "ulimit", "umask", "seek", "help", "set", "msg",
  "fc", "hist", "hash"
];

pub fn true_builtin() -> ShResult<()> {
  state::set_status(0);
  Ok(())
}

pub fn false_builtin() -> ShResult<()> {
  state::set_status(1);
  Ok(())
}

pub fn noop_builtin() -> ShResult<()> {
  state::set_status(0);
  Ok(())
}

// Join all of the word-split arguments into a single string
// Preserve the span too
pub fn join_raw_args(args: Vec<(String, Span)>) -> (String, Span) {
  join_raw_arg_iter(args.into_iter())
}

pub fn join_raw_arg_iter(args: impl Iterator<Item = (String, Span)>) -> (String, Span) {
  args.fold((String::new(), Span::default()), |mut acc, arg| {
    if acc.1 == Span::default() {
      acc.1 = arg.1.clone();
    } else {
      let new_end = arg.1.end();
      let start = acc.1.start();
      acc.1.set_range(start..new_end);
    }

    if acc.0.is_empty() {
      acc.0 = arg.0;
    } else {
      acc.0 = acc.0 + &format!(" {}", arg.0);
    }
    acc
  })
}

#[cfg(test)]
pub mod tests {
  use crate::{
    state,
    testutil::{TestGuard, test_input},
  };

  // You can never be too sure!!!!!!

  #[test]
  fn test_true() {
    let _g = TestGuard::new();
    test_input("true").unwrap();

    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_false() {
    let _g = TestGuard::new();
    test_input("false").unwrap();

    assert_eq!(state::get_status(), 1);
  }

  #[test]
  fn test_noop() {
    let _g = TestGuard::new();
    test_input(":").unwrap();

    assert_eq!(state::get_status(), 0);
  }
}
