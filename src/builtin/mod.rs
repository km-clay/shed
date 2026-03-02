use crate::{
  libsh::error::ShResult,
  state,
};

pub mod alias;
pub mod cd;
pub mod complete;
pub mod dirstack;
pub mod echo;
pub mod eval;
pub mod exec;
pub mod flowctl;
pub mod jobctl;
pub mod pwd;
pub mod read;
pub mod shift;
pub mod shopt;
pub mod source;
pub mod test; // [[ ]] thing
pub mod trap;
pub mod varcmds;
pub mod zoltraak;
pub mod map;
pub mod arrops;
pub mod intro;
pub mod getopts;

pub const BUILTINS: [&str; 44] = [
  "echo", "cd", "read", "export", "local", "pwd", "source", "shift", "jobs", "fg", "bg", "disown",
  "alias", "unalias", "return", "break", "continue", "exit", "zoltraak", "shopt", "builtin",
  "command", "trap", "pushd", "popd", "dirs", "exec", "eval", "true", "false", ":", "readonly",
  "unset", "complete", "compgen", "map", "pop", "fpop", "push", "fpush", "rotate", "wait", "type",
	"getopts"
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
