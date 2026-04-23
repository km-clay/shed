use std::{collections::VecDeque, time::Duration};

use bitflags::bitflags;
use nix::{
  errno::Errno,
  libc::STDIN_FILENO,
  poll::{PollFd, PollFlags, PollTimeout, poll},
  unistd::read,
};

use crate::{
  expand::expand_keymap,
  getopt::{Opt, OptSpec},
  parse::lex::Span,
  procio::borrow_fd,
  sherr, signal,
  state::{self, VarFlags, VarKind, with_term, write_vars},
  util::{
    error::{ShErrKind, ShResult, ShResultExt},
    with_status, write_out,
  },
};

bitflags! {
  pub struct ReadFlags: u32 {
    const NO_ESCAPES = 	0b000001;
    const NO_ECHO = 		0b000010; // TODO: unused
    const ARRAY = 			0b000100;
    const N_CHARS = 		0b001000;
    const TIMEOUT = 		0b010000;
  }
}
pub(super) struct Read;
impl super::Builtin for Read {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('r'),
      OptSpec::flag('s'),
      OptSpec::single_arg('a'),
      OptSpec::single_arg('n'),
      OptSpec::single_arg('t'),
      OptSpec::single_arg('p'),
      OptSpec::single_arg('d'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut flags = ReadFlags::empty();
    let mut prompt = None;
    let mut timeout = None;
    let mut max_bytes = None;
    let mut array_name = None;
    let mut delim = b'\n';

    for opt in &args.opts {
      match opt {
        Opt::Short('r') => flags |= ReadFlags::NO_ESCAPES,
        Opt::Short('s') => flags |= ReadFlags::NO_ECHO,
        Opt::ShortWithArg('a', a) => array_name = Some(a.clone()),
        Opt::ShortWithArg('p', p) => prompt = Some(p),
        Opt::ShortWithArg('d', d) => delim = d.chars().map(|c| c as u8).next().unwrap_or(b'\n'),
        Opt::ShortWithArg('n', n) => {
          let bytes = n
            .parse::<usize>()
            .map_err(|_| sherr!(ExecFail, "read: Invalid byte count '{n}'"))?;
          max_bytes = Some(bytes);
        }
        Opt::ShortWithArg('t', t) => {
          let seconds = t
            .parse::<f64>()
            .map_err(|_| sherr!(ExecFail, "read: Invalid timeout value '{t}'"))?;
          timeout = Some(Duration::from_secs_f64(seconds).as_millis() as i32);
        }
        _ => return Err(sherr!(ExecFail, "read: Unexpected flag '{opt}'")).promote_err(args.span),
      }
    }

    if let Some(p) = prompt {
      write_out(p)?;
    }

    let _guard = if flags.contains(ReadFlags::NO_ECHO) {
      with_term(|t| t.cooked_no_echo_guard())?
    } else {
      with_term(|t| t.cooked_mode_guard())?
    };
    let input = read_bytes(
      delim,
      !flags.contains(ReadFlags::NO_ESCAPES),
      timeout,
      max_bytes,
    )
    .promote_err(args.span())?;

    if let Some(arr) = array_name {
      field_split_arr(&input, &arr).promote_err(args.span())
    } else {
      field_split_vars(&input, &args.argv).promote_err(args.span())
    }
  }
}

fn read_bytes(
  delim: u8,
  escape_aware: bool,
  timeout: Option<i32>,
  max_bytes: Option<usize>,
) -> ShResult<String> {
  let mut buf = vec![];
  let mut escaped = false;
  let poll_fd = PollFd::new(borrow_fd(STDIN_FILENO), PollFlags::POLLIN);
  let timeout = timeout
    .map(PollTimeout::try_from)
    .and_then(Result::ok)
    .unwrap_or(PollTimeout::NONE);

  loop {
    if poll(&mut [poll_fd], timeout)? == 0 {
      state::set_status(1);
      return String::from_utf8(buf).map_err(|e| sherr!(ExecFail, "read: invalid UTF-8: {e}")); // timeout
    }

    let mut in_buf = [0u8; 1];
    match read(STDIN_FILENO, &mut in_buf) {
      Ok(0) => {
        state::set_status(1);
        let ret =
          String::from_utf8(buf).map_err(|e| sherr!(ExecFail, "read: invalid UTF-8: {e}"))?;
        return Ok(ret); // EOF
      }
      Ok(_) => {
        if in_buf[0] == delim && !(escape_aware && escaped) {
          break;
        } else if escape_aware && in_buf[0] == b'\\' {
          escaped = true;
        } else {
          escaped = false;
          buf.push(in_buf[0]);

          if let Some(max) = max_bytes
            && buf.len() >= max
          {
            break;
          }
        }
      }
      Err(Errno::EINTR) => {
        if signal::sigint_pending() {
          state::set_status(130);
          return Ok(String::new());
        }
        continue;
      }
      Err(e) => return Err(sherr!(ExecFail, "read: Failed to read from stdin: {e}")),
    }
  }

  state::set_status(0);
  String::from_utf8(buf).map_err(|e| sherr!(ExecFail, "read: invalid UTF-8: {e}"))
}

fn field_split_vars(input: &str, vars: &[(String, Span)]) -> ShResult<()> {
  if vars.is_empty() {
    write_vars(|v| v.set_var("REPLY", VarKind::Str(input.to_string()), VarFlags::NONE))?;
    return Ok(());
  }

  let sep = state::get_separators();

  let fields: Vec<&str> = input.splitn(vars.len(), |c| sep.contains(c)).collect();
  for (name, field) in vars.iter().zip(fields) {
    let field = field.trim_start_matches(|c: char| sep.contains(c));
    write_vars(|v| v.set_var(&name.0, VarKind::Str(field.to_string()), VarFlags::NONE))?;
  }

  Ok(())
}

