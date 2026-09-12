//! This module contains our IO redirection primitives.
//!
//! `shed`'s IO model revolves around the [`Sinks`] struct and the [`Sink`] trait. It is a virtual file descriptor table that allows us to
//! redirect IO to arbitrary sinks, including in-memory buffers.
//!
//! This has some pros and cons:
//!
//! Pros:
//! * We can use our own IO primitives the same way that we use actual file descriptors, meaning we can arbitrarily create
//!   new types of redirection targets, if we so choose.
//! * Redirection is O(1) and syscall-free until the redirections are actually needed.
//! * Redirection lifetimes are tied to [`RedirGuard`], so we can safely redirect IO without worrying about the underlying
//!   file descriptors being closed or reused.
//!
//! Cons:
//! * Direct interaction with file descriptors risks desyncing the virtual table and the actual process file descriptors.
//!   Safe IO operation requires disciplined use of the [`Sinks`] table and the [`Sink`] trait.
//! * Redirection must be materialized before it can be used, via [`Sinks::commit_redirects()`], which is another thing to remember when forking processes.

use std::{
  collections::VecDeque,
  fmt::Debug,
  fs::{File, OpenOptions},
  io::{self, Cursor, IsTerminal, Read, Seek, Write},
  ops::Deref,
  os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
  path::Path,
  sync::{Arc, Condvar, Mutex, OnceLock},
};

use bstr::ByteSlice;
use nix::{
  errno::Errno,
  fcntl::{self, FcntlArg, FdFlag, OFlag, fcntl},
  libc::{self, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO},
  poll::{PollFd, PollFlags, PollTimeout},
  sys::{
    resource::{self, Resource},
    stat::Mode,
    wait::{WaitPidFlag as WtFlag, WaitStatus as WtStat, waitpid},
  },
  unistd::{self, ForkResult, fork, write},
};

use crate::{
  HashMap, HashSet,
  eval::{
    execute,
    lex::{Span, Tk, TkFlags},
  },
  expand::Expander,
  lifecycle, match_loop, sherr, shopt, signal,
  state::{self, Shed, shopt::ReadLimit, terminal::Terminal, vars::VarStr},
  util::{
    self,
    error::{ShErr, ShResult},
    strops::{ByteCursor, SliceCursor},
  },
  varstr,
};

/// Minimum fd number for shell-internal file descriptors.
/// User-visible fds (0-9) are kept clear so `exec 3>&-` etc. work as expected.
pub(crate) const MIN_INTERNAL_FD: RawFd = 10;

/// The status code returned when a builtin command's output is truncated
/// due to exceeding the maximum size of the `OutputSink`
pub(crate) const SINK_TRUNCATED_STATUS: i32 = 122;

// FIONREAD reports how many bytes are available to read on an fd. Unlike
// `poll`, it distinguishes "data present" (n > 0) from "empty or EOF" (n == 0).
nix::ioctl_read_bad!(fionread, nix::libc::FIONREAD, nix::libc::c_int);

pub(crate) fn ebadf() -> io::Error {
  io::Error::from_raw_os_error(libc::EBADF)
}
pub(crate) fn validate_fd(fd: RawFd) -> io::Result<()> {
  if fd < 0 {
    return Err(ebadf());
  }
  let (soft, _) = resource::getrlimit(Resource::RLIMIT_NOFILE).unwrap_or((1024, 1024));
  if (fd as u64) >= soft {
    return Err(ebadf());
  }
  Ok(())
}

/// Like `dup()`, but places the new fd at `MIN_INTERNAL_FD` or above so it
/// doesn't collide with user-managed fds.
pub(crate) fn dup_high(fd: BorrowedFd) -> nix::Result<OwnedFd> {
  let fd = fcntl(fd, FcntlArg::F_DUPFD_CLOEXEC(MIN_INTERNAL_FD))?;
  unsafe { Ok(OwnedFd::from_raw_fd(fd)) }
}

