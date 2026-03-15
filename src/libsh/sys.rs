use std::sync::LazyLock;

use crate::prelude::*;

/// Minimum fd number for shell-internal file descriptors.
const MIN_INTERNAL_FD: RawFd = 10;

pub static TTY_FILENO: LazyLock<RawFd> = LazyLock::new(|| {
  let fd = open("/dev/tty", OFlag::O_RDWR, Mode::empty()).expect("Failed to open /dev/tty");
  // Move the tty fd above the user-accessible range so that
  // `exec 3>&-` and friends don't collide with shell internals.
  let high = fcntl(fd, FcntlArg::F_DUPFD_CLOEXEC(MIN_INTERNAL_FD)).expect("Failed to dup /dev/tty high");
  close(fd).ok();
  high
});
