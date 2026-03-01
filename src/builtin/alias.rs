use crate::{
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node},
  prelude::*,
  procio::{IoStack, borrow_fd},
  state::{self, read_logic, write_logic},
};

use super::setup_builtin;

pub fn alias(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(Some(argv), job, Some((io_stack, node.redirs)))?;
	let argv = argv.unwrap();

  if argv.is_empty() {
    // Display the environment variables
    let mut alias_output = read_logic(|l| {
      l.aliases()
        .iter()
        .map(|ent| format!("{} = \"{}\"", ent.0, ent.1))
        .collect::<Vec<_>>()
    });
    alias_output.sort(); // Sort them alphabetically
    let mut alias_output = alias_output.join("\n"); // Join them with newlines
    alias_output.push('\n'); // Push a final newline

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, alias_output.as_bytes())?; // Write it
  } else {
    for (arg, span) in argv {
      if arg == "command" || arg == "builtin" {
        return Err(ShErr::at(ShErrKind::ExecFail, span, format!("alias: Cannot assign alias to reserved name '{arg}'")));
      }

      let Some((name, body)) = arg.split_once('=') else {
        return Err(ShErr::at(ShErrKind::SyntaxErr, span, "alias: Expected an assignment in alias args"));
      };
      write_logic(|l| l.insert_alias(name, body));
    }
  }
  state::set_status(0);
  Ok(())
}

pub fn unalias(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(Some(argv), job, Some((io_stack, node.redirs)))?;
  let argv = argv.unwrap();

  if argv.is_empty() {
    // Display the environment variables
    let mut alias_output = read_logic(|l| {
      l.aliases()
        .iter()
        .map(|ent| format!("{} = \"{}\"", ent.0, ent.1))
        .collect::<Vec<_>>()
    });
    alias_output.sort(); // Sort them alphabetically
    let mut alias_output = alias_output.join("\n"); // Join them with newlines
    alias_output.push('\n'); // Push a final newline

    let stdout = borrow_fd(STDOUT_FILENO);
    write(stdout, alias_output.as_bytes())?; // Write it
  } else {
    for (arg, span) in argv {
      if read_logic(|l| l.get_alias(&arg)).is_none() {
        return Err(ShErr::at(ShErrKind::SyntaxErr, span, format!("unalias: alias '{arg}' not found")));
      };
      write_logic(|l| l.remove_alias(&arg))
    }
  }
  state::set_status(0);
  Ok(())
}
