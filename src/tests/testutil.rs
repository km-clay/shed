use std::{
  env,
  os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
  path::PathBuf,
  sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
  },
  thread::JoinHandle,
};

use nix::{
  errno::Errno,
  poll::{PollFd, PollFlags, PollTimeout, poll},
  pty::openpty,
  sys::termios::{OutputFlags, SetArg, tcgetattr, tcsetattr},
  unistd::pipe,
};

use crate::{
  HashMap, signal,
  state::{scopes::ScopeStack, util},
};

#[macro_export]
macro_rules! assert_output {
  ($guard:expr, $($arg:tt)*) => {{
    use std::fmt::Write;
    let output = $guard.read_output();
    let mut expected = String::new();
    write!(&mut expected, $($arg)*).unwrap();
    assert_eq!(output, expected);
  }};
}

/// Like `assert_output!`, but compares raw bytes against a `&[u8]` expression
/// so non-UTF-8 output survives the check unmangled.
#[macro_export]
macro_rules! assert_output_bytes {
  ($guard:expr, $expected:expr) => {{
    let output = $guard.read_output_bytes();
    let expected: &[u8] = $expected;
    assert_eq!(output, expected);
  }};
}

#[macro_export]
macro_rules! assert_file {
  ($path:expr, $($arg:tt)*) => {{
    use std::fmt::Write;
    let content = std::fs::read_to_string($path).expect("assert_file: could not read file");
    let mut expected = String::new();
    write!(&mut expected, $($arg)*).unwrap();
    assert_eq!(content, expected);
  }};
}

#[macro_export]
macro_rules! assert_status_eq {
  ($expected_status:expr) => {
    {
      assert_eq!(state::Shed::get_status(), $expected_status);
    }

  };
  ($expected_status:expr, $($args:tt)+) => {
    {
      assert_eq!(state::Shed::get_status(), $expected_status, $($args)+);
    }
  }
}

#[macro_export]
macro_rules! assert_status_ne {
  ($expected_status:expr) => {
    {
      assert_ne!(state::Shed::get_status(), $expected_status);
    }

  };
  ($expected_status:expr, $($args:tt)+) => {
    {
      assert_ne!(state::Shed::get_status(), $expected_status, $($args)+);
    }
  }
}

use crate::{
  eval::{NdKind, ParsedSrc, execute::exec_nonint, lex::LexFlags},
  expand::expand_aliases,
  procio::{RedirGuard, RedirSet, RedirSpec, RedirType},
  readline::{restore_registers, save_registers},
  state::{self, Shed, meta::MetaTab},
  util::ShResult,
};

/// Returns the canonical (symlink-resolved) form of `p`. Useful in tests
/// that assert `env::current_dir()` matches a `tempfile::TempDir` path:
/// on macOS, `getcwd()` returns `/private/var/folders/...` while
/// `TempDir::path()` returns `/var/folders/...` (because `/var` is a
/// symlink to `/private/var`). On Linux this is a no-op. Falls back to
/// the input path if the file doesn't exist (so tests that name
/// not-yet-created paths still work).
pub(crate) fn canon(p: impl AsRef<std::path::Path>) -> std::path::PathBuf {
  p.as_ref()
    .canonicalize()
    .unwrap_or_else(|_| p.as_ref().to_path_buf())
}

pub(crate) fn has_cmds(cmds: &[&str]) -> bool {
  let path_cmds = MetaTab::get_cmds_in_path();
  path_cmds
    .iter()
    .all(|c| cmds.iter().any(|&cmd| c.name() == cmd))
}

pub(crate) fn has_cmd(cmd: &str) -> bool {
  MetaTab::get_cmds_in_path()
    .into_iter()
    .any(|c| c.name() == cmd)
}

/// Marks the end of a test's output
pub(crate) const TEST_OUTPUT_SENTINEL: &[u8] = b"\x07__shed_test_end__\x07";

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
  haystack.windows(needle.len()).position(|w| w == needle)
}

