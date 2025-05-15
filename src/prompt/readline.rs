use std::{arch::asm, os::fd::BorrowedFd};

use nix::{libc::STDIN_FILENO, sys::termios::{self, Termios}, unistd::read};
use unicode_width::UnicodeWidthStr;

use crate::{libsh::error::ShResult, prelude::*};

#[derive(Clone,Copy,Debug)]
pub enum Key {
	Char(char),
	Enter,
	Backspace,
	Esc,
	Up,
	Down,
	Left,
	Right,
	Ctrl(char),
	Unknown,
}

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
	pub fn restore_termios(termios: Termios) {
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
}

impl Default for Terminal {
	fn default() -> Self {
		Self::new()
	}
}

#[derive(Default,Debug)]
pub struct FernReader {
	pub term: Terminal,
	pub prompt: String,
	pub line: LineBuf,
	pub editor: EditMode
}

impl FernReader {
	pub fn new(prompt: String) -> Self {
		Self {
			term: Terminal::new(),
			prompt,
			line: Default::default(),
			editor: Default::default()
		}
	}
	fn pack_line(&self) -> String {
		self.line
			.buffer
			.iter()
			.collect::<String>()
	}
	pub fn readline(&mut self) -> ShResult<String> {
		self.display_line(false);
		loop {
			let key = self.read_key().unwrap();
			self.process_key(key);
			self.display_line(true);
			if let Key::Enter = key {
				self.term.write_bytes(b"\r");
				break
			}
		}
		Ok(self.pack_line())
	}
	pub fn process_key(&mut self, key: Key) {
		match key {
			Key::Char(ch) => {
				self.line.insert_at_cursor(ch);
			}
			Key::Enter => {
				self.line.insert_at_cursor('\n');
			}
			Key::Backspace => self.line.backspace_at_cursor(),
			Key::Esc => todo!(),
			Key::Up => todo!(),
			Key::Down => todo!(),
			Key::Left => self.line.move_cursor_left(),
			Key::Right => self.line.move_cursor_right(),
			Key::Ctrl(ctrl) => todo!(),
			Key::Unknown => todo!(),
		}
	}
	fn clear_line(&self) {
		let prompt_lines = self.prompt.lines().count();
		let buf_lines = self.line.count_lines().saturating_sub(1); // One of the buffer's lines will overlap with the prompt
		let total = prompt_lines + buf_lines;
		self.term.write_bytes(b"\r\n");
		for _ in 0..total {
			self.term.write_bytes(b"\r\x1b[2K\x1b[1A");
		}
		self.term.write_bytes(b"\r\x1b[2K");
	}
	fn display_line(&self, refresh: bool) {
		if refresh {
			self.clear_line();
		}
		let mut prompt_lines = self.prompt.lines().peekable();
		let mut last_line_len = 0;
		while let Some(line) = prompt_lines.next() {
			if prompt_lines.peek().is_none() {
				last_line_len = strip_ansi_codes(line).width();
				self.term.write(line);
			} else {
				self.term.writeln(line);
			}
		}
		self.term.write(&self.pack_line());

		let cursor_offset = self.line.cursor + last_line_len;
		self.term.write_bytes(format!("\r\x1b[{}C", cursor_offset).as_bytes());
	}
	fn read_key(&mut self) -> Option<Key> {
		let mut buf = [0; 3];

		let n = self.term.read_byte(&mut buf);
		if n == 0 {
			return None;
		}
		match buf[0] {
			b'\x1b' => {
				if n == 3 {
					match (buf[1], buf[2]) {
						(b'[', b'A') => Some(Key::Up),
						(b'[', b'B') => Some(Key::Down),
						(b'[', b'C') => Some(Key::Right),
						(b'[', b'D') => Some(Key::Left),
						_ => Some(Key::Esc),
					}
				} else {
					Some(Key::Esc)
				}
			}
			b'\r' | b'\n' => Some(Key::Enter),
			0x7f => Some(Key::Backspace),
			c if (c as char).is_ascii_control() => {
				let ctrl = (c ^ 0x40) as char;
				Some(Key::Ctrl(ctrl))
			}
			c => Some(Key::Char(c as char))
		}
	}
}

#[derive(Default,Debug)]
pub enum EditMode {
	Normal,
	#[default]
	Insert,
}

#[derive(Default,Debug)]
pub struct LineBuf {
	buffer: Vec<char>,
	cursor: usize
}

impl LineBuf {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn count_lines(&self) -> usize {
		self.buffer.iter().filter(|&&c| c == '\n').count()
	}
	pub fn insert_at_cursor(&mut self, ch: char) {
		self.buffer.insert(self.cursor, ch);
		self.move_cursor_right();
	}
	pub fn backspace_at_cursor(&mut self) {
		if self.buffer.is_empty() {
			return
		}
		self.buffer.remove(self.cursor.saturating_sub(1));
		self.move_cursor_left();
	}
	pub fn move_cursor_left(&mut self) {
		self.cursor = self.cursor.saturating_sub(1);
	}
	pub fn move_cursor_right(&mut self) {
		self.cursor = self.cursor.saturating_add(1);
	}
}

pub fn strip_ansi_codes(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	let mut chars = s.chars().peekable();

	while let Some(c) = chars.next() {
		if c == '\x1b' && chars.peek() == Some(&'[') {
			// Skip over the escape sequence
			chars.next(); // consume '['
			while let Some(&ch) = chars.peek() {
				if ch.is_ascii_lowercase() || ch.is_ascii_uppercase() {
					chars.next(); // consume final letter
					break;
				}
				chars.next(); // consume intermediate characters
			}
		} else {
			out.push(c);
		}
	}
	out
}
