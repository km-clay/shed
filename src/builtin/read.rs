use bitflags::bitflags;
use nix::{
  errno::Errno,
  libc::{STDIN_FILENO, STDOUT_FILENO},
  unistd::{isatty, read, write},
};

use crate::{
  builtin::setup_builtin,
  getopt::{Opt, OptSpec, get_opts_from_tokens},
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt},
  parse::{NdRule, Node},
  procio::{IoStack, borrow_fd},
  readline::term::RawModeGuard,
  state::{self, VarFlags, VarKind, read_vars, write_vars},
};

pub const READ_OPTS: [OptSpec; 7] = [
  OptSpec {
    opt: Opt::Short('r'),
    takes_arg: false,
  }, // don't allow backslash escapes
  OptSpec {
    opt: Opt::Short('s'),
    takes_arg: false,
  }, // don't echo input
  OptSpec {
    opt: Opt::Short('a'),
    takes_arg: false,
  }, // read into array
  OptSpec {
    opt: Opt::Short('n'),
    takes_arg: false,
  }, // read only N characters
  OptSpec {
    opt: Opt::Short('t'),
    takes_arg: false,
  }, // timeout
  OptSpec {
    opt: Opt::Short('p'),
    takes_arg: true,
  }, // prompt
  OptSpec {
    opt: Opt::Short('d'),
    takes_arg: true,
  }, // read until delimiter
];

bitflags! {
  pub struct ReadFlags: u32 {
    const NO_ESCAPES = 	0b000001;
    const NO_ECHO = 		0b000010; // TODO: unused
    const ARRAY = 			0b000100; // TODO: unused
    const N_CHARS = 		0b001000; // TODO: unused
    const TIMEOUT = 		0b010000; // TODO: unused
  }
}

pub struct ReadOpts {
  prompt: Option<String>,
  delim: u8, // byte representation of the delimiter character
  flags: ReadFlags,
}

pub fn read_builtin(node: Node, _io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, opts) = get_opts_from_tokens(argv, &READ_OPTS)?;
  let read_opts = get_read_flags(opts).blame(blame.clone())?;
  let (argv, _) = setup_builtin(argv, job, None).blame(blame.clone())?;

  if let Some(prompt) = read_opts.prompt {
    write(borrow_fd(STDOUT_FILENO), prompt.as_bytes())?;
  }

  log::info!(
    "read_builtin: starting read with delim={}",
    read_opts.delim as char
  );

  let input = if isatty(STDIN_FILENO)? {
    // Restore default terminal settings
    RawModeGuard::with_cooked_mode(|| {
      let mut input: Vec<u8> = vec![];
      let mut escaped = false;
      loop {
        let mut buf = [0u8; 1];
        match read(STDIN_FILENO, &mut buf) {
          Ok(0) => {
            state::set_status(1);
            let str_result = String::from_utf8(input.clone()).map_err(|e| {
              ShErr::simple(
                ShErrKind::ExecFail,
                format!("read: Input was not valid UTF-8: {e}"),
              )
            })?;
            return Ok(str_result); // EOF
          }
          Ok(_) => {
            if buf[0] == read_opts.delim {
              if read_opts.flags.contains(ReadFlags::NO_ESCAPES) && escaped {
                input.push(buf[0]);
              } else {
                // Delimiter reached, stop reading
                break;
              }
            } else if read_opts.flags.contains(ReadFlags::NO_ESCAPES) && buf[0] == b'\\' {
              escaped = true;
            } else {
              input.push(buf[0]);
            }
          }
          Err(Errno::EINTR) => {
            if crate::signal::sigint_pending() {
              state::set_status(130);
              return Ok(String::new());
            }
            continue;
          }
          Err(e) => {
            return Err(ShErr::simple(
              ShErrKind::ExecFail,
              format!("read: Failed to read from stdin: {e}"),
            ));
          }
        }
      }

      state::set_status(0);
      let str_result = String::from_utf8(input.clone()).map_err(|e| {
        ShErr::simple(
          ShErrKind::ExecFail,
          format!("read: Input was not valid UTF-8: {e}"),
        )
      })?;
      Ok(str_result)
    })
    .blame(blame)?
  } else {
    let mut input: Vec<u8> = vec![];
    loop {
      let mut buf = [0u8; 1];
      match read(STDIN_FILENO, &mut buf) {
        Ok(0) => {
          state::set_status(1);
          break; // EOF
        }
        Ok(_) => {
          if buf[0] == read_opts.delim {
            state::set_status(0);
            break; // Delimiter reached, stop reading
          }
          input.push(buf[0]);
        }
        Err(Errno::EINTR) => {
          let pending = crate::signal::sigint_pending();
          if pending {
            state::set_status(130);
            break;
          }
          continue;
        }
        Err(e) => {
          return Err(ShErr::simple(
            ShErrKind::ExecFail,
            format!("read: Failed to read from stdin: {e}"),
          ));
        }
      }
    }
    String::from_utf8(input).map_err(|e| {
      ShErr::simple(
        ShErrKind::ExecFail,
        format!("read: Input was not valid UTF-8: {e}"),
      )
    })?
  };

  if argv.is_empty() {
    write_vars(|v| v.set_var("REPLY", VarKind::Str(input.clone()), VarFlags::NONE))?;
  } else {
    // get our field separator
    let mut field_sep = read_vars(|v| v.get_var("IFS"));
    if field_sep.is_empty() {
      field_sep = " ".to_string()
    }
    let mut remaining = input;

    for (i, arg) in argv.iter().enumerate() {
      if i == argv.len() - 1 {
        // Last arg, stuff the rest of the input into it
        write_vars(|v| v.set_var(&arg.0, VarKind::Str(remaining.clone()), VarFlags::NONE))?;
        break;
      }

      // trim leading IFS characters
      let trimmed = remaining.trim_start_matches(|c: char| field_sep.contains(c));

      if let Some(idx) = trimmed.find(|c: char| field_sep.contains(c)) {
        // We found a field separator, split at the char index
        let (field, rest) = trimmed.split_at(idx);
        write_vars(|v| v.set_var(&arg.0, VarKind::Str(field.to_string()), VarFlags::NONE))?;

        // note that this doesn't account for consecutive IFS characters, which is what
        // that trim above is for
        remaining = rest.to_string();
      } else {
        write_vars(|v| v.set_var(&arg.0, VarKind::Str(trimmed.to_string()), VarFlags::NONE))?;
        remaining.clear();
      }
    }
  }

  Ok(())
}

pub fn get_read_flags(opts: Vec<Opt>) -> ShResult<ReadOpts> {
  let mut read_opts = ReadOpts {
    prompt: None,
    delim: b'\n',
    flags: ReadFlags::empty(),
  };

  for opt in opts {
    match opt {
      Opt::Short('r') => read_opts.flags |= ReadFlags::NO_ESCAPES,
      Opt::Short('s') => read_opts.flags |= ReadFlags::NO_ECHO,
      Opt::Short('a') => read_opts.flags |= ReadFlags::ARRAY,
      Opt::Short('n') => read_opts.flags |= ReadFlags::N_CHARS,
      Opt::Short('t') => read_opts.flags |= ReadFlags::TIMEOUT,
      Opt::ShortWithArg('p', prompt) => read_opts.prompt = Some(prompt),
      Opt::ShortWithArg('d', delim) => {
        read_opts.delim = delim.chars().map(|c| c as u8).next().unwrap_or(b'\n')
      }
      _ => {
        return Err(ShErr::simple(
          ShErrKind::ExecFail,
          format!("read: Unexpected flag '{opt}'"),
        ));
      }
    }
  }

  Ok(read_opts)
}
