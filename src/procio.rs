use std::{
  fmt::Debug,
  iter::Map,
  ops::{Deref, DerefMut},
};

use crate::{
  expand::Expander,
  libsh::{
    error::{ShErr, ShResult},
    sys::TTY_FILENO,
  },
  parse::{Redir, RedirType, get_redir_file, lex::TkFlags},
  prelude::*,
  sherr, state,
};

// Credit to fish-shell for many of the implementation ideas present in this
// module https://fishshell.com/

/// Minimum fd number for shell-internal file descriptors.
/// User-visible fds (0-9) are kept clear so `exec 3>&-` etc. work as expected.
const MIN_INTERNAL_FD: RawFd = 10;

/// Like `dup()`, but places the new fd at `MIN_INTERNAL_FD` or above so it
/// doesn't collide with user-managed fds.
fn dup_high(fd: RawFd) -> nix::Result<RawFd> {
  fcntl(fd, FcntlArg::F_DUPFD_CLOEXEC(MIN_INTERNAL_FD))
}

#[derive(Clone, Debug)]
pub enum IoMode {
  Fd {
    tgt_fd: RawFd,
    src_fd: RawFd, // Just the fd number - dup2 will handle it at execution time
  },
  OpenedFile {
    tgt_fd: RawFd,
    file: Arc<OwnedFd>, // Owns the opened file descriptor
  },
  File {
    tgt_fd: RawFd,
    path: PathBuf,
    mode: RedirType,
  },
  Pipe {
    tgt_fd: RawFd,
    pipe: Arc<OwnedFd>,
  },
  Buffer {
    tgt_fd: RawFd,
    buf: String,
    flags: TkFlags, // so we can see if its a heredoc or not
  },
  Close {
    tgt_fd: RawFd,
  },
}

impl IoMode {
  pub fn fd(tgt_fd: RawFd, src_fd: RawFd) -> Self {
    // Just store the fd number - dup2 will use it directly at execution time
    Self::Fd { tgt_fd, src_fd }
  }
  pub fn file(tgt_fd: RawFd, path: PathBuf, mode: RedirType) -> Self {
    Self::File { tgt_fd, path, mode }
  }
  pub fn pipe(tgt_fd: RawFd, pipe: OwnedFd) -> Self {
    let pipe = pipe.into();
    Self::Pipe { tgt_fd, pipe }
  }
  pub fn tgt_fd(&self) -> RawFd {
    match self {
      IoMode::Fd { tgt_fd, .. }
      | IoMode::OpenedFile { tgt_fd, .. }
      | IoMode::File { tgt_fd, .. }
      | IoMode::Pipe { tgt_fd, .. } => *tgt_fd,
      _ => panic!(),
    }
  }
  pub fn src_fd(&self) -> RawFd {
    match self {
      IoMode::Fd { src_fd, .. } => *src_fd,
      IoMode::OpenedFile { file, .. } => file.as_raw_fd(),
      IoMode::File { .. } => panic!("Attempted to obtain src_fd from file before opening"),
      IoMode::Pipe { pipe, .. } => pipe.as_raw_fd(),
      _ => panic!(),
    }
  }
  pub fn open_file(mut self) -> ShResult<Self> {
    if let IoMode::File { tgt_fd, path, mode } = self {
      let path_raw = path.as_os_str().to_str().unwrap_or_default().to_string();

      let expanded_path = Expander::from_raw(&path_raw, TkFlags::empty())?
        .expand()?
        .join(" "); // should just be one string, will have to find some way to handle a return of multiple paths

      let expanded_pathbuf = PathBuf::from(expanded_path);

      let file = get_redir_file(mode, expanded_pathbuf)?;
      // Move the opened fd above the user-accessible range so it never
      // collides with the target fd (e.g. `3>/tmp/foo` where open() returns 3,
      // causing dup2(3,3) to be a no-op and then OwnedFd drop closes it).
      let raw = file.as_raw_fd();
      let high = fcntl(raw, FcntlArg::F_DUPFD_CLOEXEC(MIN_INTERNAL_FD)).map_err(ShErr::from)?;
      drop(file); // closes the original low fd
      self = IoMode::OpenedFile {
        tgt_fd,
        file: Arc::new(unsafe { OwnedFd::from_raw_fd(high) }),
      }
    }
    Ok(self)
  }
  pub fn buffer(tgt_fd: RawFd, buf: String, flags: TkFlags) -> ShResult<Self> {
    Ok(Self::Buffer { tgt_fd, buf, flags })
  }
  pub fn loaded_pipe(tgt_fd: RawFd, buf: &[u8]) -> ShResult<Self> {
    let (rpipe, wpipe) = nix::unistd::pipe()?;
    write(wpipe, buf)?;
    Ok(Self::Pipe {
      tgt_fd,
      pipe: rpipe.into(),
    })
  }
  pub fn get_pipes() -> (Self, Self) {
    let (rpipe, wpipe) = nix::unistd::pipe2(OFlag::O_CLOEXEC).unwrap();
    (
      Self::Pipe {
        tgt_fd: STDIN_FILENO,
        pipe: rpipe.into(),
      },
      Self::Pipe {
        tgt_fd: STDOUT_FILENO,
        pipe: wpipe.into(),
      },
    )
  }
}

