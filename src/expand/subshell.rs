use crate::expand::arithmetic::expand_arithmetic_wrapped;
use crate::libsh::error::ShResult;
use crate::parse::execute::exec_input;
use crate::parse::{Redir, RedirType};
use crate::prelude::*;
use crate::procio::{IoBuf, IoFrame, IoMode, IoStack};
use crate::sherr;
use crate::state::{self, write_jobs};

pub fn expand_proc_sub(raw: &str, is_input: bool) -> ShResult<String> {
  // FIXME: Still a lot of issues here
  // Seems like debugging will be a massive effort
  let (rpipe, wpipe) = IoMode::get_pipes();
  let rpipe_raw = rpipe.src_fd();
  let wpipe_raw = wpipe.src_fd();

  let (proc_fd, register_fd, redir_type, path) = match is_input {
    false => (
      wpipe,
      rpipe,
      RedirType::Output,
      format!("/proc/self/fd/{}", rpipe_raw),
    ),
    true => (
      rpipe,
      wpipe,
      RedirType::Input,
      format!("/proc/self/fd/{}", wpipe_raw),
    ),
  };

  match unsafe { fork()? } {
    ForkResult::Child => {
      // Close the parent's pipe end so the grandchild doesn't inherit it.
      // Without this, >(cmd) hangs because the command holds its own
      // pipe's write end open and never sees EOF.
      drop(register_fd);

      let redir = Redir::new(proc_fd, redir_type);
      let io_frame = IoFrame::from_redir(redir);
      let mut io_stack = IoStack::new();
      io_stack.push_frame(io_frame);

      if let Err(e) = exec_input(
        raw.to_string(),
        Some(io_stack),
        false,
        Some("process_sub".into()),
      ) {
        e.print_error();
        exit(1);
      }
      exit(0);
    }
    ForkResult::Parent { child } => {
      write_jobs(|j| j.register_fd(child, register_fd));
      // Do not wait; process may run in background
      Ok(path)
    }
  }
}

/// Get the command output of a given command input as a String
pub fn expand_cmd_sub(raw: &str) -> ShResult<String> {
  if raw.starts_with('(') && raw.ends_with(')') {
    return expand_arithmetic_wrapped(raw);
  }
  let (rpipe, wpipe) = IoMode::get_pipes();
  let cmd_sub_redir = Redir::new(wpipe, RedirType::Output);
  let cmd_sub_io_frame = IoFrame::from_redir(cmd_sub_redir);
  let mut io_stack = IoStack::new();
  let mut io_buf = IoBuf::new(rpipe);

  match unsafe { fork()? } {
    ForkResult::Child => {
      io_stack.push_frame(cmd_sub_io_frame);
      if let Err(e) = exec_input(
        raw.to_string(),
        Some(io_stack),
        false,
        Some("command_sub".into()),
      ) {
        e.print_error();
        unsafe { libc::_exit(1) };
      }
      let status = state::get_status();
      unsafe { libc::_exit(status) };
    }
    ForkResult::Parent { child } => {
      std::mem::drop(cmd_sub_io_frame); // Closes the write pipe

      // Read output first (before waiting) to avoid deadlock if child fills pipe
      // buffer
      loop {
        match io_buf.fill_buffer() {
          Ok(()) => break,
          Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
          Err(e) => return Err(e.into()),
        }
      }

      // Wait for child with EINTR retry
      let status = loop {
        match waitpid(child, Some(WtFlag::WSTOPPED)) {
          Ok(status) => break status,
          Err(Errno::EINTR) => continue,
          Err(e) => return Err(e.into()),
        }
      };

      match status {
        WtStat::Exited(_, code) => {
          state::set_status(code);
          Ok(io_buf.as_str()?.trim_end().to_string())
        }
        _ => Err(sherr!(InternalErr, "Command sub failed")),
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::testutil::TestGuard;

  // ===================== Command Substitution (TestGuard) =====================

  #[test]
  fn cmd_sub_echo() {
    let _guard = TestGuard::new();
    let result = expand_cmd_sub("echo hello").unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn cmd_sub_trailing_newlines_stripped() {
    let _guard = TestGuard::new();
    let result = expand_cmd_sub("printf 'hello\\n\\n'").unwrap();
    assert_eq!(result, "hello");
  }

  #[test]
  fn cmd_sub_arithmetic() {
    let result = expand_cmd_sub("(1+2)").unwrap();
    assert_eq!(result, "3");
  }
}
