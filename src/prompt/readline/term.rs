use std::{os::fd::{BorrowedFd, RawFd}, thread::sleep, time::{Duration, Instant}};
use nix::{errno::Errno, fcntl::{fcntl, FcntlArg, OFlag}, libc::{self, STDIN_FILENO}, sys::termios, unistd::{isatty, read, write}};
use nix::libc::{winsize, TIOCGWINSZ};
use unicode_width::UnicodeWidthChar;
use std::mem::zeroed;
use std::io;

use crate::libsh::error::ShResult;
use crate::prelude::*;

use super::keys::{KeyCode, KeyEvent, ModKeys};

#[derive(Default,Debug)]
struct WriteMap {
	lines: usize,
	cols: usize,
	offset: usize
}

#[derive(Debug)]
pub struct Terminal {
	stdin: RawFd,
	stdout: RawFd,
	recording: bool,
	write_records: WriteMap,
	cursor_records: WriteMap
}

impl Terminal {
	pub fn new() -> Self {
		assert!(isatty(STDIN_FILENO).unwrap());
		Self {
			stdin: STDIN_FILENO,
			stdout: 1,
			recording: false,
			// Records for buffer writes
			// Used to find the start of the buffer
			write_records: WriteMap::default(),
			// Records for cursor movements after writes
			// Used to find the end of the buffer
			cursor_records: WriteMap::default(),
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


	pub fn get_dimensions(&self) -> ShResult<(usize, usize)> {
		if !isatty(self.stdin).unwrap_or(false) {
			return Err(io::Error::new(io::ErrorKind::Other, "Not a TTY"))?;
		}

		let mut ws: winsize = unsafe { zeroed() };

		let res = unsafe { libc::ioctl(self.stdin, TIOCGWINSZ, &mut ws) };
		if res == -1 {
			return Err(io::Error::last_os_error())?;
		}

		Ok((ws.ws_row as usize, ws.ws_col as usize))
	}

	pub fn start_recording(&mut self, offset: usize) {
		self.recording = true;
		self.write_records.offset = offset;
	}

	pub fn stop_recording(&mut self) {
		self.recording = false;
	}

	pub fn save_cursor_pos(&mut self) {
		self.write("\x1b[s")
	}

	pub fn restore_cursor_pos(&mut self) {
		self.write("\x1b[u")
	}

	pub fn move_cursor_to(&mut self, (row,col): (usize,usize)) {
		self.write(&format!("\x1b[{row};{col}H",))
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

	fn read_blocks_then_read(&self, buf: &mut [u8], timeout: Duration) -> Option<usize> {
		Self::with_raw_mode(|| {
			self.read_blocks(false);
			let start = Instant::now();
			loop {
				match read(self.stdin, buf) {
					Ok(n) if n > 0 => {
						self.read_blocks(true);
						return Some(n);
					}
					Ok(_) => {}
					Err(e) if e == Errno::EAGAIN => {}
					Err(_) => return None,
				}
				if start.elapsed() > timeout {
					self.read_blocks(true);
					return None;
				}
				sleep(Duration::from_millis(1));
			}
		})
	}

	/// Same as read_byte(), only non-blocking with a very short timeout
	pub fn peek_byte(&self, buf: &mut [u8]) -> usize {
		const TIMEOUT_DUR: Duration = Duration::from_millis(50);
		Self::with_raw_mode(|| {
			self.read_blocks(false);

			let start = Instant::now();
			loop {
				match read(self.stdin, buf) {
					Ok(n) if n > 0 => {
						self.read_blocks(true);
						return n
					}
					Ok(_) => {}
					Err(Errno::EAGAIN) => {}
					Err(e) => panic!("nonblocking read failed: {e}")
				}

				if start.elapsed() >= TIMEOUT_DUR {
					self.read_blocks(true);
					return 0
				}

				sleep(Duration::from_millis(1));
			}
		})
	}

	pub fn read_blocks(&self, yn: bool) {
		let flags = OFlag::from_bits_truncate(fcntl(self.stdin, FcntlArg::F_GETFL).unwrap());
		let new_flags = if !yn {
			flags | OFlag::O_NONBLOCK
		} else {
			flags & !OFlag::O_NONBLOCK
		};
		fcntl(self.stdin, FcntlArg::F_SETFL(new_flags)).unwrap();
	}

	pub fn reset_records(&mut self) {
		self.write_records = Default::default();
		self.cursor_records = Default::default();
	}

	pub fn recorded_write(&mut self, buf: &str, offset: usize) -> ShResult<()> {
		self.start_recording(offset);
		self.write(buf);
		self.stop_recording();
		Ok(())
	}

	/// Rewinds terminal writing, clears lines and lands on the anchor point of the prompt
	pub fn unwrite(&mut self) -> ShResult<()> {
		self.unposition_cursor()?;
		let WriteMap { lines, cols, offset } = self.write_records;
		for _ in 0..lines {
			self.write_unrecorded("\x1b[2K\x1b[A")
		}
		let col = offset;
		self.write_unrecorded(&format!("\x1b[{col}G\x1b[0K"));
		self.reset_records();
		Ok(())
	}

	pub fn position_cursor(&mut self, (lines,col): (usize,usize)) -> ShResult<()> {
		flog!(DEBUG,lines);
		flog!(DEBUG,col);
		self.cursor_records.lines = lines;
		self.cursor_records.cols = col;
		self.cursor_records.offset = self.cursor_pos().1;

		for _ in 0..lines {
			self.write_unrecorded("\x1b[A")
		}

		let (_, width) = self.get_dimensions().unwrap();
		// holy hack spongebob
		// basically if we've written to the edge of the terminal
		// and the cursor is at term_width + 1 (column 1 on the next line)
		// then we are going to manually write a newline
		// to position the cursor correctly
		if self.write_records.cols == width && self.cursor_records.cols == 1 {
			self.cursor_records.lines += 1;
			self.write_records.lines += 1;
			self.cursor_records.cols = 1;
			self.write_records.cols = 1;
			write(unsafe { BorrowedFd::borrow_raw(self.stdout) }, b"\n").expect("Failed to write to stdout");
		}

		self.write_unrecorded(&format!("\x1b[{col}G"));

		Ok(())
	}

	/// Rewinds cursor positioning, lands on the end of the buffer
	pub fn unposition_cursor(&mut self) ->ShResult<()> {
		let WriteMap { lines, cols, offset } = self.cursor_records;

		for _ in 0..lines {
			self.write_unrecorded("\x1b[B")
		}

		self.write_unrecorded(&format!("\x1b[{offset}G"));

		Ok(())
	}

	pub fn write_bytes(&mut self, buf: &[u8], record: bool) {
		if self.recording && record { // The function parameter allows us to make sneaky writes while the terminal is recording
			let (_, width) = self.get_dimensions().unwrap();
			let mut bytes = buf.iter().map(|&b| b as char).peekable();
			while let Some(ch) = bytes.next() {
				match ch {
					'\n' => {
						self.write_records.lines += 1;
						self.write_records.cols = 0;
					}
					'\r' => {
						self.write_records.cols = 0;
					}
					// Consume escape sequences
					'\x1b' if bytes.peek() == Some(&'[') => {
						bytes.next();
						while let Some(&ch) = bytes.peek() {
							if ch.is_ascii_alphabetic() {
								bytes.next();
								break
							} else {
								bytes.next();
							}
						}
					}
					'\t' => {
						let tab_size = 8;
						let next_tab = tab_size - (self.write_records.cols % tab_size);
						self.write_records.cols += next_tab;
						if self.write_records.cols > width {
							self.write_records.lines += 1;
							self.write_records.cols = 0;
						}
					}
					_ if ch.is_control() => {
						// ignore control characters for visual width
					}
					_ => {
						let ch_width = ch.width().unwrap_or(0);
						if self.write_records.cols + ch_width > width {
							flog!(DEBUG,ch_width,self.write_records.cols,width,self.write_records.lines);
							self.write_records.lines += 1;
							self.write_records.cols = ch_width;
						}
						self.write_records.cols += ch_width;
					}
				}
			}
			flog!(DEBUG,self.write_records.cols);
		}
		write(unsafe { BorrowedFd::borrow_raw(self.stdout) }, buf).expect("Failed to write to stdout");
	}


	pub fn write(&mut self, s: &str) {
		self.write_bytes(s.as_bytes(), true);
	}

	pub fn write_unrecorded(&mut self, s: &str) {
		self.write_bytes(s.as_bytes(), false);
	}

	pub fn writeln(&mut self, s: &str) {
		self.write(s);
		self.write_bytes(b"\n", true);
	}

	pub fn clear(&mut self) {
		self.write_bytes(b"\x1b[2J\x1b[H", false);
	}

	pub fn read_key(&self) -> KeyEvent {
		use core::str;

		let mut buf = [0u8; 8];
		let mut collected = Vec::with_capacity(5);

		loop {
			let n = self.read_byte(&mut buf[..1]); // Read one byte at a time
			if n == 0 {
				continue;
			}
			collected.push(buf[0]);

			// ESC sequences
			if collected[0] == 0x1b && collected.len() == 1 {
				if let Some(code) = self.parse_esc_seq(&mut buf) {
					return code
				}
			}

			// Try parse valid UTF-8 from collected bytes
			if let Ok(s) = str::from_utf8(&collected) {
				return KeyEvent::new(s, ModKeys::empty());
			}

			// If it's not valid UTF-8 yet, loop to collect more bytes
			if collected.len() >= 4 {
				// UTF-8 max char length is 4; if it's still invalid, give up
				break;
			}
		}

		KeyEvent(KeyCode::Null, ModKeys::empty())
	}

	pub fn parse_esc_seq(&self, buf: &mut [u8]) -> Option<KeyEvent> {
		let mut collected = vec![0x1b];

    // Peek next byte
    let _ = self.peek_byte(&mut buf[..1]);
    let b1 = buf[0];
    collected.push(b1);

    match b1 {
        b'[' => {
            // Next byte(s) determine the sequence
            let _ = self.peek_byte(&mut buf[..1]);
            let b2 = buf[0];
            collected.push(b2);

            match b2 {
                b'A' => Some(KeyEvent(KeyCode::Up, ModKeys::empty())),
                b'B' => Some(KeyEvent(KeyCode::Down, ModKeys::empty())),
                b'C' => Some(KeyEvent(KeyCode::Right, ModKeys::empty())),
                b'D' => Some(KeyEvent(KeyCode::Left, ModKeys::empty())),
                b'1'..=b'9' => {
                    // Might be Delete/Home/etc
                    let mut digits = vec![b2];

                    // Keep reading until we hit `~` or `;` (modifiers)
                    loop {
                        let _ = self.peek_byte(&mut buf[..1]);
                        let b = buf[0];
                        collected.push(b);

                        if b == b'~' {
                            break;
                        } else if b == b';' {
                            // modifier-aware sequence, like `ESC [ 1 ; 5 ~`
                            // You may want to parse the full thing
                            break;
                        } else if !b.is_ascii_digit() {
                            break;
                        } else {
                            digits.push(b);
                        }
                    }

                    let key = match digits.as_slice() {
                        [b'1'] => KeyCode::Home,
                        [b'3'] => KeyCode::Delete,
                        [b'4'] => KeyCode::End,
                        [b'5'] => KeyCode::PageUp,
                        [b'6'] => KeyCode::PageDown,
                        [b'7'] => KeyCode::Home, // xterm alternate
                        [b'8'] => KeyCode::End,  // xterm alternate

												// Function keys
												[b'1',b'5'] => KeyCode::F(5),
												[b'1',b'7'] => KeyCode::F(6),
												[b'1',b'8'] => KeyCode::F(7),
												[b'1',b'9'] => KeyCode::F(8),
												[b'2',b'0'] => KeyCode::F(9),
												[b'2',b'1'] => KeyCode::F(10),
												[b'2',b'3'] => KeyCode::F(11),
												[b'2',b'4'] => KeyCode::F(12),
                        _ => KeyCode::Esc,
                    };

                    Some(KeyEvent(key, ModKeys::empty()))
                }
                _ => Some(KeyEvent(KeyCode::Esc, ModKeys::empty())),
            }
        }
        b'O' => {
            let _ = self.peek_byte(&mut buf[..1]);
            let b2 = buf[0];
            collected.push(b2);

            let key = match b2 {
                b'P' => KeyCode::F(1),
                b'Q' => KeyCode::F(2),
                b'R' => KeyCode::F(3),
                b'S' => KeyCode::F(4),
                _ => KeyCode::Esc,
            };

            Some(KeyEvent(key, ModKeys::empty()))
        }
        _ => Some(KeyEvent(KeyCode::Esc, ModKeys::empty())),
    }
	}

	pub fn cursor_pos(&mut self) -> (usize, usize) {
		self.write_unrecorded("\x1b[6n");
		let mut buf = [0u8;32];
		let n = self.read_byte(&mut buf);


		let response = std::str::from_utf8(&buf[..n]).unwrap_or("");
		let mut row = 0;
		let mut col = 0;
		if let Some(caps) = response.strip_prefix("\x1b[").and_then(|s| s.strip_suffix("R")) {
			let mut parts = caps.split(';');
			if let (Some(rowstr), Some(colstr)) = (parts.next(), parts.next()) {
				row = rowstr.parse().unwrap_or(1);
				col = colstr.parse().unwrap_or(1);
			}
		}
		(row,col)
	}
}

impl Default for Terminal {
	fn default() -> Self {
		Self::new()
	}
}
