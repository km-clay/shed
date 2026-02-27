use crate::{
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node},
  prelude::*,
  procio::{IoStack, borrow_fd},
  state::{self, VarFlags, VarKind, read_vars, write_vars},
};

use super::setup_builtin;

pub fn readonly(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

  if argv.is_empty() {
    // Display the local variables
    let vars_output = read_vars(|v| {
      let mut vars = v
        .flatten_vars()
        .into_iter()
        .filter(|(_, v)| v.flags().contains(VarFlags::READONLY))
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<String>>();
      vars.sort();
      let mut vars_joined = vars.join("\n");
      vars_joined.push('\n');
      vars_joined
    });

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, vars_output.as_bytes())?; // Write it
  } else {
    for (arg, _) in argv {
      if let Some((var, val)) = arg.split_once('=') {
        write_vars(|v| v.set_var(var, VarKind::Str(val.to_string()), VarFlags::READONLY))?;
      } else {
        write_vars(|v| v.set_var(&arg, VarKind::Str(String::new()), VarFlags::READONLY))?;
      }
    }
  }

  state::set_status(0);
  Ok(())
}

pub fn unset(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

  if argv.is_empty() {
    return Err(ShErr::full(
      ShErrKind::SyntaxErr,
      "unset: Expected at least one argument",
      blame,
    ));
  }

  for (arg, span) in argv {
    if !read_vars(|v| v.var_exists(&arg)) {
      return Err(ShErr::full(
        ShErrKind::ExecFail,
        format!("unset: No such variable '{arg}'"),
        span,
      ));
    }
    write_vars(|v| v.unset_var(&arg))?;
  }

  state::set_status(0);
  Ok(())
}

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
        write_vars(|v| v.set_var(var, VarKind::Str(val.to_string()), VarFlags::EXPORT))?;
      } else {
        write_vars(|v| v.export_var(&arg)); // Export an existing variable, if
        // any
      }
    }
  }
  state::set_status(0);
  Ok(())
}

pub fn local(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

  if argv.is_empty() {
    // Display the local variables
    let vars_output = read_vars(|v| {
      let mut vars = v
        .flatten_vars()
        .into_iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<String>>();
      vars.sort();
      let mut vars_joined = vars.join("\n");
      vars_joined.push('\n');
      vars_joined
    });

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, vars_output.as_bytes())?; // Write it
  } else {
    for (arg, _) in argv {
      if let Some((var, val)) = arg.split_once('=') {
        write_vars(|v| v.set_var(var, VarKind::Str(val.to_string()), VarFlags::LOCAL))?;
      } else {
        write_vars(|v| v.set_var(&arg, VarKind::Str(String::new()), VarFlags::LOCAL))?;
      }
    }
  }
  state::set_status(0);
  Ok(())
}
