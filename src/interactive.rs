use std::{
  collections::VecDeque,
  os::fd::{AsRawFd, BorrowedFd},
  path::Path,
  rc::Rc,
  sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
  },
  time::{Duration, Instant},
};

use nix::{
  errno::Errno,
  poll::{PollFd, PollFlags, PollTimeout, poll},
  sys::stat::{FchmodatFlags, Mode, fchmodat},
  unistd::{Pid, getppid},
};
use smallvec::SmallVec;

use crate::{
  exec_term,
  signal::FOCUS_GAINED,
  state::{logic::AutoCmdKind, terminal::CursorCtl, util::with_vars},
};

use super::{
  KeyEvent, KeyMapMatch, Prompt, ReadlineEvent, ShErrKind, ShResult, Shed, ShedLine, autocmd,
  errln,
  eval::execute::exec_int,
  lifecycle::{self, first_run_setup},
  outln, sherr, shopt,
  signal::{
    GOT_SIGUSR1, GOT_SIGWINCH, JOB_DONE, QUIT_CODE, check_signals, sig_setup, signals_pending,
  },
  socket::{ShedSocket, handle_socket_request},
  state::{
    self,
    meta::MetaTab,
    terminal::{TermGuard, Terminal},
    util::{rc_file_path, source_login, source_rc},
  },
  try_var, util,
};

static PARENT_PROCESS_ID: OnceLock<Pid> = OnceLock::new();

thread_local! {
  static REPL_ENTRIES: AtomicUsize = const { AtomicUsize::new(1) };
}

fn get_repl_entry_name() -> Rc<str> {
  let id = REPL_ENTRIES.with(|c| c.fetch_add(1, Ordering::SeqCst));
  format!("repl_entry #{id}").into()
}

fn was_reparented() -> bool {
  let Some(&ppid) = PARENT_PROCESS_ID.get() else {
    return false;
  };
  let now = getppid();
  now != ppid
}

fn handle_signals_interactive(readline: &mut ShedLine) -> ShResult<bool> {
  // Handle any pending signals
  while signals_pending() {
    if let Err(e) = check_signals() {
      match e.kind() {
        ShErrKind::Interrupt => {
          // We got Ctrl+C - clear current input and redraw
          readline.reset_active_widget(false)?;
        }
        ShErrKind::CleanExit(code) => {
          QUIT_CODE.store(*code, Ordering::SeqCst);
          return Ok(false);
        }
        _ => e.print_error(),
      }
    }
  }

  if GOT_SIGWINCH.swap(false, Ordering::SeqCst) {
    log::info!("Window size change detected, updating readline dimensions");
    // Restore cursor to saved row before clearing, since the terminal
    // may have moved it during resize/rewrap
    Shed::term_mut(Terminal::update_t_dims);
    readline.mark_dirty();
  }

  if JOB_DONE.swap(false, Ordering::SeqCst)
    || GOT_SIGUSR1.swap(false, Ordering::SeqCst)
    || FOCUS_GAINED.swap(false, Ordering::SeqCst)
  {
    readline.mark_dirty();
    readline.prompt_mut().refresh();
  }

  if readline.needs_redraw() {
    readline.print_line(false)?;
  }

  Ok(true)
}

enum ShedPollTimeout {
  Null,
  Zero,
  PendingKeymap,
  Override(PollTimeout),
  IdleTimeout(PollTimeout),
}

impl From<&ShedPollTimeout> for PollTimeout {
  fn from(value: &ShedPollTimeout) -> Self {
    match value {
      ShedPollTimeout::Override(poll_timeout) | ShedPollTimeout::IdleTimeout(poll_timeout) => {
        *poll_timeout
      }
      ShedPollTimeout::Null => PollTimeout::NONE,
      ShedPollTimeout::Zero => PollTimeout::ZERO,
      ShedPollTimeout::PendingKeymap => PollTimeout::from(1000u16),
    }
  }
}

