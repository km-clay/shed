use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Debug,
  fs::{File, OpenOptions},
  io::{self, Cursor, Read, Write},
  os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
  path::Path,
  rc::Rc,
  str::FromStr,
};

use nix::{
  errno::Errno,
  fcntl::{FcntlArg, OFlag, fcntl, open},
  libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO},
  sys::{
    stat::Mode,
    wait::{WaitPidFlag as WtFlag, WaitStatus as WtStat, waitpid},
  },
  unistd::{self, ForkResult, fork, isatty, read, write},
};

use crate::{
  Shed, signal,
  state::{shopt::ReadLimit, terminal::Terminal},
  util,
};

use super::{
  eval::{
    execute::exec_nonint,
    lex::{Span, Tk, TkFlags},
  },
  expand::Expander,
  match_loop, sherr, shopt, state,
  util::{ShErr, ShErrKind, ShResult},
};

/*
 * This module contains our IO redirection primitives.
 * Everything we use is basically just a thin wrapper around the std Fd types,
 * or nix system call wrappers.
 */

/// Minimum fd number for shell-internal file descriptors.
/// User-visible fds (0-9) are kept clear so `exec 3>&-` etc. work as expected.
pub const MIN_INTERNAL_FD: RawFd = 10;

/// The status code returned when a builtin command's output is truncated
/// due to exceeding the maximum size of the OutputSink
pub const SINK_TRUNCATED_STATUS: i32 = 122;

/// Like `dup()`, but places the new fd at `MIN_INTERNAL_FD` or above so it
/// doesn't collide with user-managed fds.
pub fn dup_high(fd: BorrowedFd) -> nix::Result<OwnedFd> {
  let fd = fcntl(fd, FcntlArg::F_DUPFD_CLOEXEC(MIN_INTERNAL_FD))?;
  unsafe { Ok(OwnedFd::from_raw_fd(fd)) }
}

