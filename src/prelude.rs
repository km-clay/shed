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

// Unix-specific IO abstractions
pub use std::os::unix::io::{ AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd, };

// Nix crate for POSIX APIs
pub use nix::{
    errno::Errno,
    fcntl::{ open, OFlag },
    sys::{
        signal::{ self, kill, SigHandler, Signal },
        stat::Mode,
        wait::{ waitpid, WaitStatus },
    },
		libc::{ STDIN_FILENO, STDERR_FILENO, STDOUT_FILENO },
    unistd::{ dup, read, write, close, dup2, execvpe, fork, pipe, Pid, ForkResult },
};

// Additional utilities, if needed, can be added here
