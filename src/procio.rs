use std::{fmt::Debug, ops::{Deref, DerefMut}};

use crate::{libsh::{error::{ShErr, ShErrKind, ShResult}, utils::RedirVecUtils}, parse::{Redir, RedirType}, prelude::*};

// Credit to fish-shell for many of the implementation ideas present in this module
// https://fishshell.com/

#[derive(Clone,Debug)]
pub enum IoMode {
	Fd { tgt_fd: RawFd, src_fd: Arc<OwnedFd> },
	File { tgt_fd: RawFd, file: Arc<File> },
	Pipe { tgt_fd: RawFd, pipe: Arc<OwnedFd> },
	Buffer { buf: String, pipe: Arc<OwnedFd> }
}

impl IoMode {
	pub fn fd(tgt_fd: RawFd, src_fd: RawFd) -> Self {
		let src_fd = unsafe { OwnedFd::from_raw_fd(src_fd).into() };
		Self::Fd { tgt_fd, src_fd }
	}
	pub fn file(tgt_fd: RawFd, file: File) -> Self {
		let file = file.into();
		Self::File { tgt_fd, file }
	}
	pub fn pipe(tgt_fd: RawFd, pipe: OwnedFd) -> Self {
		let pipe = pipe.into();
		Self::Pipe { tgt_fd, pipe }
	}
	pub fn tgt_fd(&self) -> RawFd {
		match self {
			IoMode::Fd { tgt_fd, src_fd: _ } |
			IoMode::File { tgt_fd, file: _ } |
			IoMode::Pipe { tgt_fd, pipe: _ } => *tgt_fd,
			_ => panic!()
		}
	}
	pub fn src_fd(&self) -> RawFd {
		match self {
			IoMode::Fd { tgt_fd: _, src_fd } => src_fd.as_raw_fd(),
			IoMode::File { tgt_fd: _, file } => file.as_raw_fd(),
			IoMode::Pipe { tgt_fd: _, pipe } => pipe.as_raw_fd(),
			_ => panic!()
		}
	}
	pub fn get_pipes() -> (Self,Self) {
		let (rpipe,wpipe) = pipe().unwrap();
		(
			Self::Pipe { tgt_fd: STDIN_FILENO, pipe: rpipe.into() },
			Self::Pipe { tgt_fd: STDOUT_FILENO, pipe: wpipe.into() }
		)
	}
}

impl Read for IoMode {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		let src_fd = self.src_fd();
		Ok(read(src_fd, buf)?)
	}
}

pub struct IoBuf<R: Read> {
	buf: Vec<u8>,
	reader: R,
}

impl<R: Read> IoBuf<R> {
	pub fn new(reader: R) -> Self {
		Self {
			buf: Vec::new(),
			reader,
		}
	}

	/// Reads exactly `size` bytes (or fewer if EOF) into the buffer
	pub fn read_buffer(&mut self, size: usize) -> io::Result<()> {
		let mut temp_buf = vec![0; size]; // Temporary buffer
		let bytes_read = self.reader.read(&mut temp_buf)?;
		self.buf.extend_from_slice(&temp_buf[..bytes_read]); // Append only what was read
		Ok(())
	}

	/// Continuously reads until EOF
	pub fn fill_buffer(&mut self) -> io::Result<()> {
		let mut temp_buf = vec![0; 1024]; // Read in chunks
		loop {
			flog!(DEBUG, "reading bytes");
			let bytes_read = self.reader.read(&mut temp_buf)?;
			flog!(DEBUG, bytes_read);
			if bytes_read == 0 {
				break; // EOF reached
			}
			self.buf.extend_from_slice(&temp_buf[..bytes_read]);
		}
		Ok(())
	}

	/// Get current buffer contents as a string (if valid UTF-8)
	pub fn as_str(&self) -> ShResult<&str> {
		std::str::from_utf8(&self.buf).map_err(|_| {
			ShErr::simple(ShErrKind::InternalErr, "Invalid utf-8 in IoBuf")
		})
	}
}

/// A struct wrapping three fildescs representing `stdin`, `stdout`, and `stderr` respectively
#[derive(Debug,Clone)]
pub struct IoGroup(RawFd,RawFd,RawFd);

/// A single stack frame used with the IoStack
/// Each stack frame represents the redirections of a single command
#[derive(Default,Clone,Debug)]
pub struct IoFrame {
	redirs: Vec<Redir>,
	saved_io: Option<IoGroup>,
}

