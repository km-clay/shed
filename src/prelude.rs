// Standard Library Common IO and FS Abstractions
pub use std::io::{
	self,
	BufRead,
	BufReader,
	BufWriter,
	Error,
	ErrorKind,
	Read,
	Seek,
	SeekFrom,
	Write,
};
pub use std::fs::{ self, File, OpenOptions };
pub use std::path::{ Path, PathBuf };
pub use std::ffi::{ CStr, CString };
pub use std::process::exit;
pub use std::time::Instant;
pub use std::mem;
pub use std::env;
pub use std::fmt;

// Unix-specific IO abstractions
pub use std::os::unix::io::{ AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd, };

// Nix crate for POSIX APIs
pub use nix::{
	errno::Errno,
	fcntl::{ open, OFlag },
	sys::{
		termios::{ self },
		signal::{ self, signal, kill, killpg, pthread_sigmask, SigSet, SigmaskHow, SigHandler, Signal },
		stat::Mode,
		wait::{ waitpid, WaitPidFlag as WtFlag, WaitStatus as WtStat },
	},
	libc::{ self, STDIN_FILENO, STDERR_FILENO, STDOUT_FILENO },
	unistd::{
		dup, read, isatty, write, close, setpgid, dup2, getpgrp, getpgid,
		execvpe, tcgetpgrp, tcsetpgrp, fork, pipe, Pid, ForkResult
	},
};
pub use bitflags::bitflags;

pub use crate::flog;
pub use crate::libsh::flog::FernLogLevel::*;

// Additional utilities, if needed, can be added here
