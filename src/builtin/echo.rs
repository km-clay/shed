use std::sync::LazyLock;

use crate::{
  builtin::setup_builtin,
  expand::expand_prompt,
  getopt::{get_opts_from_tokens, Opt, OptSpec},
  jobs::JobBldr,
  libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt},
  parse::{NdRule, Node},
  prelude::*,
  procio::{borrow_fd, IoStack},
  state,
};

pub const ECHO_OPTS: [OptSpec; 4] = [
  OptSpec {
    opt: Opt::Short('n'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('E'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('e'),
    takes_arg: false,
  },
  OptSpec {
    opt: Opt::Short('p'),
    takes_arg: false,
  },
];

bitflags! {
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub struct EchoFlags: u32 {
    const NO_NEWLINE = 0b000001;
    const USE_STDERR = 0b000010;
    const USE_ESCAPE = 0b000100;
    const USE_PROMPT = 0b001000;
  }
}

pub fn echo(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };
  assert!(!argv.is_empty());
  let (argv, opts) = get_opts_from_tokens(argv, &ECHO_OPTS)?;
  let flags = get_echo_flags(opts).blame(blame)?;
  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

  let output_channel = if flags.contains(EchoFlags::USE_STDERR) {
    borrow_fd(STDERR_FILENO)
  } else {
    borrow_fd(STDOUT_FILENO)
  };


  let mut echo_output = prepare_echo_args(
    argv
      .into_iter()
      .map(|a| a.0) // Extract the String from the tuple of (String,Span)
      .collect::<Vec<_>>(),
    flags.contains(EchoFlags::USE_ESCAPE),
    flags.contains(EchoFlags::USE_PROMPT),
  )?
  .join(" ");

  if !flags.contains(EchoFlags::NO_NEWLINE) && !echo_output.ends_with('\n') {
    echo_output.push('\n')
  }

  write(output_channel, echo_output.as_bytes())?;

  state::set_status(0);
  Ok(())
}

pub fn prepare_echo_args(
  argv: Vec<String>,
  use_escape: bool,
  use_prompt: bool,
) -> ShResult<Vec<String>> {
  if !use_escape {
    if use_prompt {
      let expanded: ShResult<Vec<String>> = argv
        .into_iter()
        .map(|s| expand_prompt(s.as_str()))
        .collect();
      return expanded;
    }
    return Ok(argv);
  }

  let mut prepared_args = Vec::with_capacity(argv.len());

  for arg in argv {
    let mut prepared_arg = String::new();
    if use_prompt {
      prepared_arg = expand_prompt(&prepared_arg)?;
    }

    let mut chars = arg.chars().peekable();

    while let Some(c) = chars.next() {
      if c == '\\' {
        if let Some(&next_char) = chars.peek() {
          match next_char {
            'n' => {
              prepared_arg.push('\n');
              chars.next();
            }
            't' => {
              prepared_arg.push('\t');
              chars.next();
            }
            'r' => {
              prepared_arg.push('\r');
              chars.next();
            }
            'a' => {
              prepared_arg.push('\x07');
              chars.next();
            }
            'b' => {
              prepared_arg.push('\x08');
              chars.next();
            }
            'e' | 'E' => {
              prepared_arg.push('\x1b');
              chars.next();
            }
            'x' => {
              chars.next(); // consume 'x'
              let mut hex_digits = String::new();
              for _ in 0..2 {
                if let Some(&hex_char) = chars.peek() {
                  if hex_char.is_ascii_hexdigit() {
                    hex_digits.push(hex_char);
                    chars.next();
                  } else {
                    break;
                  }
                } else {
                  break;
                }
              }
              if let Ok(value) = u8::from_str_radix(&hex_digits, 16) {
                prepared_arg.push(value as char);
              } else {
                prepared_arg.push('\\');
                prepared_arg.push('x');
                prepared_arg.push_str(&hex_digits);
              }
            }
            '0' => {
              chars.next(); // consume '0'
              let mut octal_digits = String::new();
              for _ in 0..3 {
                if let Some(&octal_char) = chars.peek() {
                  if ('0'..='7').contains(&octal_char) {
                    octal_digits.push(octal_char);
                    chars.next();
                  } else {
                    break;
                  }
                } else {
                  break;
                }
              }
              if let Ok(value) = u8::from_str_radix(&octal_digits, 8) {
                prepared_arg.push(value as char);
              } else {
                prepared_arg.push('\\');
                prepared_arg.push('0');
                prepared_arg.push_str(&octal_digits);
              }
            }
            '\\' => {
              prepared_arg.push('\\');
              chars.next();
            }
            _ => prepared_arg.push(c),
          }
        } else {
          prepared_arg.push(c);
        }
      } else {
        prepared_arg.push(c);
      }
    }

    prepared_args.push(prepared_arg);
  }


  Ok(prepared_args)
}

pub fn get_echo_flags(opts: Vec<Opt>) -> ShResult<EchoFlags> {
  let mut flags = EchoFlags::empty();

  for opt in opts {
    match opt {
      Opt::Short('n') => flags |= EchoFlags::NO_NEWLINE,
      Opt::Short('r') => flags |= EchoFlags::USE_STDERR,
      Opt::Short('e') => flags |= EchoFlags::USE_ESCAPE,
      Opt::Short('p') => flags |= EchoFlags::USE_PROMPT,
      _ => {
        return Err(ShErr::simple(
          ShErrKind::ExecFail,
          format!("echo: Unexpected flag '{opt}'"),
        ));
      }
    }
  }

  Ok(flags)
}
