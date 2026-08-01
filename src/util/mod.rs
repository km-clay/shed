pub(crate) mod error;
pub mod flog;
mod guards;
mod macros;
mod path;
mod pos;
pub mod posix_extension;
mod strops;
mod ui;

use std::os::fd::BorrowedFd;

use super::{Shed, eval, expand, match_loop, procio, sherr, state, system_msg, var};

use compact_str::CompactString;
pub(super) use guards::{isolation_guard, scope_guard, shared_scope_guard, var_ctx_guard};
pub(super) use path::{
  PathCache, is_executable_file, path_list_entries, resolve_in_path, split_path_list,
};
pub(super) use pos::{Pos, SignedPos};
pub(super) use ui::{
  BOT_LEFT, BOT_RIGHT, HOR_LINE, PaletteEntry, TOP_LEFT, TOP_RIGHT, VERT_LINE,
  ansi_from_description, pad_line_into, style_from_description, stylize_loglevel,
};

#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Direction {
  #[default]
  Forward,
  Backward,
}

pub(super) use error::{ShErr, ShErrKind, ShResult, ShResultExt, get_context};

pub(super) use strops::{
  QuoteState, VarStrDisplay, compile_glob, count_unescaped, ends_with_unescaped, expand_ansi_c,
  format_mode, format_size, format_time, has_any_unescaped, has_unescaped, parse_size,
  replace_posix_classes, scan_param_exp, scan_parens, split_at_unescaped, split_tk,
  starts_with_unescaped,
};

pub(super) struct FdWriter<'a>(pub BorrowedFd<'a>);

impl std::io::Write for FdWriter<'_> {
  fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    nix::unistd::write(self.0, buf).map_err(|e| std::io::Error::from_raw_os_error(e as i32))
  }
  fn flush(&mut self) -> std::io::Result<()> {
    Ok(())
  }
}

/// Returns a default `CompactString` with capacity 24
///
/// Used for temporary buffers that are ideally put together via `push` or `push_str`
/// like variable names and stuff for instance.
/// Buffers under 24 bytes in length remain on the stack.
pub(super) fn scratch_buf() -> CompactString {
  CompactString::with_capacity(24)
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

/// Sets status code and always returns Ok(())
///
/// It's easy to forget to set the status code, this helps with that
#[expect(clippy::unnecessary_wraps)]
pub(super) fn with_status(code: i32) -> ShResult<()> {
  state::Shed::set_status(code);
  Ok(())
}
