//! `shed`'s shell state management.
//!
//! This module contains the [`Shed`] struct, which is the central repository for all of the shell's state.
//! Fun fact: the [`size_of`] the [`Shed`] struct is 2584 bytes, which is a bit over 2.5 KB. This is a lot of state to manage, but it is necessary for the shell to function properly.

use chrono::{DateTime, Local};
use std::{
  cell::RefCell,
  collections::VecDeque,
  fmt::Display,
  os::{fd::AsFd, unix::net::UnixStream},
  sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
  },
  time::SystemTime,
};

use super::{
  WtStat, autocmd, builtin, errln, eval, expand, keys, match_loop, procio, readline, sherr,
  shopt as shopt_macro, signal, socket,
  state::vars::{VarFlags, VarKind},
  system_msg, try_var, two_way_display, util as crate_util,
  util::{Pos, ShErr, ShErrKind, ShResult, error::LabelBuilder},
  var, writefd,
};

pub mod jobs;
pub(super) mod logic;
pub(super) mod meta;
pub(super) mod scopes;
pub mod shopt;
pub(super) mod terminal;
pub(super) mod util;
pub(super) mod vars;

thread_local! {
  static SHED: Shed = Shed::new();
}

/// Pops a call frame's traceback labels on drop, restoring the context stack
/// to the length it had before the frame was pushed. See [`Shed::push_call_frame`].
pub(crate) struct CallFrameGuard {
  restore: usize,
}
impl Drop for CallFrameGuard {
  fn drop(&mut self) {
    SHED.with(|shed| shed.call_context.borrow_mut().truncate(self.restore));
  }
}

/// A message with a timestamp.
///
/// This is used to store both system and status messages that are posted by the shell.
/// System messages are drawn with the prompt, or echoed to stdout in non-interactive sessions.
/// Status messages appear under the prompt, and last for a few seconds.
#[derive(Clone, Debug)]
pub(super) struct Message {
  when: SystemTime,
  what: String,
}

impl Message {
  pub fn new(what: String) -> Self {
    Self {
      when: SystemTime::now(),
      what,
    }
  }
  pub fn with_timestamp(&self) -> String {
    let time: DateTime<Local> = (self.when).into();
    let formatted = time.format("[%H:%M:%S]").to_string();
    let msg = self.what.trim().replace('\n', "\n\t\t"); // aligns multiline messages

    format!("{formatted}\t{msg}")
  }
}

impl Display for Message {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.what)
  }
}

/// Generates an accessor for a field of the [`Shed`] struct.
macro_rules! access {
  ($shed:ident, $field:ident, $f:expr) => {{
    let caller = ::std::panic::Location::caller();
    $shed.with(|shed| {
      let field = shed.$field.try_borrow().unwrap_or_else(|_| {
        panic!(
          "Shed::{} already borrowed (called from {caller})",
          stringify!($field)
        )
      });
      $f(&field)
    })
  }};
}

/// Generates a mutable accessor for a field of the [`Shed`] struct.
macro_rules! access_mut {
  ($shed:ident, $field:ident, $f:expr) => {{
    let caller = ::std::panic::Location::caller();
    $shed.with(|shed| {
      let mut field = shed.$field.try_borrow_mut().unwrap_or_else(|_| {
        panic!(
          "Shed::{} already borrowed (called from {caller})",
          stringify!($field)
        )
      });
      $f(&mut field)
    })
  }};
}

/// `shed`'s internal state struct.
///
/// Every last bit of data that this program needs to track over
/// its lifecycle is stored here
#[derive(Debug)]
pub(super) struct Shed {
  // constructed in state/util.rs
  /// The job table
  jobs: RefCell<jobs::JobTab>,
  /// Shell variable scopes
  var_scopes: RefCell<scopes::ScopeStack>,
  /// Metadata and miscellaneous bookkeeping
  meta: RefCell<meta::MetaTab>,
  /// Table for functions, aliases, etc
  logic: RefCell<logic::LogTab>,
  /// The terminal state
  terminal: RefCell<terminal::Terminal>,
  /// The shell configuration options
  shopts: RefCell<shopt::ShOpts>,
  /// The last exit status code, used by `$?`
  status_code: AtomicI32,

  /// Pending status messages to be displayed under the prompt
  status_msg_queue: RefCell<VecDeque<Message>>,
  /// History of status messages that have been displayed
  status_msg_hist: RefCell<VecDeque<Message>>,

  /// Pending system messages to be displayed with the prompt
  system_msg_queue: RefCell<VecDeque<Message>>,
  /// History of system messages that have been displayed
  system_msg_hist: RefCell<VecDeque<Message>>,