pub(crate) fn test_input(input: impl Into<String>) -> ShResult<()> {
  exec_nonint(input.into().into(), None)
}

/// On creation, increments function depth and pushes a function stack frame.
///
/// On drop, this stack frame is popped.
pub(crate) struct FuncScope(state::meta::FuncGuard);

impl FuncScope {
  pub(crate) fn new() -> Self {
    let guard = Shed::meta_mut(MetaTab::enter_func);
    Shed::vars_mut(|v| v.descend_into_function(None));
    Self(guard)
  }
}

impl Drop for FuncScope {
  fn drop(&mut self) {
    // Pop the function scope; the inner FuncGuard then decrements func_depth.
    Shed::vars_mut(ScopeStack::ascend);
  }
}

pub(crate) struct TestGuard {
  redir_guard: Option<RedirGuard>,
  old_cwd: PathBuf,
  saved_env: HashMap<String, String>,

  pty_slave: Option<OwnedFd>,
  pty_master: Option<OwnedFd>,
  stdin_write_pipe: Option<OwnedFd>,
  output: Arc<(Mutex<Vec<u8>>, Condvar)>,
  reader_done: Arc<AtomicBool>,
  read_handle: Option<JoinHandle<()>>,

  cleanups: Vec<Box<dyn FnOnce()>>,
}

impl TestGuard {
  pub fn new() -> Self {
    util::register_fork_marker();
    let pty = openpty(None, None).unwrap();
    let (pty_master, pty_slave) = (pty.master, pty.slave);
    let master_raw = pty_master.as_raw_fd();

    let mut attrs = tcgetattr(&pty_slave).unwrap();
    attrs.output_flags &= !OutputFlags::ONLCR;
    tcsetattr(&pty_slave, SetArg::TCSANOW, &attrs).unwrap();

    Shed::term_mut(|t| t.set_fd_for_testing(Some(pty_slave.as_raw_fd())));

    // we need this arc mutex and read handle because large test outputs
    // will cause the test to hang if we try to do everything on one thread.
    // if we attempt to do this synchronously, we have to do both the reading and the writing.
    // we can't read if we're blocked on writing to a full pty buffer.
    let output = Arc::new((Mutex::new(vec![]), Condvar::new()));
    let output_clone = Arc::clone(&output);
    let reader_done = Arc::new(AtomicBool::new(false));
    let done_clone = Arc::clone(&reader_done);
    let read_handle = std::thread::spawn(move || {
      let mut buf = [0u8; 4096];
      let master = unsafe { BorrowedFd::borrow_raw(master_raw) };
      loop {
        let mut fds = [PollFd::new(master, PollFlags::POLLIN)];
        match poll(&mut fds, PollTimeout::from(250u16)) {
          Ok(0) => {
            if done_clone.load(Ordering::Relaxed) {
              break; // teardown asked us to stop and EOF isn't coming
            }
          }
          Ok(_) => {
            let n = unsafe {
              nix::libc::read(
                master_raw,
                buf.as_mut_ptr().cast::<nix::libc::c_void>(),
                buf.len(),
              )
            };
            if n > 0 {
              let (mu, cv) = &*output_clone;
              mu.lock().unwrap().extend_from_slice(&buf[..n as usize]);
              cv.notify_all();
            } else if n == 0 {
              break; // EOF: all slave write ends closed (the fast path)
            } else if std::io::Error::last_os_error().raw_os_error() == Some(nix::libc::EINTR) {
              // a signal interrupted the read; retry
            } else {
              break; // real read error
            }
          }
          Err(Errno::EINTR) => (), // a signal interrupted poll; retry
          Err(_) => break,
        }
      }
    });

    let (stdin_read, stdin_write) = pipe().unwrap();

    let redirs: RedirSet = vec![
      RedirSpec::dup(stdin_read.as_raw_fd(), 0, RedirType::Input),
      RedirSpec::dup(pty_slave.as_raw_fd(), 1, RedirType::Output),
      RedirSpec::dup(pty_slave.as_raw_fd(), 2, RedirType::Output),
    ]
    .into();

    let redir_guard = redirs.apply().or_fatal().ok().flatten().unwrap();

    let old_cwd = env::current_dir().unwrap();
    let saved_env = env::vars().collect();
    state::Shed::save_state();
    let scrub_keys = [
      "SHED_HPATH",
      "SHED_FUNC_PATH",
      "SHED_COMPLETE_PATH",
      "SHED_LOG",
      "SHED_COLOR_MODE",
      "SHELL_PROMPT_PREFIX",
      "SHELL_PROMPT_SUFFIX",
      "SHELL_WELCOME",
      "NO_COLOR",
    ];
    for key in scrub_keys {
      unsafe { env::remove_var(key) };
    }
    Shed::vars_mut(|v| {
      for key in scrub_keys {
        let _ = v.unset_var(key);
      }
    });
    state::util::try_hash();
    save_registers();
    // Set up an in-memory sqlite db (once per test thread; OnceLock means
    // subsequent TestGuards no-op here). Then wipe the stash table so each
    // test starts with a clean slate.
    state::util::init_test_db_conn();
    if let Some(conn) = state::util::get_db_conn() {
      // The table won't exist on first run; ignore that error.
      let _ = conn
        .lock()
        .unwrap()
        .execute_batch("DROP TABLE IF EXISTS stash");
    }
    Self {
      redir_guard: Some(redir_guard),
      old_cwd,
      saved_env,
      pty_master: Some(pty_master),
      pty_slave: Some(pty_slave),
      stdin_write_pipe: Some(stdin_write),

      output,
      reader_done,
      read_handle: Some(read_handle),

      cleanups: vec![],
    }
  }