fn get_poll_timeout(readline: &mut ShedLine) -> ShedPollTimeout {
  if Shed::term(Terminal::reader_has_pending) {
    ShedPollTimeout::Zero
  } else if !readline.pending_keymap().is_empty() {
    // wait for more keymap keys
    ShedPollTimeout::PendingKeymap
  } else if let Some(timeout) = Shed::meta_mut(MetaTab::take_poll_timeout) {
    // something gave us an explicit poll timeout to use.
    // usually this means there is a status message showing.
    // after the timeout, it will trigger a redraw that clears
    // the status message.
    ShedPollTimeout::Override(timeout)
  } else {
    let timeout = shopt!(prompt.idle_timeout);
    if timeout.is_zero() {
      ShedPollTimeout::Null
    } else {
      // instead of naively using the timeout given to us, lets compare it to the total time we've spent idle.
      // if our timeout is 10 seconds, and we've spent 13 seconds idle, then in order to catch the next bus on time,
      // we need a 7 second timeout.
      let elapsed = Shed::term_mut(Terminal::last_input_elapsed);
      let timeout = timeout.duration().saturating_sub(Duration::from_nanos(
        (elapsed.as_nanos() % timeout.duration().as_nanos()) as u64,
      ));

      let poll_timeout = PollTimeout::try_from(timeout).unwrap_or(PollTimeout::NONE);
      ShedPollTimeout::IdleTimeout(poll_timeout)
    }
  }
}

fn interactive_setup(args: &lifecycle::ShedArgs) -> ShResult<TermGuard> {
  let raw_mode = Shed::term_mut(Terminal::setup_terminal)?;

  sig_setup(args.login_shell);

  MetaTab::ensure_meta_table()?;
  Shed::create_socket()?;

  if let Some(msg) = MetaTab::welcome_message(args.welcome) {
    outln!("\n{msg}\n\n");
  }

  if args.login_shell && !args.no_rc {
    source_login().ok();
  }

  if rc_file_path().is_none_or(|f| !f.is_file()) {
    // we didn't find any runtime files at all
    // let's run a first time setup
    if let Err(e) = first_run_setup() {
      e.print_error();
    }
  }

  if !args.no_rc
    && let Err(e) = source_rc()
  {
    e.print_error();
  }

  if let Some(welcome) = try_var!("SHELL_WELCOME") {
    // support for systemd's run0 message
    errln!("\n{welcome}\n\n");
  }

  Shed::term_mut(Terminal::reserve_status_rows).ok();

  Ok(raw_mode)
}