fn field_split_arr(input: &str, arr_name: &str) -> ShResult<()> {
  if arr_name.is_empty() {
    return Err(sherr!(ExecFail, "read: Array name cannot be empty"));
  }

  let sep = state::get_separators();
  let fields: VecDeque<String> = input
    .split(|c| sep.contains(c))
    .map(|s| s.to_string())
    .collect();

  write_vars(|v| v.set_var(arr_name, VarKind::Arr(fields), VarFlags::NONE))
}

pub(super) struct ReadKey;
impl super::Builtin for ReadKey {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::single_arg('v'), // var name
      OptSpec::single_arg('w'), // char whitelist
      OptSpec::single_arg('b'), // char blacklist
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if !with_term(|t| t.isatty()) {
      return with_status(1);
    }
    let mut whitelist = None;
    let mut blacklist = None;
    let mut var_name = None;

    for opt in &args.opts {
      match opt {
        Opt::ShortWithArg('v', name) => var_name = Some(name),
        Opt::ShortWithArg('w', wl) => whitelist = Some(wl),
        Opt::ShortWithArg('b', bl) => blacklist = Some(bl),
        _ => {
          return Err(sherr!(ExecFail, "readkey: Unexpected flag '{opt}'"))
            .promote_err(args.span());
        }
      }
    }

    let key = {
      let _raw = with_term(|t| t.raw_mode_guard());
      if let Err(e) = with_term(|t| t.read()) {
        match e.kind() {
          ShErrKind::LoopBreak(_) => return with_status(1),
          ShErrKind::LoopContinue(_) => return with_status(0),
          _ => return Err(e).promote_err(args.span()),
        }
      }

      let mut keys = with_term(|t| t.drain_keys())?;
      if keys.is_empty() {
        return with_status(1);
      }

      keys.remove(0)
    };

    let vim_seq = key.as_vim_seq()?;

    if let Some(wl) = whitelist {
      let allowed = expand_keymap(wl);
      if !allowed.contains(&key) {
        return with_status(1);
      }
    }
    if let Some(bl) = blacklist {
      let disallowed = expand_keymap(bl);
      if disallowed.contains(&key) {
        return with_status(1);
      }
    }

    if let Some(var) = var_name {
      write_vars(|v| v.set_var(var, VarKind::Str(vim_seq), VarFlags::NONE))?;
    } else {
      write_out(vim_seq)?;
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{self, VarFlags, VarKind, read_vars, write_vars};
  use crate::testutil::{TestGuard, test_input};

  // ===================== Basic read into REPLY =====================

  #[test]
  fn read_pipe_into_reply() {
    let _g = TestGuard::new();
    test_input("read < <(echo hello)").unwrap();
    let val = read_vars(|v| v.get_var("REPLY"));
    assert_eq!(val, "hello");
  }

  #[test]
  fn read_pipe_into_named_var() {
    let _g = TestGuard::new();
    test_input("read myvar < <(echo world)").unwrap();
    let val = read_vars(|v| v.get_var("myvar"));
    assert_eq!(val, "world");
  }

  // ===================== Field splitting =====================

  #[test]
  fn read_two_vars() {
    let _g = TestGuard::new();
    test_input("read a b < <(echo 'hello world')").unwrap();
    assert_eq!(read_vars(|v| v.get_var("a")), "hello");
    assert_eq!(read_vars(|v| v.get_var("b")), "world");
  }

  #[test]
  fn read_last_var_gets_remainder() {
    let _g = TestGuard::new();
    test_input("read a b < <(echo 'one two three four')").unwrap();
    assert_eq!(read_vars(|v| v.get_var("a")), "one");
    assert_eq!(read_vars(|v| v.get_var("b")), "two three four");
  }

  #[test]
  fn read_more_vars_than_fields() {
    let _g = TestGuard::new();
    test_input("read a b c < <(echo 'only')").unwrap();
    assert_eq!(read_vars(|v| v.get_var("a")), "only");
    // b and c get empty strings since there are no more fields
    assert_eq!(read_vars(|v| v.get_var("b")), "");
    assert_eq!(read_vars(|v| v.get_var("c")), "");
  }

  // ===================== Custom IFS =====================

  #[test]
  fn read_custom_ifs() {
    let _g = TestGuard::new();
    write_vars(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::NONE)).unwrap();

    test_input("read x y z < <(echo 'a:b:c')").unwrap();
    assert_eq!(read_vars(|v| v.get_var("x")), "a");
    assert_eq!(read_vars(|v| v.get_var("y")), "b");
    assert_eq!(read_vars(|v| v.get_var("z")), "c");
  }

  #[test]
  fn read_custom_ifs_remainder() {
    let _g = TestGuard::new();
    write_vars(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::NONE)).unwrap();

    test_input("read x y < <(echo 'a:b:c:d')").unwrap();
    assert_eq!(read_vars(|v| v.get_var("x")), "a");
    assert_eq!(read_vars(|v| v.get_var("y")), "b:c:d");
  }

  // ===================== Custom delimiter =====================

  #[test]
  fn read_custom_delim() {
    let _g = TestGuard::new();
    // -d sets the delimiter; printf sends "hello,world" - read stops at ','
    test_input("read -d , myvar < <(echo -n 'hello,world')").unwrap();
    assert_eq!(read_vars(|v| v.get_var("myvar")), "hello");
  }

  // ===================== Status =====================

  #[test]
  fn read_status_zero() {
    let _g = TestGuard::new();
    test_input("read < <(echo hello)").unwrap();
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn read_eof_status_one() {
    let _g = TestGuard::new();
    // Empty input / EOF should set status 1
    test_input("read < <(echo -n '')").unwrap();
    assert_eq!(state::get_status(), 1);
  }
}
