use nix::unistd::Pid;

use crate::{
  jobs::{ChildProc, JobBldr},
  libsh::error::ShResult,
  parse::{
    Redir,
    execute::prepare_argv,
    lex::{Span, Tk},
  },
  procio::{IoStack, RedirGuard},
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

pub const BUILTINS: [&str; 35] = [
  "echo", "cd", "read", "export", "local", "pwd", "source", "shift", "jobs", "fg", "bg", "disown",
  "alias", "unalias", "return", "break", "continue", "exit", "zoltraak", "shopt", "builtin",
  "command", "trap", "pushd", "popd", "dirs", "exec", "eval", "true", "false", ":", "readonly",
  "unset", "complete", "compgen",
];

/// Sets up a builtin command
///
/// Prepares a builtin for execution by processing arguments, setting up
/// redirections, and registering the command as a child process in the given
/// `JobBldr`
///
/// # Parameters
/// * argv - The vector of raw argument tokens
/// * job - A mutable reference to a `JobBldr`
/// * io_mode - An optional 2-tuple consisting of a mutable reference to an
///   `IoStack` and a vector of `Redirs`
///
/// # Behavior
/// * Cleans, expands, and word splits the arg vector
/// * Adds a new `ChildProc` to the job builder
/// * Performs redirections, if any.
///
/// # Returns
/// * The processed arg vector
/// * The popped `IoFrame`, if any
///
/// # Notes
/// * If redirections are given to this function, the caller must call
///   `IoFrame.restore()` on the returned `IoFrame`
/// * If redirections are given, the second field of the resulting tuple will
///   *always* be `Some()`
/// * If no redirections are given, the second field will *always* be `None`
type SetupReturns = ShResult<(Vec<(String, Span)>, Option<RedirGuard>)>;
pub fn setup_builtin(
  argv: Vec<Tk>,
  job: &mut JobBldr,
  io_mode: Option<(&mut IoStack, Vec<Redir>)>,
) -> SetupReturns {
  let mut argv: Vec<(String, Span)> = prepare_argv(argv)?;

  let child_pgid = if let Some(pgid) = job.pgid() {
    pgid
  } else {
    job.set_pgid(Pid::this());
    Pid::this()
  };
  let cmd_name = argv.remove(0).0;
  let child = ChildProc::new(Pid::this(), Some(&cmd_name), Some(child_pgid))?;
  job.push_child(child);

  let guard = if let Some((io_stack, redirs)) = io_mode {
    io_stack.append_to_frame(redirs);
    let io_frame = io_stack.pop_frame();
    let guard = io_frame.redirect()?;
    Some(guard)
  } else {
    None
  };

  // We return the io_frame because the caller needs to also call
  // io_frame.restore()
  Ok((argv, guard))
}

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
