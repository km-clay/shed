use crate::{
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node, execute::prepare_argv, lex::split_tk_at},
  prelude::*,
  procio::borrow_fd,
  state::{self, VarFlags, VarKind, read_vars, write_vars},
};

pub fn readonly(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  // Remove "readonly" from argv
  let argv = if !argv.is_empty() { &argv[1..] } else { &argv[..] };

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
    for tk in argv {
      if let Some((var_tk, val_tk)) = split_tk_at(tk, "=") {
        let var = var_tk.expand()?.get_words().join(" ");
        let val = if val_tk.as_str().starts_with('(') && val_tk.as_str().ends_with(')') {
          VarKind::arr_from_tk(val_tk.clone())?
        } else {
          VarKind::Str(val_tk.expand()?.get_words().join(" "))
        };
        write_vars(|v| v.set_var(&var, val, VarFlags::READONLY))?;
      } else {
        let arg = tk.clone().expand()?.get_words().join(" ");
        write_vars(|v| v.set_var(&arg, VarKind::Str(String::new()), VarFlags::READONLY))?;
      }
    }
  }

  state::set_status(0);
  Ok(())
}

pub fn unset(node: Node) -> ShResult<()> {
  let blame = node.get_span().clone();
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
    return Err(ShErr::at(ShErrKind::SyntaxErr, blame, "unset: Expected at least one argument"));
  }

  for (arg, span) in argv {
    if !read_vars(|v| v.var_exists(&arg)) {
      return Err(ShErr::at(ShErrKind::ExecFail, span, format!("unset: No such variable '{arg}'")));
    }
    write_vars(|v| v.unset_var(&arg))?;
  }

  state::set_status(0);
  Ok(())
}

pub fn export(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  // Remove "export" from argv
  let argv = if !argv.is_empty() { &argv[1..] } else { &argv[..] };

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
    for tk in argv {
      if let Some((var_tk, val_tk)) = split_tk_at(tk, "=") {
        let var = var_tk.expand()?.get_words().join(" ");
        let val = if val_tk.as_str().starts_with('(') && val_tk.as_str().ends_with(')') {
          VarKind::arr_from_tk(val_tk.clone())?
        } else {
          VarKind::Str(val_tk.expand()?.get_words().join(" "))
        };
        write_vars(|v| v.set_var(&var, val, VarFlags::EXPORT))?;
      } else {
        let arg = tk.clone().expand()?.get_words().join(" ");
        write_vars(|v| v.export_var(&arg)); // Export an existing variable, if any
      }
    }
  }
  state::set_status(0);
  Ok(())
}

pub fn local(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  // Remove "local" from argv
  let argv = if !argv.is_empty() { &argv[1..] } else { &argv[..] };

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
    for tk in argv {
      if let Some((var_tk, val_tk)) = split_tk_at(tk, "=") {
        let var = var_tk.expand()?.get_words().join(" ");
        let val = if val_tk.as_str().starts_with('(') && val_tk.as_str().ends_with(')') {
          VarKind::arr_from_tk(val_tk.clone())?
        } else {
          VarKind::Str(val_tk.expand()?.get_words().join(" "))
        };
        write_vars(|v| v.set_var(&var, val, VarFlags::LOCAL))?;
      } else {
        let arg = tk.clone().expand()?.get_words().join(" ");
        write_vars(|v| v.set_var(&arg, VarKind::Str(String::new()), VarFlags::LOCAL))?;
      }
    }
  }
  state::set_status(0);
  Ok(())
}