/// Same as `dup_high`, but does not set the `CLOEXEC` flag on the new fd.
/// Good for fds that should be inherited across fork/exec
fn dup_high_no_cloexec(fd: BorrowedFd) -> nix::Result<OwnedFd> {
  let fd = fcntl(fd, FcntlArg::F_DUPFD(MIN_INTERNAL_FD))?;
  Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[expect(clippy::needless_pass_by_value)]
/// Like `dup_high()` but takes and closes an existing `OwnedFd`.
pub(crate) fn move_high(fd: OwnedFd) -> nix::Result<OwnedFd> {
  let new_fd = dup_high(fd.as_fd())?;
  Ok(new_fd)
} // fd is closed here

#[expect(clippy::needless_pass_by_value)]
pub(crate) fn move_high_no_cloexec(fd: OwnedFd) -> nix::Result<OwnedFd> {
  let new_fd = dup_high_no_cloexec(fd.as_fd())?;
  Ok(new_fd)
}

/// Anonymous, seekable, cloexec file descriptor
fn scratch_fd() -> io::Result<OwnedFd> {
  #[cfg(linux_like)]
  let fd = {
    use nix::sys::memfd::{MFdFlags, memfd_create};
    memfd_create("shed-scratch", MFdFlags::MFD_CLOEXEC)
  };
  #[cfg(not(linux_like))]
  let fd = tempfile::tempfile().map(OwnedFd::from);

  move_high(fd?).map_err(|e| io::Error::from_raw_os_error(e as i32))
}

/// `SQLite` opens long-lived file descriptors on its own and we cant call `move_high` on them.
///
/// These files usually end up polluting the user-space 3-10 range which we work so hard to keep clear
/// so that users can open resources on those file descriptors without any weirdness happening.
///
/// Later on we will probably have to do something like using a custom sqlite VFS
/// to limit the fd numbers it can use, but for now this will do. I guess.
pub(crate) fn do_something_that_opens_fds_that_we_cant_access_hack<F, T>(
  min_fd: RawFd,
  something: F,
) -> T
where
  F: FnOnce() -> T,
{
  // these close at the end of the function
  let _dummies = (3..min_fd)
    .filter_map(|_| {
      // painful to write
      fcntl::open(
        "/dev/null",
        OFlag::O_RDONLY | OFlag::O_CLOEXEC,
        Mode::empty(),
      )
      .ok()
    })
    .collect::<Vec<_>>();

  // now if this opens fds, they will be at least the value of min_fd
  something()
}

/// Creates pipes outside of the userspace range of FDs
pub(crate) fn pipes_high() -> nix::Result<(OwnedFd, OwnedFd)> {
  let (r, w) = nix::unistd::pipe()?;
  Ok((move_high(r)?, move_high(w)?))
}

pub(crate) fn pipes_high_no_cloexec() -> nix::Result<(OwnedFd, OwnedFd)> {
  let (r, w) = nix::unistd::pipe()?;
  Ok((move_high_no_cloexec(r)?, move_high_no_cloexec(w)?))
}

/// Step one of our redirection building pipeline.
///
/// The parser uses these to create `RedirSpecs`.
#[derive(Default, Debug)]
pub(super) struct RedirBldr {
  pub fd: Option<RawFd>,
  pub class: Option<RedirType>,
  pub target: Option<RedirTarget>,
  pub span: Option<Span>,
  pub dup_from_word: bool, // target fd is not a literal digit
}

impl RedirBldr {
  pub(crate) fn new() -> Self {
    RedirBldr::default()
  }
  pub(crate) fn with_fd(self, fd: RawFd) -> Self {
    Self {
      fd: Some(fd),
      ..self
    }
  }
  pub(crate) fn with_class(self, class: RedirType) -> Self {
    Self {
      class: Some(class),
      ..self
    }
  }
  pub(crate) fn with_target(self, target: RedirTarget) -> Self {
    Self {
      target: Some(target),
      ..self
    }
  }
  pub(crate) fn with_span(self, span: Span) -> Self {
    Self {
      span: Some(span),
      ..self
    }
  }
  pub(crate) fn with_dup_from_word(self) -> Self {
    Self {
      dup_from_word: true,
      ..self
    }
  }
  pub(crate) fn build(self) -> ShResult<RedirSpec> {
    let Some(fd) = self.fd else {
      return Err(sherr!(ParseErr, "Redirection missing target fd").option_promote(self.span));
    };
    let Some(class) = self.class else {
      return Err(sherr!(ParseErr, "Redirection missing class").option_promote(self.span));
    };
    let Some(target) = self.target else {
      return Err(sherr!(ParseErr, "Redirection missing target").option_promote(self.span));
    };

    match target {
      RedirTarget::Path(path) if class.is_file_op() => Ok(RedirSpec::file(fd, path, class)),
      RedirTarget::Close => Ok(RedirSpec::close(fd)),
      RedirTarget::Fd(src_fd) if class.is_dup_op() => Ok(RedirSpec::dup_spanned(src_fd, fd, class)),
      RedirTarget::FdExpr(word) if class.is_dup_op() => Ok(RedirSpec::dup_expr(word, fd, class)),
      RedirTarget::HereDoc { body, flags } => {
        // Strip leading tabs per line BEFORE expansion (POSIX order).
        let buf: VarStr = if flags.contains(TkFlags::HERESTRING) {
          // Raw word; expanded and newline-terminated at redirection time.
          body
        } else if flags.contains(TkFlags::TAB_HEREDOC) {
          if body.is_empty() {
            body
          } else {
            // strip the tabs
            let mut out = Vec::new();
            for line in body.lines() {
              let tabs = line.iter().take_while(|&&b| b == b'\t').count();
              out.extend_from_slice(&line[tabs..]);
              out.push(b'\n');
            }
            out.into()
          }
        } else if !body.is_empty() && !body.ends_with(b"\n") {
          varstr!("{body}\n")
        } else {
          body
        };

        Ok(RedirSpec::buffer(fd, buf, flags))
      }
      _ => Err(
        sherr!(ParseErr, "Invalid redirection target for redirection type")
          .option_promote(self.span),
      ),
    }
  }
}

impl RedirBldr {
  /// Attempt parsing a redirection operator from a byte slice.
  /// Returns a `RedirBldr` with the parsed components, or an error if the input is invalid.
  pub(crate) fn parse(bytes: &[u8]) -> ShResult<Self> {
    let mut cur = SliceCursor::new(bytes);
    let mut src_fd = util::scratch_buf();
    let mut tgt_fd = util::scratch_buf();
    let mut redir = RedirBldr::new();

    match_loop!(cur.next_byte() => ch, {
      b'>' => {
        redir = redir.with_class(RedirType::Output);
        if cur.bump_if_eq(b'>') {
          redir = redir.with_class(RedirType::Append);
        } else if cur.bump_if_eq(b'|') {
          redir = redir.with_class(RedirType::OutputForce);
        }
      }
      b'<' => {
        redir = redir.with_class(RedirType::Input);
        let mut count = 0;

        if cur.bump_if_eq(b'>') {
          redir = redir.with_class(RedirType::ReadWrite);
        } else {
          while count < 2 && cur.bump_if_eq(b'<') {
            count += 1;
          }
        }

        redir = match count {
          1 => redir.with_class(RedirType::HereDoc),
          2 => redir.with_class(RedirType::HereString),
          _ => redir, // Default case remains RedirType::Input
        };
      }
      b'&' => {
        if cur.peek_byte() == Some(b'>') {
          continue
        } else if cur.bump_if_eq(b'-') {
          src_fd.push(b'-');
        } else {
          while let Some(next_ch) = cur.next_byte_if(|b| b.is_ascii_digit()) {
            src_fd.push(next_ch);
          }
        }
        if src_fd.is_empty() {
          // No inline fd or `-`: the dup source is a following word, expanded
          // at redirection time (e.g. `>&$fd`).
          redir = redir.with_dup_from_word();
        }
      }
      _ if ch.is_ascii_digit() && tgt_fd.is_empty() => {
        tgt_fd.push(ch);
        while let Some(next_ch) = cur.next_byte_if(|b| b.is_ascii_digit()) {
          tgt_fd.push(next_ch);
        }
      }
      _ => {
        return Err(sherr!(
            ParseErr,
            "Invalid character '{}' in redirection operator",
            ch as char,
        ));
      }
    });

    let tgt_fd = util::parse_bytes::<i32>(&tgt_fd).unwrap_or_else(|| match redir.class.unwrap() {
      RedirType::Input | RedirType::ReadWrite | RedirType::HereDoc | RedirType::HereString => 0,
      _ => 1,
    });
    redir = redir.with_fd(tgt_fd);
    if *src_fd == *b"-" {
      redir = redir.with_target(RedirTarget::Close);
    } else if let Some(src_fd) = util::parse_bytes::<i32>(&src_fd) {
      redir = redir.with_target(RedirTarget::Fd(src_fd));
    }
    Ok(redir)
  }
}

impl TryFrom<Tk> for RedirBldr {
  type Error = ShErr;
  fn try_from(tk: Tk) -> Result<Self, Self::Error> {
    let span = tk.span;
    if tk.flags.contains(TkFlags::IS_HEREDOC) {
      let flags = tk.flags;

      Ok(RedirBldr {
        fd: Some(0),
        class: Some(RedirType::HereDoc),
        target: Some(RedirTarget::HereDoc {
          body: tk.word(),
          flags,
        }),
        span: Some(span),
        dup_from_word: false,
      })
    } else {
      match Self::parse(&tk.slice()) {
        Ok(bldr) => Ok(bldr.with_span(span)),
        Err(e) => Err(e.promote(span)),
      }
    }
  }
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub(super) enum RedirType {
  Null,        // Default
  Input,       // <
  Output,      // >
  OutputForce, // >|
  Append,      // >>
  HereDoc,     // <<
  HereString,  // <<<
  ReadWrite,   // <>, fd is opened for reading and writing
}

impl RedirType {
  pub(crate) fn is_input(self) -> bool {
    matches!(
      self,
      RedirType::Input | RedirType::HereDoc | RedirType::HereString | RedirType::ReadWrite
    )
  }
  pub(crate) fn is_output(self) -> bool {
    matches!(
      self,
      RedirType::Output | RedirType::OutputForce | RedirType::Append | RedirType::ReadWrite
    )
  }
  /// Returns true if this redirection type is a file operation (i.e. not a dup or close).
  pub(crate) fn is_file_op(self) -> bool {
    matches!(
      self,
      RedirType::Output
        | RedirType::OutputForce
        | RedirType::Append
        | RedirType::Input
        | RedirType::ReadWrite
    )
  }
  pub(crate) fn is_dup_op(self) -> bool {
    matches!(self, RedirType::Output | RedirType::Input)
  }
}

/// The target of a redirection, as parsed from the command line.
/// This is an intermediate representation that is later converted into a [`RedirSpec`] for execution.
#[derive(Clone, Debug)]
pub(super) enum RedirTarget {
  Path(Tk),
  Fd(RawFd),
  FdExpr(Tk),
  Close,
  HereDoc { body: VarStr, flags: TkFlags },
}

/// The final representation of a redirection.
///
/// Will eventually be consumed and turned into a [`Redir`] for execution.
#[derive(Debug, Clone)]
pub(super) enum RedirSpec {
  File {
    fd: RawFd,
    path: Tk,
    mode: RedirType,
  },
  Dup {
    from: RawFd,
    to: RawFd,
    mode: RedirType,
  },
  DupExpr {
    word: Tk,
    to: RawFd,
    mode: RedirType,
  },
  Close {
    fd: RawFd,
  },
  Buffer {
    fd: RawFd,
    buf: VarStr,
    flags: TkFlags,
  },
}

impl RedirSpec {
  pub(crate) fn file(fd: RawFd, path: Tk, mode: RedirType) -> Self {
    Self::File { fd, path, mode }
  }
  pub(crate) fn dup(from: RawFd, to: RawFd, mode: RedirType) -> Self {
    Self::Dup { from, to, mode }
  }
  pub(crate) fn dup_spanned(from: RawFd, to: RawFd, mode: RedirType) -> Self {
    Self::Dup { from, to, mode }
  }
  pub(crate) fn dup_expr(word: Tk, to: RawFd, mode: RedirType) -> Self {
    Self::DupExpr { word, to, mode }
  }
  pub(crate) fn close(fd: RawFd) -> Self {
    Self::Close { fd }
  }
  /// The span of the redirection operator, if this spec carries one. Used to
  /// point errors at the offending redirect.
  pub(crate) fn buffer(fd: RawFd, buf: VarStr, flags: TkFlags) -> Self {
    Self::Buffer { fd, buf, flags }
  }
  pub(crate) fn target_fd(&self) -> RawFd {
    match self {
      RedirSpec::Dup { to, .. } | RedirSpec::DupExpr { to, .. } => *to,
      RedirSpec::File { fd, .. } | RedirSpec::Close { fd, .. } | RedirSpec::Buffer { fd, .. } => {
        *fd
      }
    }
  }
  pub(crate) fn mode(&self) -> RedirType {
    match self {
      RedirSpec::File { mode, .. }
      | RedirSpec::Dup { mode, .. }
      | RedirSpec::DupExpr { mode, .. } => *mode,
      RedirSpec::Close { .. } => RedirType::Null,
      RedirSpec::Buffer { .. } => RedirType::HereDoc,
    }
  }
  /// Resolve this spec into its target sink.
  ///
  /// Runs any expansion (heredoc bodies, redirect paths, dup-target words), so
  /// it must be called *outside* a [`Shed::sinks()`] borrow: expansion can run
  /// command substitutions that re-enter the table. The dup arms take their own
  /// brief borrow to read the current fd.
  pub(crate) fn as_sink(&self) -> ShResult<Arc<dyn Sink>> {
    let sink: Arc<dyn Sink> = match self {
      RedirSpec::Dup { from, .. } => {
        let sink = Shed::sinks(|s| s.get(*from)).ok_or_else(ebadf)?;
        if sink.kind() == SinkKind::Close {
          return Err(ebadf().into());
        }
        sink
      }
      RedirSpec::Close { .. } => Arc::new(CloseSink),
      RedirSpec::DupExpr { word, .. } => match expand_fd(word)? {
        None => Arc::new(CloseSink), // got '-' as the word
        Some(fd) => {
          let sink = Shed::sinks(|s| s.get(fd)).ok_or_else(ebadf)?;
          if sink.kind() == SinkKind::Close {
            return Err(ebadf().into());
          }
          sink
        }
      },
      RedirSpec::File { path, mode, .. } => {
        let span = path.span;
        let path = path
          .clone()
          .expand()
          .map(|tk| tk.get_words())
          .unwrap_or_default();
        if path.len() != 1 {
          return Err(sherr!(
            ExecFail @ span,
            "Redirection path must expand to exactly one word"
          ));
        }
        let path = path.iter().next().unwrap();

        if path.as_bytes() == b"/dev/null" {
          Arc::new(NullSink::new())
        } else {
          Arc::new(OsSink::new(open_redir_file(*mode, path)?))
        }
      }
      RedirSpec::Buffer { buf, flags, .. } => {
        let bytes: Vec<u8> = if flags.contains(TkFlags::HERESTRING) {
          let mut expanded: Vec<u8> = Expander::from_raw(buf.as_bytes(), *flags)
            .no_glob()
            .no_split()
            .expand_no_split()?
            .into();
          expanded.push(b'\n');
          expanded
        } else if flags.contains(TkFlags::IS_HEREDOC) && !flags.contains(TkFlags::LIT_HEREDOC) {
          Expander::from_raw(buf.as_bytes(), *flags)
            .no_glob()
            .no_split()
            .expand_no_split()?
            .into()
        } else {
          buf.as_bytes().to_vec()
        };

        Arc::new(BufSink::from_bytes(&bytes)) as Arc<dyn Sink>
      }
    };

    Ok(sink)
  }
}

/// A set of redirections to be applied together.
#[derive(Default, Debug)]
pub(super) struct RedirSet(pub Vec<RedirSpec>);

impl RedirSet {
  pub(crate) fn specs(&self) -> &[RedirSpec] {
    &self.0
  }

  /// Separate input redirs and output redirs into two separate `RedirSet`s
  ///
  /// Returns (`in_redirs`, `out_redirs`)
  pub(crate) fn split_by_channel(self) -> (RedirSet, RedirSet) {
    let mut in_redirs = vec![];
    let mut out_redirs = vec![];
    for spec in self.0 {
      if spec.mode().is_input() {
        in_redirs.push(spec);
      } else if spec.mode().is_output() {
        out_redirs.push(spec);
      }
    }
    (RedirSet(in_redirs), RedirSet(out_redirs))
  }
}

impl From<&[RedirSpec]> for RedirSet {
  fn from(value: &[RedirSpec]) -> Self {
    Self(value.to_vec())
  }
}

impl From<&Vec<RedirSpec>> for RedirSet {
  fn from(value: &Vec<RedirSpec>) -> Self {
    Self(value.clone())
  }
}
impl From<Vec<RedirSpec>> for RedirSet {
  fn from(value: Vec<RedirSpec>) -> Self {
    Self(value)
  }
}

impl From<RedirSpec> for RedirSet {
  fn from(value: RedirSpec) -> Self {
    Self(vec![value])
  }
}

/// A trait for abstracting over different types of I/O sinks (e.g., files, buffers, pipes).
///
/// This trait is used by the [`Sinks`] struct, which is `shed`'s virtual FD table. Having a virtual
/// fd table allows us to also do I/O redirection internally, and keep pipelines in-process if forking
/// is unnecessary (e.g. a pipeline with only builtins)
pub(crate) trait Sink: Send + Sync {
  fn read(&self, buf: &mut [u8]) -> io::Result<usize>;
  fn write(&self, buf: &[u8]) -> io::Result<usize>;
  fn flush(&self) -> io::Result<()>;
  fn as_os_fd(&self) -> io::Result<BorrowedFd<'_>>;
  fn kind(&self) -> SinkKind;
  fn poll(&self, timeout: Option<PollTimeout>) -> io::Result<usize>;
  fn isatty(&self) -> bool {
    matches!(self.kind(), SinkKind::Tty)
  }

  fn has_data(&self) -> bool {
    false
  }
  fn was_truncated(&self) -> bool {
    false
  }
  fn seek(&self, _pos: io::SeekFrom) -> io::Result<u64> {
    Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "seek not supported on this i/o sink",
    ))
  }
}