  /// The IPC socket
  socket: RefCell<Option<Arc<socket::ShedSocket>>>,
  /// The list of subscribers to the IPC socket
  subscribers: RefCell<Vec<Arc<UnixStream>>>,

  /// The call context stack, used for traceback and error reporting
  call_context: RefCell<Vec<LabelBuilder>>,

  /// Internal I/O sinks, used to allow builtins to chain in pipelines without forking.
  sinks: RefCell<procio::Sinks>,

  #[cfg(test)]
  /// A saved copy of the shell state, used for testing.
  saved: RefCell<Option<Box<Self>>>,
}

impl Shed {
  pub fn new() -> Self {
    Self {
      jobs: RefCell::new(jobs::JobTab::new()),
      var_scopes: RefCell::new(scopes::ScopeStack::new()),
      meta: RefCell::new(meta::MetaTab::new()),
      logic: RefCell::new(logic::LogTab::new()),
      terminal: RefCell::new(terminal::Terminal::new()),
      shopts: RefCell::new(shopt::ShOpts::default()),
      status_code: AtomicI32::new(0),

      status_msg_queue: RefCell::new(VecDeque::new()),
      status_msg_hist: RefCell::new(VecDeque::new()),

      system_msg_queue: RefCell::new(VecDeque::new()),
      system_msg_hist: RefCell::new(VecDeque::new()),

      socket: RefCell::new(None),
      subscribers: RefCell::new(vec![]),
      call_context: RefCell::new(vec![]),

      sinks: RefCell::new(procio::Sinks::new()),

      #[cfg(test)]
      saved: RefCell::new(None),
    }
  }

  /*
   * State Accessor Functions
   *
   * READ THIS!!!!!!!!!!!!!!!!!!!!!!!!!!!!
   *
   * The reason we use this "take a function, execute it on a borrow" pattern
   * is to make positively sure that the lifetimes of the borrows are handled safely.
   *
   * The idea is that this makes it much harder to have overlapping borrows of the same field.
   * Like, you wouldn't call Shed::vars() inside of Shed::vars(), for instance. (hopefully)
   *
   * The main footgun associated with using these is re-entrancy.
   * For instance, If you call Shed::vars_mut() in a place that can be accessed
   * by Shed::vars_mut(), (e.g. inside the VarTab methods), the shell will crash with a borrow error.
   * Let's not do that!
   *
   * This pattern results in the codebase being split into two parts:
   * 1. The part that can call these functions.
   * 2. The part that can be interacted with from inside these functions.
   *
   * The second part is pretty much entirely housed within this module.
   * These two parts must be as separated as possible. It's not possible to get complete isolation,
   * since codepaths like expansion can find ways to escape back into regular execution contexts (command substitution).
   *
   * Overall, if we only use these to get and set data and not perform any actual calculations, we should be fine.
   */

  /// Access the I/O sinks
  #[track_caller]
  pub fn sinks<T, F>(f: F) -> T
  where
    F: FnOnce(&mut procio::Sinks) -> T,
  {
    access_mut!(SHED, sinks, f)
  }

  /// Read from the job table
  #[track_caller]
  pub fn jobs<T, F>(f: F) -> T
  where
    F: FnOnce(&jobs::JobTab) -> T,
  {
    access!(SHED, jobs, f)
  }
  /// Mutate the job table
  #[track_caller]
  pub fn jobs_mut<T, F>(f: F) -> T
  where
    F: FnOnce(&mut jobs::JobTab) -> T,
  {
    access_mut!(SHED, jobs, f)
  }

  /// Attempt hanging up running jobs.
  pub fn try_hang_up() {
    SHED.with(|shed| {
      if let Ok(mut jobs) = shed.jobs.try_borrow_mut() {
        jobs.hang_up();
      }
    });
  }

  /// Read from the var scope stack
  #[track_caller]
  pub fn vars<T, F>(f: F) -> T
  where
    F: FnOnce(&scopes::ScopeStack) -> T,
  {
    access!(SHED, var_scopes, f)
  }
  /// Mutate the var scope stack
  #[track_caller]
  pub fn vars_mut<T, F>(f: F) -> T
  where
    F: FnOnce(&mut scopes::ScopeStack) -> T,
  {
    access_mut!(SHED, var_scopes, f)
  }

