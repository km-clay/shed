use std::{env, fmt::Write, io::{BufRead, BufReader, Read}, ops::{Deref, DerefMut}, os::fd::{AsFd, BorrowedFd, RawFd}};

use nix::{errno::Errno, libc, poll::{self, PollFlags, PollTimeout}, unistd::isatty};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::libsh::error::{ShErr, ShErrKind, ShResult};

use super::linebuf::LineBuf;

pub type Row = u16;
pub type Col = u16;

#[derive(Default,Clone,Copy,PartialEq,Eq,PartialOrd,Ord,Debug)]
pub struct Pos {
	col: Col,
	row: Row
}

// I'd like to thank rustyline for this idea
nix::ioctl_read_bad!(win_size, libc::TIOCGWINSZ, libc::winsize);

pub fn get_win_size(fd: RawFd) -> (Col,Row) {
	use std::mem::zeroed;

	if cfg!(test) {
		return (80,24)
	}

	unsafe {
		let mut size: libc::winsize = zeroed();
		match win_size(fd, &mut size) {
			Ok(0) => {
				/* rustyline code says:
				 In linux pseudo-terminals are created with dimensions of
				 zero. If host application didn't initialize the correct
				 size before start we treat zero size as 80 columns and
				 infinite rows
				*/
				let cols = if size.ws_col == 0 { 80 } else { size.ws_col };
				let rows = if size.ws_row == 0 {
					u16::MAX
				} else {
					size.ws_row
				};
				(cols.into(), rows.into())
			}
			_ => (80,24)
		}
	}
}

fn write_all(fd: RawFd, buf: &str) -> nix::Result<()> {
	let mut bytes = buf.as_bytes();
	while !bytes.is_empty() {
		match nix::unistd::write(unsafe { BorrowedFd::borrow_raw(fd) }, bytes) {
			Ok(0) => return Err(Errno::EIO),
			Ok(n) => bytes = &bytes[n..],
			Err(Errno::EINTR) => {}
			Err(r) => return Err(r),
		}
	}
	Ok(())
}

// Big credit to rustyline for this
fn width(s: &str, esc_seq: &mut u8) -> u16 {
	let w_calc = width_calculator();
	if *esc_seq == 1 {
		if s == "[" {
			// CSI
			*esc_seq = 2;
		} else {
			// two-character sequence
			*esc_seq = 0;
		}
		0
	} else if *esc_seq == 2 {
		if s == ";" || (s.as_bytes()[0] >= b'0' && s.as_bytes()[0] <= b'9') {
			/*} else if s == "m" {
			// last
			 *esc_seq = 0;*/
	} else {
		// not supported
		*esc_seq = 0;
	}
	0
	} else if s == "\x1b" {
		*esc_seq = 1;
		0
	} else if s == "\n" {
		0
	} else {
		w_calc.width(s) as u16
	}
}

pub fn width_calculator() -> Box<dyn WidthCalculator> {
	match env::var("TERM_PROGRAM").as_deref() {
		Ok("Apple_Terminal") => Box::new(UnicodeWidth),
		Ok("iTerm.app") => Box::new(UnicodeWidth),
		Ok("WezTerm") => Box::new(UnicodeWidth),
		Err(std::env::VarError::NotPresent) => match std::env::var("TERM").as_deref() {
			Ok("xterm-kitty") => Box::new(NoZwj),
			_ => Box::new(WcWidth)
		},
		_ => Box::new(WcWidth)
	}
}

fn read_digits_until(rdr: &mut TermReader, sep: char) -> ShResult<Option<u32>> {
	let mut num: u32 = 0;
	loop {
		match rdr.next_byte()? as char {
			digit @ '0'..='9' => {
				let digit = digit.to_digit(10).unwrap();
				num = append_digit(num, digit);
				continue;
			}
			c if c == sep => break,
			_ => return Ok(None),
		}
	}
	Ok(Some(num))
}

pub fn append_digit(left: u32, right: u32) -> u32 {
	left.saturating_mul(10)
		.saturating_add(right)
}


pub trait WidthCalculator {
	fn width(&self, text: &str) -> usize;
}

#[derive(Clone,Copy,Debug)]
pub struct UnicodeWidth;

impl WidthCalculator for UnicodeWidth {
	fn width(&self, text: &str) -> usize {
		text.width()
	}
}

#[derive(Clone,Copy,Debug)]
pub struct WcWidth;

impl WcWidth {
	pub fn cwidth(&self, ch: char) -> usize {
		ch.width().unwrap()
	}
}