pub(crate) fn drain_sink(sink: &dyn Sink) -> io::Result<Vec<u8>> {
  let mut buf = Vec::new();
  let mut tmp = [0u8; 4096];
  loop {
    match sink.read(&mut tmp) {
      Ok(0) => break,
      Ok(n) => buf.extend_from_slice(&tmp[..n]),
      Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
        // Sink is non-blocking and has no data available; stop draining.
        break;
      }
      Err(e) if e.kind() == io::ErrorKind::Interrupted => {
        // Interrupted by a signal; retry the read.
        // TODO: make sure this handles Ctrl+C and stuff
      }
      Err(e) => {
        return Err(e);
      }
    }
  }
  Ok(buf)
}

pub(crate) struct BufSink {
  buf: Mutex<Cursor<Vec<u8>>>,

  /// If this buffer ever needs to be used across a fork/exec boundary, we can
  /// lazily create an OS-level fd for it and cache it in this field.
  os_fd: OnceLock<OwnedFd>,
}

impl BufSink {
  pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
    Self {
      buf: Mutex::new(Cursor::new(bytes.to_vec())),
      os_fd: OnceLock::new(),
    }
  }
}

impl Sink for BufSink {
  fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
    self.buf.lock().unwrap().read(buf)
  }
  fn write(&self, buf: &[u8]) -> io::Result<usize> {
    self.buf.lock().unwrap().write(buf)
  }
  fn flush(&self) -> io::Result<()> {
    self.buf.lock().unwrap().flush()
  }
  fn poll(&self, _timeout: Option<PollTimeout>) -> io::Result<usize> {
    // BufSink is a fixed-size buffer, so we don't use the timeout here.
    // It either has data or it doesn't, it won't receive any more.
    let cur = self.buf.lock().unwrap();
    Ok(cur.get_ref().len().saturating_sub(cur.position() as usize))
  }
  fn as_os_fd(&self) -> io::Result<BorrowedFd<'_>> {
    match self.os_fd.get() {
      None => {
        let fd = {
          let cur = self.buf.lock().unwrap();
          let remaining = &cur.get_ref()[cur.position() as usize..];
          let fd = scratch_fd()?;
          write_all_to_fd(fd.as_fd(), remaining);
          unistd::lseek(fd.as_fd(), 0, unistd::Whence::SeekSet)?;
          fd
        };
        self.os_fd.set(fd).expect("we just checked that it's None");

        self.as_os_fd() // try again
      }
      Some(fd) => Ok(fd.as_fd()),
    }
  }
  fn seek(&self, pos: io::SeekFrom) -> io::Result<u64> {
    self.buf.lock().unwrap().seek(pos)
  }
  fn has_data(&self) -> bool {
    let cur = self.buf.lock().unwrap();
    (cur.position() as usize) < cur.get_ref().len()
  }
  fn kind(&self) -> SinkKind {
    SinkKind::Buffer
  }
}