  pub fn add_cleanup(&mut self, f: impl FnOnce() + 'static) {
    self.cleanups.push(Box::new(f));
  }

  /// Create a unique temp directory and cd into it.
  /// The directory is deleted and cwd is restored on drop.
  pub fn in_temp_dir(&mut self) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
      "shed_test_{}",
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    env::set_current_dir(&dir).unwrap();
    let dir_clone = dir.clone();
    self.add_cleanup(move || {
      std::fs::remove_dir_all(&dir_clone).ok();
    });
    dir
  }

  pub fn feed_stdin(&mut self, data: &[u8]) {
    if let Some(fd) = self.stdin_write_pipe.take() {
      let borrowed = fd.as_fd();
      nix::unistd::write(borrowed, data).unwrap();
      // drops, closes
    }
  }

  /// Write bytes to the pty master, which will appear as readable input on
  /// the pty slave that Shed's Terminal reads from. Use this to drive
  /// poll/read paths in tests.
  pub fn feed_tty(&self, data: &[u8]) {
    if let Some(master) = self.pty_master.as_ref() {
      nix::unistd::write(master.as_fd(), data).unwrap();
    }
  }

  /// Close the pty master fd. The slave (shed's tty) will then see POLLHUP
  /// on the next poll, exercising the disconnect-cleanup branch in
  /// `shed_loop_iter`.
  pub fn close_tty_master(&mut self) {
    self.pty_master.take(); // drops, closes
  }

  pub fn read_output(&self) -> String {
    String::from_utf8_lossy(&self.read_output_bytes()).to_string()
  }

  /// Like `read_output`, but returns the raw bytes without a lossy UTF-8
  /// conversion. Use this to assert byte-transparent output (e.g. `printf`
  /// emitting non-UTF-8 bytes), which `read_output` would mangle.
  pub fn read_output_bytes(&self) -> Vec<u8> {
    // if we are here, then that means we have probably finished executing
    // our test. we now write this to the pty
    if let Some(slave) = self.pty_slave.as_ref() {
      let _ = nix::unistd::write(slave.as_fd(), TEST_OUTPUT_SENTINEL);
    }

    let (mu, cv) = &*self.output;
    let mut buf = mu.lock().unwrap();

    // 2-second deadline is a "your test deadlocked" backstop, not a
    // tuning knob. In normal operation we exit as soon as the sentinel
    // arrives (typically sub-millisecond).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while find_subsequence(&buf, TEST_OUTPUT_SENTINEL).is_none() {
      let now = std::time::Instant::now();
      if now >= deadline {
        break;
      }
      let r = cv.wait_timeout(buf, deadline - now).unwrap();
      buf = r.0;
    }

    // drain until our sentinel sequence
    let sentinel_pos = find_subsequence(&buf, TEST_OUTPUT_SENTINEL);
    let (end, drain_to) = match sentinel_pos {
      Some(pos) => (pos, pos + TEST_OUTPUT_SENTINEL.len()),
      None => (buf.len(), buf.len()),
    };
    let res = buf[..end].to_vec();
    buf.drain(..drain_to);

    res // done
  }
}

