use std::{
  io::Read as _,
  os::fd::{AsRawFd, BorrowedFd},
  time::Duration,
};

use bitflags::bitflags;
use nix::{
  errno::Errno,
  poll::{PollFd, PollFlags, PollTimeout, poll},
  unistd::{self, read},
};

use crate::{builtin::quote, match_loop, state::vars::VarStr, varstr};

use super::{
  super::state::terminal::Terminal,
  Shed,
  eval::lex::Span,
  expand::expand_keymap,
  opt::OptSpec,
  out, procio,
  procio::stdin_fileno,
  sherr, signal,
  state::{
    self,
    vars::{VarFlags, VarKind},
  },
  util::{ShErrKind, ShResult, ShResultExt, with_status},
};

const CHUNK_SIZE: usize = 4096; // 4kb

// FIONREAD reports how many bytes are available to read on an fd. Unlike
// `poll`, it distinguishes "data present" (n > 0) from "empty or EOF" (n == 0).
nix::ioctl_read_bad!(fionread, nix::libc::FIONREAD, nix::libc::c_int);

/// Whether stdin currently has data available, without consuming any. Used by
/// `read -t 0` to poll non-destructively.
fn stdin_has_data() -> bool {
  if let Some(has) = Shed::sinks(|s| s.input_available()) {
    return has;
  }
  let mut nbytes: nix::libc::c_int = 0;
  unsafe { fionread(stdin_fileno().as_raw_fd(), &raw mut nbytes) }.is_ok() && nbytes > 0
}

bitflags! {
  pub struct ReadFlags: u32 {
    const NO_ESCAPE = 	0b0000_0001;
    const NO_ECHO = 		0b0000_0010;
    const ARRAY = 			0b0000_0100;
    const N_CHARS = 		0b0000_1000;
    const TIMEOUT = 		0b0001_0000;
    const QUOTED  = 		0b0010_0000;
  }
}
pub(super) struct Read;
impl super::Builtin for Read {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("no-escape", 'r'),
      OptSpec::new_short("no-echo", 's'),
      OptSpec::new_long("quoted").short('q'),
      OptSpec::new_short("array", 'a').argc(1),
      OptSpec::new_short("n-chars", 'n').argc(1),
      OptSpec::new_short("timeout", 't').argc(1),
      OptSpec::new_short("prompt", 'p').argc(1),
      OptSpec::new_short("delim", 'd').argc(1),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let mut flags = ReadFlags::empty();
    let mut prompt = None;
    let mut timeout = None;
    let mut max_bytes = None;
    let mut array_name = None;
    let mut delim = b'\n';

    let (arg_vec, opts) = args.take_argv();

    for opt in opts {
      match opt.key() {
        "array" => {
          let arr = opt.value()?;
          array_name = Some(varstr!("{arr}"));
        }
        "n-chars" => {
          let n = opt.value()?;
          let bytes = n
            .parse::<usize>()
            .map_err(|_| sherr!(ExecFail @ opt.span(), "invalid byte count '{n}'"))?;
          max_bytes = Some(bytes);
        }
        "prompt" => {
          let p = opt.value()?;
          prompt = Some(varstr!("{p}"));
        }
        "delim" => {
          let d = opt.value()?;

          // empty argument means NUL byte
          delim = d.chars().map(|c| c as u8).next().unwrap_or(b'\0');
        }
        "timeout" => {
          let t = opt.value()?;
          let seconds = t
            .parse::<f64>()
            .map_err(|_| sherr!(ExecFail @ opt.span(), "invalid timeout value '{t}'"))?;
          let dur = Duration::try_from_secs_f64(seconds)
            .map_err(|_| sherr!(ExecFail @ opt.span(), "invalid timeout value '{t}'"))?;
          timeout = Some(dur.as_millis().min(i32::MAX as u128) as i32);
        }
        "quoted" => flags |= ReadFlags::QUOTED | ReadFlags::NO_ESCAPE,
        "no-escape" => flags |= ReadFlags::NO_ESCAPE,
        "no-echo" => flags |= ReadFlags::NO_ECHO,
        _ => return Err(sherr!(ExecFail @ opt.span(), "unexpected flag '{opt}'")),
      }
    }