pub(crate) struct PipeBuf {
  queue: VecDeque<u8>,
  writer_open: bool,
  reader_open: bool,
  limit: usize,
  truncated: bool,
}

impl PipeBuf {
  pub(crate) fn new() -> Self {
    Self {
      queue: VecDeque::new(),
      writer_open: true,
      reader_open: true,
      limit: *shopt!(core.max_read_limit) as usize,
      truncated: false,
    }
  }

  fn is_full_and_readable(&mut self) -> bool {
    self.queue.len() >= self.limit && self.reader_open
  }

  fn is_empty_and_writable(&mut self) -> bool {
    self.queue.is_empty() && self.writer_open
  }
}

pub(crate) enum PipeSink {
  Write(Arc<Mutex<PipeBuf>>),
  Read(Arc<Mutex<PipeBuf>>),
}

impl PipeSink {
  fn new() -> (Self, Self) {
    let buf = Arc::new(Mutex::new(PipeBuf::new()));
    (Self::Read(buf.clone()), Self::Write(buf))
  }
}

impl Sink for PipeSink {
  fn read(&self, out: &mut [u8]) -> io::Result<usize> {
    match self {
      Self::Read(buf) => {
        let mut buf = buf.lock().unwrap();
        let n = out.len().min(buf.queue.len());
        for (slot, byte) in out.iter_mut().zip(buf.queue.drain(..n)) {
          *slot = byte;
        }
        Ok(n)
      }
      Self::Write(_) => Err(ebadf()),
    }
  }
  fn write(&self, buf: &[u8]) -> io::Result<usize> {
    match self {
      Self::Write(b) => {
        let mut b = b.lock().unwrap();
        // already capped: silently drop but report a full write so the producer
        // doesn't error/retry (matches the old OutputSink behavior)
        if b.truncated {
          return Ok(buf.len());
        }
        if b.queue.len() + buf.len() > b.limit {
          b.truncated = true;
          let remaining = b.limit - b.queue.len();
          b.queue.extend(&buf[..remaining]);
        } else {
          b.queue.extend(buf);
        }
        Ok(buf.len())
      }
      Self::Read(_) => Err(ebadf()),
    }
  }
  fn was_truncated(&self) -> bool {
    match self {
      Self::Write(b) | Self::Read(b) => b.lock().unwrap().truncated,
    }
  }
  fn poll(&self, _timeout: Option<PollTimeout>) -> io::Result<usize> {
    match self {
      Self::Read(b) => Ok(b.lock().unwrap().queue.len()),
      Self::Write(_) => Err(ebadf()),
    }
  }
  fn flush(&self) -> io::Result<()> {
    Ok(())
  }
  fn as_os_fd(&self) -> io::Result<BorrowedFd<'_>> {
    // PipeSink is an in-process-only construct
    // cross-fork pipelines use real pipes backed by OsSink
    Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "PipeSink cannot be downcast to an OS-level file descriptor",
    ))
  }
  fn kind(&self) -> SinkKind {
    SinkKind::Pipe
  }
}

impl Drop for PipeSink {
  fn drop(&mut self) {
    if let Self::Write(b) = self {
      // EOF equivalent
      b.lock().unwrap().writer_open = false;
    }
  }
}

pub(crate) struct ThreadPipe {
  buf: Mutex<PipeBuf>,
  notif: Condvar,
}

impl ThreadPipe {
  pub(crate) fn new() -> Self {
    Self {
      buf: Mutex::new(PipeBuf::new()),
      notif: Condvar::new(),
    }
  }
}

pub(crate) enum ThreadSink {
  Read(Arc<ThreadPipe>),
  Write(Arc<ThreadPipe>),
}

impl ThreadSink {
  pub(crate) fn new() -> (Self, Self) {
    let r = Arc::new(ThreadPipe::new());
    let w = Arc::clone(&r);
    (Self::Read(r), Self::Write(w))
  }

  fn pipe_lock(&mut self) -> std::sync::MutexGuard<'_, PipeBuf> {
    match self {
      ThreadSink::Read(p) | ThreadSink::Write(p) => p.buf.lock().unwrap(),
    }
  }

  fn notify_all(&self) {
    match self {
      ThreadSink::Read(p) | ThreadSink::Write(p) => p.notif.notify_all(),
    }
  }

  fn is_writer(&self) -> bool {
    matches!(self, ThreadSink::Write(_))
  }

  fn close(&mut self) {
    let is_writer = self.is_writer();
    {
      let mut buf = self.pipe_lock();
      if is_writer {
        buf.writer_open = false;
      } else {
        buf.reader_open = false;
      }
    }
    self.notify_all();
  }
}

impl Sink for ThreadSink {
  fn read(&self, out: &mut [u8]) -> io::Result<usize> {
    let Self::Read(pipe) = self else {
      return Err(ebadf());
    };
    let mut buf = pipe.buf.lock().unwrap();

    buf = pipe
      .notif
      .wait_while(buf, PipeBuf::is_empty_and_writable)
      .unwrap();

    if buf.queue.is_empty() {
      return Ok(0); // buf.writer_open is false, so we got EOF
    }

    let n = out.len().min(buf.queue.len());
    for (slot, byte) in out.iter_mut().zip(buf.queue.drain(..n)) {
      *slot = byte;
    }
    self.notify_all();
    Ok(n)
  }
  fn write(&self, data: &[u8]) -> io::Result<usize> {
    let Self::Write(pipe) = self else {
      return Err(ebadf());
    };
    let mut buf = pipe.buf.lock().unwrap();

    if !buf.reader_open {
      return Err(io::Error::from(io::ErrorKind::BrokenPipe));
    }

    buf = pipe
      .notif
      .wait_while(buf, PipeBuf::is_full_and_readable)
      .unwrap();

    if !buf.reader_open {
      return Err(io::Error::from(io::ErrorKind::BrokenPipe));
    }

    buf.queue.extend(data);
    self.notify_all();
    Ok(data.len())
  }
  fn as_os_fd(&self) -> io::Result<BorrowedFd<'_>> {
    Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "ThreadSink is in-process only",
    ))
  }
  fn kind(&self) -> SinkKind {
    SinkKind::Pipe
  }
  fn flush(&self) -> io::Result<()> {
    Ok(())
  }
  fn was_truncated(&self) -> bool {
    false
  }
  fn poll(&self, timeout: Option<PollTimeout>) -> io::Result<usize> {
    let Self::Read(pipe) = self else {
      return Err(ebadf());
    };
    let mut buf = pipe.buf.lock().unwrap();
    let timeout_dur = timeout
      .filter(PollTimeout::is_some)
      .and_then(|t| t.duration());

    // a blocking poll waits for data or the writer's close instead of reporting
    // the queue as empty. A concurrent writer thread has usually not produced yet
    // when the reader first polls.
    buf = match timeout_dur {
      None => pipe
        .notif
        .wait_while(buf, PipeBuf::is_empty_and_writable)
        .unwrap(),

      Some(dur) => {
        pipe
          .notif
          .wait_timeout_while(buf, dur, PipeBuf::is_empty_and_writable)
          .unwrap()
          .0
      }
    };

    Ok(buf.queue.len())
  }
}

impl Drop for ThreadSink {
  fn drop(&mut self) {
    self.close();
  }
}

