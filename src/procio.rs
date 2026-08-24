//! This module contains our IO redirection primitives.
//! Everything we use is basically just a thin wrapper around the std Fd types,
//! or nix system call wrappers.

use std::{
  collections::{BTreeMap, BTreeSet},
  fmt::Debug,
  fs::{File, OpenOptions},
  io::{self, Cursor, Read, Write},
  os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
  path::Path,
};

use bstr::ByteSlice;
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
  Shed,
  eval::execute,
  lifecycle, signal,
  state::{shopt::ReadLimit, terminal::Terminal, vars::VarStr},
  util::{self, ByteCursor, SliceCursor, parse_bytes},
  varstr,
};

use super::{
  eval::{
    execute::exec_nonint,
    lex::{Span, Tk, TkFlags},
  },
  expand::Expander,
  match_loop, sherr, shopt, state,
  util::{ShErr, ShResult},
};

/// Minimum fd number for shell-internal file descriptors.
/// User-visible fds (0-9) are kept clear so `exec 3>&-` etc. work as expected.
pub const MIN_INTERNAL_FD: RawFd = 10;

/// The status code returned when a builtin command's output is truncated
/// due to exceeding the maximum size of the `OutputSink`
pub const SINK_TRUNCATED_STATUS: i32 = 122;

/// Like `dup()`, but places the new fd at `MIN_INTERNAL_FD` or above so it
/// doesn't collide with user-managed fds.
pub fn dup_high(fd: BorrowedFd) -> nix::Result<OwnedFd> {
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
pub fn move_high(fd: OwnedFd) -> nix::Result<OwnedFd> {
  let new_fd = dup_high(fd.as_fd())?;
  Ok(new_fd)
} // fd is closed here

#[expect(clippy::needless_pass_by_value)]
pub fn move_high_no_cloexec(fd: OwnedFd) -> nix::Result<OwnedFd> {
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
  /// Trigger the redirection by calling [`nix::libc::dup2`] or closing the target fd.
  /// Returns an error if the redirection fails.
  pub fn apply(&mut self) -> ShResult<()> {
    if let Some(from) = &self.from {
      let ret = unsafe { nix::libc::dup2(from.as_raw_fd(), self.fd) };
      if ret < 0 {
        return Err(nix::Error::last().into());
      }
    } else if let Err(e) = nix::unistd::close(self.fd) {
      match e {
        Errno::EBADF => {
          // fd is already closed; ignore
        }
        _ => {
          return Err(e.into());
        }
      }
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
  pub dup_from_word: bool, // target fd is not a literal digit
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
  pub fn with_dup_from_word(self) -> Self {
    Self {
      dup_from_word: true,
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
      RedirTarget::Close => Ok(RedirSpec::close(fd, self.span.clone())),
      RedirTarget::Fd(src_fd) if class.is_dup_op() => {
        Ok(RedirSpec::dup_spanned(src_fd, fd, class, self.span.clone()))
      }
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
  pub fn parse(bytes: &[u8]) -> ShResult<Self> {
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

    let tgt_fd = parse_bytes::<i32>(&tgt_fd).unwrap_or_else(|| match redir.class.unwrap() {
      RedirType::Input | RedirType::ReadWrite | RedirType::HereDoc | RedirType::HereString => 0,
      _ => 1,
    });
    redir = redir.with_fd(tgt_fd);
    if *src_fd == *b"-" {
      redir = redir.with_target(RedirTarget::Close);
    } else if let Some(src_fd) = parse_bytes::<i32>(&src_fd) {
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
          body: tk.word(),
          flags,
        }),
        span: Some(span),
        dup_from_word: false,
      })
    } else {
      match Self::parse(tk.as_bytes()) {
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
  /// Returns true if this redirection type is a file operation (i.e. not a dup or close).
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
    span: Option<Span>,
  },
  DupExpr {
    word: Tk,
    to: RawFd,
    mode: RedirType,
  },
  Close {
    fd: RawFd,
    span: Option<Span>,
  },
  Buffer {
    fd: RawFd,
    buf: VarStr,
    flags: TkFlags,
  },
}

impl RedirSpec {
  pub fn file(fd: RawFd, path: Tk, mode: RedirType) -> Self {
    Self::File { fd, path, mode }
  }
  pub fn dup(from: RawFd, to: RawFd, mode: RedirType) -> Self {
    Self::Dup {
      from,
      to,
      mode,
      span: None,
    }
  }
  pub fn dup_spanned(from: RawFd, to: RawFd, mode: RedirType, span: Option<Span>) -> Self {
    Self::Dup {
      from,
      to,
      mode,
      span,
    }
  }
  pub fn dup_expr(word: Tk, to: RawFd, mode: RedirType) -> Self {
    Self::DupExpr { word, to, mode }
  }
  pub fn close(fd: RawFd, span: Option<Span>) -> Self {
    Self::Close { fd, span }
  }
  /// The span of the redirection operator, if this spec carries one. Used to
  /// point errors at the offending redirect.
  pub fn span(&self) -> Option<Span> {
    match self {
      RedirSpec::File { path, .. } => Some(path.span.clone()),
      RedirSpec::DupExpr { word, .. } => Some(word.span.clone()),
      RedirSpec::Dup { span, .. } | RedirSpec::Close { span, .. } => span.clone(),
      RedirSpec::Buffer { .. } => None,
    }
  }
  pub fn buffer(fd: RawFd, buf: VarStr, flags: TkFlags) -> Self {
    Self::Buffer { fd, buf, flags }
  }
  pub fn target_fd(&self) -> RawFd {
    match self {
      RedirSpec::Dup { to, .. } | RedirSpec::DupExpr { to, .. } => *to,
      RedirSpec::File { fd, .. } | RedirSpec::Close { fd, .. } | RedirSpec::Buffer { fd, .. } => {
        *fd
      }
    }
  }
  pub fn mode(&self) -> RedirType {
    match self {
      RedirSpec::File { mode, .. }
      | RedirSpec::Dup { mode, .. }
      | RedirSpec::DupExpr { mode, .. } => *mode,
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
      RedirSpec::Dup { from, to, .. } => {
        let borrowed = unsafe { BorrowedFd::borrow_raw(from) };
        let owned = borrowed
          .try_clone_to_owned()
          .map_err(|e| sherr!(InternalErr, "Failed to duplicate fd {}: {}", from, e))?;
        let owned = move_high(owned)?;
        Ok(Redir::new(to, owned))
      }
      RedirSpec::DupExpr { word, to, mode: _ } => {
        let span = word.span.clone();
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
          return Ok(Redir::close(to));
        }

        let from = src.parse::<RawFd>().map_err(|_| {
          sherr!(
            ExecFail @ span.clone(),
            "ambiguous redirect: `{src}` is not a valid file descriptor"
          )
        })?;
        let borrowed = unsafe { BorrowedFd::borrow_raw(from) };
        let owned = borrowed
          .try_clone_to_owned()
          .map_err(|e| sherr!(InternalErr @ span, "Failed to duplicate fd {from}: {e}"))?;
        let owned = move_high(owned)?;
        Ok(Redir::new(to, owned))
      }
      RedirSpec::Close { fd, .. } => Ok(Redir::close(fd)),
      RedirSpec::Buffer { fd, buf, flags } => {
        use io::{Seek, SeekFrom, Write};

        let file = tempfile::tempfile()
          .map_err(|e| sherr!(InternalErr, "heredoc tempfile creation failed: {e}"))?;
        let owned: OwnedFd = file.into();
        let owned = move_high(owned)?;

        let bytes: Vec<u8> = if flags.contains(TkFlags::HERESTRING) {
          let mut expanded: Vec<u8> = Expander::from_raw(buf.as_bytes(), flags)
            .no_glob()
            .no_split()
            .expand_no_split()?
            .into();
          expanded.push(b'\n');
          expanded
        } else if flags.contains(TkFlags::IS_HEREDOC) && !flags.contains(TkFlags::LIT_HEREDOC) {
          Expander::from_raw(buf.as_bytes(), flags)
            .no_glob()
            .no_split()
            .expand_no_split()?
            .into()
        } else {
          buf.into()
        };

        let mut file = std::fs::File::from(owned);
        file
          .write_all(&bytes)
          .map_err(|e| sherr!(InternalErr, "heredoc write failed: {e}"))?;
        file
          .seek(SeekFrom::Start(0))
          .map_err(|e| sherr!(InternalErr, "heredoc seek failed: {e}"))?;

        Ok(Redir::new(fd, file.into()))
      }
    }
  }
}

/// The result of attempting to apply a [`RedirSet`].
pub(super) enum RedirResult {
  Applied(RedirGuard),
  NoRedirs,
  Skipped,
  Error(ShErr),
}

impl RedirResult {
  /// Collapse into a plain result, propagating any error. For callers where a
  /// redirection failure is fatal (the pre-non-fatal default). Callers that can
  /// continue should instead match the variants and handle [`Self::Skipped`].
  pub fn or_fatal(self) -> ShResult<Option<RedirGuard>> {
    match self {
      RedirResult::Applied(guard) => Ok(Some(guard)),
      // `apply()` never yields `Skipped`; proceed defensively if it somehow does.
      RedirResult::NoRedirs | RedirResult::Skipped => Ok(None),
      RedirResult::Error(e) => Err(e),
    }
  }
}

/// A set of redirections to be applied together.
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
  /// Apply the redirections, classifying a failure as fatal or not. When
  /// `fatal` is false, a failure is reported (printed + `$?` set) and turned
  /// into [`RedirResult::Skipped`] so the caller can skip the command and
  /// continue; when `fatal` is true, the error is left to propagate.
  pub fn try_apply(self, fatal: bool) -> RedirResult {
    match self.apply() {
      RedirResult::Error(e) if !fatal => {
        e.print_error();
        Shed::set_status(1);
        RedirResult::Skipped
      }
      // Applied / NoRedirs / (fatal) Error pass through unchanged.
      res => res,
    }
  }
  /// Apply the redirections, returning a guard that will restore the original fds when dropped.
  pub fn apply(self) -> RedirResult {
    if self.0.is_empty() {
      return RedirResult::NoRedirs;
    }
    let targets: BTreeSet<RawFd> = self.0.iter().map(RedirSpec::target_fd).collect();

    let guard = match RedirGuard::new(&targets) {
      Ok(g) => g,
      Err(e) => return RedirResult::Error(e),
    };

    // apply each redir
    for spec in self.0 {
      let span = spec.span();

      let res = spec
        .into_redir()
        .map_err(|e| e.option_promote(span.clone()));

      let mut redir = match res {
        Ok(r) => r,
        Err(e) => return RedirResult::Error(e),
      };

      if let Err(e) = redir.apply().map_err(|e| e.option_promote(span)) {
        return RedirResult::Error(e);
      }
    }
    RedirResult::Applied(guard)
  }
  /// Separate input redirs and output redirs into two separate `RedirSet`s
  ///
  /// Returns (`in_redirs`, `out_redirs`)
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

/// A guard that restores the original file descriptors when dropped.
#[derive(Debug)]
pub(super) struct RedirGuard {
  saved: Option<IoGroup>,
}

impl RedirGuard {
  pub fn new(targets: &BTreeSet<RawFd>) -> ShResult<Self> {
    let saved = Some(IoGroup::capture_targets(targets)?);
    Ok(Self { saved })
  }
  /// Create a `RedirGuard` that captures the current state of stdin, stdout, and stderr (fd 0, 1, 2).
  pub fn stdio() -> ShResult<Self> {
    let stdio_fds = [0, 1, 2].iter().copied().collect();
    Self::new(&stdio_fds)
  }
  /// Persist the redirections, preventing the guard from restoring the original fds when dropped.
  pub fn persist(mut self) {
    use std::mem::{drop, take};
    drop(take(&mut self.saved));
  }
}

impl Drop for RedirGuard {
  fn drop(&mut self) {
    if let Some(saved) = self.saved.take() {
      saved.restore().ok();
    }
  }
}

/// A group of file descriptors that can be captured and restored.
/// Stores them as (`RawFd`, `Option<OwnedFd>`) pairs, where the `Option` is `None` if the fd is to be closed.
#[derive(Debug)]
pub(super) struct IoGroup(BTreeMap<RawFd, Option<OwnedFd>>);

impl IoGroup {
  /// Capture the current state of the given file descriptors, saving them for later restoration.
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
  num_cmds: usize, // The number of pipes to create
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
  type Item = (Option<Redir>, Option<Redir>, Option<RawFd>);
  /// Returns a tuple of (read end of previous pipe, write end of current pipe, read end of current pipe).
  fn next(&mut self) -> Option<Self::Item> {
    if self.cursor >= self.num_cmds {
      return None;
    }

    let needs_write = self.cursor + 1 < self.num_cmds; // this is not the last command

    let rpipe = self.last_rpipe.take(); // None if this is the first command
    let mut downstream_read = None;
    let wpipe = needs_write
      .then(|| {
        let (r, w) = pipes_high().ok()?;
        downstream_read = Some(r.as_raw_fd());
        let read = Redir::new(0, r);
        let write = Redir::new(1, w);
        self.last_rpipe = Some(read);
        Some(write)
      })
      .flatten();

    self.cursor += 1;
    Some((rpipe, wpipe, downstream_read))
  }
}

/// A sink used for internal IO transfers.
/// `shed` is capable of running builtin-only pipelines
/// where the output of one builtin is fed into the input of another builtin
/// and no intermediate system calls are necessary.
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

/// A sink used for internal IO transfers.
/// Is readable in the same way as a regular file descriptor, but reads from an internal buffer.
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

/// A collection of input and output sinks for internal IO transfers.
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

  /// Whether the top input sink still has unread bytes. `None` if there is no
  /// input sink frame (the caller should then check the real fd instead). Used
  /// by `read -t 0` to poll for data without consuming it.
  pub(crate) fn input_available(&self) -> Option<bool> {
    self
      .input_sinks
      .last()
      .map(|cur| (cur.buf.position() as usize) < cur.buf.get_ref().len())
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

impl std::fmt::Write for Sinks {
  fn write_str(&mut self, s: &str) -> std::fmt::Result {
    self.write_all(s.as_bytes()).map_err(|_| std::fmt::Error)
  }
}

pub(crate) fn has_out_sink() -> bool {
  Shed::sinks(|s| s.has_output())
}

pub(crate) fn has_in_sink() -> bool {
  Shed::sinks(|s| s.has_input())
}

pub(crate) fn stdin_is_tty() -> bool {
  isatty(stdin_fileno()).unwrap_or(false)
}

/// Drain the remaining piped stdin so it can be handed to a forked child. The
/// in-process read path goes through `Shed::sinks` (`io::Read`) instead.
pub(crate) fn take_stdin() -> Option<Vec<u8>> {
  Shed::sinks(Sinks::drain_input)
}

pub(crate) struct SinkScope {
  taken: bool,
}
impl SinkScope {
  pub fn new() -> Self {
    Shed::sinks(Sinks::push_output);
    Self { taken: false }
  }

  pub fn take(mut self) -> OutputSink {
    self.taken = true;
    Shed::sinks(Sinks::pop_output).expect("SinkScope should have an out sink")
  }
}

impl Drop for SinkScope {
  fn drop(&mut self) {
    if !self.taken {
      Shed::sinks(Sinks::pop_output).expect("SinkScope should have an out sink");
    }
  }
}

/// A guard struct that pushes an input sink onto the `Shed` stack and pops it when dropped.
pub(crate) struct StdinScope;
impl StdinScope {
  pub fn push(sink: OutputSink) -> Self {
    Shed::sinks(|s| s.push_input(sink));
    Self
  }
}

impl Drop for StdinScope {
  fn drop(&mut self) {
    Shed::sinks(Sinks::pop_input);
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

/// Read all bytes from the given file descriptor into an `OutputSink`, respecting the `core.max_read_limit` shell option.
/// Used for boundaries between builtins and external commands
/// Returns an error if the read fails.
pub(super) fn read_to_sink(fd: BorrowedFd) -> ShResult<OutputSink> {
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

/// Convert a vector of bytes to a string, replacing invalid UTF-8 sequences with the replacement character.
pub(super) fn bytes_to_string(buf: Vec<u8>) -> String {
  match String::from_utf8(buf) {
    Ok(s) => s,
    Err(e) => String::from_utf8_lossy(&e.into_bytes()).into_owned(),
  }
}

/// Write raw bytes to the current output sink, byte-native counterpart to `out!`.
pub(super) fn out_bytes(buf: &[u8]) {
  let _ = Shed::sinks(|s| s.write_all(buf));
}

/// Write raw bytes followed by a newline, byte-native counterpart to `outln!`.
pub(super) fn outln_bytes(buf: &[u8]) {
  let _ = Shed::sinks(|s| s.write_all(buf).and_then(|()| s.write_all(b"\n")));
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
  let mut buf = [0u8; 8192]; // 8 KiB
  loop {
    match unistd::read(fd, &mut buf) {
      Ok(0) => break,
      Ok(n) => {
        if Shed::sinks(|s| s.write_all(&buf[..n])).is_err() {
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

/// Feed `bytes` to `fd` from a background thread, closing `fd` (signalling EOF)
/// when done. Read the child's output before joining the returned handle so a
/// large payload can't deadlock.
pub(crate) fn feed_fd_async(fd: OwnedFd, bytes: Vec<u8>) -> std::thread::JoinHandle<()> {
  std::thread::spawn(move || {
    write_all_to_fd(fd.as_fd(), &bytes);
  })
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
      let _guard = redirs.apply().or_fatal()?;

      execute::catch_exit(
        || exec_nonint(cmd.into(), name.cloned()),
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
          read_to_sink(rpipe.as_fd())
        })?
      } else {
        read_to_sink(rpipe.as_fd())?
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

  // A dup/close redirection error (e.g. `>&9` on a closed fd) must be able to
  // point at the operator, like file redirects do, so user-facing specs carry
  // the operator span. Internally synthesized specs (pipe wiring, `|&`
  // desugaring) have no source location and carry none.
  #[test]
  fn dup_and_close_specs_carry_operator_span() {
    use super::{RedirSpec, RedirType};
    use crate::eval::lex::Span;

    let span = Span::new(0..3, "2>&".into());

    assert!(
      RedirSpec::dup_spanned(1, 2, RedirType::Output, Some(span.clone()))
        .span()
        .is_some(),
      "a dup built from source should retain its operator span"
    );
    assert!(
      RedirSpec::close(3, Some(span)).span().is_some(),
      "a close built from source should retain its operator span"
    );
    assert!(
      RedirSpec::dup(1, 2, RedirType::Output).span().is_none(),
      "an internally synthesized dup has no source span"
    );
  }

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
    let out = capture_command(b"echo hello", None, None).unwrap();
    assert_eq!(out, "hello\n");
  }

  #[test]
  fn capture_preserves_internal_newlines() {
    let _g = TestGuard::new();
    let out = capture_command(b"printf 'one\\ntwo\\nthree'", None, None).unwrap();
    assert_eq!(out, "one\ntwo\nthree");
  }

  #[test]
  fn capture_empty_output() {
    let _g = TestGuard::new();
    let out = capture_command(b"true", None, None).unwrap();
    assert_eq!(out, "");
  }

  #[test]
  fn capture_command_sets_exit_status() {
    let _g = TestGuard::new();
    // `false` exits 1; capture_command should propagate that into
    // Shed::get_status while still returning captured output (empty).
    let out = capture_command(b"false", None, None).unwrap();
    assert_eq!(out, "");
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn capture_nonzero_status_still_captures_output() {
    let _g = TestGuard::new();
    // Multi-statement: prints output then fails.
    let out = capture_command(b"echo before-fail; false", None, None).unwrap();
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
    let out = capture_command(b"cat", Some(b"piped input"), None).unwrap();
    assert_eq!(out, "piped input");
  }

  #[test]
  fn capture_stdin_with_multiline_input() {
    let _g = TestGuard::new();
    if !has_cmd("cat") {
      return;
    }
    let out = capture_command(b"cat", Some(b"line1\nline2\nline3\n"), None).unwrap();
    assert_eq!(out, "line1\nline2\nline3\n");
  }

  #[test]
  fn capture_stdin_seen_by_read_builtin() {
    let _g = TestGuard::new();
    // The child's `read` builtin should successfully consume the
    // stdin we feed.
    let out = capture_command(b"read x; echo \"got=$x\"", Some(b"hello world\n"), None).unwrap();
    assert_eq!(out, "got=hello world\n");
  }

  // Note: there's no `no-stdin → child sees EOF` test because TestGuard
  // keeps its stdin write-end open for the lifetime of the test. A child
  // reading from the inherited stdin pipe would block forever waiting on
  // data nobody is closing. The `Option<&str>` stdin parameter handles
  // the genuinely-disconnected case in production via the
  // `stdin_pipes.is_some()` check at the top of capture_command.
}
