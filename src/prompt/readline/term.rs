use std::os::fd::{BorrowedFd, RawFd};
use nix::{libc::STDIN_FILENO, sys::termios, unistd::{isatty, read, write}};

use super::keys::{KeyCode, KeyEvent, ModKeys};

#[derive(Debug)]
pub struct Terminal {
	stdin: RawFd,
	stdout: RawFd,
}

impl Terminal {
	pub fn new() -> Self {
		assert!(isatty(STDIN_FILENO).unwrap());
		Self {
			stdin: STDIN_FILENO,
			stdout: 1,
		}
	}

	fn raw_mode() -> termios::Termios {
		let orig = termios::tcgetattr(unsafe{BorrowedFd::borrow_raw(STDIN_FILENO)}).expect("Failed to get terminal attributes");
		let mut raw = orig.clone();
		termios::cfmakeraw(&mut raw);
		termios::tcsetattr(unsafe{BorrowedFd::borrow_raw(STDIN_FILENO)}, termios::SetArg::TCSANOW, &raw)
			.expect("Failed to set terminal to raw mode");
		orig
	}

	pub fn restore_termios(termios: termios::Termios) {
		termios::tcsetattr(unsafe{BorrowedFd::borrow_raw(STDIN_FILENO)}, termios::SetArg::TCSANOW, &termios)
			.expect("Failed to restore terminal settings");
	}

	pub fn with_raw_mode<F: FnOnce() -> R, R>(func: F) -> R {
		let saved = Self::raw_mode();
		let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(func));
		Self::restore_termios(saved);
		match result {
			Ok(r) => r,
			Err(e) => std::panic::resume_unwind(e),
		}
	}

	pub fn read_byte(&self, buf: &mut [u8]) -> usize {
		Self::with_raw_mode(|| {
			read(self.stdin, buf).expect("Failed to read from stdin")
		})
	}

	pub fn write_bytes(&self, buf: &[u8]) {
		Self::with_raw_mode(|| {
			write(unsafe{BorrowedFd::borrow_raw(self.stdout)}, buf).expect("Failed to write to stdout");
		});
	}


	pub fn write(&self, s: &str) {
		self.write_bytes(s.as_bytes());
	}

	pub fn writeln(&self, s: &str) {
		self.write(s);
		self.write_bytes(b"\r\n");
	}

	pub fn clear(&self) {
		self.write_bytes(b"\x1b[2J\x1b[H");
	}

	pub fn read_key(&self) -> KeyEvent {
		let mut buf = [0;8];
		let n = self.read_byte(&mut buf);

		if buf[0] == 0x1b {
			if n >= 3 && buf[1] == b'[' {
				return match buf[2] {
					b'A' => KeyEvent(KeyCode::Up, ModKeys::empty()),
					b'B' => KeyEvent(KeyCode::Down, ModKeys::empty()),
					b'C' => KeyEvent(KeyCode::Right, ModKeys::empty()),
					b'D' => KeyEvent(KeyCode::Left, ModKeys::empty()),
					_ => KeyEvent(KeyCode::Esc, ModKeys::empty()),
				};
			}
			return KeyEvent(KeyCode::Esc, ModKeys::empty());
		}

		if let Ok(s) = core::str::from_utf8(&buf[..n]) {
			if let Some(ch) = s.chars().next() {
				return KeyEvent::new(ch, ModKeys::NONE);
			}
		}
		KeyEvent(KeyCode::Null, ModKeys::empty())
	}
}

impl Default for Terminal {
	fn default() -> Self {
		Self::new()
	}
}