pub(crate) struct OsSink {
  fd: OwnedFd,
  is_tty: bool,
}
impl OsSink {
  pub(crate) fn new(fd: OwnedFd) -> Self {
    Self {
      // cache the isatty() call, so further checks are trivial field reads instead of syscalls
      // TODO: test whether calling this on every fd once is actually faster than just calling it on the fd when needed
      is_tty: fd.is_terminal(),

      fd,
    }
  }
}
impl Sink for OsSink {
  fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
    unistd::read(self.fd.as_fd(), buf).map_err(|e| io::Error::from_raw_os_error(e as i32))
  }
  fn write(&self, buf: &[u8]) -> io::Result<usize> {
    unistd::write(self.fd.as_fd(), buf).map_err(|e| io::Error::from_raw_os_error(e as i32))
  }
  fn poll(&self, timeout: Option<PollTimeout>) -> io::Result<usize> {
    let mut fds = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN)];

    match nix::poll::poll(&mut fds, timeout) {
      Ok(n) => Ok(n as usize),
      Err(e) => Err(io::Error::from_raw_os_error(e as i32)),
    }
  }
  fn flush(&self) -> io::Result<()> {
    Ok(())
  }
  fn as_os_fd(&self) -> io::Result<BorrowedFd<'_>> {
    Ok(self.fd.as_fd())
  }
  fn has_data(&self) -> bool {
    let mut nbytes: nix::libc::c_int = 0;
    unsafe { fionread(self.fd.as_raw_fd(), &raw mut nbytes) }.is_ok() && nbytes > 0
  }
  fn kind(&self) -> SinkKind {
    if self.is_tty {
      SinkKind::Tty
    } else {
      SinkKind::Os
    }
  }
  fn seek(&self, pos: io::SeekFrom) -> io::Result<u64> {
    let (whence, off) = match pos {
      io::SeekFrom::Current(o) => (unistd::Whence::SeekCur, o),
      io::SeekFrom::Start(o) => (unistd::Whence::SeekSet, o as i64),
      io::SeekFrom::End(o) => (unistd::Whence::SeekEnd, o),
    };

    unistd::lseek(self.fd.as_fd(), off, whence)
      .map(|p| p as u64)
      .map_err(|e| io::Error::from_raw_os_error(e as i32))
  }
}

/// Internal `/dev/null` equivalent
///
/// Redirections to `/dev/null` create one of these
pub(crate) struct NullSink {
  devnull_fd: OnceLock<OwnedFd>,
}
impl NullSink {
  pub(crate) fn new() -> Self {
    Self {
      devnull_fd: OnceLock::new(),
    }
  }
}
impl Sink for NullSink {
  fn read(&self, _buf: &mut [u8]) -> io::Result<usize> {
    Ok(0)
  }

  fn write(&self, buf: &[u8]) -> io::Result<usize> {
    Ok(buf.len())
  }

  fn flush(&self) -> io::Result<()> {
    Ok(())
  }

  fn as_os_fd(&self) -> io::Result<BorrowedFd<'_>> {
    match self.devnull_fd.get() {
      None => {
        let devnull = fcntl::open("/dev/null", OFlag::O_RDWR | OFlag::O_CLOEXEC, Mode::empty())?;
        let _ = self.devnull_fd.set(devnull);
        self.as_os_fd()
      }
      Some(fd) => Ok(fd.as_fd()),
    }
  }

  fn kind(&self) -> SinkKind {
    SinkKind::Null
  }

  fn poll(&self, _timeout: Option<PollTimeout>) -> io::Result<usize> {
    Ok(0)
  }
}

/// A sink that always returns EBADF for all operations.
///
/// Used for `N>&-` redirections, and redirs that open new file descriptors
/// are replaced with this after closing in the fd table, instead of having their
/// entries removed.
pub(crate) struct CloseSink;
impl Sink for CloseSink {
  fn read(&self, _buf: &mut [u8]) -> io::Result<usize> {
    Err(ebadf())
  }

  fn write(&self, _buf: &[u8]) -> io::Result<usize> {
    Err(ebadf())
  }

  fn flush(&self) -> io::Result<()> {
    Err(ebadf())
  }

  fn as_os_fd(&self) -> io::Result<BorrowedFd<'_>> {
    Err(ebadf())
  }

  fn kind(&self) -> SinkKind {
    SinkKind::Close
  }

  fn poll(&self, _timeout: Option<PollTimeout>) -> io::Result<usize> {
    Err(ebadf())
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SinkKind {
  Buffer,
  Os,
  Tty,
  Pipe,
  Null,
  Close,
}

/// Adapter struct for implementing `io::Write`, `io::Read`, and `fmt::Write` for [`Sink`].
///
/// Necessary because the signatures require `&mut self`, and `Sink` is immutable behind an Rc.
pub(crate) struct SinkIo(pub Arc<dyn Sink>);

impl io::Read for SinkIo {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    self.0.read(buf)
  }
}

impl io::Write for SinkIo {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.0.write(buf)
  }
  fn flush(&mut self) -> io::Result<()> {
    self.0.flush()
  }
}

impl std::fmt::Write for SinkIo {
  fn write_str(&mut self, s: &str) -> std::fmt::Result {
    io::Write::write_all(self, s.as_bytes()).map_err(|_| std::fmt::Error)
  }
}

/// The virtual fd table that `shed` uses for I/O redirection
///
/// The wrapped `table` is a [`HashMap`] of [`RawFd`] -> [`Arc<dyn Sink>`]. The `RawFd` is the target fd (e.g. 0 for stdin, 1 for stdout, etc.), and the [`Sink`] is the source of data for that fd.
/// The Sink can be an OS-level fd (e.g. a file or pipe), or it can be an in-process buffer (e.g. a heredoc or here-string).
/// The Sink trait allows us to use our own I/O channels in the same way that we use file descriptors.
///
/// The table itself is interacted with arbitrarily using [`Shed::sinks()`] which allows for passing a closure that operates
/// on a mutable reference to the [`Shed`] struct's `Sinks` instance.
/// In general, the table operates by passing out [`RedirGuard`]s whenever a redirection happens. Any existing Arc<dyn Sink> for a given fd is stored on the `RedirGuard`, and when the `RedirGuard` is dropped, the old Sink is restored to the table. This allows for arbitrarily nested redirections.
///
/// The table's held redirections are not actually applied until a child forks; this keeps the parent process's fds intact,
/// similar to how exported variables are not actually applied to the environment until a child process is spawned.
#[derive(Clone)]
pub(crate) struct Sinks {
  table: HashMap<RawFd, Arc<dyn Sink>>,
}

impl Debug for Sinks {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    // dyn Sink doesn't implement Debug, so we can't print the table directly.
    // Instead, we can print the fd and the kind of sink.
    let table = self
      .table
      .iter()
      .map(|(fd, sink)| (fd, sink.kind()))
      .collect::<Vec<_>>();

    f.debug_struct("Sinks").field("table", &table).finish()
  }
}

impl Sinks {
  pub(crate) fn new() -> Self {
    let mut table: HashMap<RawFd, Arc<dyn Sink>> = HashMap::default();

    // seed the standard streams; a stream that was closed at launch (e.g. the
    // shell started with fd 1 shut) simply stays absent -> EBADF on use, which
    // matches the inherited state. dup can't meaningfully fail otherwise.
    for fd in [STDIN_FILENO, STDOUT_FILENO, STDERR_FILENO] {
      if let Ok(sink) = Self::base(fd) {
        table.insert(fd, sink);
      }
    }

    Self { table }
  }
  pub(crate) fn sink_pipes() -> (Arc<dyn Sink>, Arc<dyn Sink>) {
    let (read, write) = PipeSink::new();
    let read = Arc::new(read);
    let write = Arc::new(write);
    (read, write)
  }
  pub(crate) fn os_pipes() -> io::Result<(Arc<dyn Sink>, Arc<dyn Sink>)> {
    let (r, w) = pipes_high()?;
    let r = Arc::new(OsSink::new(r));
    let w = Arc::new(OsSink::new(w));
    Ok((r, w))
  }
  pub(crate) fn thread_pipes() -> (Arc<dyn Sink>, Arc<dyn Sink>) {
    let (read, write) = ThreadSink::new();
    let read = Arc::new(read);
    let write = Arc::new(write);
    (read, write)
  }
  /// Get an empty redir guard
  pub(crate) fn redir_scope() -> RedirGuard {
    RedirGuard::new()
  }
  /// Applies the stored redirections to the shell process' kernel fd table.
  ///
  /// This is called after a child is forked, so that the child inherits the redirected fds.
  /// The parent process's fds are not affected in this case.
  pub(crate) fn commit_redirects(&self) -> io::Result<()> {
    for (target_fd, sink) in &self.table {
      if sink.kind() == SinkKind::Close {
        // try closing it
        let _ = unistd::close(*target_fd);
        continue;
      }
      let sink_fd = sink.as_os_fd()?;

      // call into_raw_fd() here to get the fd out of OwnedFd so it doesn't close on drop
      let _ = unsafe { unistd::dup2_raw(sink_fd, *target_fd)? }.into_raw_fd();
    }
    Ok(())
  }