pub(super) fn shed_interactive(
  args: &lifecycle::ShedArgs,
  script_keys: Option<Vec<KeyEvent>>,
) -> ShResult<()> {
  let _raw_mode = interactive_setup(args)?;
  state::util::try_hash();
  Shed::meta_mut(|m| m.set_interactive_shell(true));
  let _ = PARENT_PROCESS_ID.set(getppid());

  let mut readline = match ShedLine::new(Prompt::new()) {
    Ok(rl) => rl,
    Err(e) => {
      // try to fall back to no hist
      match ShedLine::new_no_hist(Prompt::new()) {
        Ok(rl) => {
          errln!("Failed to load history: {e}");
          rl
        }
        Err(e) => {
          // that failed too. we probably arent in a context where readline can work at all.
          errln!("Failed to initialize readline: {e}");
          QUIT_CODE.store(1, Ordering::SeqCst);
          return Err(sherr!(CleanExit(1), "readline initialization failed",));
        }
      }
    }
  };
  if let Some(keys) = script_keys {
    return run_script_keys(&mut readline, keys);
  }

  let mut vi_mode = shopt!(set.vi);
  let mut socket_mode = ShedSocket::mode();

  let mut poll_fds: SmallVec<[PollFd; 2]> = SmallVec::new();
  let Some(tty_fd) = Shed::term(|t| t.tty().map(|fd| fd.as_raw_fd())) else {
    errln!("Failed to access terminal file descriptor");
    QUIT_CODE.store(1, Ordering::SeqCst);
    return Err(sherr!(CleanExit(1), "terminal access failed",));
  };
  let tty_poll = PollFd::new(unsafe { BorrowedFd::borrow_raw(tty_fd) }, PollFlags::POLLIN);

  let socket_fd = Shed::get_socket().map(|s| s.as_raw_fd());
  let socket_poll =
    socket_fd.map(|fd| PollFd::new(unsafe { BorrowedFd::borrow_raw(fd) }, PollFlags::POLLIN));

  // Main poll loop
  loop {
    match shed_loop_iter(
      &mut readline,
      &mut poll_fds,
      &tty_poll,
      socket_poll.as_ref(),
      &mut vi_mode,
      &mut socket_mode,
    )? {
      LoopAction::Continue => (),
      LoopAction::Break => return Ok(()),
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopAction {
  Continue,
  Break,
}

#[expect(clippy::too_many_lines)]
fn shed_loop_iter(
  readline: &mut ShedLine,
  poll_fds: &mut SmallVec<[PollFd<'static>; 2]>,
  tty_poll: &PollFd<'static>,
  socket_poll: Option<&PollFd<'static>>,
  vi_mode: &mut bool,
  socket_mode: &mut Mode,
) -> ShResult<LoopAction> {
  if was_reparented() {
    // our parent is dead. tragic.
    // we should also die.
    QUIT_CODE.store(0, Ordering::SeqCst);
    return Ok(LoopAction::Break);
  }

  // make absolutely sure we are in raw mode here.
  // we did enable raw mode above, but there do exist extreme corner cases where
  // raw mode can be disabled in such a way that it is still turned off once we get back here.
  //
  // one example:
  // 1. fork child process
  // 2. child process forks another child
  // 3. we reap our child process, and the grandchild is orphaned
  // 4. we get back to the loop here, grandchild alters termios
  // 5. shell is softlocked
  if let Err(e) = Shed::term_mut(Terminal::enforce_raw_mode)
    && let ShErrKind::Errno(errno) = e.kind()
  {
    if let Errno::EINTR = errno {
      return Ok(LoopAction::Continue);
    }
    return Err(sherr!(CleanExit(1), "Failed to set raw mode: {e}"));
  }
  exec_term!(
    TermCtl::SetAttr(BracketPaste(On)),
    TermCtl::SetAttr(FocusReport(On))
  )
  .ok();

  state::util::try_hash();
  util::flog::update_log_level();
  let _flush_guard = state::terminal::FlushGuard; // flushes terminal on drop

  poll_fds.clear();
  poll_fds.push(tty_poll.clone());
  if let Some(fd) = socket_poll {
    poll_fds.push(fd.clone());
  }

  if shopt!(set.vi) != *vi_mode {
    // the editing mode option changed.
    // we have to make sure the edit mode reflects the option now
    readline.fix_editing_mode();

    *vi_mode = !(*vi_mode); // and toggle this
  } else if Shed::num_subscribers() == 0 && readline.in_insert_mode() {
    // we are in remote mode with no consumers for our broadcasted input.
    // That effectively soft locks the shell, so let's fix that
    readline.fix_editing_mode();
  }

  if !handle_signals_interactive(readline)? {
    return Ok(LoopAction::Break);
  }

  let timeout = get_poll_timeout(readline);
  Shed::term_mut(std::io::Write::flush)?;

  match poll(poll_fds, &timeout) {
    Ok(0) => {
      // We timed out. Dispatch based on what kind of timeout fired.
      match timeout {
        ShedPollTimeout::Override(_) => {
          // status message or something
          // let's redraw
          readline.mark_dirty();
        }
        ShedPollTimeout::IdleTimeout(_) => {
          // We have to scaffold the autocmd execution here manually
          // instead of using `autocmd!`, because the macro executes non-interactively.
          // IdleTimeout autocmds execute in an interactive context.
          let cmds = Shed::logic(|l| l.get_autocmds(AutoCmdKind::OnIdleTimeout));
          Shed::notify_autocmd(AutoCmdKind::OnIdleTimeout);
          let saved_status = Shed::get_status();
          let idle_secs = Shed::term_mut(Terminal::last_input_elapsed)
            .as_secs_f64()
            .round() as u64;

          let res = with_vars(
            [("IDLE_SECONDS".to_string(), idle_secs.to_string())],
            || -> ShResult<LoopAction> {
              scopeguard::defer! {
                Shed::set_status(saved_status);
              }

              for cmd in cmds {
                if let LoopAction::Break =
                  run_prompt_command(cmd.command().to_string(), None, None)?
                {
                  return Ok(LoopAction::Break);
                }
              }
              Ok(LoopAction::Continue)
            },
          );

          readline.mark_dirty();
          return res;
        }
        ShedPollTimeout::Null | ShedPollTimeout::Zero | ShedPollTimeout::PendingKeymap => { /* do nothing */
        }
      }
    }
    Err(Errno::EINTR) => {
      // Interrupted by signal, loop back to handle it
      return Ok(LoopAction::Continue);
    }
    Err(e) => {
      errln!("poll error: {e}");
      return Ok(LoopAction::Break);
    }
    Ok(_) => {}
  }

  // resolve pending keymap ambiguity
  if !readline.pending_keymap().is_empty()
    && poll_fds[0]
      .revents()
      .is_none_or(|r| !r.contains(PollFlags::POLLIN))
  {
    resolve_keymap(readline)?;
    return Ok(LoopAction::Continue);
  }

  // Check if stdin has data
  if let Some(revents) = poll_fds[0].revents() {
    if revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL) {
      // we don't have a terminal anymore. let's get outta here
      QUIT_CODE.store(0, Ordering::SeqCst);
      return Ok(LoopAction::Break);
    }
    if revents.contains(PollFlags::POLLIN) {
      // read data here, process it below
      if let Err(e) = Shed::term_mut(Terminal::read) {
        match e.kind() {
          ShErrKind::LoopBreak(_) => return Ok(LoopAction::Break),
          ShErrKind::LoopContinue(_) => return Ok(LoopAction::Continue),
          _ => {
            e.print_error();
            return Ok(LoopAction::Break);
          }
        }
      }
    }
  }

  // check socket fd
  if poll_fds
    .get(1)
    .and_then(nix::poll::PollFd::revents)
    .is_some_and(|r| r.contains(PollFlags::POLLIN))
  {
    let requests = Shed::read_socket();
    for (conn, req) in requests {
      let res = handle_socket_request(conn, req, readline).transpose();
      if let Some(event) = res
        && let LoopAction::Break = handle_readline_event(readline, event)?
      {
        return Ok(LoopAction::Break);
      }
    }
  }

  // Process the input that we read above
  let keys = Shed::term_mut(Terminal::drain_keys);
  let event = readline.process_input(keys);

  if let LoopAction::Break = handle_readline_event(readline, event)? {
    return Ok(LoopAction::Break);
  }

  let requests = Shed::read_socket();
  if !requests.is_empty() {
    for (conn, req) in requests {
      handle_socket_request(conn, req, readline).ok();
    }
    readline.print_line(false)?;
  }

  // check the socket mode
  let curr_socket_mode = ShedSocket::mode();

  if curr_socket_mode != *socket_mode {
    // the mode changed, call chmod
    let path = ShedSocket::path();
    fchmodat(
      nix::fcntl::AT_FDCWD,
      Path::new(&path),
      curr_socket_mode,
      FchmodatFlags::FollowSymlink,
    )
    .ok();
    *socket_mode = curr_socket_mode;
  }

  Ok(LoopAction::Continue)
}

/// Run some provided key events. These come from the --edit-script flag,
/// and are meant to simulate a user typing those keys interactively.
/// Mainly useful for testing/profiling, but may have some other legitimate niche use-cases
fn run_script_keys(readline: &mut ShedLine, keys: Vec<KeyEvent>) -> ShResult<()> {
  let mut queue: VecDeque<KeyEvent> = keys.into();
  while let Some(key) = queue.pop_front() {
    let event = readline.process_input(vec![key]);
    if let LoopAction::Break = handle_readline_event(readline, event)? {
      return Ok(());
    }
  }
  if !readline.pending_keymap().is_empty() {
    resolve_keymap(readline)?;
  }
  Ok(())
}

/// Handle a `ReadlineEvent`. Returns a boolean, `true` means "exit the shell", `false` means "keep looping"
fn handle_readline_event(
  readline: &mut ShedLine,
  event: ShResult<ReadlineEvent>,
) -> ShResult<LoopAction> {
  match event {
    Ok(ReadlineEvent::Line(input)) => {
      let token = shopt!(core.auto_hist)
        .then(|| readline.history_mut().push(&input).ok().flatten())
        .flatten(); // token is used as a stable identifier for the command in the history

      autocmd!(PreCmd);

      let cmd_start = Instant::now();
      Shed::meta_mut(MetaTab::start_timer);

      if let LoopAction::Break = run_prompt_command(
        input.clone(),
        Some(Redraw::Full),
        Some(get_repl_entry_name()),
      )? {
        return Ok(LoopAction::Break);
      }

      Shed::term_mut(Terminal::reset_last_input);

      let command_run_time = cmd_start.elapsed();
      log::info!("Command executed in {command_run_time:.2?}");
      let runtime = Shed::meta_mut(MetaTab::stop_timer);

      let autocmd_start = Instant::now();

      autocmd!(PostCmd);
      log::trace!("PostCmd autocmds done in {autocmd_start:.2?}");

      let no_hist_save = Shed::meta_mut(MetaTab::no_hist_save);

      let was_func_def = Shed::meta_mut(MetaTab::take_last_was_func_def);
      let nolog = was_func_def && shopt!(set.nolog);

      let should_write = shopt!(core.auto_hist) && !nolog && !no_hist_save && !input.is_empty();

      let hist_update_start = Instant::now();

      if let Some(token) = token
        && !should_write
      {
        readline
          .history_mut()
          .delete("WHERE token = ?1", rusqlite::params![token.to_string()])?;
      }

      if shopt!(core.auto_hist)
        && should_write
        && let Some(token) = token
      {
        readline
          .history_mut()
          .set_status(token, runtime, state::Shed::get_status());
      }
      log::trace!("History update done in {:.2?}", hist_update_start.elapsed());

      let term_start = Instant::now();

      log::trace!("Terminal adjustments done in {:.2?}", term_start.elapsed());

      // Reset for next command with fresh prompt
      readline.reset(true)?;

      log::trace!("Readline event handled in {:.2?}", cmd_start.elapsed());
      Ok(LoopAction::Continue)
    }
    Ok(ReadlineEvent::Eof) => {
      // Ctrl+D on empty line
      QUIT_CODE.store(0, Ordering::SeqCst);
      Ok(LoopAction::Break)
    }
    Ok(ReadlineEvent::Pending) => {
      // No complete input yet, keep polling
      Ok(LoopAction::Continue)
    }
    Err(e) => {
      if let ShErrKind::CleanExit(code) = e.kind() {
        QUIT_CODE.store(*code, Ordering::SeqCst);
        Ok(LoopAction::Break)
      } else {
        let err = e.to_string();
        Shed::post_system_msg(err);
        readline.reset(true)?;
        Ok(LoopAction::Continue)
      }
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Redraw {
  /// Redraw the prompt with a newline
  Full,
  /// Redraw with no newline
  Partial,
}

pub(crate) fn run_prompt_command(
  input: String,
  clear_prompt: Option<Redraw>,
  source: Option<Rc<str>>,
) -> ShResult<LoopAction> {
  exec_term!(TermCtl::Osc(ExecStart)).ok();

  let position = (clear_prompt.is_none())
    .then(|| Shed::term_mut(Terminal::get_cursor_pos))
    .transpose()?
    .flatten();

  let res = {
    let _scroll_guard = Shed::term_mut(|t| t.yield_terminal(clear_prompt.is_some()));
    exec_int(input, source)
  };

  if let Some((row, col)) = position {
    exec_term!(TermCtl::Cursor(CursorCtl::Absolute {
      row: row.0 as u16,
      col: col.0 as u16
    }))
    .ok();
  }

  exec_term!(TermCtl::Osc(ExecEnd(Shed::get_status()))).ok();

  if let Some(clear) = clear_prompt {
    Shed::term_mut(|t| t.fix_cursor_column(clear == Redraw::Full))?;
  }

  if let Err(e) = res {
    match e.kind() {
      ShErrKind::Interrupt => {
        // We got Ctrl+C during command execution
        // Just fall through here
      }
      ShErrKind::CleanExit(code) => {
        QUIT_CODE.store(*code, Ordering::SeqCst);
        return Ok(LoopAction::Break);
      }
      ShErrKind::ErrInterrupt => {
        // set -e exit path
        QUIT_CODE.store(0, Ordering::SeqCst);
        e.print_error();
        return Ok(LoopAction::Break);
      }
      _ => e.print_error(),
    }
  }
  Ok(LoopAction::Continue)
}

fn resolve_keymap(readline: &mut ShedLine) -> ShResult<()> {
  let keymap_flags = readline.curr_keymap_flags();
  let matches = Shed::logic(|l| l.keymaps_filtered(keymap_flags, readline.pending_keymap()));
  // If there's an exact match, fire it; otherwise flush as normal keys
  let exact = matches
    .iter()
    .find(|km| km.compare(readline.pending_keymap()) == KeyMapMatch::IsExact);
  if let Some(km) = exact {
    let action = km.action_expanded();
    readline.pending_keymap_mut().clear();
    for key in action {
      let event = readline.handle_key(&key).transpose();
      if let Some(event) = event {
        handle_readline_event(readline, event)?;
      }
    }
  } else {
    let buffered = std::mem::take(readline.pending_keymap_mut());
    for key in buffered {
      let event = readline.handle_key(&key).transpose();
      if let Some(event) = event {
        handle_readline_event(readline, event)?;
      }
    }
  }
  readline.print_line(false)?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::shopt_mut;
  use crate::state::logic::AutoCmd;
  use crate::tests::testutil::TestGuard;
  use crate::{keys::KeyMap, var};

  /// Bundle of state needed to call `shed_loop_iter` from a test, so
  /// individual cases don't repeat the boilerplate.
  struct LoopHarness {
    g: TestGuard,
    readline: ShedLine,
    poll_fds: SmallVec<[PollFd<'static>; 2]>,
    tty_poll: PollFd<'static>,
    socket_poll: Option<PollFd<'static>>,
    vi_mode: bool,
    socket_mode: Mode,
  }

  impl LoopHarness {
    fn new() -> Self {
      let g = TestGuard::new();
      let readline = ShedLine::new_no_hist(Prompt::default()).unwrap();
      let tty_fd = Shed::term(|t| t.tty().map(|fd| fd.as_raw_fd())).unwrap();
      let tty_poll = PollFd::new(unsafe { BorrowedFd::borrow_raw(tty_fd) }, PollFlags::POLLIN);
      // Put the tty in raw mode up-front so subsequent type_chars writes
      // aren't intercepted by the kernel's cooked-mode special chars
      // (VEOF=^D, VINTR=^C, VERASE=^?, etc.). Without this, those bytes
      // are consumed by the tty driver before they reach shed.
      Shed::term_mut(Terminal::enforce_raw_mode).unwrap();
      Self {
        g,
        readline,
        poll_fds: SmallVec::new(),
        tty_poll,
        socket_poll: None,
        vi_mode: shopt!(set.vi),
        socket_mode: ShedSocket::mode(),
      }
    }

    /// Run a single loop iteration.
    fn iterate(&mut self) -> ShResult<LoopAction> {
      shed_loop_iter(
        &mut self.readline,
        &mut self.poll_fds,
        &self.tty_poll,
        self.socket_poll.as_ref(),
        &mut self.vi_mode,
        &mut self.socket_mode,
      )
    }

    fn emacs() -> Self {
      let new = Self::new();
      shopt_mut!(set.vi = false);
      new
    }

    #[expect(dead_code)]
    fn vi() -> Self {
      let new = Self::new();
      shopt_mut!(set.vi = true);
      new
    }

    /// Push bytes onto the pty master so they appear as readable input on
    /// shed's tty fd.
    fn type_chars(&self, data: &[u8]) {
      self.g.feed_tty(data);
    }

    fn editor_content(&self) -> String {
      self.readline.editor().to_string()
    }
  }

  /// Force the next `poll()` to time out almost immediately so a test
  /// without typed input doesn't hang on the default `PollTimeout::NONE`.
  fn arm_short_poll_timeout() {
    Shed::meta_mut(|m| m.set_poll_timeout(Some(PollTimeout::from(20u8))));
  }

  #[test]
  fn loop_iter_continues_when_poll_times_out_with_no_input() {
    let mut h = LoopHarness::emacs();
    arm_short_poll_timeout();
    let action = h.iterate().unwrap();
    assert!(matches!(action, LoopAction::Continue));
  }

  #[test]
  fn loop_iter_consumes_typed_char_into_buffer() {
    let mut h = LoopHarness::emacs();
    h.type_chars(b"x");
    let action = h.iterate().unwrap();
    let content = h.editor_content();
    assert!(matches!(action, LoopAction::Continue));
    assert_eq!(content, "x");
  }

  #[test]
  fn loop_iter_consumes_multi_byte_input() {
    let mut h = LoopHarness::emacs();
    h.type_chars(b"abc");
    let action = h.iterate().unwrap();
    assert!(matches!(action, LoopAction::Continue));
    let content = h.editor_content();
    assert_eq!(content, "abc");
  }

  #[test]
  fn loop_iter_ctrl_d_behavior() {
    // Ctrl+D against a non-empty buffer is a delete-char operation,
    // never an EOF, so we always stay in the loop.
    {
      let mut h = LoopHarness::emacs();
      h.type_chars(b"not empty");
      h.type_chars(b"\x01"); // ctrl+a (beginning of line)
      h.type_chars(b"\x04"); // ctrl+d (delete char under cursor)
      let action = h.iterate().unwrap();
      let content = h.editor_content();
      assert!(matches!(action, LoopAction::Continue));
      assert_eq!(content, "ot empty");
    }

    // Ctrl+D by itself - breaks loop
    {
      let mut h = LoopHarness::emacs();
      h.type_chars(b"\x04");
      let action = h.iterate().unwrap();
      assert!(matches!(action, LoopAction::Break));
    }
  }

  #[test]
  fn loop_iter_swaps_editing_mode_when_shopt_changes() {
    let mut h = LoopHarness::emacs();
    assert!(!h.vi_mode, "harness should start in emacs");

    // Flip the shopt: shed_loop_iter detects the mismatch with its cached
    // vi_mode and calls fix_editing_mode to swap modes for real.
    shopt_mut!(set.vi = true);
    arm_short_poll_timeout();
    h.iterate().unwrap();

    assert!(h.vi_mode, "harness vi_mode flag should now be true");
    // ShedLine's actual mode should be ViInsert, which reports "INSERT".
    assert_eq!(var!("SHED_EDIT_MODE"), "INSERT");
  }

  #[test]
  fn loop_iter_fires_idle_timeout_autocmd_with_idle_seconds_visible() {
    let mut h = LoopHarness::emacs();

    // Register an on-idle-timeout autocmd that captures IDLE_SECONDS into
    // IDLE_FIRED. Asserting on IDLE_FIRED at the end proves both that the
    // autocmd ran AND that $IDLE_SECONDS was visible to its command body
    // (an empty value would indicate the variable wasn't exposed).
    Shed::logic_mut(|l| {
      l.clear_autocmds(AutoCmdKind::OnIdleTimeout);
      l.insert_autocmd(AutoCmd::new(
        AutoCmdKind::OnIdleTimeout,
        "export IDLE_FIRED=$IDLE_SECONDS".into(),
      ));
    });

    // 50ms — long enough for poll to wake on its own timeout, short enough
    // to keep the test fast.
    shopt_mut!(prompt.idle_timeout = 0.05.into());

    let action = h.iterate().unwrap();
    assert!(matches!(action, LoopAction::Continue));

    // For a 50ms idle interval, as_secs() truncates to 0, so IDLE_SECONDS
    // expands to "0". The non-empty check guards against the regression
    // where the variable isn't set at all.
    let fired = var!("IDLE_FIRED");
    assert!(
      !fired.is_empty(),
      "expected IDLE_FIRED to be set by on-idle-timeout autocmd; got empty"
    );
    assert_eq!(fired, "0", "IDLE_SECONDS for a sub-second interval is 0");
  }

  #[test]
  fn loop_iter_buffers_partial_multi_key_keymap() {
    let mut h = LoopHarness::emacs();
    // Register "jk" → <esc>. After typing just "j", pending_keymap should
    // hold the 'j' KeyEvent waiting for the next key to disambiguate.
    Shed::logic_mut(|l| {
      l.insert_keymap(KeyMap {
        flags: crate::keys::KeyMapFlags::EMACS,
        keys: "jk".into(),
        action: "<esc>".into(),
      });
    });

    h.type_chars(b"j");
    let action = h.iterate().unwrap();
    assert!(matches!(action, LoopAction::Continue));
    assert!(
      !h.readline.pending_keymap().is_empty(),
      "expected 'j' to be buffered as a partial keymap match"
    );
    // The buffer itself should still be empty — the 'j' is held pending,
    // not yet inserted as a literal.
    assert_eq!(h.readline.editor().to_string(), "");
  }

  #[test]
  fn loop_iter_consumes_pending_sigwinch_inside_handle_signals() {
    let mut h = LoopHarness::new();
    GOT_SIGWINCH.store(true, Ordering::SeqCst);
    arm_short_poll_timeout();
    let action = h.iterate().unwrap();
    assert!(matches!(action, LoopAction::Continue));
    // The signal-handling path swap()s the flag back to false; verify
    // shed_loop_iter actually reached and processed it.
    assert!(!GOT_SIGWINCH.load(Ordering::SeqCst));
  }

  #[test]
  #[cfg_attr(
    target_os = "macos",
    ignore = "macOS doesn't deliver POLLHUP on pty master close"
  )]
  fn loop_iter_breaks_on_tty_pollhup() {
    let mut h = LoopHarness::new();
    // Closing the master end makes the slave (shed's tty) raise POLLHUP
    // on the next poll, signalling the terminal disappeared.
    h.g.close_tty_master();
    let action = h.iterate().unwrap();
    assert!(matches!(action, LoopAction::Break));
  }

  // ===================== resolve_keymap =====================

  use crate::expand::expand_keymap;
  use crate::keys::KeyMapFlags;

  fn fresh_readline() -> (ShedLine, TestGuard) {
    let g = TestGuard::new();
    let mut readline = ShedLine::new_no_hist(Prompt::default()).unwrap();
    // print_line needs interactive guard to write properly; mirror what
    // the loop sets up.
    let _guard = Shed::term_mut(|t| t.interactive_guard(true));
    // Disable any leftover keymaps from prior tests.
    Shed::logic_mut(|l| {
      // We can't easily clear all keymaps; just reset to known state by
      // overwriting any KeyMap we set up.
      let _ = l;
    });
    // Start in raw mode to keep things deterministic.
    Shed::term_mut(Terminal::enforce_raw_mode).ok();
    readline.print_line(false).ok();
    g.read_output();
    (readline, g)
  }

  #[test]
  fn resolve_keymap_no_match_flushes_keys_as_typed() {
    let (mut readline, _g) = fresh_readline();
    // Push 'a' to pending; no matching keymap should be registered.
    for k in expand_keymap("a") {
      readline.pending_keymap_mut().push(k);
    }
    resolve_keymap(&mut readline).unwrap();
    assert_eq!(readline.editor().to_string(), "a");
    assert!(readline.pending_keymap().is_empty());
  }

  #[test]
  fn resolve_keymap_exact_match_fires_action() {
    let (mut readline, _g) = fresh_readline();
    // Default mode is Emacs (vi shopt off), so the keymap must have
    // EMACS flag to be considered.
    let km = KeyMap {
      flags: KeyMapFlags::EMACS,
      keys: "ab".into(),
      action: "xy".into(),
    };
    Shed::logic_mut(|l| l.insert_keymap(km));
    // Feed the pending bytes that match.
    for k in expand_keymap("ab") {
      readline.pending_keymap_mut().push(k);
    }
    resolve_keymap(&mut readline).unwrap();
    // The keymap action 'xy' should have been typed into the buffer.
    assert_eq!(readline.editor().to_string(), "xy");
    assert!(readline.pending_keymap().is_empty());
  }

  #[test]
  fn resolve_keymap_empty_pending_is_noop() {
    let (mut readline, _g) = fresh_readline();
    assert!(readline.pending_keymap().is_empty());
    resolve_keymap(&mut readline).unwrap();
    assert_eq!(readline.editor().to_string(), "");
  }

  // ===================== handle_readline_event =====================

  use crate::util::{ShErr, ShErrKind};

  #[test]
  fn handle_event_eof_returns_true() {
    let (mut readline, _g) = fresh_readline();
    let should_exit = handle_readline_event(&mut readline, Ok(ReadlineEvent::Eof)).unwrap();
    assert!(matches!(should_exit, LoopAction::Break));
  }

  #[test]
  fn handle_event_pending_returns_false() {
    let (mut readline, _g) = fresh_readline();
    let should_exit = handle_readline_event(&mut readline, Ok(ReadlineEvent::Pending)).unwrap();
    assert!(matches!(should_exit, LoopAction::Continue));
  }

  #[test]
  fn handle_event_err_clean_exit_returns_true() {
    let (mut readline, _g) = fresh_readline();
    let err = ShErr::new(ShErrKind::CleanExit(0), crate::eval::lex::Span::default());
    let should_exit = handle_readline_event(&mut readline, Err(err)).unwrap();
    assert!(matches!(should_exit, LoopAction::Break));
  }

  #[test]
  fn handle_event_err_other_returns_false() {
    let (mut readline, _g) = fresh_readline();
    let err = ShErr::new(ShErrKind::ParseErr, crate::eval::lex::Span::default());
    let should_exit = handle_readline_event(&mut readline, Err(err)).unwrap();
    assert!(matches!(should_exit, LoopAction::Continue));
  }

  #[test]
  fn handle_event_line_runs_command_returns_false() {
    let (mut readline, _g) = fresh_readline();
    // A trivial no-op command. Should execute and resume looping.
    let should_exit =
      handle_readline_event(&mut readline, Ok(ReadlineEvent::Line(":".into()))).unwrap();
    assert!(matches!(should_exit, LoopAction::Continue));
  }

  #[test]
  fn handle_event_line_empty_input_returns_false() {
    let (mut readline, _g) = fresh_readline();
    let should_exit =
      handle_readline_event(&mut readline, Ok(ReadlineEvent::Line(String::new()))).unwrap();
    assert!(matches!(should_exit, LoopAction::Continue));
  }

  // ===================== run_script_keys =====================

  #[test]
  fn run_script_keys_empty_vec_is_ok() {
    let (mut readline, _g) = fresh_readline();
    // Empty queue → loop body never runs, pending_keymap is empty, Ok.
    run_script_keys(&mut readline, vec![]).unwrap();
    assert_eq!(readline.editor().to_string(), "");
  }

  #[test]
  fn run_script_keys_types_into_editor_buffer() {
    let (mut readline, _g) = fresh_readline();
    // Type "abc" with no Enter — chars go into the editor buffer.
    let keys = expand_keymap("abc");
    run_script_keys(&mut readline, keys).unwrap();
    assert_eq!(readline.editor().to_string(), "abc");
  }

  #[test]
  fn run_script_keys_runs_command_on_enter() {
    let (mut readline, g) = fresh_readline();
    // After Enter we should hit the Line event arm; the command runs
    // via handle_readline_event and the editor buffer is reset.
    let keys = expand_keymap("echo run_script_keys_hello<CR>");
    run_script_keys(&mut readline, keys).unwrap();
    let out = g.read_output();
    assert!(
      out.contains("run_script_keys_hello"),
      "expected echo output, got: {out:?}"
    );
  }
}