    // `read -t 0` polls without consuming or assigning: status 0 if input is
    // available on stdin right now, non-zero otherwise (matches bash).
    if timeout == Some(0) {
      return with_status(i32::from(!stdin_has_data()));
    }

    if let Some(p) = prompt {
      out!("{p}");
    }

    let _guard = unistd::isatty(stdin_fileno()).unwrap_or(false).then(|| {
      if flags.contains(ReadFlags::NO_ECHO) {
        Shed::term_mut(Terminal::cooked_no_echo_guard)
      } else {
        Shed::term_mut(Terminal::cooked_mode_guard)
      }
    });

    let input = do_read(
      delim,
      !flags.contains(ReadFlags::NO_ESCAPE),
      timeout,
      max_bytes,
    )?;

    if let Some(arr) = array_name {
      if flags.contains(ReadFlags::QUOTED) {
        field_split_arr_quoted(&input, &arr.to_str_lossy()).promote_err(args.span())
      } else {
        field_split_arr(&input, &arr.to_str_lossy()).promote_err(args.span())
      }
    } else {
      if flags.contains(ReadFlags::QUOTED) {
        field_split_vars_quoted(&input, &arg_vec).promote_err(args.span())
      } else {
        field_split_vars(&input, &arg_vec).promote_err(args.span())
      }
    }
  }
}

fn do_read(
  delim: u8,
  escape_aware: bool,
  timeout: Option<i32>,
  max_bytes: Option<usize>,
) -> ShResult<Vec<u8>> {
  let fd = stdin_fileno();

  if !procio::has_in_sink()
    && timeout.is_none()
    && unistd::lseek(fd, 0, unistd::Whence::SeekCur).is_ok()
  {
    seeking_read(fd, delim, escape_aware, max_bytes)
  } else {
    walking_read(fd, delim, escape_aware, timeout, max_bytes)
  }
}

fn walking_read(
  fd: BorrowedFd,
  delim: u8,
  escape_aware: bool,
  timeout: Option<i32>,
  max_bytes: Option<usize>,
) -> ShResult<Vec<u8>> {
  let use_sink = procio::has_in_sink();
  let mut buf = vec![];
  let mut escaped = false;
  let poll_fd = PollFd::new(fd, PollFlags::POLLIN);
  let timeout = timeout
    .map(PollTimeout::try_from)
    .and_then(Result::ok)
    .unwrap_or(PollTimeout::NONE);

  loop {
    if !use_sink {
      let ready = match poll(&mut [poll_fd.clone()], timeout) {
        Ok(n) => n,
        Err(Errno::EINTR) => {
          if signal::sigint_pending() {
            state::Shed::set_status(130);
            return Ok(Vec::new());
          }
          if signal::has_actionable_pending() {
            state::Shed::set_status(1);
            return Ok(buf);
          }
          continue; // untrapped SIGCHLD/SIGWINCH etc., retry the poll
        }
        Err(e) => return Err(e.into()),
      };
      if ready == 0 {
        state::Shed::set_status(1);
        return Ok(buf); // timeout
      }
    }

    let mut in_buf = [0u8; 1];
    let n = if use_sink {
      match Shed::sinks(|s| s.read(&mut in_buf)) {
        Ok(n) => n,
        Err(e) => return Err(sherr!(ExecFail, "read: Failed to read from stdin: {e}")),
      }
    } else {
      match read(fd, &mut in_buf) {
        Ok(n) => n,
        Err(Errno::EINTR) => {
          if signal::sigint_pending() {
            state::Shed::set_status(130);
            return Ok(Vec::new());
          }
          if signal::has_actionable_pending() {
            state::Shed::set_status(1);
            return Ok(buf);
          }
          continue;
        }
        Err(e) => return Err(sherr!(ExecFail, "read: Failed to read from stdin: {e}")),
      }
    };

    if n == 0 {
      state::Shed::set_status(1);
      return Ok(buf); // EOF
    }

    if escape_aware && escaped {
      escaped = false;
      if in_buf[0] != b'\n' {
        buf.push(in_buf[0]);
        if let Some(max) = max_bytes
          && buf.len() >= max
        {
          break;
        }
      }
    } else if in_buf[0] == delim {
      break;
    } else if escape_aware && in_buf[0] == b'\\' {
      escaped = true;
    } else {
      buf.push(in_buf[0]);
      if let Some(max) = max_bytes
        && buf.len() >= max
      {
        break;
      }
    }
  }

  state::Shed::set_status(0);
  Ok(buf)
}