  /// Read from the metadata table
  #[track_caller]
  pub fn meta<T, F>(f: F) -> T
  where
    F: FnOnce(&meta::MetaTab) -> T,
  {
    access!(SHED, meta, f)
  }
  /// Mutate the metadata table
  #[track_caller]
  pub fn meta_mut<T, F>(f: F) -> T
  where
    F: FnOnce(&mut meta::MetaTab) -> T,
  {
    access_mut!(SHED, meta, f)
  }

  /// Read from the logic table
  #[track_caller]
  pub fn logic<T, F>(f: F) -> T
  where
    F: FnOnce(&logic::LogTab) -> T,
  {
    access!(SHED, logic, f)
  }
  /// Mutate the logic table
  #[track_caller]
  pub fn logic_mut<T, F>(f: F) -> T
  where
    F: FnOnce(&mut logic::LogTab) -> T,
  {
    access_mut!(SHED, logic, f)
  }

  /// Read from the shell options
  #[track_caller]
  pub fn shopts<T, F>(f: F) -> T
  where
    F: FnOnce(&shopt::ShOpts) -> T,
  {
    access!(SHED, shopts, f)
  }
  /// Mutate the shell options
  #[track_caller]
  pub fn shopts_mut<T, F>(f: F) -> T
  where
    F: FnOnce(&mut shopt::ShOpts) -> T,
  {
    access_mut!(SHED, shopts, f)
  }

  /// Read from the terminal state
  #[track_caller]
  pub fn term<T, F>(f: F) -> T
  where
    F: FnOnce(&terminal::Terminal) -> T,
  {
    access!(SHED, terminal, f)
  }
  /// Mutate the terminal state
  #[track_caller]
  pub fn term_mut<T, F>(f: F) -> T
  where
    F: FnOnce(&mut terminal::Terminal) -> T,
  {
    access_mut!(SHED, terminal, f)
  }

  /// Broadcast a message to all subscribers of the IPC socket.
  fn broadcast<F>(mut f: F)
  where
    F: FnMut(&Arc<UnixStream>) -> std::io::Result<()>,
  {
    SHED.with(|shed| {
      let mut subs = shed.subscribers.borrow_mut();
      let mut dead = vec![];
      for (i, subscriber) in subs.iter().enumerate() {
        if f(subscriber).is_err() {
          dead.push(i);
        }
      }
      for i in dead.into_iter().rev() {
        subs.remove(i);
      }
    });
  }

  pub fn system_msg_pending() -> bool {
    SHED.with(|shed| !shed.system_msg_queue.borrow().is_empty())
  }

  pub fn post_status_msg(msg: String) {
    SHED.with(|shed| {
      let msg = Message::new(msg);
      shed.status_msg_queue.borrow_mut().push_back(msg);
    });
  }
  pub fn pop_status_msg() -> Option<String> {
    SHED.with(|shed| {
      let mut queue = shed.status_msg_queue.borrow_mut();
      let mut hist = shed.status_msg_hist.borrow_mut();
      Self::pop_msg(&mut queue, &mut hist)
    })
  }
  pub fn post_system_msg(msg: String) {
    if Self::meta(meta::MetaTab::interactive_shell) {
      SHED.with(|shed| {
        let msg = Message::new(msg);
        shed.system_msg_queue.borrow_mut().push_back(msg);
      });
    } else {
      errln!("{msg}");
    }
  }
  pub fn pop_system_msg() -> Option<String> {
    SHED.with(|shed| {
      let mut queue = shed.system_msg_queue.borrow_mut();
      let mut hist = shed.system_msg_hist.borrow_mut();
      Self::pop_msg(&mut queue, &mut hist)
    })
  }
  fn pop_msg(queue: &mut VecDeque<Message>, hist: &mut VecDeque<Message>) -> Option<String> {
    let msg = queue.pop_front()?;

    hist.push_back(msg.clone());
    if hist.len() > 1000 {
      hist.pop_front();
    }

    Some(msg.to_string())
  }