impl<'e> IoFrame {
	pub fn new() -> Self {
		Default::default()
	}
	pub fn from_redirs(redirs: Vec<Redir>) -> Self {
		Self { redirs, saved_io: None }
	}
	pub fn from_redir(redir: Redir) -> Self {
		Self { redirs: vec![redir], saved_io: None }
	}

	/// Splits the frame into two frames
	///
	/// One frame contains input redirections, the other contains output redirections
	/// This is used in shell structures to route redirections either *to* the condition, or *from* the body
	/// The first field of the tuple contains input redirections (used for the condition)
	/// The second field contains output redirections (used for the body)
	pub fn split_frame(self) -> (Self,Self) {
		let Self { redirs, saved_io: _ } = self;
		let (input_redirs,output_redirs) = redirs.split_by_channel();
		(
			Self::from_redirs(input_redirs),
			Self::from_redirs(output_redirs)
		)
	}
	pub fn save(&'e mut self)  {
		let saved_in = dup(STDIN_FILENO).unwrap();
		let saved_out = dup(STDOUT_FILENO).unwrap();
		let saved_err = dup(STDERR_FILENO).unwrap();
		self.saved_io = Some(IoGroup(saved_in,saved_out,saved_err));
	}
	pub fn redirect(&mut self) -> ShResult<()> {
		self.save();
		for redir in &mut self.redirs {
			let io_mode = &mut redir.io_mode;
			let tgt_fd = io_mode.tgt_fd();
			let src_fd = io_mode.src_fd();
			dup2(src_fd, tgt_fd)?;
		}
		Ok(())
	}
	pub fn restore(&mut self) -> ShResult<()> {
		if let Some(saved) = self.saved_io.take() {
			dup2(saved.0, STDIN_FILENO)?;
			close(saved.0)?;
			dup2(saved.1, STDOUT_FILENO)?;
			close(saved.1)?;
			dup2(saved.2, STDERR_FILENO)?;
			close(saved.2)?;
		}
		Ok(())
	}
}

impl Deref for IoFrame {
	type Target = Vec<Redir>;
	fn deref(&self) -> &Self::Target {
		&self.redirs
	}
}

impl DerefMut for IoFrame {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.redirs
	}
}

/// A stack that maintains the current state of I/O for commands
///
/// This struct maintains the current state of I/O for the `Dispatcher` struct
/// Each executed command requires an `IoFrame` in order to perform redirections.
/// As nodes are walked through by the `Dispatcher`, it pushes new frames in certain contexts, and pops frames in others.
/// Each command calls pop_frame() in order to get the current IoFrame in order to perform redirection
#[derive(Debug,Default)]
pub struct IoStack {
	stack: Vec<IoFrame>,
}

impl<'e> IoStack {
	pub fn new() -> Self {
		Self {
			stack: vec![IoFrame::new()],
		}
	}
	pub fn curr_frame(&self) -> &IoFrame {
		self.stack.last().unwrap()
	}
	pub fn curr_frame_mut(&mut self) -> &mut IoFrame {
		self.stack.last_mut().unwrap()
	}
	pub fn push_to_frame(&mut self, redir: Redir) {
		self.curr_frame_mut().push(redir)
	}
	pub fn append_to_frame(&mut self, mut other: Vec<Redir>) {
		self.curr_frame_mut().append(&mut other)
	}
	/// Pop the current stack frame
	/// This differs from using `pop()` because it always returns a stack frame
	/// If `self.pop()` would empty the `IoStack`, it instead uses `std::mem::take()` to take the last frame
	/// There will always be at least one frame in the `IoStack`.
	pub fn pop_frame(&mut self) -> IoFrame {
		if self.stack.len() > 1 {
			self.pop().unwrap()
		} else {
			std::mem::take(self.curr_frame_mut())
		}
	}
	/// Push a new stack frame.
	pub fn push_frame(&mut self, frame: IoFrame) {
		self.push(frame)
	}
	/// Flatten the `IoStack`
	/// All of the current stack frames will be flattened into a single one
	/// Not sure what use this will serve, but my gut said this was worthy of writing
	pub fn flatten(&mut self) {
		let mut flat_frame = IoFrame::new();
		while let Some(mut frame) = self.pop() {
			flat_frame.append(&mut frame)
		}
		self.push(flat_frame);
	}
}

impl Deref for IoStack {
	type Target = Vec<IoFrame>;
	fn deref(&self) -> &Self::Target {
		&self.stack
	}
}

impl DerefMut for IoStack {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.stack
	}
}

pub fn borrow_fd<'f>(fd: i32) -> BorrowedFd<'f> {
	unsafe { BorrowedFd::borrow_raw(fd) }
}
