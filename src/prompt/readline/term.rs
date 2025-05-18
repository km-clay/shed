use std::{arch::asm, os::fd::{BorrowedFd, RawFd}};

use nix::{libc::STDIN_FILENO, sys::termios, unistd::isatty};


#[derive(Debug)]
pub struct Terminal {
	stdin: RawFd,
	stdout: RawFd,
}

impl Terminal {
	pub fn new() -> Self {
		assert!(isatty(0).unwrap());
		Self {
			stdin: 0,
			stdout: 1,
		}
	}
	fn raw_mode() -> termios::Termios {
    // Get the current terminal attributes
    let orig_termios = unsafe { termios::tcgetattr(BorrowedFd::borrow_raw(STDIN_FILENO)).expect("Failed to get terminal attributes") };

    // Make a mutable copy
    let mut raw = orig_termios.clone();

    // Apply raw mode flags
    termios::cfmakeraw(&mut raw);

    // Set the attributes immediately
    unsafe { termios::tcsetattr(BorrowedFd::borrow_raw(STDIN_FILENO), termios::SetArg::TCSANOW, &raw) }
        .expect("Failed to set terminal to raw mode");

    // Return original attributes so they can be restored later
    orig_termios
	}
	pub fn restore_termios(termios: termios::Termios) {
    unsafe { termios::tcsetattr(BorrowedFd::borrow_raw(STDIN_FILENO), termios::SetArg::TCSANOW, &termios) }
        .expect("Failed to restore terminal settings");
	}
	pub fn with_raw_mode<F: FnOnce() -> R,R>(func: F) -> R {
		let saved = Self::raw_mode();
		let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(func));
		Self::restore_termios(saved);

		match result {
			Ok(r) => r,
			Err(e) => std::panic::resume_unwind(e)
		}
	}
	pub fn read_byte(&self, buf: &mut [u8]) -> usize {
		Self::with_raw_mode(|| {
			let ret: usize;
			unsafe {
				let buf_ptr = buf.as_mut_ptr();
				let len = buf.len();
				asm! (
					"syscall",
					in("rax") 0,
					in("rdi") self.stdin,
					in("rsi") buf_ptr,
					in("rdx") len,
					lateout("rax") ret,
					out("rcx") _,
					out("r11") _,
				);
			}
			ret
		})
	}
	pub fn write_bytes(&self, buf: &[u8]) {
		Self::with_raw_mode(|| {
			let _ret: usize;
			unsafe {
				let buf_ptr = buf.as_ptr();
				let len = buf.len();
				asm!(
					"syscall",
					in("rax") 1,             
					in("rdi") self.stdout,   
					in("rsi") buf_ptr,       
					in("rdx") len,           
					lateout("rax") _ret,     
					out("rcx") _,
					out("r11") _,
				);
			}
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
}

impl Default for Terminal {
	fn default() -> Self {
		Self::new()
	}
}