  /// Close inherited pipe leftovers by hand
  pub(crate) fn close_orphan_pipes(&self) {
    // Per-process fd directory: procfs on Linux, fdescfs on the BSDs/macOS.
    #[cfg(linux_like)]
    const FD_DIR: &str = "/proc/self/fd";
    #[cfg(not(linux_like))]
    const FD_DIR: &str = "/dev/fd";

    let keep: HashSet<RawFd> = self
      .table
      .values()
      .filter_map(|s| s.as_os_fd().ok().map(|fd| fd.as_raw_fd()))
      .collect();

    let Ok(entries) = std::fs::read_dir(FD_DIR) else {
      return;
    };
    let orphans: Vec<RawFd> = entries
      .filter_map(Result::ok)
      .filter_map(|e| e.file_name().to_str()?.parse::<RawFd>().ok())
      .filter(|fd| *fd >= MIN_INTERNAL_FD && !keep.contains(fd))
      .collect();

    for fd in orphans {
      let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };

      let cloexec = fcntl(borrowed, FcntlArg::F_GETFD)
        .is_ok_and(|f| FdFlag::from_bits_truncate(f).contains(FdFlag::FD_CLOEXEC));
      if !cloexec {
        continue;
      }

      let mut st: libc::stat = unsafe { std::mem::zeroed() };
      if unsafe { libc::fstat(fd, &raw mut st) } != 0 {
        continue;
      }

      if st.st_mode & libc::S_IFMT == libc::S_IFIFO {
        let _ = unistd::close(fd);
      }
    }
  }
  pub(crate) fn get(&mut self, fd: RawFd) -> Option<Arc<dyn Sink>> {
    if let Some(s) = self.table.get(&fd) {
      return Some(s.clone());
    }

    let owned = dup_high(unsafe { BorrowedFd::borrow_raw(fd) }).ok()?;
    let sink: Arc<dyn Sink> = Arc::new(OsSink::new(owned));
    self.table.insert(fd, sink.clone());
    Some(sink)
  }
  pub(crate) fn get_stdin(&mut self) -> Option<Arc<dyn Sink>> {
    self.get(0)
  }
  pub(crate) fn get_stdout(&mut self) -> Option<Arc<dyn Sink>> {
    self.get(1)
  }
  pub(crate) fn get_stderr(&mut self) -> Option<Arc<dyn Sink>> {
    self.get(2)
  }
  pub(crate) fn apply_sink(sink: Arc<dyn Sink>, fd: RawFd) -> ShResult<RedirGuard> {
    RedirGuard::from_sink(sink, fd)
  }
  pub(crate) fn apply_set(s: &RedirSet) -> ShResult<RedirGuard> {
    RedirGuard::from_redirs(s)
  }
  pub(crate) fn try_apply_set(s: &RedirSet, fatal: bool) -> ShResult<Option<RedirGuard>> {
    RedirGuard::try_from_redirs(s, fatal)
  }
  pub(crate) fn redirect(&mut self, fd: RawFd, sink: Arc<dyn Sink>) -> Option<Arc<dyn Sink>> {
    // getting 'None' here is equivalent to closing the fd, which is valid
    self.table.insert(fd, sink)
  }
  pub(crate) fn input_available(&mut self) -> bool {
    match self.get_stdin() {
      None => false,
      Some(sink) => sink.has_data(),
    }
  }
  fn base(fd: RawFd) -> io::Result<Arc<dyn Sink>> {
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let owned = dup_high(borrowed)?;
    Ok(Arc::new(OsSink::new(owned)))
  }
}

enum RedirResult {
  Success,
  Fail,
}

impl RedirResult {
  pub(crate) fn failed(&self) -> bool {
    matches!(self, RedirResult::Fail)
  }
}

pub(crate) struct RedirGuard {
  saved: Vec<(RawFd, Arc<dyn Sink>)>,
  active: bool,
}

impl Debug for RedirGuard {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let saved = self
      .saved
      .iter()
      .map(|(fd, sink)| (fd, sink.kind()))
      .collect::<Vec<_>>();
    f.debug_struct("RedirGuard")
      .field("saved", &saved)
      .field("active", &self.active)
      .finish()
  }
}

impl RedirGuard {
  fn new() -> Self {
    Self {
      saved: Vec::new(),
      active: true,
    }
  }

  fn from_sink(sink: Arc<dyn Sink>, fd: RawFd) -> ShResult<Self> {
    let mut guard = Self::new();
    guard.apply_sink(fd, sink)?;
    Ok(guard)
  }

  fn from_redirs(redirs: &RedirSet) -> ShResult<Self> {
    let mut guard = Self::new();
    // on error the guard drops here and its Drop restores the partial redirs.
    guard.apply_set(redirs)?;
    Ok(guard)
  }

  fn try_from_redirs(redirs: &RedirSet, fatal: bool) -> ShResult<Option<Self>> {
    let mut guard = Self::new();
    if guard.try_apply_set(redirs, fatal)?.failed() {
      Ok(None)
    } else {
      Ok(Some(guard))
    }
  }

  pub(crate) fn apply_sink(&mut self, fd: RawFd, sink: Arc<dyn Sink>) -> ShResult<()> {
    validate_fd(fd)?;

    Shed::sinks(|sinks| {
      if !self.saved.iter().any(|(f, _)| *f == fd) {
        // only save once
        self
          .saved
          .push((fd, sinks.get(fd).unwrap_or_else(|| Arc::new(CloseSink))));
      }
      sinks.redirect(fd, sink);
    });

    Ok(())
  }

  /// Apply a redirection spec
  ///
  /// Swaps the current sink for the target fd with the new sink specified by the redirection spec.
  /// Swaps it back on drop, unless [`RedirGuard::persist()`] is called.
  pub(crate) fn apply(&mut self, r: &RedirSpec) -> ShResult<()> {
    let fd = r.target_fd();
    // resolve before taking the borrow: as_sink runs expansion, which can run
    // command substitutions that re-enter the table.
    let sink = r.as_sink()?;
    self.apply_sink(fd, sink)
  }

  fn try_apply_set(&mut self, s: &RedirSet, fatal: bool) -> ShResult<RedirResult> {
    if let Err(e) = self.apply_set(s) {
      self.restore_into();
      if fatal {
        return Err(e);
      }
      e.print_error();
      Shed::set_status(1);
      return Ok(RedirResult::Fail);
    }
    Ok(RedirResult::Success)
  }

  pub(crate) fn apply_set(&mut self, s: &RedirSet) -> ShResult<()> {
    for r in s.specs() {
      self.apply(r)?;
    }

    Ok(())
  }

  /// Unwind the applied redirs immediately and disarm, so the eventual `Drop`
  /// is a no-op.
  fn restore_into(&mut self) {
    Shed::sinks(|sinks| {
      for (fd, old) in self.saved.drain(..).rev() {
        sinks.redirect(fd, old);
      }
    });
    self.active = false;
  }

  /// Drop the guard without restoring the redirections
  ///
  /// Used by contexts like the `exec` builtin
  pub(crate) fn persist(mut self) {
    self.active = false;

    // i know it happens anyway, just making it obvious that
    // this is the intention
    std::mem::drop(self);
  }
}

impl Drop for RedirGuard {
  fn drop(&mut self) {
    if !self.active {
      return;
    }

    self.restore_into();
  }
}

