use crate::prelude::*;

bitflags! {
	#[derive(Copy,Clone,Debug,PartialEq,PartialOrd)]
	pub struct ExecFlags: u32 {
		const NO_FORK = 0x00000001;
		const IN_FUNC = 0x00000010;
	}
}

#[derive(Clone,Debug)]
pub struct ExecCtx {
	redirs: Vec<Redir>,
	depth: usize,
	max_depth: usize,
	flags: ExecFlags,
	io_masks: IoMasks,
	saved_io: Option<SavedIo>
}

impl ExecCtx {
	pub fn new() -> Self {
		Self {
			redirs: vec![],
			depth: 0,
			max_depth: 1500,
			flags: ExecFlags::empty(),
			io_masks: IoMasks::new(),
			saved_io: None
		}
	}
	pub fn as_cond(&self) -> Self {
		let mut clone = self.clone();
		let (cond_redirs,_) = self.sort_redirs();
		clone.redirs = cond_redirs;
		clone
	}
	pub fn as_body(&self) -> Self {
		let mut clone = self.clone();
		let (_,body_redirs) = self.sort_redirs();
		clone.redirs = body_redirs;
		clone
	}
	pub fn redirs(&self) -> &Vec<Redir> {
		&self.redirs
	}
	pub fn clear_redirs(&mut self) {
		self.redirs.clear()
	}
	pub fn sort_redirs(&self) -> (Vec<Redir>,Vec<Redir>) {
		let mut cond_redirs = vec![];
		let mut body_redirs = vec![];
		for redir in self.redirs.clone() {
			match redir.op {
				RedirType::Input |
				RedirType::HereString |
				RedirType::HereDoc => cond_redirs.push(redir),
				RedirType::Output |
				RedirType::Append => body_redirs.push(redir)
			}
		}
		(cond_redirs,body_redirs)
	}
	pub fn masks(&self) -> &IoMasks {
		&self.io_masks
	}
	pub fn push_rdr(&mut self, redir: Redir) {
		self.redirs.push(redir)
	}
	pub fn saved_io(&mut self) -> &mut Option<SavedIo> {
		&mut self.saved_io
	}
	pub fn activate_rdrs(&mut self) -> ShResult<()> {
		let mut redirs = CmdRedirs::new(core::mem::take(&mut self.redirs));
		self.redirs = vec![];
		redirs.activate()?;
		Ok(())
	}
	pub fn flags(&self) -> ExecFlags {
		self.flags
	}
	pub fn set_flag(&mut self, flag: ExecFlags) {
		self.flags |= flag
	}
	pub fn unset_flag(&mut self, flag: ExecFlags) {
		self.flags &= !flag
	}
}

#[derive(Debug,Clone)]
pub struct SavedIo {
	pub stdin: RawFd,
	pub stdout: RawFd,
	pub stderr: RawFd
}

impl SavedIo {
	pub fn save(stdin: RawFd, stdout: RawFd, stderr: RawFd) -> Self {
		Self { stdin, stdout, stderr }
	}
}

#[derive(Debug,Clone)]
pub struct IoMask {
	default: RawFd,
	mask: Option<RawFd>
}

impl IoMask {
	pub fn new(default: RawFd) -> Self {
		Self { default, mask: None }
	}
	pub fn new_mask(&mut self, mask: RawFd) {
		self.mask = Some(mask)
	}
	pub fn unmask(&mut self) {
		self.mask = None
	}
	pub fn get_fd(&self) -> RawFd {
		if let Some(fd) = self.mask {
			fd
		} else {
			self.default
		}
	}
}

#[derive(Clone,Debug)]
/// Necessary for when process file descriptors are permanently redirected using `exec`
pub struct IoMasks {
	stdin: IoMask,
	stdout: IoMask,
	stderr: IoMask
}

impl IoMasks {
	pub fn new() -> Self {
		Self {
			stdin: IoMask::new(0),
			stdout: IoMask::new(1),
			stderr: IoMask::new(2),
		}
	}
	pub fn stdin(&self) -> &IoMask {
		&self.stdin
	}
	pub fn stdout(&self) -> &IoMask {
		&self.stdout
	}
	pub fn stderr(&self) -> &IoMask {
		&self.stderr
	}
}