impl Default for TestGuard {
  fn default() -> Self {
    Self::new()
  }
}

impl Drop for TestGuard {
  fn drop(&mut self) {
    env::set_current_dir(&self.old_cwd).ok();
    for (k, _) in env::vars() {
      unsafe {
        env::remove_var(&k);
      }
    }
    for (k, v) in &self.saved_env {
      unsafe {
        env::set_var(k, v);
      }
    }
    for cleanup in self.cleanups.drain(..).rev() {
      cleanup();
    }
    state::Shed::restore_state();
    state::Shed::sinks(|s| *s = crate::procio::Sinks::new());
    restore_registers();
    signal::clear_quit_latch();

    self.reader_done.store(true, Ordering::Relaxed);
    self.redir_guard.take();
    self.pty_slave.take();
    if let Some(h) = self.read_handle.take() {
      let _ = h.join();
    }
  }
}

pub(crate) fn get_ast(input: &str) -> ShResult<crate::eval::parse::Ast> {
  let input = expand_aliases(input);

  let mut parser = ParsedSrc::new(input.into())
    .with_lex_flags(LexFlags::empty())
    .with_name("test_input".into());

  parser
    .parse_src()
    .map_err(|e| e.into_iter().next().unwrap())?;

  Ok(parser.into_ast())
}

impl crate::eval::parse::Ast {
  pub fn assert_structure(
    &self,
    expected: &mut impl Iterator<Item = NdKind>,
  ) -> Result<(), String> {
    let root = self.get_root().expect("assert_structure: AST has no root");
    let mut full_structure = vec![];
    let mut before = vec![];
    let mut after = vec![];
    let mut offender = None;

    self.walk_tree(root, &mut |s| {
      let expected_rule = expected.next();
      full_structure.push(s.class.as_nd_kind());

      if offender.is_none()
        && expected_rule
          .as_ref()
          .is_none_or(|e| *e != s.class.as_nd_kind())
      {
        offender = Some((s.class.as_nd_kind(), expected_rule));
      } else if offender.is_none() {
        before.push(s.class.as_nd_kind());
      } else {
        after.push(s.class.as_nd_kind());
      }
    });

    assert!(
      expected.next().is_none(),
      "Expected structure has more nodes than actual structure"
    );

    if let Some((nd_kind, expected_rule)) = offender {
      let expected_rule = expected_rule.map_or("(none - expected array too short)".into(), |e| {
        format!("{e:?}")
      });
      let full_structure_hint = full_structure
        .into_iter()
        .map(|s| format!("\tNdKind::{s:?},"))
        .collect::<Vec<String>>()
        .join("\n");
      let full_structure_hint =
        format!("let expected = &mut [\n{full_structure_hint}\n].into_iter();");

      let output = [
        "Structure assertion failed!\n".into(),
        format!("Expected node type '{expected_rule:?}', found '{nd_kind:?}'"),
        format!("Before offender: {before:?}"),
        format!("After offender: {after:?}\n"),
        format!("hint: here is the full structure as an array\n {full_structure_hint}"),
      ]
      .join("\n");

      Err(output)
    } else {
      Ok(())
    }
  }
}
