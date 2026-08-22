use std::os::fd::BorrowedFd;

use nix::unistd::{Whence, lseek};

use super::{
  opt::OptSpec,
  outln, sherr,
  util::{ShResult, with_status},
};

pub(super) struct Seek;
impl super::Builtin for Seek {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("cursor-rel", b'c'),
      OptSpec::new_short("end-rel", b'e'),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let mut cursor_rel = false;
    let mut end_rel = false;
    let (arg_vec, opts) = args.take_argv();

    let mut arg_iter = arg_vec.into_iter();

    for opt in opts {
      match opt.key() {
        "cursor-rel" => cursor_rel = true,
        "end-rel" => end_rel = true,
        _ => {
          return Err(sherr!(ExecFail @ opt.span(), "lseek: Unexpected flag '{opt}'",));
        }
      }
    }

    let Some((fd, fd_span)) = arg_iter.next() else {
      return Err(sherr!(ExecFail @ span, "lseek: Missing required argument 'fd'",));
    };
    let Ok(fd) = fd.to_str_lossy().parse::<u32>() else {
      return Err(
        sherr!(ExecFail @ fd_span, "Invalid file descriptor")
          .with_note("file descriptors are integers".into()),
      );
    };

    let Some((offset, offset_span)) = arg_iter.next() else {
      return Err(sherr!(
        ExecFail @ span,
        "lseek: Missing required argument 'offset'",
      ));
    };
    let Ok(offset) = offset.to_str_lossy().parse::<i64>() else {
      return Err(
        sherr!(ExecFail @ offset_span, "Invalid offset")
          .with_note("offset can be a positive or negative integer".into()),
      );
    };

    let whence = if cursor_rel {
      Whence::SeekCur
    } else if end_rel {
      Whence::SeekEnd
    } else {
      Whence::SeekSet
    };

    let new_off = lseek(
      unsafe { BorrowedFd::borrow_raw(fd.cast_signed()) }, // lseek will validate this for us
      offset,
      whence,
    )
    .map_err(|e| sherr!(ExecFail @ span, "lseek failed: {e}"))?;

    outln!("{new_off}");

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};
  use crate::var;
  use pretty_assertions::assert_eq;

  #[test]
  fn seek_set_beginning() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek 9 0").unwrap();

    let out = g.read_output();
    assert_eq!(out, "0\n");
  }

  #[test]
  fn seek_set_offset() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek 9 6").unwrap();

    let out = g.read_output();
    assert_eq!(out, "6\n");
  }

  #[test]
  fn seek_then_read() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek 9 6").unwrap();
    // Clear the seek output
    g.read_output();

    test_input("read line <&9").unwrap();
    let val = var!("line");
    assert_eq!(val, "world");
  }

  #[test]
  fn seek_cur_relative() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "abcdefghij\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek 9 3").unwrap();
    test_input("seek -c 9 4").unwrap();

    let out = g.read_output();
    assert_eq!(out, "3\n7\n");
  }

  #[test]
  fn seek_end() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello\n").unwrap(); // 6 bytes
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek -e 9 0").unwrap();

    let out = g.read_output();
    assert_eq!(out, "6\n");
  }

  #[test]
  fn seek_end_negative() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello\n").unwrap(); // 6 bytes
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek -e 9 -2").unwrap();

    let out = g.read_output();
    assert_eq!(out, "4\n");
  }

  #[test]
  fn seek_write_overwrite() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let _g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek 9 6").unwrap();
    test_input("echo -n 'WORLD' >&9").unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents, "hello WORLD\n");
  }

  #[test]
  fn seek_rewind_full_read() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "abc\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    // Read moves cursor to EOF
    test_input("read line <&9").unwrap();
    // Rewind
    test_input("seek 9 0").unwrap();
    // Clear output from seek
    g.read_output();
    // Read again from beginning
    test_input("read line <&9").unwrap();

    let val = var!("line");
    assert_eq!(val, "abc");
  }

  #[test]
  fn seek_bad_fd() {
    let _g = TestGuard::new();

    test_input("seek 99 0").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn seek_missing_args() {
    let _g = TestGuard::new();

    test_input("seek").ok();
    assert_ne!(state::Shed::get_status(), 0);

    test_input("seek 9").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
