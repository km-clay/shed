use crate::{
  jobs::JobBldr,
  libsh::error::ShResult,
  parse::{NdRule, Node},
  prelude::*,
  procio::{IoStack, borrow_fd},
  state::{self, VarFlags, write_vars},
};

use super::setup_builtin;

pub fn export(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

  if argv.is_empty() {
    // Display the environment variables
    let mut env_output = env::vars()
      .map(|var| format!("{}={}", var.0, var.1)) // Get all of them, zip them into one string
      .collect::<Vec<_>>();
    env_output.sort(); // Sort them alphabetically
    let mut env_output = env_output.join("\n"); // Join them with newlines
    env_output.push('\n'); // Push a final newline

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, env_output.as_bytes())?; // Write it
  } else {
    for (arg, _) in argv {
      if let Some((var, val)) = arg.split_once('=') {
        write_vars(|v| v.set_var(var, val, VarFlags::EXPORT)); // Export an assignment like
                                                   // 'foo=bar'
      } else {
        write_vars(|v| v.export_var(&arg)); // Export an existing variable, if
                                            // any
      }
    }
  }
  state::set_status(0);
  Ok(())
}