pub(crate) fn stdin_is_tty() -> bool {
  Shed::sinks(Sinks::get_stdin).is_some_and(|s| s.isatty())
}

// TODO: drop impl

pub(super) fn stdin_fileno() -> BorrowedFd<'static> {
  unsafe { BorrowedFd::borrow_raw(STDIN_FILENO) }
}

pub(super) fn stdin_sink() -> ShResult<Arc<dyn Sink>> {
  Shed::sinks(Sinks::get_stdin).ok_or_else(|| ShErr::from(ebadf()))
}

pub(crate) fn stdout_sink() -> ShResult<Arc<dyn Sink>> {
  Shed::sinks(Sinks::get_stdout).ok_or_else(|| ShErr::from(ebadf()))
}

pub(crate) fn stderr_sink() -> ShResult<Arc<dyn Sink>> {
  Shed::sinks(Sinks::get_stderr).ok_or_else(|| ShErr::from(ebadf()))
}

pub(crate) struct CappedRead {
  buf: Vec<u8>,
  limit: ReadLimit,
  was_truncated: bool,
}

impl CappedRead {
  pub(crate) fn was_truncated(&self) -> bool {
    self.was_truncated
  }
  pub(crate) fn into_inner(self) -> Vec<u8> {
    self.buf
  }
  pub(crate) fn limit(&self) -> ReadLimit {
    self.limit
  }
}

impl Deref for CappedRead {
  type Target = Vec<u8>;
  fn deref(&self) -> &Self::Target {
    &self.buf
  }
}

pub(crate) fn read_capped(fd: BorrowedFd) -> ShResult<CappedRead> {
  let limit = shopt!(core.max_read_limit);

  let mut out = Vec::new();
  let mut buf = [0u8; 8192];
  let mut remaining = *limit as usize;
  let mut truncated = false;

  loop {
    match unistd::read(fd.as_fd(), &mut buf) {
      Ok(0) => break,
      Ok(n) => {
        let bytes_read = n.min(remaining);
        out.extend_from_slice(&buf[..bytes_read]);
        remaining = remaining.saturating_sub(bytes_read);
        if remaining == 0 {
          truncated = true;
          break;
        }
      }
      Err(Errno::EINTR) => {
        if signal::sigint_pending() {
          state::Shed::set_status(130);
          break;
        }
      }
      Err(e) => return Err(e.into()),
    }
  }

  Ok(CappedRead {
    buf: out,
    limit,
    was_truncated: truncated,
  })
}

/// Convert a vector of bytes to a string, replacing invalid UTF-8 sequences with the replacement character.
pub(super) fn bytes_to_string(buf: Vec<u8>) -> String {
  match String::from_utf8(buf) {
    Ok(s) => s,
    Err(e) => String::from_utf8_lossy(&e.into_bytes()).into_owned(),
  }
}

/// Write raw bytes to the current output sink, byte-native counterpart to `out!`.
pub(super) fn out_bytes(buf: &[u8]) {
  let Some(out) = Shed::sinks(Sinks::get_stdout) else {
    return;
  };
  SinkIo(out).write_all(buf).ok();
}

/// Write raw bytes followed by a newline, byte-native counterpart to `outln!`.
pub(super) fn outln_bytes(buf: &[u8]) {
  let Some(out) = Shed::sinks(Sinks::get_stdout) else {
    return;
  };
  let mut sink_io = SinkIo(out);
  sink_io.write_all(buf).ok();
  sink_io.write_all(b"\n").ok();
}

/// A pipe created before a fork to deliver stdin bytes to the child on fd 0.
///
/// Used to materialize an in-process pipeline stdin sink (or any byte buffer)
/// onto a real fd when a stage forks a child that reads stdin, e.g. an external
/// command inside a command substitution.
pub(crate) struct StdinPipe {
  read: OwnedFd,
  write: OwnedFd,
}

impl StdinPipe {
  /// Create the pipe. Call before forking.
  pub(crate) fn new() -> ShResult<Self> {
    let (read, write) = pipes_high()?;
    Ok(Self { read, write })
  }

  /// Child side: register the fd-0 dup into `specs`, drop the write end so the
  /// child sees EOF once the parent finishes feeding, and return the read end
  /// to keep alive until the redirs are applied.
  pub(crate) fn into_child(self, specs: &mut Vec<RedirSpec>) -> OwnedFd {
    specs.push(RedirSpec::dup(
      self.read.as_raw_fd(),
      STDIN_FILENO,
      RedirType::Input,
    ));
    drop(self.write);
    self.read
  }

  /// Parent side: drop the read end and return the write end for feeding.
  pub(crate) fn into_writer(self) -> OwnedFd {
    drop(self.read);
    self.write
  }
}

/// Read from the given file descriptor, then write the results to stdout
/// This process loops until the read returns EOF or returns some error.
pub(crate) fn stream_to_sink(fd: BorrowedFd) -> ShResult<()> {
  let Some(out) = Shed::sinks(Sinks::get_stdout) else {
    return Ok(());
  };
  let mut buf = [0u8; 8192]; // 8 KiB
  let mut sink = SinkIo(out);

  loop {
    match unistd::read(fd, &mut buf) {
      Ok(0) => break,
      Ok(n) => {
        if sink.write_all(&buf[..n]).is_err() {
          break; // downstream closed
        }
      }
      Err(Errno::EINTR) => signal::check_signals()?,
      Err(e) => return Err(e.into()),
    }
  }

  Ok(())
}

/// Write all of `bytes` to `fd`, tolerating `EINTR` and a child that closes its
/// end early (`EPIPE`, e.g. `head`). Does not close `fd`.
pub(crate) fn write_all_to_fd(fd: BorrowedFd, bytes: &[u8]) {
  let mut written = 0;
  while written < bytes.len() {
    match write(fd, &bytes[written..]) {
      Ok(0) | Err(Errno::EPIPE) => break,
      Ok(n) => written += n,
      Err(Errno::EINTR) => {
        if signal::sigint_pending() {
          state::Shed::set_status(130);
          break;
        }
      }
      Err(_) => break,
    }
  }
}

/// Write all of `bytes` to `fd`, surfacing failures to the caller.
///
/// The checked counterpart to [`write_all_to_fd`]: instead of silently stopping,
/// a hung-up peer (`EPIPE`), a zero-length write, or any other write error is
/// returned as an `Err`. Retries on `EINTR`, propagating a pending signal
/// through [`signal::check_signals`]. Does not close `fd`.
pub(crate) fn write_all_to_fd_checked(fd: BorrowedFd, bytes: &[u8]) -> ShResult<()> {
  let mut written = 0;
  while written < bytes.len() {
    match write(fd, &bytes[written..]) {
      Ok(0) => return Err(sherr!(ExecFail, "write to fd returned zero bytes")),
      Ok(n) => written += n,
      Err(Errno::EINTR) => signal::check_signals()?,
      Err(e) => return Err(e.into()),
    }
  }
  Ok(())
}

