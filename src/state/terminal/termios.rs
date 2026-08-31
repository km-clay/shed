use std::os::fd::RawFd;

use nix::{
  libc,
  sys::termios::{self, Termios},
};

// I'd like to thank rustyline for this idea
nix::ioctl_read_bad!(win_size, libc::TIOCGWINSZ, libc::winsize);

/// Get the dimensions of thejterminal.
///
/// Returned as (cols,rows)
pub(crate) fn get_win_size(fd: RawFd) -> (u16, u16) {
  use std::mem::zeroed;

  if cfg!(test) {
    return (80, 24);
  }

  unsafe {
    let mut size: libc::winsize = zeroed();
    match win_size(fd, &raw mut size) {
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
        (cols, rows)
      }
      _ => (80, 24),
    }
  }
}

pub(super) fn enable_raw_mode(term: &mut Termios) {
  termios::cfmakeraw(term);
  // Keep ISIG enabled so Ctrl+C/Ctrl+Z still generate signals
  term.local_flags |= termios::LocalFlags::ISIG;
  // Keep OPOST enabled so \n is translated to \r\n on output
  term.output_flags |= termios::OutputFlags::OPOST;
}

pub(super) fn enable_cooked_mode(term: &mut Termios) {
  term.local_flags |= termios::LocalFlags::ICANON
    | termios::LocalFlags::ECHO
    | termios::LocalFlags::ECHOE
    | termios::LocalFlags::ECHOK
    | termios::LocalFlags::ECHONL
    | termios::LocalFlags::ISIG
    | termios::LocalFlags::IEXTEN;
  term.input_flags |= termios::InputFlags::ICRNL | termios::InputFlags::IXON;
  term.output_flags |= termios::OutputFlags::OPOST;
  // Restore VMIN/VTIME to canonical mode defaults
  term.control_chars[termios::SpecialCharacterIndices::VMIN as usize] = 1;
  term.control_chars[termios::SpecialCharacterIndices::VTIME as usize] = 0;
}
