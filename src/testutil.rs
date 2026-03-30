use std::{
  collections::{HashMap, HashSet},
  env,
  os::fd::{AsRawFd, BorrowedFd, OwnedFd},
  path::PathBuf,
  sync::{self, Arc, MutexGuard},
};

use nix::{
  fcntl::{FcntlArg, OFlag, fcntl},
  pty::openpty,
  sys::termios::{OutputFlags, SetArg, tcgetattr, tcsetattr},
  unistd::read,
};

use crate::{
  expand::expand_aliases,
  libsh::error::ShResult,
  parse::{ParsedSrc, Redir, RedirType, execute::exec_input, lex::LexFlags},
  procio::{IoFrame, IoMode, RedirGuard},
  readline::register::{restore_registers, save_registers},
  state::{MetaTab, SHED, read_logic},
};

static TEST_MUTEX: sync::Mutex<()> = sync::Mutex::new(());

pub fn has_cmds(cmds: &[&str]) -> bool {
  let path_cmds = MetaTab::get_cmds_in_path();
  path_cmds.iter().all(|c| cmds.iter().any(|&cmd| c == cmd))
}

pub fn has_cmd(cmd: &str) -> bool {
  MetaTab::get_cmds_in_path().into_iter().any(|c| c == cmd)
}

pub fn test_input(input: impl Into<String>) -> ShResult<()> {
  exec_input(input.into(), None, false, None)
}

pub struct TestGuard {
  _redir_guard: RedirGuard,
  old_cwd: PathBuf,
  saved_env: HashMap<String, String>,
  pty_master: OwnedFd,
  pty_slave: OwnedFd,

  cleanups: Vec<Box<dyn FnOnce()>>,
}

impl TestGuard {
  pub fn new() -> Self {
    let pty = openpty(None, None).unwrap();
    let (pty_master, pty_slave) = (pty.master, pty.slave);
    let mut attrs = tcgetattr(&pty_slave).unwrap();
    attrs.output_flags &= !OutputFlags::ONLCR;
    tcsetattr(&pty_slave, SetArg::TCSANOW, &attrs).unwrap();

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
    SHED.with(|s| s.save());
    save_registers();
    Self {
      _redir_guard,
      old_cwd,
      saved_env,
      pty_master,
      pty_slave,
      cleanups: vec![],
    }
  }

  pub fn pty_slave(&self) -> BorrowedFd<'_> {
    unsafe { BorrowedFd::borrow_raw(self.pty_slave.as_raw_fd()) }
  }

  pub fn add_cleanup(&mut self, f: impl FnOnce() + 'static) {
    self.cleanups.push(Box::new(f));
  }

  pub fn read_output(&self) -> String {
    let flags = fcntl(self.pty_master.as_raw_fd(), FcntlArg::F_GETFL).unwrap();
    let flags = OFlag::from_bits_truncate(flags);
    fcntl(
      self.pty_master.as_raw_fd(),
      FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK),
    )
    .unwrap();

    let mut out = vec![];
    let mut buf = [0; 4096];
    loop {
      match read(self.pty_master.as_raw_fd(), &mut buf) {
        Ok(0) => break,
        Ok(n) => out.extend_from_slice(&buf[..n]),
        Err(_) => break,
      }
    }

    fcntl(self.pty_master.as_raw_fd(), FcntlArg::F_SETFL(flags)).unwrap();

    String::from_utf8_lossy(&out).to_string()
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
    SHED.with(|s| s.restore());
    restore_registers();
  }
}

pub fn get_ast(input: &str) -> ShResult<Vec<crate::parse::Node>> {
  let log_tab = read_logic(|l| l.clone());
  let input = expand_aliases(input.into(), HashSet::new(), &log_tab);

  let source_name = "test_input".to_string();
  let mut parser = ParsedSrc::new(Arc::new(input))
    .with_lex_flags(LexFlags::empty())
    .with_name(source_name.clone());

  parser
    .parse_src()
    .map_err(|e| e.into_iter().next().unwrap())?;

  Ok(parser.extract_nodes())
}

impl crate::parse::Node {
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
      let expected_rule = expected_rule.map_or("(none — expected array too short)".into(), |e| {
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

#[derive(Clone, Debug, PartialEq)]
pub enum NdKind {
  IfNode,
  LoopNode,
  ForNode,
  CaseNode,
  Command,
  Pipeline,
  Conjunction,
  Assignment,
  BraceGrp,
  Negate,
  Test,
  FuncDef,
}

impl crate::parse::NdRule {
  pub fn as_nd_kind(&self) -> NdKind {
    match self {
      Self::Negate { .. } => NdKind::Negate,
      Self::IfNode { .. } => NdKind::IfNode,
      Self::LoopNode { .. } => NdKind::LoopNode,
      Self::ForNode { .. } => NdKind::ForNode,
      Self::CaseNode { .. } => NdKind::CaseNode,
      Self::Command { .. } => NdKind::Command,
      Self::Pipeline { .. } => NdKind::Pipeline,
      Self::Conjunction { .. } => NdKind::Conjunction,
      Self::Assignment { .. } => NdKind::Assignment,
      Self::BraceGrp { .. } => NdKind::BraceGrp,
      Self::Test { .. } => NdKind::Test,
      Self::FuncDef { .. } => NdKind::FuncDef,
    }
  }
}