fn delim_scan(delim: u8, slice: &[u8], escape_aware: bool) -> Option<usize> {
  if !escape_aware {
    return slice.iter().position(|&b| b == delim);
  }

  let mut byte_enum = slice.iter().enumerate();
  match_loop!(byte_enum.next() => (i,&byte) => byte, {
    b'\\' => {
      byte_enum.next();
    },
    _ if byte == delim => return Some(i),
    _ => {}
  });
  None
}

fn seeking_read(
  fd: BorrowedFd,
  delim: u8,
  escape_aware: bool,
  max_bytes: Option<usize>,
) -> ShResult<Vec<u8>> {
  let mut buf = [0u8; CHUNK_SIZE];
  let mut line = Vec::new();
  let mut last_was_escaped = false;

  loop {
    let scan_start = usize::from(last_was_escaped && escape_aware);

    let n = match read(fd, &mut buf) {
      Ok(0) => {
        if line.is_empty() {
          state::Shed::set_status(1);
          return Ok(Vec::new());
        }
        return finalize(line, escape_aware);
      }
      Ok(n) => n,

      Err(Errno::EINTR) => {
        if signal::sigint_pending() {
          // we got ctrl+c
          state::Shed::set_status(130);
          return Ok(Vec::new());
        }
        if signal::has_actionable_pending() {
          state::Shed::set_status(1);
          return finalize(line, escape_aware);
        }
        continue;
      }
      Err(e) => return Err(e.into()),
    };

    let chunk = &buf[..n];
    let scan_slice = &chunk[scan_start..];

    if let Some(pos_in_scan) = delim_scan(delim, scan_slice, escape_aware) {
      let pos = scan_start + pos_in_scan;
      line.extend_from_slice(&chunk[..pos]);
      let consumed = pos + 1; // include the delimiter
      let leftover = n - consumed;
      if leftover > 0 {
        // lseek backwards to the delimiter's position
        // next read starts there
        unistd::lseek(fd, -(leftover as i64), unistd::Whence::SeekCur)?;
      }
      return finalize(line, escape_aware);
    }

    line.extend_from_slice(chunk);

    if escape_aware && n > 0 {
      let mut escaped = false;
      let mut i = n;

      while i > scan_start {
        i -= 1;
        if chunk[i] != b'\\' {
          break;
        }
        escaped = !escaped;
      }

      last_was_escaped = escaped;
    } else {
      last_was_escaped = false;
    }

    if let Some(max) = max_bytes
      && line.len() >= max
    {
      let leftover = line.len() - max;
      if leftover > 0 {
        unistd::lseek(fd, -(leftover as i64), unistd::Whence::SeekCur)?;
        line.truncate(max);
      }
      return finalize(line, escape_aware);
    }
  }
}

fn finalize(mut line: Vec<u8>, escape_aware: bool) -> ShResult<Vec<u8>> {
  state::Shed::set_status(0);
  if escape_aware {
    line = unescape(&line);
  }
  Ok(line)
}

fn unescape(line: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(line.len());
  let mut byte_enum = line.iter();

  match_loop!(byte_enum.next() => &byte => byte, {
    b'\\' => {
      match byte_enum.next() {
        Some(&b'\n') | None => {}
        Some(&next) => out.push(next),
      }
    },
    _ => out.push(byte)
  });

  out
}

