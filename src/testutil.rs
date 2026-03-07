use std::{
	collections::HashMap,
	env,
	os::fd::{AsRawFd, OwnedFd},
	path::PathBuf,
	sync::{self, MutexGuard},
};

use nix::{
	fcntl::{FcntlArg, OFlag, fcntl},
	pty::openpty,
	sys::termios::{OutputFlags, SetArg, tcgetattr, tcsetattr},
	unistd::read,
};

use crate::{
	libsh::error::ShResult,
	parse::{Redir, RedirType, execute::exec_input},
	procio::{IoFrame, IoMode, RedirGuard},
	state::{MetaTab, SHED},
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
	exec_input(input.into(), None, true, None)
}

pub struct TestGuard {
	_lock: MutexGuard<'static, ()>,
	_redir_guard: RedirGuard,
	old_cwd: PathBuf,
	saved_env: HashMap<String, String>,
	pty_master: OwnedFd,
	pty_slave: OwnedFd,

	cleanups: Vec<Box<dyn FnOnce()>>
}

impl TestGuard {
	pub fn new() -> Self {
		let _lock = TEST_MUTEX.lock().unwrap();

		let pty = openpty(None, None).unwrap();
		let (pty_master,pty_slave) = (pty.master, pty.slave);
		let mut attrs = tcgetattr(&pty_slave).unwrap();
		attrs.output_flags &= !OutputFlags::ONLCR;
		tcsetattr(&pty_slave, SetArg::TCSANOW, &attrs).unwrap();

		let mut frame = IoFrame::new();
		frame.push(
			Redir::new(
				IoMode::Fd {
					tgt_fd: 0,
					src_fd: pty_slave.as_raw_fd(),
				},
				RedirType::Input,
			),
		);
		frame.push(
			Redir::new(
				IoMode::Fd {
					tgt_fd: 1,
					src_fd: pty_slave.as_raw_fd(),
				},
				RedirType::Output,
			),
		);
		frame.push(
			Redir::new(
				IoMode::Fd {
					tgt_fd: 2,
					src_fd: pty_slave.as_raw_fd(),
				},
				RedirType::Output,
			),
		);

		let _redir_guard = frame.redirect().unwrap();

		let old_cwd = env::current_dir().unwrap();
		let saved_env = env::vars().collect();
		SHED.with(|s| s.save());
		Self {
			_lock,
			_redir_guard,
			old_cwd,
			saved_env,
			pty_master,
			pty_slave,
			cleanups: vec![],
		}
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
		).unwrap();

		let mut out = vec![];
		let mut buf = [0;4096];
		loop {
			match read(self.pty_master.as_raw_fd(), &mut buf) {
				Ok(0) => break,
				Ok(n) => out.extend_from_slice(&buf[..n]),
				Err(_) => break,
			}
		}

		fcntl(
			self.pty_master.as_raw_fd(),
			FcntlArg::F_SETFL(flags),
		).unwrap();

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
			unsafe { env::remove_var(&k); }
		}
		for (k, v) in &self.saved_env {
			unsafe { env::set_var(k, v); }
		}
		for cleanup in self.cleanups.drain(..).rev() {
			cleanup();
		}
		SHED.with(|s| s.restore());
	}
}