/// Run a command in a child process, feeding it `stdin` if provided, and capturing its stdout into a string.
/// Returns the captured output or an error if the command failed to execute or was terminated abnormally.
pub(super) fn capture_command(
  cmd: &[u8],
  stdin: Option<&[u8]>,
  name: Option<&VarStr>,
) -> ShResult<String> {
  let (rpipe, wpipe) = pipes_high()?;
  let stdin_pipe = if stdin.is_some() {
    Some(StdinPipe::new()?)
  } else {
    None
  };

  match unsafe { fork()? } {
    ForkResult::Child => {
      lifecycle::setup_child();

      let mut specs = vec![RedirSpec::dup(wpipe.as_raw_fd(), 1, RedirType::Output)];
      // Keep the read end alive until redirs.apply() dups it onto fd 0.
      let _stdin_r_keep_alive = stdin_pipe.map(|p| p.into_child(&mut specs));
      let redirs: RedirSet = specs.into();
      // TODO: make sure this is the correct migration for "or_fatal()?"
      let _guard = Sinks::apply_set(&redirs);

      execute::catch_exit(
        || execute::exec_nonint(cmd.into(), name.cloned()),
        |code| unsafe { nix::libc::_exit(code) },
      );

      let status = state::Shed::get_status();
      unsafe { nix::libc::_exit(status) };
    }
    ForkResult::Parent { child } => {
      drop(wpipe);

      // Feed stdin from a thread while we read stdout here; writing it all
      // first would deadlock once both pipes fill. We borrow `stdin` (rather
      // than owning bytes) so a scoped thread is used instead of feed_fd_async.
      let sink = if let Some(pipe) = stdin_pipe {
        let writer = pipe.into_writer();
        let bytes = stdin.unwrap().as_bytes();
        std::thread::scope(|scope| {
          scope.spawn(move || {
            write_all_to_fd(writer.as_fd(), bytes);
            // Closing the write end signals EOF to the child's stdin.
            drop(writer);
          });
          read_capped(rpipe.as_fd())
        })?
      } else {
        read_capped(rpipe.as_fd())?
      };
      let truncated = sink.was_truncated();
      let size = sink.limit();
      let captured = bytes_to_string(sink.into_inner());

      let status = loop {
        match waitpid(child, Some(WtFlag::WUNTRACED)) {
          Ok(status) => break status,
          Err(Errno::EINTR) => (),
          Err(e) => return Err(e.into()),
        }
      };

      match status {
        WtStat::Exited(_, code) => {
          state::Shed::set_status(code);
          if truncated {
            state::Shed::set_status(SINK_TRUNCATED_STATUS);
            crate::errln!("shed: command output truncated (exceeded {size})");
          }
          Ok(captured)
        }
        _ => Err(sherr!(InternalErr, "Command sub failed")),
      }
    }
  }
}
fn expand_fd(word: &Tk) -> ShResult<Option<RawFd>> {
  let span = word.span;
  let words = word
    .clone()
    .expand()
    .map(|tk| tk.get_words())
    .unwrap_or_default();

  if words.len() != 1 {
    return Err(sherr!(
        ExecFail @ span,
        "ambiguous redirect: file descriptor must expand to a single word"
    ));
  }
  let word_val = words.iter().next().unwrap();
  let word_val = word_val.to_str_lossy();
  let src = word_val.trim();

  // A word that expands to `-` closes the target fd, mirroring `>&-`.
  if src == "-" {
    return Ok(None);
  }

  let from = src.parse::<RawFd>().map_err(|_| {
    sherr!(
      ExecFail @ span,
      "ambiguous redirect: `{src}` is not a valid file descriptor"
    )
  })?;

  Ok(Some(from))
}
/// Open a file for redirection, respecting the `noclobber` shell option for output redirections.
pub(super) fn open_redir_file(class: RedirType, path: &VarStr) -> ShResult<OwnedFd> {
  let file: OwnedFd = get_redir_file(class, path)?.into();
  let file = move_high(file)?;
  Ok(file)
}

/// Open a file for redirection, respecting the `noclobber` shell option for output redirections.
pub(super) fn get_redir_file<P: AsRef<Path>>(class: RedirType, path: P) -> ShResult<File> {
  let path = path.as_ref();
  let result = match class {
    RedirType::Input => OpenOptions::new().read(true).open(Path::new(&path)),
    RedirType::Output => {
      if shopt!(set.noclobber) && path.is_file() {
        return Err(sherr!(
          ExecFail,
          "shopt core.noclobber is set, refusing to overwrite existing file `{}`",
          path.display()
        ));
      }
      OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
    }
    RedirType::ReadWrite => OpenOptions::new()
      .write(true)
      .read(true)
      .create(true)
      .truncate(false)
      .open(path),
    RedirType::OutputForce => OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .open(path),
    RedirType::Append => OpenOptions::new().create(true).append(true).open(path),
    _ => unimplemented!("Unimplemented redir type: {:?}", class),
  };
  Ok(result?)
}

/// Read all bytes from stdin into a vector, returning an error if the read fails.
/// If a SIGINT is pending, set the status to 130 and return an empty vector.
pub(super) fn read_input() -> ShResult<Vec<u8>> {
  let _guard = stdin_is_tty().then(|| Shed::term_mut(Terminal::prepare_for_exec));
  let sink = stdin_sink()?;

  let mut input = vec![];
  let mut read_buf = [0u8; 4096];

  loop {
    match sink.read(&mut read_buf) {
      Ok(0) => break,
      Ok(n) => input.extend_from_slice(&read_buf[..n]),
      Err(e) if e.kind() == io::ErrorKind::Interrupted => {
        if signal::sigint_pending() {
          state::Shed::set_status(130);
          return Ok(vec![]);
        }
      }
      Err(e) => {
        return Err(sherr!(InternalErr, "error reading from stdin: {e}"));
      }
    }
  }

  Ok(input)
}

#[cfg(test)]
pub(crate) mod tests {
  use crate::tests::testutil::{TestGuard, has_cmd, has_cmds, test_input};
  use pretty_assertions::assert_eq;

  // Run a command line and assert its captured stdout. `needs` skips the test
  // when the listed external commands aren't installed.
  macro_rules! run_output {
    ($( $name:ident : $cmd:literal => $out:literal $(, needs $($needs:literal),+)? ; )*) => {
      $(
        #[test]
        fn $name() {
          $( if !has_cmds(&[$($needs),+]) { return; } )?
          let g = TestGuard::new();
          test_input($cmd).unwrap();
          assert_eq!(g.read_output(), $out, "{}", stringify!($name));
        }
      )*
    };
  }

  run_output! {
    pipeline_simple        : "echo foo | sed 's/foo/bar/'" => "bar\n", needs "sed";
    pipeline_multi         : "echo foo bar baz | cut -d ' ' -f 2 | sed 's/a/A/'" => "bAr\n", needs "cut", "sed";
    rube_goldberg_pipeline : "{ echo foo; echo bar } | if cat; then :; else echo failed; fi | (read line && echo $line | sed 's/foo/baz/'; sed 's/bar/buzz/')" => "baz\nbuzz\n", needs "sed", "cat";
    pipe_and_stderr        : "echo on stderr >&2 |& cat" => "on stderr\n", needs "cat";
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
    let status = crate::state::Shed::get_status();
    assert_eq!(status, 0);

    test_input("cat < /dev/null | false").unwrap();

    let status = crate::state::Shed::get_status();
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

  // ===================== capture_command =====================

  use super::capture_command;
  use crate::state;

  macro_rules! capture_test {
    ($(
      $name:ident : $cmd:literal $(, stdin $stdin:literal)? $(, needs $needs:literal)?
          => $out:literal $(, $fail:ident)? ;
    )*) => {
      $(
        #[test]
        fn $name() {
          let _g = TestGuard::new();
          $( if !has_cmd($needs) { return; } )?
          let stdin: Option<&[u8]> = None $(.or(Some($stdin.as_bytes())))?;
          let out = capture_command($cmd.as_bytes(), stdin, None).unwrap();
          assert_eq!(out, $out, "{}: output", stringify!($name));
          $( let _ = stringify!($fail);
             assert_ne!(state::Shed::get_status(), 0, "{}: status", stringify!($name)); )?
        }
      )*
    };
  }

  capture_test! {
    capture_simple_echo                          : "echo hello" => "hello\n";
    capture_preserves_internal_newlines          : "printf 'one\\ntwo\\nthree'" => "one\ntwo\nthree";
    capture_empty_output                         : "true" => "";
    capture_command_sets_exit_status             : "false" => "", fails;
    capture_nonzero_status_still_captures_output : "echo before-fail; false" => "before-fail\n", fails;
    capture_feeds_stdin_to_command               : "cat", stdin "piped input", needs "cat" => "piped input";
    capture_stdin_with_multiline_input           : "cat", stdin "line1\nline2\nline3\n", needs "cat" => "line1\nline2\nline3\n";
    capture_stdin_seen_by_read_builtin           : "read x; echo \"got=$x\"", stdin "hello world\n" => "got=hello world\n";
  }

  // Note: there's no `no-stdin → child sees EOF` test because TestGuard
  // keeps its stdin write-end open for the lifetime of the test. A child
  // reading from the inherited stdin pipe would block forever waiting on
  // data nobody is closing. The `Option<&str>` stdin parameter handles
  // the genuinely-disconnected case in production via the
  // `stdin_pipes.is_some()` check at the top of capture_command.
}
