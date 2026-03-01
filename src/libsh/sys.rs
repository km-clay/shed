use std::sync::LazyLock;

use crate::prelude::*;

pub static TTY_FILENO: LazyLock<RawFd> = LazyLock::new(|| {
  open("/dev/tty", OFlag::O_RDWR, Mode::empty()).expect("Failed to open /dev/tty")
});
