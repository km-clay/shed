//! Small general-purpose helpers shared across the crate, and the root of the `util` submodules.

pub(crate) mod error;
pub(crate) mod flog;
pub(crate) mod guards;
pub(crate) mod macros;
pub(crate) mod pos;
pub(crate) mod posix;
pub(crate) mod random;
pub(crate) mod strops;
pub(crate) mod ui;

use std::{os::fd::BorrowedFd, str::FromStr};

use bstr::ByteSlice;

#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Direction {
  #[default]
  Forward,
  Backward,
}

use smallvec::SmallVec;

use crate::{state::Shed, util::error::ShResult};
pub(super) struct FdWriter<'a>(pub BorrowedFd<'a>);

impl std::io::Write for FdWriter<'_> {
  fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    nix::unistd::write(self.0, buf).map_err(|e| std::io::Error::from_raw_os_error(e as i32))
  }
  fn flush(&mut self) -> std::io::Result<()> {
    Ok(())
  }
}

/// Returns a smallvec that can be used as a scratch buffer for temporary data.
///
/// Trivially convertible to [`crate::state::vars::VarStr`] since it implements `AsRef<[u8]>`.
pub(super) fn scratch_buf() -> SmallVec<[u8; 24]> {
  SmallVec::new()
}

pub(super) fn with_saved_status<F, T>(f: F) -> T
where
  F: FnOnce() -> T,
{
  let saved = Shed::get_status();
  let res = f();
  Shed::set_status(saved);
  res
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(super) fn base64_encode(buf: &[u8]) -> String {
  let mut out = String::with_capacity(buf.len().div_ceil(3) * 4);

  for chunk in buf.chunks(3) {
    let b = [
      chunk[0],
      *chunk.get(1).unwrap_or(&0),
      *chunk.get(2).unwrap_or(&0),
    ];
    let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);

    out.push(B64[(n >> 18 & 63) as usize] as char);
    out.push(B64[(n >> 12 & 63) as usize] as char);
    out.push(if chunk.len() > 1 {
      B64[(n >> 6 & 63) as usize] as char
    } else {
      '='
    });
    out.push(if chunk.len() > 2 {
      B64[(n & 63) as usize] as char
    } else {
      '='
    });
  }

  out
}

/// Given two things that implement Ord, make sure that the left is less than the right
pub(super) fn ordered<T: Ord>(start: T, end: T) -> (T, T) {
  if start > end {
    (end, start)
  } else {
    (start, end)
  }
}

/// Allows an implementor of [`core::str::FromStr`] to parse a slice of bytes.
///
/// Non-UTF-8 bytes will return None, as will any parse errors.
pub(super) fn parse_bytes<T: FromStr>(bytes: &[u8]) -> Option<T> {
  bytes.to_str().ok()?.parse().ok()
}

/// Sets status code and always returns Ok(())
///
/// It's easy to forget to set the status code, this helps with that
#[expect(clippy::unnecessary_wraps)]
pub(super) fn with_status(code: i32) -> ShResult<()> {
  Shed::set_status(code);
  Ok(())
}