/// POSIX field splitting. IFS-whitespace runs collapse into a single
/// delimiter and are stripped from both ends; non-whitespace IFS characters
/// are hard delimiters that can yield empty fields. When `max` is set,
/// splitting stops after `max - 1` fields and the untouched remainder (with
/// trailing IFS-whitespace trimmed) becomes the final field, mirroring
/// `read var1 var2 ...` where the last variable absorbs the rest of the line.
fn ifs_split(input: &[u8], ifs: &[u8], max: Option<usize>) -> Vec<Vec<u8>> {
  let is_ws = |b: u8| b.is_ascii_whitespace() && ifs.contains(&b);
  let is_hard = |b: u8| !b.is_ascii_whitespace() && ifs.contains(&b);

  let mut fields: Vec<Vec<u8>> = Vec::new();
  let mut cur: Vec<u8> = Vec::new();
  let mut bytes = input.iter().copied().enumerate().peekable();

  while bytes.peek().is_some_and(|&(_, c)| is_ws(c)) {
    bytes.next();
  }

  while let Some(&(i, c)) = bytes.peek() {
    if max.is_some_and(|max| fields.len() == max - 1) {
      let mut rest = input[i..].to_vec();
      while rest.last().is_some_and(|&b| is_ws(b)) {
        rest.pop();
      }
      fields.push(rest);
      return fields;
    }

    bytes.next();

    if is_ws(c) {
      while bytes.peek().is_some_and(|&(_, c)| is_ws(c)) {
        bytes.next();
      }
      if bytes.peek().is_some_and(|&(_, c)| is_hard(c)) {
        bytes.next();
        while bytes.peek().is_some_and(|&(_, c)| is_ws(c)) {
          bytes.next();
        }
      }
      // trailing whitespace must not produce an empty field
      if bytes.peek().is_some() {
        fields.push(std::mem::take(&mut cur));
      }
    } else if is_hard(c) {
      fields.push(std::mem::take(&mut cur));
      while bytes.peek().is_some_and(|&(_, c)| is_ws(c)) {
        bytes.next();
      }
    } else {
      cur.push(c);
    }
  }

  if !cur.is_empty() {
    fields.push(cur);
  }

  fields
}

/// Merge zero-width fields into the following one. A field that renders to no
/// glyphs (a lone SGR color escape, say) can't stand alone, so it attaches to
/// the next field. Genuine empty fields (zero bytes, from adjacent
/// non-whitespace IFS delimiters) are kept, since those carry meaning. This
/// deliberately diverges from bash, which would leave the escape orphaned;
/// the divergence makes splitting colored, column-aligned command output
/// (`eza`, `ls`, `ps`) survive positional indexing.
fn glue_zero_width(fields: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
  let mut out: Vec<Vec<u8>> = Vec::with_capacity(fields.len());
  let mut pending: Vec<u8> = Vec::new();

  for field in fields {
    if !field.is_empty() && state::terminal::calc_str_width(&String::from_utf8_lossy(&field)) == 0 {
      pending.extend_from_slice(&field);
    } else if pending.is_empty() {
      out.push(field);
    } else {
      pending.extend_from_slice(&field);
      out.push(std::mem::take(&mut pending));
    }
  }

  // a dangling zero-width run (e.g. a trailing reset) has nothing to attach to
  if !pending.is_empty() {
    out.push(pending);
  }

  out
}

fn field_split_vars(input: &[u8], vars: &[(VarStr, Span)]) -> ShResult<()> {
  if vars.is_empty() {
    Shed::vars_mut(|v| {
      v.set_var(
        "REPLY",
        VarKind::string(VarStr::from(input)),
        VarFlags::empty(),
      )
    })?;
    return Ok(());
  }

  let sep = state::util::get_separators();
  let fields = ifs_split(input, sep.as_bytes(), Some(vars.len()));

  for (i, (name, _)) in vars.iter().enumerate() {
    let value: &[u8] = fields.get(i).map_or(&[][..], Vec::as_slice);
    Shed::vars_mut(|v| {
      v.set_var(
        &name.to_str_lossy(),
        VarKind::string(VarStr::from(value)),
        VarFlags::empty(),
      )
    })?;
  }

  Ok(())
}