impl Read for IoMode {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    let src_fd = self.src_fd();
    Ok(read(src_fd, buf)?)
  }
}

pub struct IoBuf<R: Read> {
  buf: Vec<u8>,
  reader: R,
}

impl<R: Read> IoBuf<R> {
  pub fn new(reader: R) -> Self {
    Self {
      buf: Vec::new(),
      reader,
    }
  }

  /// Reads exactly `size` bytes (or fewer if EOF) into the buffer
  pub fn read_buffer(&mut self, size: usize) -> io::Result<()> {
    let mut temp_buf = vec![0; size]; // Temporary buffer
    let bytes_read = self.reader.read(&mut temp_buf)?;
    self.buf.extend_from_slice(&temp_buf[..bytes_read]); // Append only what was read
    Ok(())
  }

  /// Continuously reads until EOF
  pub fn fill_buffer(&mut self) -> io::Result<()> {
    let mut temp_buf = vec![0; 1024]; // Read in chunks
    loop {
      let bytes_read = self.reader.read(&mut temp_buf)?;
      if bytes_read == 0 {
        break; // EOF reached
      }
      self.buf.extend_from_slice(&temp_buf[..bytes_read]);
    }
    Ok(())
  }

  /// Get current buffer contents as a string (if valid UTF-8)
  pub fn as_str(&self) -> ShResult<&str> {
    std::str::from_utf8(&self.buf).map_err(|_| sherr!(InternalErr, "Invalid utf-8 in IoBuf"))
  }
}

// this was originally here, but moved to libsh::guards
pub use crate::libsh::guards::RedirGuard;

/// A struct wrapping three fildescs representing `stdin`, `stdout`, and
/// `stderr` respectively
#[derive(Debug, Clone)]
pub struct IoGroup(pub RawFd, pub RawFd, pub RawFd);

/// A single stack frame used with the IoStack
/// Each stack frame represents the redirections of a single command
#[derive(Default, Clone, Debug)]
pub struct IoFrame {
  pub redirs: Vec<Redir>,
  pub saved_io: Option<IoGroup>,
}