impl WidthCalculator for WcWidth {
	fn width(&self, text: &str) -> usize {
		let mut width = 0;
		for ch in text.chars() {
			width += self.cwidth(ch)
		}
		width
	}
}

const ZWJ: char = '\u{200D}';
#[derive(Clone,Copy,Debug)]
pub struct NoZwj;

impl WidthCalculator for NoZwj {
	fn width(&self, text: &str) -> usize {
		let mut width = 0;
		for slice in text.split(ZWJ) {
			width += UnicodeWidth.width(slice);
		}
		width
	}
}

pub struct TermBuffer {
	tty: RawFd
}

impl TermBuffer {
	pub fn new(tty: RawFd) -> Self {
		assert!(isatty(tty).is_ok_and(|r| r == true));
		Self {
			tty
		}
	}
}

impl Read for TermBuffer {
	fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
		assert!(isatty(self.tty).is_ok_and(|r| r == true));
		loop {
			match nix::unistd::read(self.tty, buf) {
				Ok(n) => return Ok(n),
				Err(Errno::EINTR) => {}
				Err(e) => return Err(std::io::Error::from_raw_os_error(e as i32))
			}
		}
	}
}

pub struct TermReader {
	buffer: BufReader<TermBuffer>
}

impl TermReader {
	pub fn new() -> Self {
		Self {
			buffer: BufReader::new(TermBuffer::new(1))
		}
	}

	pub fn poll(&mut self, timeout: PollTimeout) -> ShResult<bool> {
		if self.buffer.buffer().len() > 0 {
			return Ok(true)
		}

		let mut fds = [poll::PollFd::new(self.as_fd(),PollFlags::POLLIN)];
		let r = poll::poll(&mut fds, timeout);
		match r {
			Ok(n) => Ok(n != 0),
			Err(Errno::EINTR) => Ok(false),
			Err(e) => Err(e.into())
		}
	}

	pub fn next_byte(&mut self) -> std::io::Result<u8> {
		let mut buf = [0u8];
		self.buffer.read_exact(&mut buf)?;
		Ok(buf[0])
	}

	pub fn peek_byte(&mut self) -> std::io::Result<u8> {
		let buf = self.buffer.fill_buf()?;
		if buf.is_empty() {
			Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "EOF"))
		} else {
			Ok(buf[0])
		}
	}

	pub fn consume_byte(&mut self) {
		self.buffer.consume(1);
	}
}

impl AsFd for TermReader {
	fn as_fd(&self) -> BorrowedFd<'_> {
		let fd = self.buffer.get_ref().tty;
		unsafe { BorrowedFd::borrow_raw(fd) }
	}
}

pub struct Layout {
	pub w_calc: Box<dyn WidthCalculator>,
	pub prompt_end: Pos,
	pub cursor: Pos,
	pub end: Pos
}

impl Layout {
	pub fn new() -> Self {
		let w_calc = width_calculator();
		Self {
			w_calc,
			prompt_end: Pos::default(),
			cursor: Pos::default(),
			end: Pos::default(),
		}
	}
}

pub struct LineWriter {
	out: RawFd,
	t_cols: Col, // terminal width
	buffer: String,
	w_calc: Box<dyn WidthCalculator>,
	tab_stop: u16,
}

impl LineWriter {
	pub fn new(out: RawFd) -> Self {
		let w_calc = width_calculator();
		let (t_cols,_) = get_win_size(out);
		Self {
			out,
			t_cols,
			buffer: String::new(),
			w_calc,
			tab_stop: 8 // TODO: add a way to configure this
		}
	}
	pub fn flush_write(&mut self, buf: &str) -> ShResult<()> {
		write_all(self.out, buf)?;
		Ok(())
	}
	pub fn clear_rows(&mut self, layout: &Layout) {
		let rows_to_clear = layout.end.row;
		let cursor_row = layout.cursor.row;

		let cursor_motion = rows_to_clear.saturating_sub(cursor_row);
		if cursor_motion > 0 {
			write!(self.buffer, "\x1b[{cursor_motion}B").unwrap()
		}

		for _ in 0..rows_to_clear {
			self.buffer.push_str("\x1b[K\x1b[A");
		}
		self.buffer.push_str("\x1b[K");
	}
	pub fn move_cursor(&mut self, old: Pos, new: Pos) -> ShResult<()> {
		self.buffer.clear();
		let err = |_| ShErr::simple(ShErrKind::InternalErr, "Failed to write to LineWriter internal buffer");

		match new.row.cmp(&old.row) {
			std::cmp::Ordering::Greater => {
				let shift = new.row - old.row;
				match shift {
					1 => self.buffer.push_str("\x1b[B"),
					_ => write!(self.buffer, "\x1b[{shift}B").map_err(err)?
				}
			}
			std::cmp::Ordering::Less => {
				let shift = old.row - new.row;
				match shift {
					1 => self.buffer.push_str("\x1b[A"),
					_ => write!(self.buffer, "\x1b[{shift}A").map_err(err)?
				}
			}
			std::cmp::Ordering::Equal => { /* Do nothing */ }
		}

		match new.col.cmp(&old.col) {
			std::cmp::Ordering::Greater => {
				let shift = new.col - old.col;
				match shift {
					1 => self.buffer.push_str("\x1b[C"),
					_ => write!(self.buffer, "\x1b[{shift}C").map_err(err)?
				}
			}
			std::cmp::Ordering::Less => {
				let shift = old.col - new.col;
				match shift {
					1 => self.buffer.push_str("\x1b[D"),
					_ => write!(self.buffer, "\x1b[{shift}D").map_err(err)?
				}
			}
			std::cmp::Ordering::Equal => { /* Do nothing */ }
		}
		write_all(self.out, self.buffer.as_str())?;
		Ok(())
	}

