use std::{fmt::Debug, ops::{Deref, DerefMut}};

use crate::{libsh::error::ShResult, parse::{Redir, RedirType}, prelude::*};

// Credit to fish-shell for many of the implementation ideas present in this module
// https://fishshell.com/

pub enum IoMode {
	Fd,
	File,
	Pipe,
}

pub trait IoInfo: Read {
	fn mode(&self) -> IoMode;
	/// The fildesc that is replaced by src_fd in dup2()
	/// e.g. `dup2(src_fd, tgt_fd)`
	fn tgt_fd(&self) -> RawFd;
	/// The fildesc that replaces tgt_fd in dup2()
	/// e.g. `dup2(src_fd, tgt_fd)`
	fn src_fd(&self) -> RawFd;
	fn print(&self) -> String;
	fn close(&mut self) -> ShResult<()>;
}

macro_rules! read_impl {
	($type:path) => {
		impl Read for $type {
			fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
				let src_fd = self.src_fd();

				Ok(read(src_fd, buf)?)
			}
		}
	};
}
read_impl!(IoPipe);
read_impl!(IoFile);
read_impl!(IoFd);


// TODO: implement this
impl Debug for Box<dyn IoInfo> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f,"{}",self.print())
	}
}

/// A redirection to a raw fildesc
/// e.g. `2>&1`
#[derive(Debug)]
pub struct IoFd {
	tgt_fd: RawFd,
	src_fd: RawFd
}

impl IoFd {
	pub fn new(tgt_fd: RawFd, src_fd: RawFd) -> Self {
		Self { tgt_fd, src_fd }
	}
}

impl IoInfo for IoFd {
	fn mode(&self) -> IoMode {
		IoMode::Fd
	}
	fn tgt_fd(&self) -> RawFd {
		self.tgt_fd
	}
	fn src_fd(&self) -> RawFd {
		self.src_fd
	}
	fn close(&mut self) -> ShResult<()> {
		if self.src_fd == -1 {
			return Ok(())
		}
		close(self.src_fd)?;
		self.src_fd = -1;
		Ok(())
	}
	fn print(&self) -> String {
		format!("{:?}",self)
	}
}

/// A redirection to a file
/// e.g. `> file.txt`
#[derive(Debug)]
pub struct IoFile {
	tgt_fd: RawFd,
	file: File
}

impl IoFile {
	pub fn new(tgt_fd: RawFd, file: File) -> Self {
		Self { tgt_fd, file }
	}
}

impl IoInfo for IoFile {
	fn mode(&self) -> IoMode {
		IoMode::File
	}
	fn tgt_fd(&self) -> RawFd {
		self.tgt_fd
	}
	fn src_fd(&self) -> RawFd {
		self.file.as_raw_fd()
	}
	fn close(&mut self) -> ShResult<()> {
		// Closes on it's own when it's dropped
		Ok(())
	}
	fn print(&self) -> String {
		format!("{:?}",self)
	}
}

/// A redirection to a pipe
/// e.g. `echo foo | sed s/foo/bar/`
#[derive(Debug)]
pub struct IoPipe {
	tgt_fd: RawFd,
	pipe_fd: OwnedFd
}

impl IoPipe {
	pub fn new(tgt_fd: RawFd, pipe_fd: OwnedFd) -> Self {
		Self { tgt_fd, pipe_fd }
	}
	pub fn get_pipes() -> (Self, Self) {
		let (rpipe,wpipe) = pipe().unwrap();
		let r_iopipe = Self::new(STDIN_FILENO, rpipe);
		let w_iopipe = Self::new(STDOUT_FILENO, wpipe);

		(r_iopipe,w_iopipe)
	}
}

impl IoInfo for IoPipe {
	fn mode(&self) -> IoMode {
		IoMode::Pipe
	}
	fn tgt_fd(&self) -> RawFd {
		self.tgt_fd
	}
	fn src_fd(&self) -> RawFd {
		self.pipe_fd.as_raw_fd()
	}
	fn close(&mut self) -> ShResult<()> {
		// Closes on it's own
		Ok(())
	}
	fn print(&self) -> String {
		format!("{:?}",self)
	}
}

pub struct FdWriter {
	tgt: OwnedFd
}

impl FdWriter {
	pub fn new(fd: i32) -> Self {
		let tgt = unsafe { OwnedFd::from_raw_fd(fd) };
		Self { tgt }
	}
}

impl Write for FdWriter {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		Ok(write(&self.tgt, buf)?)
	}
	fn flush(&mut self) -> io::Result<()> {
	  Ok(())
	}
}

/// A struct wrapping three fildescs representing `stdin`, `stdout`, and `stderr` respectively
#[derive(Debug)]
pub struct IoGroup(OwnedFd,OwnedFd,OwnedFd);

/// A single stack frame used with the IoStack
/// Each stack frame represents the redirections of a single command
#[derive(Default,Debug)]
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

	/// This method returns a 2-tuple of `IoFrames`.
	/// This is to be used in the case of shell structures such as `if-then` and `while-do`.
	/// # Params
	/// * redirs: a vector of redirections
	///
	/// # Returns
	/// * An `IoFrame` containing all of the redirections which target stdin
	/// * An `IoFrame` containing all of the redirections which target stdout/stderr
	///
	/// # Purpose
	/// In the case of something like `if cat; then echo foo; fi < input.txt > output.txt`
	/// This will cleanly separate the redirections such that `cat` can receive the input from input.txt
	/// and `echo foo` can redirect it's output to output.txt
	pub fn cond_and_body(redirs: Vec<Redir>) -> (Self, Self) {
		let mut output_redirs = vec![];
		let mut input_redirs = vec![];
		for redir in redirs {
			match redir.class {
				RedirType::Input => input_redirs.push(redir),
				RedirType::Pipe => {
					match redir.io_info.tgt_fd() {
						STDIN_FILENO => input_redirs.push(redir),
						STDOUT_FILENO |
						STDERR_FILENO => output_redirs.push(redir),
						_ => unreachable!()
					}
				}
				_ => output_redirs.push(redir)
			}
		}
		(Self::from_redirs(input_redirs),Self::from_redirs(output_redirs))
	}
	pub fn save(&'e mut self)  {
		unsafe {
			let saved_in = OwnedFd::from_raw_fd(dup(STDIN_FILENO).unwrap());
			let saved_out = OwnedFd::from_raw_fd(dup(STDOUT_FILENO).unwrap());
			let saved_err = OwnedFd::from_raw_fd(dup(STDERR_FILENO).unwrap());
			self.saved_io = Some(IoGroup(saved_in,saved_out,saved_err));
		}
	}
	pub fn redirect(&mut self) -> ShResult<()> {
		self.save();
		for redir in &mut self.redirs {
			let io_info = &mut redir.io_info;
			let tgt_fd = io_info.tgt_fd();
			let src_fd = io_info.src_fd();
			dup2(src_fd, tgt_fd)?;
			io_info.close()?;
		}
		Ok(())
	}
	pub fn restore(&mut self) -> ShResult<()> {
		while let Some(mut redir) = self.pop() {
			redir.io_info.close()?;
		}
		if let Some(saved) = self.saved_io.take() {
			dup2(saved.0.as_raw_fd(), STDIN_FILENO)?;
			dup2(saved.1.as_raw_fd(), STDOUT_FILENO)?;
			dup2(saved.2.as_raw_fd(), STDERR_FILENO)?;
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
#[derive(Default)]
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