impl<'e> IoFrame {
  pub fn new() -> Self {
    Default::default()
  }
  pub fn from_redirs(redirs: Vec<Redir>) -> Self {
    Self {
      redirs,
      saved_io: None,
    }
  }
  pub fn from_redir(redir: Redir) -> Self {
    Self {
      redirs: vec![redir],
      saved_io: None,
    }
  }

  pub fn save(&'e mut self) {
    let saved_in = dup_high(STDIN_FILENO).unwrap();
    let saved_out = dup_high(STDOUT_FILENO).unwrap();
    let saved_err = dup_high(STDERR_FILENO).unwrap();
    self.saved_io = Some(IoGroup(saved_in, saved_out, saved_err));
  }
  pub fn redirect(mut self) -> ShResult<RedirGuard> {
    self.save();
    if let Err(e) = self.apply_redirs() {
      // Restore saved fds before propagating the error so they don't leak.
      self.restore().ok();
      return Err(e);
    }
    Ok(RedirGuard::new(self))
  }
  fn apply_redirs(&mut self) -> ShResult<()> {
    for redir in &mut self.redirs {
      let io_mode = &mut redir.io_mode;
      match io_mode {
        IoMode::Close { tgt_fd } => {
          if *tgt_fd == *TTY_FILENO {
            // Don't let user close the shell's tty fd.
            continue;
          }
          close(*tgt_fd).ok();
          continue;
        }
        IoMode::File { .. } => match io_mode.clone().open_file() {
          Ok(file) => *io_mode = file,
          Err(e) => {
            if let Some(span) = redir.span.as_ref() {
              return Err(e.promote(span.clone()));
            }
            return Err(e);
          }
        },
        IoMode::Buffer { tgt_fd, buf, flags } => {
          let (rpipe, wpipe) = nix::unistd::pipe()?;
          let mut text = if flags.contains(TkFlags::LIT_HEREDOC) {
            buf.clone()
          } else {
            let words = Expander::from_raw(buf, *flags)?.expand()?;
            if flags.contains(TkFlags::IS_HEREDOC) {
              words.into_iter().next().unwrap_or_default()
            } else {
              let ifs = state::get_separator();
              words.join(&ifs).trim().to_string() + "\n"
            }
          };
          if flags.contains(TkFlags::TAB_HEREDOC) {
            let lines = text.lines();
            let mut min_tabs = usize::MAX;
            for line in lines {
              if line.is_empty() {
                continue;
              }
              let line_len = line.len();
              let after_strip = line.trim_start_matches('\t').len();
              let delta = line_len - after_strip;
              min_tabs = min_tabs.min(delta);
            }
            if min_tabs == usize::MAX {
              // let's avoid possibly allocating a string with 18 quintillion tabs
              min_tabs = 0;
            }

            if min_tabs > 0 {
              let stripped = text
                .lines()
                .fold(vec![], |mut acc, ln| {
                  if ln.is_empty() {
                    acc.push("");
                    return acc;
                  }
                  let stripped_ln = ln.strip_prefix(&"\t".repeat(min_tabs)).unwrap();
                  acc.push(stripped_ln);
                  acc
                })
                .join("\n");
              text = stripped + "\n";
            }
          }
          write(wpipe, text.as_bytes())?;
          *io_mode = IoMode::Pipe {
            tgt_fd: *tgt_fd,
            pipe: rpipe.into(),
          };
        }
        _ => {}
      }
      let tgt_fd = io_mode.tgt_fd();
      let src_fd = io_mode.src_fd();
      if let Err(e) = dup2(src_fd, tgt_fd) {
        if let Some(span) = redir.span.as_ref() {
          return Err(ShErr::from(e).promote(span.clone()));
        } else {
          return Err(e.into());
        }
      }
    }
    Ok(())
  }
  pub fn restore(&mut self) -> ShResult<()> {
    if let Some(saved) = self.saved_io.take() {
      dup2(saved.0, STDIN_FILENO)?;
      close(saved.0)?;
      dup2(saved.1, STDOUT_FILENO)?;
      close(saved.1)?;
      dup2(saved.2, STDERR_FILENO)?;
      close(saved.2)?;
    }
    Ok(())
  }
}

impl Deref for IoFrame {
  type Target = Vec<Redir>;
  fn deref(&self) -> &Self::Target {
    &self.redirs
  }
}

