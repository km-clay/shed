// Standard Library Common IO and FS Abstractions
pub use std::env;
pub use std::ffi::{CStr, CString};
pub use std::fmt;
pub use std::fs::{self, File, OpenOptions};
pub use std::io::{
  self, BufRead, BufReader, BufWriter, Error, ErrorKind, Read, Seek, SeekFrom, Write,
};
pub use std::mem;
pub use std::path::{Path, PathBuf};
pub use std::process::exit;
pub use std::sync::Arc;
pub use std::time::Instant;

// Unix-specific IO abstractions
pub use std::os::unix::io::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};

// Nix crate for POSIX APIs
pub use bitflags::bitflags;
pub use nix::{
  errno::Errno,
  fcntl::{OFlag, open},
  libc::{self, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO},
  sys::{
    signal::{self, SigHandler, SigSet, SigmaskHow, Signal, kill, killpg, pthread_sigmask, signal},
    stat::Mode,
    termios::{self},
    wait::{WaitPidFlag as WtFlag, WaitStatus as WtStat, waitpid},
  },
  unistd::{
    ForkResult, Pid, close, dup, dup2, execvpe, fork, getpgid, getpgrp, isatty, pipe, read,
    setpgid, tcgetpgrp, tcsetpgrp, write,
  },
};

pub use crate::flog;
pub use crate::libsh::flog::ShedLogLevel::*;

// Additional utilities, if needed, can be added here
