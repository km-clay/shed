use std::{
  collections::HashMap,
  env,
  os::fd::{AsRawFd, BorrowedFd, OwnedFd},
  path::PathBuf,
  sync::{Arc, Mutex},
  thread::JoinHandle,
};

use nix::{
  pty::openpty,
  sys::termios::{OutputFlags, SetArg, tcgetattr, tcsetattr},
  unistd::read,
};

#[macro_export]
macro_rules! assert_status_eq {
  ($expected_status:expr) => {
    {
      assert_eq!(state::get_status(), $expected_status);
    }

  };
  ($expected_status:expr, $($args:tt)+) => {
    {
      assert_eq!(state::get_status(), $expected_status, $($args)+);
    }
  }
}

#[macro_export]
macro_rules! assert_status_ne {
  ($expected_status:expr) => {
    {
      assert_ne!(state::get_status(), $expected_status);
    }

  };
  ($expected_status:expr, $($args:tt)+) => {
    {
      assert_ne!(state::get_status(), $expected_status, $($args)+);
    }
  }
}

use crate::{
  expand::expand_aliases,
  parse::{NdKind, ParsedSrc, Redir, RedirType, execute::exec_nonint, lex::LexFlags},
  procio::{IoFrame, IoMode, RedirGuard, borrow_fd},
  readline::register::{restore_registers, save_registers},
  state::{self, MetaTab, with_term},
  util::error::ShResult,
};

pub fn has_cmds(cmds: &[&str]) -> bool {
  let path_cmds = MetaTab::get_cmds_in_path();
  path_cmds
    .iter()
    .all(|c| cmds.iter().any(|&cmd| c.name() == cmd))
}

pub fn has_cmd(cmd: &str) -> bool {
  MetaTab::get_cmds_in_path()
    .into_iter()
    .any(|c| c.name() == cmd)
}

pub fn test_input(input: impl Into<String>) -> ShResult<()> {
  exec_nonint(input.into(), None, None)
}

pub struct TestGuard {
  _redir_guard: RedirGuard,
  old_cwd: PathBuf,
  saved_env: HashMap<String, String>,

  _pty_master: OwnedFd,
  pty_slave: OwnedFd,
  output: Arc<Mutex<Vec<u8>>>,
  _read_handle: JoinHandle<()>,

  cleanups: Vec<Box<dyn FnOnce()>>,
}

impl TestGuard {
  pub fn new() -> Self {
    let pty = openpty(None, None).unwrap();
    let (pty_master, pty_slave) = (pty.master, pty.slave);
    let mut attrs = tcgetattr(&pty_slave).unwrap();
    attrs.output_flags &= !OutputFlags::ONLCR;
    tcsetattr(&pty_slave, SetArg::TCSANOW, &attrs).unwrap();
    let master_raw = pty_master.as_raw_fd();
    with_term(|t| t.set_fd_for_testing(Some(pty_slave.as_raw_fd())));

    // we need this arc mutex and read handle because large test outputs
    // will cause the test to hang if we try to do everything on one thread.
    // if we attempt to do this synchronously, we have to do both the reading and the writing.
    // we can't read if we're blocked on writing to a full pty buffer.
    let output = Arc::new(Mutex::new(vec![]));
    let output_clone = Arc::clone(&output);
    let _read_handle = std::thread::spawn(move || {
      let mut buf = [0u8; 4096];
      loop {
        match read(master_raw, &mut buf) {
          Ok(0) => break,
          Ok(n) => output_clone.lock().unwrap().extend_from_slice(&buf[..n]),
          Err(_) => break,
        }
      }
    });

    let mut frame = IoFrame::new();
    frame.push(Redir::new(
      IoMode::Fd {
        tgt_fd: 0,
        src_fd: pty_slave.as_raw_fd(),
      },
      RedirType::Input,
    ));
    frame.push(Redir::new(
      IoMode::Fd {
        tgt_fd: 1,
        src_fd: pty_slave.as_raw_fd(),
      },
      RedirType::Output,
    ));
    frame.push(Redir::new(
      IoMode::Fd {
        tgt_fd: 2,
        src_fd: pty_slave.as_raw_fd(),
      },
      RedirType::Output,
    ));

    let _redir_guard = frame.redirect().unwrap();

    let old_cwd = env::current_dir().unwrap();
    let saved_env = env::vars().collect();
    state::util::save_state();
    state::try_hash();
    save_registers();
    Self {
      _redir_guard,
      old_cwd,
      saved_env,
      _pty_master: pty_master,
      pty_slave,

      output,
      _read_handle,

      cleanups: vec![],
    }
  }

  pub fn pty_slave(&self) -> BorrowedFd<'_> {
    borrow_fd(self.pty_slave.as_raw_fd())
  }

  pub fn add_cleanup(&mut self, f: impl FnOnce() + 'static) {
    self.cleanups.push(Box::new(f));
  }

  pub fn read_output(&self) -> String {
    loop {
      // wait a little bit for read thread to do its read
      std::thread::sleep(std::time::Duration::from_millis(5));
      let buf = self.output.lock().unwrap();
      // check current length of output buffer
      let snapshot_len = buf.len();
      drop(buf);
      // wait a little bit more
      std::thread::sleep(std::time::Duration::from_millis(5));
      let mut buf = self.output.lock().unwrap();
      if buf.len() == snapshot_len {
        // no new output came in during the second sleep, assume we're done
        let result = String::from_utf8_lossy(&buf).to_string();
        buf.clear();
        return result;
      }
      // more data came in, loop again
    }
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
    state::util::restore_state();
    restore_registers();
  }
}

pub fn get_ast(input: &str) -> ShResult<Vec<crate::parse::Node>> {
  let input = expand_aliases(input.into());

  let mut parser = ParsedSrc::new(input.into())
    .with_lex_flags(LexFlags::empty())
    .with_name("test_input".into());

  parser
    .parse_src()
    .map_err(|e| e.into_iter().next().unwrap())?;

  Ok(parser.extract_nodes())
}

impl crate::parse::Node {
  pub fn count_noderules(&mut self, kind: NdKind) -> usize {
    let mut count = 0;
    self.walk_tree(&mut |s| {
      if s.class.as_nd_kind() == kind {
        count += 1;
      }
    });
    count
  }
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
        format!(
          "Expected node type '{:?}', found '{:?}'",
          expected_rule, nd_kind
        ),
        format!("Before offender: {:?}", before),
        format!("After offender: {:?}\n", after),
        format!("hint: here is the full structure as an array\n {full_structure_hint}"),
      ]
      .join("\n");

      Err(output)
    } else {
      Ok(())
    }
  }
}