impl DerefMut for IoFrame {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.redirs
  }
}

/// A stack that maintains the current state of I/O for commands
///
/// This struct maintains the current state of I/O for the `Dispatcher` struct
/// Each executed command requires an `IoFrame` in order to perform
/// redirections. As nodes are walked through by the `Dispatcher`, it pushes new
/// frames in certain contexts, and pops frames in others. Each command calls
/// pop_frame() in order to get the current IoFrame in order to perform
/// redirection
#[derive(Debug, Default)]
pub struct IoStack {
  pub stack: Vec<IoFrame>,
}

impl IoStack {
  pub fn new() -> Self {
    Self {
      stack: vec![IoFrame::new()],
    }
  }
  pub fn curr_frame(&self) -> &IoFrame {
    self.stack.last().unwrap()
  }
  pub fn curr_frame_mut(&mut self) -> &mut IoFrame {
    self.stack.last_mut().unwrap()
  }
  pub fn push_to_frame(&mut self, redir: Redir) {
    self.curr_frame_mut().push(redir)
  }
  pub fn append_to_frame(&mut self, mut other: Vec<Redir>) {
    self.curr_frame_mut().append(&mut other)
  }
  /// Pop the current stack frame
  /// This differs from using `pop()` because it always returns a stack frame
  /// If `self.pop()` would empty the `IoStack`, it instead uses
  /// `std::mem::take()` to take the last frame There will always be at least
  /// one frame in the `IoStack`.
  pub fn pop_frame(&mut self) -> IoFrame {
    if self.stack.len() > 1 {
      self.pop().unwrap()
    } else {
      std::mem::take(self.curr_frame_mut())
    }
  }
  /// Push a new stack frame.
  pub fn push_frame(&mut self, frame: IoFrame) {
    self.push(frame)
  }
}

impl Deref for IoStack {
  type Target = Vec<IoFrame>;
  fn deref(&self) -> &Self::Target {
    &self.stack
  }
}

impl DerefMut for IoStack {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.stack
  }
}

impl From<Vec<IoFrame>> for IoStack {
  fn from(frames: Vec<IoFrame>) -> Self {
    Self { stack: frames }
  }
}

pub fn borrow_fd<'f>(fd: i32) -> BorrowedFd<'f> {
  unsafe { BorrowedFd::borrow_raw(fd) }
}

type PipeFrames = Map<PipeGenerator, fn((Option<Redir>, Option<Redir>)) -> IoFrame>;

/// An iterator that lazily creates a specific number of pipes.
pub struct PipeGenerator {
  num_cmds: usize,
  cursor: usize,
  last_rpipe: Option<Redir>,
}

impl PipeGenerator {
  pub fn new(num_cmds: usize) -> Self {
    Self {
      num_cmds,
      cursor: 0,
      last_rpipe: None,
    }
  }
  pub fn as_io_frames(self) -> PipeFrames {
    self.map(|(r, w)| {
      let mut frame = IoFrame::new();
      if let Some(r) = r {
        frame.push(r);
      }
      if let Some(w) = w {
        frame.push(w);
      }
      frame
    })
  }
}

impl Iterator for PipeGenerator {
  type Item = (Option<Redir>, Option<Redir>);
  fn next(&mut self) -> Option<Self::Item> {
    if self.cursor == self.num_cmds {
      return None;
    }
    if self.cursor + 1 == self.num_cmds {
      if self.num_cmds == 1 {
        return None;
      } else {
        self.cursor += 1;
        return Some((self.last_rpipe.take(), None));
      }
    }
    let (r, w) = IoMode::get_pipes();
    let mut rpipe = Some(Redir::new(r, RedirType::Input));
    std::mem::swap(&mut self.last_rpipe, &mut rpipe);

    let wpipe = Redir::new(w, RedirType::Output);

    self.cursor += 1;
    Some((rpipe, Some(wpipe)))
  }
}