	pub fn redraw(
		&mut self,
		prompt: &str,
		line: &LineBuf,
		old_layout: &Layout,
		new_layout: &Layout,
	) -> ShResult<()> {
		let err = |_| ShErr::simple(ShErrKind::InternalErr, "Failed to write to LineWriter internal buffer");
		self.buffer.clear();

		self.clear_rows(old_layout);

		let end = new_layout.end;
		let cursor = new_layout.cursor;

		self.buffer.push_str(prompt);
		self.buffer.push_str(line.as_str());

		if end.col == 0 
			&& end.row > 0
		{
			// The line has wrapped. We need to use our own line break.
			self.buffer.push('\n')
		}

		let cursor_row_offset = end.row - cursor.row;

		match cursor_row_offset {
			0 => { /* Do nothing */ }
			1 => self.buffer.push_str("\x1b[A"),
			_ => write!(self.buffer, "\x1b[{cursor_row_offset}A").map_err(err)?
		}

		let cursor_col = cursor.col;
		match cursor_col {
			0 => self.buffer.push('\r'),
			1 => self.buffer.push_str("\x1b[C"),
			_ => write!(self.buffer, "\x1b[{cursor_col}C").map_err(err)?
		}

		write_all(self.out, self.buffer.as_str())?;
		Ok(())
	}

	pub fn calc_pos(&self, s: &str, orig: Pos) -> Pos {
		let mut pos = orig;
		let mut esc_seq = 0;
		for c in s.graphemes(true) {
			if c == "\n" {
				pos.row += 1;
				pos.col = 0;
			}
			let c_width = if c == "\t" {
				self.tab_stop - (pos.col % self.tab_stop)
			} else {
				width(c, &mut esc_seq)
			};
			pos.col += c_width;
			if pos.col > self.t_cols {
				pos.row += 1;
				pos.col = c_width;
			}
		}
		if pos.col > self.t_cols {
			pos.row += 1;
			pos.col = 0;
		}

		pos
	}

	pub fn update_t_cols(&mut self) {
		let (t_cols,_) = get_win_size(self.out);
		self.t_cols = t_cols;
	}

	pub fn move_cursor_at_leftmost(&mut self, rdr: &mut TermReader) -> ShResult<()> {
		if rdr.poll(PollTimeout::ZERO)? {
			// The terminals reply is going to be stuck behind the currently buffered output
			// So let's get out of here
			return Ok(()) 
		}

		// Ping the cursor's position
		self.flush_write("\x1b[6n")?;

		// Excessively paranoid invariant checking
		if !rdr.poll(PollTimeout::from(100u8))?
			|| rdr.next_byte()? as char != '\x1b'
			|| rdr.next_byte()? as char != '['
			|| read_digits_until(rdr, ';')?.is_none() {
				// Invariant is broken, get out
				return Ok(())
		}
		// We just consumed everything up to the column number, so let's get that now
		let col = read_digits_until(rdr, 'R')?;

		// The cursor is not at the leftmost, so let's fix that
		if col != Some(1) {
			// We use '\n' instead of '\r' because if there's a bunch of garbage on this line,
			// It might pollute the prompt/line buffer if those are shorter than said garbage
			self.flush_write("\n")?;
		}

		Ok(())
	}
}