fn dup_high_no_cloexec(fd: BorrowedFd) -> nix::Result<OwnedFd> {
  let fd = fcntl(fd, FcntlArg::F_DUPFD(MIN_INTERNAL_FD))?;
  Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[expect(clippy::needless_pass_by_value)]
/// Like `dup_high()` but takes and closes an existing `OwnedFd`.
pub fn move_high(fd: OwnedFd) -> nix::Result<OwnedFd> {
  let new_fd = dup_high(fd.as_fd())?;
  Ok(new_fd)
} // fd is closed here

#[expect(clippy::needless_pass_by_value)]
fn move_high_no_cloexec(fd: OwnedFd) -> nix::Result<OwnedFd> {
  let new_fd = dup_high_no_cloexec(fd.as_fd())?;
  Ok(new_fd)
}

/// `SQLite` opens long-lived file descriptors on its own and we cant call `move_high` on them.
///
/// These files usually end up polluting the user-space 3-10 range which we work so hard to keep clear
/// so that users can open resources on those file descriptors without any weirdness happening.
///
/// Later on we will probably have to do something like using a custom sqlite VFS
/// to limit the fd numbers it can use, but for now this will do. I guess.
pub fn do_something_that_opens_fds_that_we_cant_access_hack<F, T>(min_fd: RawFd, something: F) -> T
where
  F: FnOnce() -> T,
{
  // these close at the end of the function
  let _dummies = (3..min_fd)
    .filter_map(|_| {
      // painful to write
      open(
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
pub fn pipes_high() -> nix::Result<(OwnedFd, OwnedFd)> {
  let (r, w) = nix::unistd::pipe()?;
  Ok((move_high(r)?, move_high(w)?))
}

pub fn pipes_high_no_cloexec() -> nix::Result<(OwnedFd, OwnedFd)> {
  let (r, w) = nix::unistd::pipe()?;
  Ok((move_high_no_cloexec(r)?, move_high_no_cloexec(w)?))
}

/// Basically just a fancy deferred `dup2()` call.
///
/// If constructed using `Redir::close()`, this will close the target fd when applied.
#[derive(Debug)]
pub struct Redir {
  fd: RawFd,
  from: Option<OwnedFd>,
}

impl Redir {
  pub fn new(fd: RawFd, from: OwnedFd) -> Self {
    Self {
      fd,
      from: Some(from),
    }
  }
  pub fn close(fd: RawFd) -> Self {
    Self { fd, from: None }
  }
  pub fn apply(&mut self) -> ShResult<()> {
    if let Some(from) = &self.from {
      let ret = unsafe { nix::libc::dup2(from.as_raw_fd(), self.fd) };
      if ret < 0 {
        return Err(nix::Error::last().into());
      }
    } else {
      nix::unistd::close(self.fd)?;
    }
    Ok(())
  }
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
}

impl RedirBldr {
  pub fn new() -> Self {
    RedirBldr::default()
  }
  pub fn with_fd(self, fd: RawFd) -> Self {
    Self {
      fd: Some(fd),
      ..self
    }
  }
  pub fn with_class(self, class: RedirType) -> Self {
    Self {
      class: Some(class),
      ..self
    }
  }
  pub fn with_target(self, target: RedirTarget) -> Self {
    Self {
      target: Some(target),
      ..self
    }
  }
  pub fn with_span(self, span: Span) -> Self {
    Self {
      span: Some(span),
      ..self
    }
  }
  pub fn build(self) -> ShResult<RedirSpec> {
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
      RedirTarget::Fd(src_fd) if class.is_dup_op() => Ok(RedirSpec::dup(src_fd, fd, class)),
      RedirTarget::HereDoc { body, flags } => {
        log::debug!("heredoc body: {body:?}");
        // Strip leading tabs per line BEFORE expansion (POSIX order).
        let buf = if flags.contains(TkFlags::HERESTRING) {
          // Raw word; expanded and newline-terminated at redirection time.
          body
        } else if flags.contains(TkFlags::TAB_HEREDOC) {
          if body.is_empty() {
            String::new()
          } else {
            let stripped: Vec<&str> = body
              .lines()
              .map(|line| line.trim_start_matches('\t'))
              .collect();
            let mut s = stripped.join("\n");
            s.push('\n');
            s
          }
        } else {
          let mut s = body;
          if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
          }
          s
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

impl FromStr for RedirBldr {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let mut chars = s.chars().peekable();
    let mut src_fd = util::scratch_buf();
    let mut tgt_fd = util::scratch_buf();
    let mut redir = RedirBldr::new();

    match_loop!(chars.next() => ch, {
      '>' => {
        redir = redir.with_class(RedirType::Output);
        if let Some('>') = chars.peek() {
          chars.next();
          redir = redir.with_class(RedirType::Append);
        } else if let Some('|') = chars.peek() {
          chars.next();
          redir = redir.with_class(RedirType::OutputForce);
        }
      }
      '<' => {
        redir = redir.with_class(RedirType::Input);
        let mut count = 0;

        if chars.peek() == Some(&'>') {
          chars.next(); // consume the '>'
          redir = redir.with_class(RedirType::ReadWrite);
        } else {
          while count < 2 && matches!(chars.peek(), Some('<')) {
            chars.next();
            count += 1;
          }
        }

        redir = match count {
          1 => redir.with_class(RedirType::HereDoc),
          2 => redir.with_class(RedirType::HereString),
          _ => redir, // Default case remains RedirType::Input
        };
      }
      '&' => {
        if chars.peek() == Some(&'>') {
          continue
        } else if chars.peek() == Some(&'-') {
          chars.next();
          src_fd.push('-');
        } else {
          while let Some(next_ch) = chars.next() {
            if next_ch.is_ascii_digit() {
              src_fd.push(next_ch);
            } else {
              break;
            }
          }
        }
        if src_fd.is_empty() {
          return Err(sherr!(
              ParseErr,
              "Invalid character '{}' in redirection operator",
              ch,
          ));
        }
      }
      _ if ch.is_ascii_digit() && tgt_fd.is_empty() => {
        tgt_fd.push(ch);
        while let Some(next_ch) = chars.peek() {
          if next_ch.is_ascii_digit() {
            let next_ch = chars.next().unwrap();
            tgt_fd.push(next_ch);
          } else {
            break;
          }
        }
      }
      _ => {
        return Err(sherr!(
            ParseErr,
            "Invalid character '{}' in redirection operator",
            ch,
        ));
      }
    });

    let tgt_fd = tgt_fd
      .parse::<i32>()
      .unwrap_or_else(|_| match redir.class.unwrap() {
        RedirType::Input | RedirType::ReadWrite | RedirType::HereDoc | RedirType::HereString => 0,
        _ => 1,
      });
    redir = redir.with_fd(tgt_fd);
    if src_fd.as_str() == "-" {
      redir = redir.with_target(RedirTarget::Close);
    } else if let Ok(src_fd) = src_fd.parse::<i32>() {
      redir = redir.with_target(RedirTarget::Fd(src_fd));
    }
    Ok(redir)
  }
}

impl TryFrom<Tk> for RedirBldr {
  type Error = ShErr;
  fn try_from(tk: Tk) -> Result<Self, Self::Error> {
    let span = tk.span.clone();
    if tk.flags.contains(TkFlags::IS_HEREDOC) {
      let flags = tk.flags;

      Ok(RedirBldr {
        fd: Some(0),
        class: Some(RedirType::HereDoc),
        target: Some(RedirTarget::HereDoc {
          body: tk.to_string(),
          flags,
        }),
        span: Some(span),
      })
    } else {
      match Self::from_str(tk.as_str()) {
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
  pub fn is_input(self) -> bool {
    matches!(
      self,
      RedirType::Input | RedirType::HereDoc | RedirType::HereString | RedirType::ReadWrite
    )
  }
  pub fn is_output(self) -> bool {
    matches!(
      self,
      RedirType::Output | RedirType::OutputForce | RedirType::Append | RedirType::ReadWrite
    )
  }
  pub fn is_file_op(self) -> bool {
    matches!(
      self,
      RedirType::Output
        | RedirType::OutputForce
        | RedirType::Append
        | RedirType::Input
        | RedirType::ReadWrite
    )
  }
  pub fn is_dup_op(self) -> bool {
    matches!(self, RedirType::Output | RedirType::Input)
  }
}

#[derive(Clone, Debug)]
pub(super) enum RedirTarget {
  Path(Tk),
  Fd(RawFd),
  Close,
  HereDoc { body: String, flags: TkFlags },
}

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
  Close {
    fd: RawFd,
  },
  Buffer {
    fd: RawFd,
    buf: String,
    flags: TkFlags,
  },
}

impl RedirSpec {
  pub fn file(fd: RawFd, path: Tk, mode: RedirType) -> Self {
    Self::File { fd, path, mode }
  }
  pub fn dup(from: RawFd, to: RawFd, mode: RedirType) -> Self {
    Self::Dup { from, to, mode }
  }
  pub fn close(fd: RawFd) -> Self {
    Self::Close { fd }
  }
  pub fn buffer(fd: RawFd, buf: String, flags: TkFlags) -> Self {
    Self::Buffer { fd, buf, flags }
  }
  pub fn target_fd(&self) -> RawFd {
    match self {
      RedirSpec::Dup { to, .. } => *to,
      RedirSpec::File { fd, .. } | RedirSpec::Close { fd } | RedirSpec::Buffer { fd, .. } => *fd,
    }
  }
  pub fn mode(&self) -> RedirType {
    match self {
      RedirSpec::File { mode, .. } | RedirSpec::Dup { mode, .. } => *mode,
      RedirSpec::Close { .. } => RedirType::Null,
      RedirSpec::Buffer { .. } => RedirType::HereDoc,
    }
  }
  pub fn into_redir(self) -> ShResult<Redir> {
    match self {
      RedirSpec::File { fd, path, mode } => {
        let span = path.span.clone();
        let path = path
          .clone()
          .expand()
          .map(|tk| tk.get_words())
          .unwrap_or_default();

        if path.len() != 1 {
          return Err(sherr!(ExecFail @ span, "Redirection path must expand to exactly one word"));
        }

        let path = path.iter().next().unwrap();

        let file: OwnedFd = get_redir_file(mode, path)?.into();
        let file = move_high(file)?;
        Ok(Redir::new(fd, file))
      }
      RedirSpec::Dup { from, to, mode: _ } => {
        let borrowed = unsafe { BorrowedFd::borrow_raw(from) };
        let owned = borrowed
          .try_clone_to_owned()
          .map_err(|e| sherr!(InternalErr, "Failed to duplicate fd {}: {}", from, e))?;
        let owned = move_high(owned)?;
        Ok(Redir::new(to, owned))
      }
      RedirSpec::Close { fd } => Ok(Redir::close(fd)),
      RedirSpec::Buffer { fd, mut buf, flags } => {
        use io::{Seek, SeekFrom, Write};

        let file = tempfile::tempfile()
          .map_err(|e| sherr!(InternalErr, "heredoc tempfile creation failed: {e}"))?;
        let owned: OwnedFd = file.into();
        let owned = move_high(owned)?;

        if flags.contains(TkFlags::HERESTRING) {
          buf = Expander::from_raw(&buf, flags)
            .no_glob()
            .no_split()
            .expand_no_split()?
            .to_string();
          buf.push('\n');
        } else if flags.contains(TkFlags::IS_HEREDOC) && !flags.contains(TkFlags::LIT_HEREDOC) {
          buf = Expander::from_raw(&buf, flags)
            .expand()?
            .into_iter()
            .next()
            .map(|s| s.to_string())
            .unwrap_or_default();
        }

        let mut file = std::fs::File::from(owned);
        file
          .write_all(buf.as_bytes())
          .map_err(|e| sherr!(InternalErr, "heredoc write failed: {e}"))?;
        file
          .seek(SeekFrom::Start(0))
          .map_err(|e| sherr!(InternalErr, "heredoc seek failed: {e}"))?;

        Ok(Redir::new(fd, file.into()))
      }
    }
  }
}

#[derive(Default, Debug)]
pub(super) struct RedirSet(pub Vec<RedirSpec>);

impl RedirSet {
  pub fn apply_persistent(self) -> ShResult<()> {
    for spec in self.0 {
      let mut redir = spec.into_redir()?;
      redir.apply()?;
    }
    Ok(())
  }
  pub fn apply(self) -> ShResult<Option<RedirGuard>> {
    if self.0.is_empty() {
      return Ok(None);
    }
    let targets: BTreeSet<RawFd> = self.0.iter().map(RedirSpec::target_fd).collect();

    let guard = RedirGuard::new(&targets)?;
    for spec in self.0 {
      let span = if let RedirSpec::File { ref path, .. } = spec {
        Some(path.span.clone())
      } else {
        None
      };

      let mut redir = spec
        .into_redir()
        .map_err(|e| e.option_promote(span.clone()))?;

      redir.apply().map_err(|e| e.option_promote(span))?;
    }
    Ok(Some(guard))
  }
  pub fn split_by_channel(self) -> (RedirSet, RedirSet) {
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
    Self(value.to_vec())
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

#[derive(Debug)]
pub(super) struct RedirGuard {
  saved: Option<IoGroup>,
}

impl RedirGuard {
  pub fn new(targets: &BTreeSet<RawFd>) -> ShResult<Self> {
    let saved = Some(IoGroup::capture_targets(targets)?);
    Ok(Self { saved })
  }
  pub fn stdio() -> ShResult<Self> {
    let stdio_fds = [0, 1, 2].iter().copied().collect();
    Self::new(&stdio_fds)
  }
  pub fn persist(mut self) {
    use std::mem::{drop, take};
    drop(take(&mut self.saved));
  }
}

impl Drop for RedirGuard {
  fn drop(&mut self) {
    if let Some(saved) = self.saved.as_ref() {
      saved.restore().ok();
    }
  }
}

#[derive(Debug)]
pub(super) struct IoGroup(BTreeMap<RawFd, Option<OwnedFd>>);

impl IoGroup {
  pub fn capture_targets(targets: &BTreeSet<RawFd>) -> ShResult<Self> {
    let mut saved = BTreeMap::new();

    for &fd in targets {
      let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
      match dup_high(borrowed) {
        Ok(owned) => saved.insert(fd, Some(owned)),
        Err(Errno::EBADF) => saved.insert(fd, None), // fd is not open
        Err(e) => return Err(e.into()),
      };
    }

    Ok(Self(saved))
  }
  pub fn restore(&self) -> ShResult<()> {
    for (&fd, saved) in &self.0 {
      match saved {
        Some(owned) => {
          // we use libc::dup2() here instead of unistd::dup2()
          // because unistd::dup2() requires an ownedfd for the right side
          // libc::dup2() takes a raw fd instead
          let ret = unsafe { nix::libc::dup2(owned.as_raw_fd(), fd) };
          if ret < 0 {
            return Err(nix::Error::last().into());
          }
        }
        None => {
          nix::unistd::close(fd).ok();
        }
      }
    }
    Ok(())
  }
}

/// An iterator that lazily creates a specific number of pipes.
pub(super) struct PipeGenerator {
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
}

impl Iterator for PipeGenerator {
  type Item = (Option<Redir>, Option<Redir>);
  fn next(&mut self) -> Option<Self::Item> {
    if self.cursor >= self.num_cmds {
      return None;
    }

    let needs_write = self.cursor + 1 < self.num_cmds; // this is not the last command

    let rpipe = self.last_rpipe.take(); // None if this is the first command
    let wpipe = needs_write
      .then(|| {
        let (r, w) = pipes_high().ok()?;
        let read = Redir::new(0, r);
        let write = Redir::new(1, w);
        self.last_rpipe = Some(read);
        Some(write)
      })
      .flatten();

    self.cursor += 1;
    Some((rpipe, wpipe))
  }
}

#[derive(Debug, Clone)]
pub(crate) struct OutputSink {
  limit: ReadLimit,
  buf: Vec<u8>,
  truncated: bool,
}

impl Default for OutputSink {
  fn default() -> Self {
    Self {
      limit: shopt!(core.max_read_limit),
      buf: Vec::new(),
      truncated: false,
    }
  }
}

impl OutputSink {
  fn new() -> Self {
    Self::default()
  }

  pub fn limit(&self) -> ReadLimit {
    self.limit
  }

  pub fn was_truncated(&self) -> bool {
    self.truncated
  }

  pub fn into_buf(self) -> Vec<u8> {
    self.buf
  }
}

impl io::Write for OutputSink {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    if self.truncated {
      return Ok(buf.len());
    }
    let limit = *self.limit as usize;

    if self.buf.len() + buf.len() > limit {
      self.truncated = true;
      let remaining_space = limit - self.buf.len();
      self.buf.extend_from_slice(&buf[..remaining_space]);
      Ok(buf.len())
    } else {
      self.buf.extend_from_slice(buf);
      Ok(buf.len())
    }
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct InputSink {
  buf: Cursor<Vec<u8>>,
}

impl InputSink {
  fn from_input(sink: OutputSink) -> Self {
    Self {
      buf: Cursor::new(sink.buf),
    }
  }
}

impl io::Read for InputSink {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    self.buf.read(buf)
  }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Sinks {
  output_sinks: Vec<OutputSink>,
  input_sinks: Vec<InputSink>,
}

impl Sinks {
  pub const fn new() -> Self {
    Self {
      output_sinks: Vec::new(),
      input_sinks: Vec::new(),
    }
  }

  pub(crate) fn push_output(&mut self) {
    self.output_sinks.push(OutputSink::new());
  }
  pub(crate) fn pop_output(&mut self) -> Option<OutputSink> {
    self.output_sinks.pop()
  }
  pub(crate) fn has_output(&self) -> bool {
    !self.output_sinks.is_empty()
  }

  pub(crate) fn push_input(&mut self, sink: OutputSink) {
    self.input_sinks.push(InputSink::from_input(sink));
  }
  pub(crate) fn pop_input(&mut self) {
    self.input_sinks.pop();
  }
  pub(crate) fn has_input(&self) -> bool {
    !self.input_sinks.is_empty()
  }

  /// Drain whatever is left in the top input cursor (from its current position)
  /// so it can be handed to a forked child's fd 0. `None` if there is no input
  /// sink frame.
  pub(crate) fn drain_input(&mut self) -> Option<Vec<u8>> {
    self.input_sinks.last_mut().map(|cur| {
      let mut rest = Vec::new();
      cur.read_to_end(&mut rest).ok();
      rest
    })
  }
}

impl io::Read for Sinks {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    if let Some(sink) = self.input_sinks.last_mut() {
      return sink.read(buf);
    }
    unistd::read(stdin_fileno(), buf).map_err(|e| io::Error::from_raw_os_error(e as i32))
  }
}

impl io::Write for Sinks {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    if let Some(sink) = self.output_sinks.last_mut() {
      return sink.write(buf);
    }

    unistd::write(stdout_fileno(), buf).map_err(|e| io::Error::from_raw_os_error(e as i32))
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

pub(crate) fn has_out_sink() -> bool {
  Shed::sinks(|s| s.has_output())
}

pub(crate) fn has_in_sink() -> bool {
  Shed::sinks(|s| s.has_input())
}

/// Drain the remaining piped stdin so it can be handed to a forked child. The
/// in-process read path goes through `Shed::sinks` (`io::Read`) instead.
pub(crate) fn take_stdin() -> Option<Vec<u8>> {
  Shed::sinks(|s| s.drain_input())
}

pub(crate) struct SinkScope {
  taken: bool,
}
impl SinkScope {
  pub fn new() -> Self {
    Shed::sinks(|s| s.push_output());
    Self { taken: false }
  }

  pub fn take(mut self) -> OutputSink {
    self.taken = true;
    Shed::sinks(|s| s.pop_output()).expect("SinkScope should have an out sink")
  }
}

impl Drop for SinkScope {
  fn drop(&mut self) {
    if !self.taken {
      Shed::sinks(|s| s.pop_output()).expect("SinkScope should have an out sink");
    }
  }
}

pub(crate) struct StdinScope;
impl StdinScope {
  pub fn push(sink: OutputSink) -> Self {
    Shed::sinks(|s| s.push_input(sink));
    Self
  }
}

impl Drop for StdinScope {
  fn drop(&mut self) {
    Shed::sinks(|s| s.pop_input());
  }
}

pub(super) fn stdin_fileno() -> BorrowedFd<'static> {
  unsafe { BorrowedFd::borrow_raw(STDIN_FILENO) }
}

pub(super) fn stdout_fileno() -> BorrowedFd<'static> {
  unsafe { BorrowedFd::borrow_raw(STDOUT_FILENO) }
}

pub(super) fn stderr_fileno() -> BorrowedFd<'static> {
  unsafe { BorrowedFd::borrow_raw(STDERR_FILENO) }
}

pub(super) fn read_to_sink(fd: OwnedFd) -> ShResult<OutputSink> {
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

  Ok(OutputSink {
    limit,
    buf: out,
    truncated,
  })
}

pub(super) fn bytes_to_string(buf: Vec<u8>) -> String {
  match String::from_utf8(buf) {
    Ok(s) => s,
    Err(e) => String::from_utf8_lossy(&e.into_bytes()).into_owned(),
  }
}

pub(super) fn out_bytes(buf: &[u8]) {
  let _ = Shed::sinks(|s| s.write_all(buf));
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

/// Write all of `bytes` to `fd`, tolerating `EINTR` and a child that closes its
/// end early (`EPIPE`, e.g. `head`). Does not close `fd`.
pub(crate) fn write_all_to_fd(fd: BorrowedFd, bytes: &[u8]) {
  let mut written = 0;
  while written < bytes.len() {
    match write(fd, &bytes[written..]) {
      Ok(0) => break,
      Ok(n) => written += n,
      Err(Errno::EINTR) => {
        if signal::sigint_pending() {
          state::Shed::set_status(130);
          break;
        }
      }
      Err(Errno::EPIPE) => break,
      Err(_) => break,
    }
  }
}

/// Feed `bytes` to `fd` from a background thread, closing `fd` (signalling EOF)
/// when done. Read the child's output before joining the returned handle so a
/// large payload can't deadlock.
pub(crate) fn feed_fd_async(fd: OwnedFd, bytes: Vec<u8>) -> std::thread::JoinHandle<()> {
  std::thread::spawn(move || {
    write_all_to_fd(fd.as_fd(), &bytes);
  })
}

pub(super) fn capture_command(
  cmd: &str,
  stdin: Option<&str>,
  name: Option<Rc<str>>,
) -> ShResult<String> {
  let (rpipe, wpipe) = pipes_high()?;
  let stdin_pipe = if stdin.is_some() {
    Some(StdinPipe::new()?)
  } else {
    None
  };

  match unsafe { fork()? } {
    ForkResult::Child => {
      let mut specs = vec![RedirSpec::dup(wpipe.as_raw_fd(), 1, RedirType::Output)];
      // Keep the read end alive until redirs.apply() dups it onto fd 0.
      let _stdin_r_keep_alive = stdin_pipe.map(|p| p.into_child(&mut specs));
      let redirs: RedirSet = specs.into();
      let _guard = redirs.apply()?;

      if let Err(e) = exec_nonint(cmd.into(), name) {
        if let ShErrKind::CleanExit(code) = e.kind() {
          std::process::exit(*code);
        }
        e.print_error();
        unsafe { nix::libc::_exit(1) };
      }
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
          read_to_sink(rpipe)
        })?
      } else {
        read_to_sink(rpipe)?
      };
      let truncated = sink.was_truncated();
      let size = sink.limit();
      let captured = bytes_to_string(sink.into_buf());

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

pub(super) fn read_input() -> ShResult<Vec<u8>> {
  let _guard = isatty(stdin_fileno())
    .unwrap_or(false)
    .then(|| Shed::term_mut(Terminal::prepare_for_exec));

  let mut input = vec![];
  let mut read_buf = [0u8; 4096];

  loop {
    match read(stdin_fileno(), &mut read_buf) {
      Ok(0) => break,
      Ok(n) => input.extend_from_slice(&read_buf[..n]),
      Err(Errno::EINTR) => {
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
pub mod tests {
  use crate::tests::testutil::{TestGuard, has_cmd, has_cmds, test_input};
  use pretty_assertions::assert_eq;

  #[test]
  fn pipeline_simple() {
    if !has_cmd("sed") {
      return;
    }
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

  #[test]
  fn capture_simple_echo() {
    let _g = TestGuard::new();
    let out = capture_command("echo hello", None, None).unwrap();
    assert_eq!(out, "hello\n");
  }

  #[test]
  fn capture_preserves_internal_newlines() {
    let _g = TestGuard::new();
    let out = capture_command("printf 'one\\ntwo\\nthree'", None, None).unwrap();
    assert_eq!(out, "one\ntwo\nthree");
  }

  #[test]
  fn capture_empty_output() {
    let _g = TestGuard::new();
    let out = capture_command("true", None, None).unwrap();
    assert_eq!(out, "");
  }

  #[test]
  fn capture_command_sets_exit_status() {
    let _g = TestGuard::new();
    // `false` exits 1; capture_command should propagate that into
    // Shed::get_status while still returning captured output (empty).
    let out = capture_command("false", None, None).unwrap();
    assert_eq!(out, "");
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn capture_nonzero_status_still_captures_output() {
    let _g = TestGuard::new();
    // Multi-statement: prints output then fails.
    let out = capture_command("echo before-fail; false", None, None).unwrap();
    assert_eq!(out, "before-fail\n");
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── With stdin piped to child ──────────────────────────────────────

  #[test]
  fn capture_feeds_stdin_to_command() {
    let _g = TestGuard::new();
    if !has_cmd("cat") {
      return;
    }
    let out = capture_command("cat", Some("piped input"), None).unwrap();
    assert_eq!(out, "piped input");
  }

  #[test]
  fn capture_stdin_with_multiline_input() {
    let _g = TestGuard::new();
    if !has_cmd("cat") {
      return;
    }
    let out = capture_command("cat", Some("line1\nline2\nline3\n"), None).unwrap();
    assert_eq!(out, "line1\nline2\nline3\n");
  }

  #[test]
  fn capture_stdin_seen_by_read_builtin() {
    let _g = TestGuard::new();
    // The child's `read` builtin should successfully consume the
    // stdin we feed.
    let out = capture_command("read x; echo \"got=$x\"", Some("hello world\n"), None).unwrap();
    assert_eq!(out, "got=hello world\n");
  }

  // Note: there's no `no-stdin → child sees EOF` test because TestGuard
  // keeps its stdin write-end open for the lifetime of the test. A child
  // reading from the inherited stdin pipe would block forever waiting on
  // data nobody is closing. The `Option<&str>` stdin parameter handles
  // the genuinely-disconnected case in production via the
  // `stdin_pipes.is_some()` check at the top of capture_command.
}