fn field_split_arr(input: &[u8], arr_name: &str) -> ShResult<()> {
  if arr_name.is_empty() {
    return Err(sherr!(ExecFail, "read: Array name cannot be empty"));
  }

  let sep = state::util::get_separators();
  let fields = glue_zero_width(ifs_split(input, sep.as_bytes(), None));

  Shed::vars_mut(|v| {
    v.set_var(
      arr_name,
      VarKind::arr(fields.into_iter().map(VarStr::from)),
      VarFlags::empty(),
    )
  })
}

/// Join byte-slice fields with `sep` (no trailing separator).
fn join_fields(fields: &[Vec<u8>], sep: &[u8]) -> Vec<u8> {
  let mut out = Vec::new();
  for (i, f) in fields.iter().enumerate() {
    if i > 0 {
      out.extend_from_slice(sep);
    }
    out.extend_from_slice(f);
  }
  out
}

fn field_split_vars_quoted(input: &[u8], vars: &[(VarStr, Span)]) -> ShResult<()> {
  let fields = glue_zero_width(quote::unquote_raw(input)?);

  if vars.is_empty() {
    let joined = join_fields(&fields, b" ");
    Shed::vars_mut(|v| {
      v.set_var(
        "REPLY",
        VarKind::string(VarStr::from(joined)),
        VarFlags::empty(),
      )
    })?;
    return Ok(());
  }

  for (i, (name, _)) in vars.iter().enumerate() {
    let value: Vec<u8> = if i >= fields.len() {
      Vec::new()
    } else if i + 1 == vars.len() {
      join_fields(&fields[i..], b" ")
    } else {
      fields[i].clone()
    };

    Shed::vars_mut(|v| {
      v.set_var(
        &name.to_str_lossy(),
        VarKind::string(VarStr::from(value)),
        VarFlags::empty(),
      )
    })?;
  }

  Ok(())
}

fn field_split_arr_quoted(input: &[u8], arr_name: &str) -> ShResult<()> {
  if arr_name.is_empty() {
    return Err(sherr!(ExecFail, "read: Array name cannot be empty"));
  }

  let fields = glue_zero_width(quote::unquote_raw(input)?);

  Shed::vars_mut(|v| {
    v.set_var(
      arr_name,
      VarKind::arr(fields.into_iter().map(VarStr::from)),
      VarFlags::empty(),
    )
  })
}

pub(super) struct ReadKey;
impl super::Builtin for ReadKey {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("var", 'v').argc(1),
      OptSpec::new_short("whitelist", 'w').argc(1),
      OptSpec::new_short("blacklist", 'b').argc(1),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if !Shed::term(Terminal::isatty) {
      return with_status(1);
    }
    let mut whitelist = None;
    let mut blacklist = None;
    let mut var_name = None;

    for opt in args.options() {
      match opt.key() {
        "var" => {
          var_name = Some(opt.value()?);
        }
        "whitelist" => {
          whitelist = Some(opt.value()?);
        }
        "blacklist" => {
          blacklist = Some(opt.value()?);
        }
        _ => {
          return Err(sherr!(ExecFail @ opt.span(), "readkey: Unexpected flag '{opt}'"));
        }
      }
    }

    let key = {
      let _raw = Shed::term_mut(Terminal::raw_mode_guard);
      if let Err(e) = Shed::term_mut(Terminal::read) {
        match e.kind() {
          ShErrKind::LoopBreak(_) => return with_status(1),
          ShErrKind::LoopContinue(_) => return with_status(0),
          _ => return Err(e).promote_err(args.span()),
        }
      }

      let mut keys = Shed::term_mut(Terminal::drain_keys);
      if keys.is_empty() {
        return with_status(1);
      }

      keys.remove(0)
    };