#[cfg(test)]
pub mod tests {
  use crate::testutil::{TestGuard, has_cmd, has_cmds, test_input};
  use pretty_assertions::assert_eq;

  #[test]
  fn pipeline_simple() {
    if !has_cmd("sed") {
      return;
    };
    let g = TestGuard::new();

    test_input("echo foo | sed 's/foo/bar/'").unwrap();

    let out = g.read_output();
    assert_eq!(out, "bar\n");
  }

  #[test]
  fn pipeline_multi() {
    if !has_cmds(&["cut", "sed"]) {
      return;
    }
    let g = TestGuard::new();

    test_input("echo foo bar baz | cut -d ' ' -f 2 | sed 's/a/A/'").unwrap();

    let out = g.read_output();
    assert_eq!(out, "bAr\n");
  }

  #[test]
  fn rube_goldberg_pipeline() {
    if !has_cmds(&["sed", "cat"]) {
      return;
    }
    let g = TestGuard::new();

    test_input("{ echo foo; echo bar } | if cat; then :; else echo failed; fi | (read line && echo $line | sed 's/foo/baz/'; sed 's/bar/buzz/')").unwrap();

    let out = g.read_output();
    assert_eq!(out, "baz\nbuzz\n");
  }

  #[test]
  fn simple_file_redir() {
    let mut g = TestGuard::new();

    test_input("echo this is in a file > /tmp/simple_file_redir.txt").unwrap();

    g.add_cleanup(|| {
      std::fs::remove_file("/tmp/simple_file_redir.txt").ok();
    });
    let contents = std::fs::read_to_string("/tmp/simple_file_redir.txt").unwrap();

    assert_eq!(contents, "this is in a file\n");
  }

  #[test]
  fn append_file_redir() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("append.txt");
    let _g = TestGuard::new();

    test_input(format!("echo first > {}", path.display())).unwrap();
    test_input(format!("echo second >> {}", path.display())).unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents, "first\nsecond\n");
  }

  #[test]
  fn input_redir() {
    if !has_cmd("cat") {
      return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("input.txt");
    std::fs::write(&path, "hello from file\n").unwrap();
    let g = TestGuard::new();

    test_input(format!("cat < {}", path.display())).unwrap();

    let out = g.read_output();
    assert_eq!(out, "hello from file\n");
  }

  #[test]
  fn stderr_redir_to_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("err.txt");
    let g = TestGuard::new();

    test_input(format!("echo error msg 2> {} >&2", path.display())).unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents, "error msg\n");
    // stdout should be empty since we redirected to stderr
    let out = g.read_output();
    assert_eq!(out, "");
  }

  #[test]
  fn pipe_and_stderr() {
    if !has_cmd("cat") {
      return;
    }
    let g = TestGuard::new();

    test_input("echo on stderr >&2 |& cat").unwrap();

    let out = g.read_output();
    assert_eq!(out, "on stderr\n");
  }

  #[test]
  fn output_redir_clobber() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("clobber.txt");
    let _g = TestGuard::new();

    test_input(format!("echo first > {}", path.display())).unwrap();
    test_input(format!("echo second > {}", path.display())).unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents, "second\n");
  }

  #[test]
  fn pipeline_preserves_exit_status() {
    if !has_cmd("cat") {
      return;
    }
    let _g = TestGuard::new();

    test_input("false | cat").unwrap();

    // Pipeline exit status is the last command
    let status = crate::state::get_status();
    assert_eq!(status, 0);

    test_input("cat < /dev/null | false").unwrap();

    let status = crate::state::get_status();
    assert_ne!(status, 0);
  }

  #[test]
  fn fd_duplication() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("dup.txt");
    let _g = TestGuard::new();

    test_input(format!(
      "{{ echo out; echo err >&2; }} > {} 2>&1",
      path.display()
    ))
    .unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("out"));
    assert!(contents.contains("err"));
  }
}
