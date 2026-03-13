use crate::{libsh::error::ShResult, state};

pub mod alias;
pub mod arrops;
pub mod autocmd;
pub mod cd;
pub mod complete;
pub mod dirstack;
pub mod echo;
pub mod eval;
pub mod exec;
pub mod flowctl;
pub mod getopts;
pub mod intro;
pub mod jobctl;
pub mod keymap;
pub mod map;
pub mod pwd;
pub mod read;
pub mod resource;
pub mod shift;
pub mod shopt;
pub mod source;
pub mod test; // [[ ]] thing
pub mod trap;
pub mod varcmds;

pub const BUILTINS: [&str; 49] = [
  "echo", "cd", "read", "export", "local", "pwd", "source", ".", "shift", "jobs", "fg", "bg",
  "disown", "alias", "unalias", "return", "break", "continue", "exit", "shopt", "builtin",
  "command", "trap", "pushd", "popd", "dirs", "exec", "eval", "true", "false", ":", "readonly",
  "unset", "complete", "compgen", "map", "pop", "fpop", "push", "fpush", "rotate", "wait", "type",
  "getopts", "keymap", "read_key", "autocmd", "ulimit", "umask",
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
