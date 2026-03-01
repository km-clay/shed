use ariadne::Fmt;

use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult, next_color},
  parse::{NdRule, Node, execute::prepare_argv},
  prelude::*,
  procio::borrow_fd,
  state::{self, read_logic, write_logic},
};

pub fn alias(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }

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
      write_logic(|l| l.insert_alias(name, body, span.clone()));
    }
  }
  state::set_status(0);
  Ok(())
}

pub fn unalias(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }

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
        return Err(ShErr::at(ShErrKind::SyntaxErr, span, format!("unalias: alias '{}' not found",arg.fg(next_color()))));
      };
      write_logic(|l| l.remove_alias(&arg))
    }
  }
  state::set_status(0);
  Ok(())
}