  pub fn create_socket() -> ShResult<()> {
    let sock = socket::ShedSocket::new()?;
    SHED.with(|shed| {
      *shed.socket.borrow_mut() = Some(sock.into());
    });
    Ok(())
  }
  pub fn get_socket() -> Option<Arc<socket::ShedSocket>> {
    SHED.with(|shed| shed.socket.borrow().clone())
  }
  /// Read all pending requests from the IPC socket, returning a vector of (connection, request) tuples.
  pub fn read_socket() -> Vec<(UnixStream, socket::SocketRequest)> {
    let mut requests = vec![];
    let Some(listener) = Self::get_socket() else {
      return requests;
    };

    while let Ok((conn, _)) = listener.listener().accept()
      && let Some(req) = Self::read_request(&conn)
    {
      requests.push((conn, req));
    }

    requests
  }
  pub fn read_request(conn: &UnixStream) -> Option<socket::SocketRequest> {
    use nix::{
      errno::Errno,
      unistd::{read, write},
    };
    const MAX_IDLE_ITERS: u32 = 50;

    // Nonblocking read; the request ends at EOF on the client's write half
    conn.set_nonblocking(true).ok();
    let mut bytes = vec![];
    let mut idle_iters = 0;
    loop {
      let mut buffer = [0u8; 1024];
      match read(conn, &mut buffer) {
        Ok(0) => break,
        Ok(n) => {
          bytes.extend_from_slice(&buffer[..n]);
          idle_iters = 0;
        }
        Err(Errno::EAGAIN) => {
          idle_iters += 1;
          if idle_iters >= MAX_IDLE_ITERS {
            break;
          }
          std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Err(Errno::EINTR) => (),
        Err(e) => {
          writefd!(conn, "error>> failed to parse request: {e}\n").ok();
          break;
        }
      }
    }

    if bytes.last() == Some(&b'\n') {
      bytes.pop();
    }

    let request = match socket::SocketRequest::parse_request(&bytes) {
      Ok(req) => req,
      Err(e) => {
        write(
          conn,
          format!("error>> failed to parse request: {e}\n").as_bytes(),
        )
        .ok();
        return None;
      }
    };

    Some(request)
  }
  pub fn push_subscriber(subscriber: UnixStream) {
    SHED.with(|shed| {
      shed.subscribers.borrow_mut().push(Arc::new(subscriber));
    });
  }
  pub fn num_subscribers() -> usize {
    SHED.with(|shed| shed.subscribers.borrow().len())
  }
  /// Push a call frame's traceback labels; the returned guard pops them when
  /// the frame exits (restoring the stack to its prior length).
  pub fn push_call_frame(labels: Vec<LabelBuilder>) -> CallFrameGuard {
    let restore = SHED.with(|shed| {
      let mut cc = shed.call_context.borrow_mut();
      let restore = cc.len();
      cc.extend(labels);
      restore
    });
    CallFrameGuard { restore }
  }
  /// Snapshot the active traceback-context stack, for attaching to an error
  /// as it is printed.
  pub fn call_context() -> Vec<LabelBuilder> {
    SHED.with(|shed| shed.call_context.borrow().clone())
  }
  /// Broadcast a message to all subscribers of the IPC socket.
  pub fn broadcast_msg(msg: &str) {
    let payload = msg
      .lines()
      .map(|l| format!("msg>>{l}"))
      .collect::<Vec<String>>()
      .join("\n");

    Self::broadcast(|sub| writefd!(sub, "{payload}\n"));
  }
  /// Broadcast an autocmd event to all subscribers of the IPC socket.
  pub fn notify_autocmd(kind: logic::AutoCmdKind) {
    Self::broadcast(|sub| writefd!(sub, "autocmd_event>>{kind}\n"));
  }
  /// Broadcast a job completion event to all subscribers of the IPC socket.
  pub fn notify_job_complete(job: &jobs::Job) {
    use itertools::izip;
    use std::fmt::Write as _;

    let id = job.tabid().map(|i| (i + 1).to_string()).unwrap_or_default();
    let pids = job.get_pids();
    let stats = job.get_stats();
    let cmds = job.get_cmds();

    Self::broadcast(|sub| {
      let mut buf = format!("job>>begin>>{id} {}\n", pids.len());
      for (pid, stat, cmd) in izip!(&pids, &stats, &cmds) {
        let stat_str = match stat {
          WtStat::Exited(_, 0) => "done".to_string(),
          WtStat::Exited(_, n) => format!("failed:{n}"),
          WtStat::Signaled(_, sig, _) => format!("signaled:{sig:?}"),
          other => format!("{other:?}"),
        };
        let _ = writeln!(buf, "job>>child>>{pid} {stat_str} {cmd}");
      }
      writefd!(sub, "{buf}")?;
      Ok(())
    });
  }
  /// Broadcast a line edit event to all subscribers of the IPC socket.
  pub fn notify_line_edit(data: readline::LineData) {
    use nix::unistd::write;
    use std::fmt::Write as _;

    let readline::LineData {
      buffer,
      cursor,
      anchor,
      hint,
      mode,
    } = data;

    Self::broadcast(|sub| {
      let mut buf = String::new();
      let _ = writeln!(buf, "line>>buffer>>{buffer}");
      let _ = writeln!(buf, "line>>cursor>>{cursor}");
      if let Some(anchor) = anchor {
        let _ = writeln!(buf, "line>>anchor>>{anchor}");
      }
      if let Some(hint) = &hint {
        let _ = writeln!(buf, "line>>hint>>{hint}");
      }
      let _ = writeln!(buf, "line>>mode>>{mode}");

      write(sub, buf.as_bytes())?;
      Ok(())
    });
  }
  /// Broadcast a key event to all subscribers of the IPC socket.
  pub fn notify_key_event(event: &keys::KeyEvent) {
    use nix::unistd::write;

    let seq = event.as_vim_seq();

    Self::broadcast(|sub| {
      let buf = format!("line>>key_event>>{seq}\n");
      write(sub, buf.as_bytes())?;
      Ok(())
    });
  }
  pub fn status_msg_hist() -> Vec<Message> {
    SHED.with(|shed| {
      shed
        .status_msg_hist
        .borrow()
        .iter()
        .cloned()
        .collect::<Vec<Message>>()
    })
  }
  pub fn system_msg_hist() -> Vec<Message> {
    SHED.with(|shed| {
      shed
        .system_msg_hist
        .borrow()
        .iter()
        .cloned()
        .collect::<Vec<Message>>()
    })
  }

  /// Get the last exit status code, used by `$?`.
  ///
  /// The value is masked to 8 bits to match bash behavior.
  pub fn get_status() -> i32 {
    SHED.with(|shed| shed.status_code.load(Ordering::Relaxed)) & 255
  }
  /// Set the last exit status code, used by `$?`.
  pub fn set_status(code: i32) {
    SHED.with(|shed| shed.status_code.store(code, Ordering::Relaxed));
  }
  /// Set the last exit status code from a boolean value, where `true` is success (0) and `false` is failure (1).
  pub fn set_status_from_bool(code: bool) {
    Self::set_status(i32::from(!code));
  }
  pub fn set_pipe_status(stats: &[WtStat]) -> ShResult<()> {
    if let Some(pipe_status) = jobs::Job::pipe_status(stats) {
      let pipe_status = pipe_status
        .into_iter()
        .map(|s| s.to_string())
        .collect::<VecDeque<String>>();

      Self::vars_mut(|v| {
        v.set_var(
          "PIPESTATUS",
          VarKind::arr(pipe_status.into_iter().map(Into::into)),
          VarFlags::empty(),
        )
      })?;
    }
    Ok(())
  }

  #[cfg(test)]
  pub fn save_state() {
    SHED.with(Shed::save);
  }

  #[cfg(test)]
  pub fn restore_state() {
    SHED.with(Shed::restore);
  }
}

impl Default for Shed {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
impl Shed {
  pub fn save(&self) {
    let saved = Self {
      jobs: RefCell::new(std::mem::take(&mut self.jobs.borrow_mut())),
      var_scopes: RefCell::new(self.var_scopes.borrow().clone()),
      meta: RefCell::new(self.meta.borrow().clone()),
      logic: RefCell::new(self.logic.borrow().clone()),
      shopts: RefCell::new(self.shopts.borrow().clone()),
      terminal: RefCell::new(self.terminal.borrow().clone()),
      status_msg_queue: RefCell::new(self.status_msg_queue.borrow().clone()),
      status_msg_hist: RefCell::new(self.status_msg_hist.borrow().clone()),
      system_msg_queue: RefCell::new(self.system_msg_queue.borrow().clone()),
      system_msg_hist: RefCell::new(self.system_msg_hist.borrow().clone()),
      socket: RefCell::new(self.socket.borrow().clone()),
      subscribers: RefCell::new(self.subscribers.borrow().clone()),
      call_context: RefCell::new(self.call_context.borrow().clone()),
      sinks: RefCell::new(self.sinks.borrow().clone()),
      saved: RefCell::new(None),
      status_code: AtomicI32::new(self.status_code.load(Ordering::Relaxed)),
    };
    *self.saved.borrow_mut() = Some(Box::new(saved));
  }

  pub fn restore(&self) {
    if let Some(saved) = self.saved.take() {
      *self.jobs.borrow_mut() = saved.jobs.into_inner();
      *self.var_scopes.borrow_mut() = saved.var_scopes.into_inner();
      *self.meta.borrow_mut() = saved.meta.into_inner();
      *self.logic.borrow_mut() = saved.logic.into_inner();
      *self.shopts.borrow_mut() = saved.shopts.into_inner();
    }
  }
}
