use nix::{
  libc::STDOUT_FILENO,
  unistd::{Whence, lseek, write},
};

use crate::{
  getopt::{Opt, OptArg, OptSpec, get_opts_from_tokens},
  libsh::error::ShResult,
  parse::{NdRule, Node},
  procio::borrow_fd,
  sherr, state,
};

pub const LSEEK_OPTS: [OptSpec; 2] = [
  OptSpec {
    opt: Opt::Short('c'),
    takes_arg: OptArg::None,
  },
  OptSpec {
    opt: Opt::Short('e'),
    takes_arg: OptArg::None,
  },
];

pub struct LseekOpts {
  cursor_rel: bool,
  end_rel: bool,
}

pub fn seek(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (mut argv, opts) = get_opts_from_tokens(argv, &LSEEK_OPTS)?;
  let lseek_opts = get_lseek_opts(opts)?;
  if !argv.is_empty() {
    argv.remove(0); // drop 'seek'
  }
  let mut argv = argv.into_iter();

  let Some(fd) = argv.next() else {
    return Err(sherr!(ExecFail, "lseek: Missing required argument 'fd'",));
  };
  let Ok(fd) = fd.0.parse::<u32>() else {
    return Err(
      sherr!(ExecFail @ fd.1, "Invalid file descriptor").with_note("file descriptors are integers"),
    );
  };

  let Some(offset) = argv.next() else {
    return Err(sherr!(
      ExecFail,
      "lseek: Missing required argument 'offset'",
    ));
  };
  let Ok(offset) = offset.0.parse::<i64>() else {
    return Err(
      sherr!(ExecFail @ offset.1, "Invalid offset")
        .with_note("offset can be a positive or negative integer"),
    );
  };

  let whence = if lseek_opts.cursor_rel {
    Whence::SeekCur
  } else if lseek_opts.end_rel {
    Whence::SeekEnd
  } else {
    Whence::SeekSet
  };

  match lseek(fd as i32, offset, whence) {
    Ok(new_offset) => {
      let stdout = borrow_fd(STDOUT_FILENO);
      let buf = new_offset.to_string() + "\n";
      write(stdout, buf.as_bytes())?;
    }
    Err(e) => {
      state::set_status(1);
      return Err(e.into());
    }
  }

  state::set_status(0);
  Ok(())
}

pub fn get_lseek_opts(opts: Vec<Opt>) -> ShResult<LseekOpts> {
  let mut lseek_opts = LseekOpts {
    cursor_rel: false,
    end_rel: false,
  };

  for opt in opts {
    match opt {
      Opt::Short('c') => lseek_opts.cursor_rel = true,
      Opt::Short('e') => lseek_opts.end_rel = true,
      _ => {
        return Err(sherr!(ExecFail, "lseek: Unexpected flag '{opt}'",));
      }
    }
  }

  Ok(lseek_opts)
}

#[cfg(test)]
mod tests {
  use crate::testutil::{TestGuard, test_input};
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
    let val = crate::state::read_vars(|v| v.get_var("line"));
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

    let val = crate::state::read_vars(|v| v.get_var("line"));
    assert_eq!(val, "abc");
  }

  #[test]
  fn seek_bad_fd() {
    let _g = TestGuard::new();

    let result = test_input("seek 99 0");
    assert!(result.is_err());
  }

  #[test]
  fn seek_missing_args() {
    let _g = TestGuard::new();

    let result = test_input("seek");
    assert!(result.is_err());

    let result = test_input("seek 9");
    assert!(result.is_err());
  }
}
