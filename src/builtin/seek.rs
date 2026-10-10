use std::io;

use crate::{
  opt, outln, sherr,
  state::Shed,
  util::{self, error::ShResult},
};

use super::opt::OptSpec;

pub(super) struct Seek;
impl super::Builtin for Seek {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("current" | b'c'),
      opt!("end"     | b'e'),
      opt!("print"   | b'p'),
    ]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let     span       = args.span();
    let mut arguments  = args.arguments();

    let     cursor_rel = args.has_opt("current");
    let     end_rel    = args.has_opt("end");
    let     print      = args.has_opt("print");

    if cursor_rel && end_rel {
      let cur_span = args.opt_span("current").unwrap();
      let end_span = args.opt_span("end").unwrap();
      let cur_str  = cur_span.slice();
      let end_str  = end_span.slice();
      return Err(sherr!(
        ExecFail @ cur_span,
        "cannot specify both {cur_str} and {end_str}"
      ));
    }

    let Some((fd, fd_span)) = arguments.next() else {
      return Err(sherr!(ExecFail @ span, "missing required argument 'fd'",).with_code(2));
    };

    let fd = fd.parse::<u32>().map_err(|v| {
      sherr!(ExecFail @ fd_span, "invalid file descriptor `{v}`")
        .with_code(2)
        .with_note("file descriptors are non-negative integers".into())
    })?;

    let Some((offset, offset_span)) = arguments.next() else {
      return Err(sherr!(
        ExecFail @ span,
        "missing required argument 'offset'",
      ));
    };
    let Ok(offset) = offset.to_str_lossy().parse::<i64>() else {
      return Err(
        sherr!(ExecFail @ offset_span, "invalid offset")
          .with_note("offset can be a positive or negative integer".into()),
      );
    };

    if let Some((extra, extra_span)) = arguments.next() {
      return Err(sherr!(ExecFail @ extra_span, "unexpected argument: '{extra}'").with_code(2));
    }

    let seek_from = if cursor_rel {
      io::SeekFrom::Current(offset)
    } else if end_rel {
      io::SeekFrom::End(offset)
    } else {
      io::SeekFrom::Start(offset.cast_unsigned())
    };

    let sink = Shed::sinks(|s| s.get(fd.cast_signed()))
      .ok_or_else(|| sherr!(ExecFail @ span, "lseek failed: EBADF: Bad file number"))?;
    let new_off = sink
      .seek(seek_from)
      .map_err(|e| sherr!(ExecFail @ span, "lseek failed: {e}"))?;

    if print {
      outln!("{new_off}");
    }

    util::with_status(0)
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
    let dir  = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek -p 9 0").unwrap();

    let out = g.read_output();
    assert_eq!(out, "0\n");
  }

  #[test]
  fn seek_set_offset() {
    let dir  = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek -p 9 6").unwrap();

    let out = g.read_output();
    assert_eq!(out, "6\n");
  }

  #[test]
  fn seek_then_read() {
    let dir  = tempfile::TempDir::new().unwrap();
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
    let dir  = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "abcdefghij\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek -p 9 3").unwrap();
    test_input("seek -p -c 9 4").unwrap();

    let out = g.read_output();
    assert_eq!(out, "3\n7\n");
  }

  #[test]
  fn seek_end() {
    let dir  = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello\n").unwrap(); // 6 bytes
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek -p -e 9 0").unwrap();

    let out = g.read_output();
    assert_eq!(out, "6\n");
  }

  #[test]
  fn seek_end_negative() {
    let dir  = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello\n").unwrap(); // 6 bytes
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek -p -e 9 -2").unwrap();

    let out = g.read_output();
    assert_eq!(out, "4\n");
  }

  #[test]
  fn seek_write_overwrite() {
    let dir  = tempfile::TempDir::new().unwrap();
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
    let dir  = tempfile::TempDir::new().unwrap();
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
  fn seek_is_silent_without_print() {
    let dir  = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello world\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek 9 6").unwrap();

    assert_eq!(
      g.read_output(),
      "",
      "seek must not print unless asked with -p"
    );
  }

  #[test]
  fn seek_origin_flags_do_not_imply_print() {
    let dir  = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("seek.txt");
    std::fs::write(&path, "hello\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("exec 9<> {}", path.display())).unwrap();
    test_input("seek -e 9 0").unwrap();
    test_input("seek -c 9 -1").unwrap();

    assert_eq!(g.read_output(), "");
  }

  #[test]
  fn seek_bad_fd() {
    let _g = TestGuard::new();

    test_input("seek 99 0").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn seek_surplus_positional_errors() {
    let _g = TestGuard::new();

    test_input("exec 9<> /dev/null").unwrap();
    // `end` looks like a whence but the origin is a flag, so it must not be
    // silently dropped -- that reads as a successful seek to offset 0.
    test_input("seek 9 0 end").ok();
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