    let vim_seq = key.as_vim_seq();

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
      Shed::vars_mut(|v| v.set_var(var, VarKind::string(vim_seq), VarFlags::empty()))?;
    } else {
      out!("{vim_seq}");
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::terminal::Terminal;
  use crate::state::vars::VarStr;
  use crate::state::{self, Shed, vars::VarFlags, vars::VarKind};
  use crate::tests::testutil::{TestGuard, test_input};
  use crate::var;

  fn arr(name: &str) -> Vec<VarStr> {
    Shed::vars(|v| v.try_get_arr_elems(name)).unwrap()
  }

  // ===================== Basic read into REPLY =====================

  #[test]
  fn read_pipe_into_reply() {
    let _g = TestGuard::new();
    test_input("read < <(echo hello)").unwrap();
    let val = var!("REPLY");
    assert_eq!(val, "hello");
  }

  #[test]
  fn read_pipe_into_named_var() {
    let _g = TestGuard::new();
    test_input("read myvar < <(echo world)").unwrap();
    let val = var!("myvar");
    assert_eq!(val, "world");
  }

  #[test]
  fn read_preserves_non_utf8_bytes() {
    // Regression: `read` used to reject non-UTF-8 input ("read: invalid UTF-8").
    // It must now store the raw bytes verbatim, like bash.
    let _g = TestGuard::new();
    test_input("read x < <(printf 'a\\377b')").unwrap();
    assert_eq!(var!("x").as_bytes(), &b"a\xffb"[..]);
  }

  #[test]
  fn read_field_split_preserves_non_utf8_bytes() {
    let _g = TestGuard::new();
    test_input("read a b < <(printf 'x\\377y z')").unwrap();
    assert_eq!(var!("a").as_bytes(), &b"x\xffy"[..]);
    assert_eq!(var!("b").as_bytes(), &b"z"[..]);
  }

  // ===================== Field splitting =====================

  #[test]
  fn read_two_vars() {
    let _g = TestGuard::new();
    test_input("read a b < <(echo 'hello world')").unwrap();
    assert_eq!(var!("a"), "hello");
    assert_eq!(var!("b"), "world");
  }

  #[test]
  fn read_last_var_gets_remainder() {
    let _g = TestGuard::new();
    test_input("read a b < <(echo 'one two three four')").unwrap();
    assert_eq!(var!("a"), "one");
    assert_eq!(var!("b"), "two three four");
  }

  #[test]
  fn read_more_vars_than_fields() {
    let _g = TestGuard::new();
    test_input("read a b c < <(echo 'only')").unwrap();
    assert_eq!(var!("a"), "only");
    // b and c get empty strings since there are no more fields
    assert_eq!(var!("b"), "");
    assert_eq!(var!("c"), "");
  }

  #[test]
  fn read_collapses_whitespace_runs() {
    // Regression: column-aligned output (ls/eza/ps) pads with runs of spaces.
    // Default-IFS splitting must collapse them rather than emit empty fields.
    let _g = TestGuard::new();
    test_input("read -a f < <(echo 'a    b   c')").unwrap();
    assert_eq!(arr("f"), vec!["a", "b", "c"]);
  }

  #[test]
  fn read_arr_trims_leading_and_trailing_whitespace() {
    let _g = TestGuard::new();
    test_input("read -a f < <(printf '   a b   \\n')").unwrap();
    assert_eq!(arr("f"), vec!["a", "b"]);
  }

  #[test]
  fn read_vars_preserve_remainder_spacing() {
    // The last variable absorbs the rest of the line with its internal
    // whitespace intact, only trailing IFS-whitespace stripped.
    let _g = TestGuard::new();
    test_input("read a b < <(echo 'one   two   three')").unwrap();
    assert_eq!(var!("a"), "one");
    assert_eq!(var!("b"), "two   three");
  }

  #[test]
  fn read_arr_hard_ifs_keeps_empty_fields() {
    // Non-whitespace IFS delimiters are hard: adjacent ones yield empties.
    // The zero-width gluing pass must not swallow these (zero bytes != zero
    // display width).
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::empty())).unwrap();
    test_input("read -a f < <(echo 'a::b')").unwrap();
    assert_eq!(arr("f"), vec!["a", "", "b"]);
  }

  #[test]
  fn read_ws_hard_ws_folds_into_one_separator() {
    // POSIX: `ws* hard ws*` is a single delimiter. `a , b` with IFS=' ,'
    // must yield two fields, not three (no spurious empty from trailing ws).
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(" ,".into()), VarFlags::empty())).unwrap();
    test_input("read -a f < <(echo 'a , b')").unwrap();
    assert_eq!(arr("f"), vec!["a", "b"]);
  }

  #[test]
  fn read_ws_hard_ws_scalar_split() {
    let g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(" ,".into()), VarFlags::empty())).unwrap();
    test_input("read x y < <(echo 'a , b'); echo \"[$x][$y]\"").unwrap();
    assert_eq!(g.read_output().trim(), "[a][b]");
  }

  #[test]
  fn read_arr_glues_orphan_escape_into_next_field() {
    // A bare color escape between whitespace renders to nothing, so it merges
    // into the following field rather than becoming its own. This is what lets
    // colored, column-aligned output (eza/ls) survive positional splitting.
    let _g = TestGuard::new();
    test_input(r"read -a f < <(printf '\x1b[34m foo bar\n')").unwrap();
    assert_eq!(arr("f"), vec!["\u{1b}[34mfoo", "bar"]);
  }

  #[test]
  fn read_arr_trailing_orphan_escape_is_kept() {
    // A dangling escape with nothing after it has no field to glue onto, so it
    // survives as its own element rather than being dropped.
    let _g = TestGuard::new();
    test_input(r"read -a f < <(printf 'foo \x1b[0m\n')").unwrap();
    assert_eq!(arr("f"), vec!["foo", "\u{1b}[0m"]);
  }

  #[test]
  fn read_q_preserves_quoted_whitespace_and_glues_escape() {
    // The quoted path honors single-quoting (so a name with runs of spaces is
    // kept verbatim, unlike whitespace-splitting), and still glues an orphan
    // color escape into the next field. This is what lets `read -q -a` parse
    // colored, quoted, column-aligned output (eza) losslessly.
    let _g = TestGuard::new();
    test_input(r#"read -q -a f < <(printf "\x1b[34m foo 'a   b   c'\n")"#).unwrap();
    assert_eq!(arr("f"), vec!["\u{1b}[34mfoo", "a   b   c"]);
  }

  // ===================== Custom IFS =====================

  #[test]
  fn read_custom_ifs() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::empty())).unwrap();

    test_input("read x y z < <(echo 'a:b:c')").unwrap();
    assert_eq!(var!("x"), "a");
    assert_eq!(var!("y"), "b");
    assert_eq!(var!("z"), "c");
  }

  #[test]
  fn read_custom_ifs_remainder() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::empty())).unwrap();

    test_input("read x y < <(echo 'a:b:c:d')").unwrap();
    assert_eq!(var!("x"), "a");
    assert_eq!(var!("y"), "b:c:d");
  }

  // ===================== Custom delimiter =====================

  #[test]
  fn read_custom_delim() {
    let _g = TestGuard::new();
    // -d sets the delimiter; printf sends "hello,world" - read stops at ','
    test_input("read -d , myvar < <(echo -n 'hello,world')").unwrap();
    assert_eq!(var!("myvar"), "hello");
  }

  #[test]
  fn read_custom_delim_escaped_is_literal() {
    // Regression: on the walking path (pipe/process-sub), `\<delim>` used to be
    // dropped entirely. It should become a literal delimiter char (backslash
    // consumed), matching bash and the seekable path.
    let _g = TestGuard::new();
    test_input(r"read -d : x < <(printf '%s' 'a\:b:')").unwrap();
    assert_eq!(var!("x"), "a:b");
  }

  // ===================== Status =====================

  #[test]
  fn read_status_zero() {
    let _g = TestGuard::new();
    test_input("read < <(echo hello)").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn read_eof_status_one() {
    let _g = TestGuard::new();
    // Empty input / EOF should set status 1
    test_input("read < <(echo -n '')").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  // ===================== readkey =====================

  /// Set the tty to raw mode at test start so subsequently `feed_tty`'d
  /// bytes pass through without the kernel buffering them until newline
  /// or interpreting special chars (Ctrl+D as VEOF, etc.).
  fn arm_raw_tty() {
    Shed::term_mut(Terminal::enforce_raw_mode).unwrap();
  }

  #[test]
  fn readkey_stores_into_named_var() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"a");
    test_input("readkey -v key").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(var!("key"), "a");
  }

  #[test]
  fn readkey_with_no_var_writes_to_stdout() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"a");
    test_input("readkey").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    // The vim-seq for plain 'a' (no mods) is just "a".
    assert!(
      g.read_output().contains('a'),
      "expected 'a' in output, got: {:?}",
      g.read_output()
    );
  }

  #[test]
  fn readkey_whitelist_accepts_listed_char() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"y");
    test_input("readkey -v ans -w yn").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(var!("ans"), "y");
  }

  #[test]
  fn readkey_whitelist_rejects_unlisted_char() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"x");
    test_input("readkey -v ans -w yn").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
    // var should not be set when whitelist rejects.
    assert_eq!(var!("ans"), "");
  }

  #[test]
  fn readkey_blacklist_rejects_listed_char() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"q");
    test_input("readkey -v ans -b q").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
    assert_eq!(var!("ans"), "");
  }

  #[test]
  fn readkey_blacklist_accepts_unlisted_char() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"a");
    test_input("readkey -v ans -b q").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(var!("ans"), "a");
  }

  #[test]
  fn readkey_renders_special_key_as_vim_seq() {
    // Carriage return (Enter) — feeds \r, which the parser maps to
    // KeyCode::Enter, rendered as <Enter>.
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"\r");
    test_input("readkey -v k").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(var!("k"), "<Enter>");
  }

  #[test]
  fn readkey_renders_ctrl_char_as_vim_seq() {
    // Ctrl+A is \x01, parsed as KeyCode::Char('a') with CTRL → "<C-a>".
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"\x01");
    test_input("readkey -v k").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    assert_eq!(var!("k"), "<C-a>");
  }

  // ===================== Read::execute remaining flags =====================

  #[test]
  fn read_r_flag_disables_escape_processing() {
    let _g = TestGuard::new();
    // Without -r the backslash would be consumed as an escape. With -r
    // it is preserved verbatim.
    test_input("read -r line < <(printf 'a\\\\b\\n')").unwrap();
    assert_eq!(var!("line"), "a\\b");
  }

  #[test]
  fn read_n_flag_limits_byte_count() {
    let _g = TestGuard::new();
    test_input("read -n 3 short < <(echo -n 'helloworld')").unwrap();
    assert_eq!(var!("short"), "hel");
  }

  #[test]
  fn read_n_flag_invalid_count_errors() {
    let _g = TestGuard::new();
    test_input("read -n notanumber line < <(echo hi)").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn read_t_flag_invalid_value_errors() {
    let _g = TestGuard::new();
    test_input("read -t abc line < <(echo hi)").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn read_a_flag_populates_array() {
    let _g = TestGuard::new();
    test_input("read -a arr < <(echo 'one two three')").unwrap();
    // Index into the array; an unspecified element returns empty.
    test_input("echo $arr[0]:$arr[1]:$arr[2]").unwrap();
    // Just verify that the array element accesses succeed and the
    // status is 0.
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn read_p_flag_emits_prompt() {
    let g = TestGuard::new();
    test_input("read -p 'enter> ' line < <(echo hi)").unwrap();
    let out = g.read_output();
    assert!(out.contains("enter> "), "got: {out:?}");
    assert_eq!(var!("line"), "hi");
  }
}
