use std::{
  cmp::Ordering,
  env,
  os::fd::{AsFd, AsRawFd, OwnedFd},
  path::PathBuf,
  sync::{Arc, Condvar, Mutex},
  thread::JoinHandle,
};

use nix::{
  pty::openpty,
  sys::termios::{OutputFlags, SetArg, tcgetattr, tcsetattr},
  unistd::pipe,
};

use crate::HashMap;

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
  builtin,
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
  exec_nonint(input.into(), None)
}

pub(crate) struct TestGuard {
  _redir_guard: RedirGuard,
  old_cwd: PathBuf,
  saved_env: HashMap<String, String>,

  // Drop order matters: pty_slave MUST come before _pty_master in this
  // struct. On macOS, `close(master)` blocks until any in-flight read on
  // that fd completes. Our reader thread is doing a blocking read on the
  // master. Closing the slave first sends EOF to that read, the thread
  // exits, and then closing the master returns cleanly. Field declaration
  // order is drop order, so this ordering is load-bearing.
  pty_slave: OwnedFd,
  pty_master: Option<OwnedFd>,
  stdin_write_pipe: Option<OwnedFd>,
  output: Arc<(Mutex<Vec<u8>>, Condvar)>,
  _read_handle: JoinHandle<()>,

  cleanups: Vec<Box<dyn FnOnce()>>,
}

impl TestGuard {
  pub fn new() -> Self {
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
    let read_handle = std::thread::spawn(move || {
      let mut buf = [0u8; 4096];
      loop {
        let n = unsafe {
          nix::libc::read(
            master_raw,
            buf.as_mut_ptr().cast::<nix::libc::c_void>(),
            buf.len(),
          )
        };
        match n.cmp(&0) {
          Ordering::Greater => {
            let n = n as usize;
            let (mu, cv) = &*output_clone;
            mu.lock().unwrap().extend_from_slice(&buf[..n]);
            cv.notify_all();
          }
          Ordering::Less | Ordering::Equal => break,
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

    let redir_guard = redirs.apply().ok().flatten().unwrap();

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
      _redir_guard: redir_guard,
      old_cwd,
      saved_env,
      pty_master: Some(pty_master),
      pty_slave,
      stdin_write_pipe: Some(stdin_write),

      output,
      _read_handle: read_handle,

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
    // if we are here, then that means we have probably finished executing
    // our test. we now write this to the pty
    let _ = nix::unistd::write(self.pty_slave.as_fd(), TEST_OUTPUT_SENTINEL);

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
    let res = String::from_utf8_lossy(&buf[..end]).to_string();
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
    builtin::OUT_SINK.with(|o| o.borrow_mut().clear());
    builtin::IN_SINK.with(|o| o.borrow_mut().clear());
    restore_registers();
  }
}

pub(crate) fn get_ast(input: &str) -> ShResult<Vec<crate::eval::Node>> {
  let input = expand_aliases(input.into());

  let mut parser = ParsedSrc::new(input.into())
    .with_lex_flags(LexFlags::empty())
    .with_name("test_input".into());

  parser
    .parse_src()
    .map_err(|e| e.into_iter().next().unwrap())?;

  Ok(parser.extract_nodes())
}

impl crate::eval::Node {
  pub fn assert_structure(
    &mut self,
    expected: &mut impl Iterator<Item = NdKind>,
  ) -> Result<(), String> {
    let mut full_structure = vec![];
    let mut before = vec![];
    let mut after = vec![];
    let mut offender = None;

    self.walk_tree(&mut |s| {
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
