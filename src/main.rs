#![allow(
  clippy::derivable_impls,
  clippy::tabs_in_doc_comments,
  clippy::while_let_on_iterator,
  clippy::result_large_err
)]
pub mod builtin;
pub mod expand;
pub mod getopt;
pub mod jobs;
pub mod libsh;
pub mod parse;
pub mod prelude;
pub mod procio;
pub mod readline;
pub mod shopt;
pub mod signal;
pub mod state;

#[cfg(test)]
mod tests;
#[cfg(test)]
pub mod testutil;

use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::stat::{FchmodatFlags, fchmodat};
use nix::unistd::read;
use smallvec::SmallVec;

use crate::builtin::keymap::KeyMapMatch;
use crate::builtin::trap::TrapTarget;
use crate::libsh::error::{self, ShErrKind, ShResult};
use crate::libsh::utils::AutoCmdVecUtils;
use crate::parse::execute::{exec_dash_c, exec_input};
use crate::prelude::*;
use crate::procio::borrow_fd;
use crate::readline::editmode::ModeReport;
use crate::readline::linebuf::{Hint, Pos, to_lines};
use crate::readline::term::{OSC_EXEC_START, osc_exec_end};
use crate::readline::{LineData, Prompt, ReadlineEvent, ShedLine};
use crate::signal::{
  GOT_SIGUSR1, GOT_SIGWINCH, JOB_DONE, QUIT_CODE, check_signals, sig_setup, signals_pending,
};
use crate::state::{
  AutoCmdKind, LineHeader, QueryHeader, ShedSocket, SocketRequest, StatusHeader, VarFlags, VarKind, generate_default_rc, rc_file_path, read_logic, read_meta, read_shopts, read_vars, source_env, source_login, source_rc, with_term, write_jobs, write_meta, write_shopts
};
use clap::Parser;
use state::write_vars;

#[derive(Parser, Debug)]
struct ShedArgs {
  #[arg(short)]
  command: Option<String>,

  #[arg(trailing_var_arg = true)]
  script_args: Vec<String>,

  #[arg(long)]
  version: bool,

  #[arg(short)]
  interactive: bool,

  #[arg(short)]
  stdin: bool,

  #[arg(long, short)]
  login_shell: bool,

  #[arg(long, short)]
  welcome: bool,
}

/// We need to make sure that even if we panic, our child processes get sighup
fn setup_panic_handler() {
  let default_panic_hook = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    write_jobs(|j| j.hang_up());

    let data_dir = dirs::data_dir().unwrap_or_else(|| {
      let home = env::var("HOME").unwrap();
      PathBuf::from(format!("{home}/.local/share"))
    });
    let log_dir = data_dir.join("shed").join("log");
    std::fs::create_dir_all(&log_dir).unwrap();
    let log_file_path = log_dir.join("panic.log");
    let mut log_file = parse::get_redir_file(parse::RedirType::Output, log_file_path).unwrap();

    let panic_info_raw = info.to_string();
    log_file.write_all(panic_info_raw.as_bytes()).unwrap();

    let backtrace = std::backtrace::Backtrace::force_capture();
    log_file
      .write_all(format!("\nBacktrace:\n{:?}", backtrace).as_bytes())
      .unwrap();

    default_panic_hook(info);
  }));
}

fn main() -> ExitCode {
  yansi::enable();
  env_logger::init();
  setup_panic_handler();

  let mut args = ShedArgs::parse();
  if env::args().next().is_some_and(|a| a.starts_with('-')) {
    // first arg is '-shed'
    // meaning we are in a login shell
    args.login_shell = true;
  }
  if args.version {
    println!(
      "shed {} ({} {})",
      env!("CARGO_PKG_VERSION"),
      std::env::consts::ARCH,
      std::env::consts::OS
    );
    return ExitCode::SUCCESS;
  }

  // Increment SHLVL, or set to 1 if not present or invalid.
  // This var represents how many nested shell instances we're in
  if let Ok(var) = env::var("SHLVL")
    && let Ok(lvl) = var.parse::<u32>()
  {
    write_vars(|v| {
      v.set_var(
        "SHLVL",
        VarKind::Str((lvl + 1).to_string()),
        VarFlags::EXPORT,
      )
    })
    .ok();
  } else {
    write_vars(|v| v.set_var("SHLVL", VarKind::Str("1".into()), VarFlags::EXPORT)).ok();
  }

  if let Err(e) = source_env() {
    e.print_error();
  }

  state::init_db_conn();

  if let Err(e) = if let Some(cmd) = args.command {
    exec_dash_c(cmd)
  } else if args.stdin || !isatty(STDIN_FILENO).unwrap_or(false) {
    read_commands(args.script_args)
  } else if !args.script_args.is_empty() {
    let path = args.script_args.remove(0);
    run_script(path, args.script_args)
  } else {
    shed_interactive(args)
  } {
    e.print_error();
  };

  if let Some(trap) = read_logic(|l| l.get_trap(TrapTarget::Exit))
    && let Err(e) = exec_input(trap, None, false, Some("trap".into()))
  {
    e.print_error();
  }

  let on_exit_autocmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnExit));
  on_exit_autocmds.exec();

  write_jobs(|j| j.hang_up());

  let code = QUIT_CODE.load(Ordering::SeqCst) as u8;
  if code == 0 && isatty(STDIN_FILENO).unwrap_or_default() {
    write(borrow_fd(STDERR_FILENO), b"\nexit\n").ok();
  }

  ExitCode::from(QUIT_CODE.load(Ordering::SeqCst) as u8)
}

fn read_commands(args: Vec<String>) -> ShResult<()> {
  let mut input = vec![];
  let mut read_buf = [0u8; 4096];
  loop {
    match read(STDIN_FILENO, &mut read_buf) {
      Ok(0) => break,
      Ok(n) => input.extend_from_slice(&read_buf[..n]),
      Err(Errno::EINTR) => continue,
      Err(e) => {
        QUIT_CODE.store(1, Ordering::SeqCst);
        return Err(sherr!(CleanExit(1), "error reading from stdin: {e}",));
      }
    }
  }

  let commands = String::from_utf8_lossy(&input).to_string();
  for arg in args {
    write_vars(|v| v.cur_scope_mut().bpush_arg(arg))
  }

  exec_input(commands, None, false, None)
}

fn run_script<P: AsRef<Path>>(path: P, args: Vec<String>) -> ShResult<()> {
  let path = path.as_ref();
  let path_raw = path.to_string_lossy().to_string();
  if !path.is_file() {
    eprintln!("shed: Failed to open input file: {}", path.display());
    QUIT_CODE.store(1, Ordering::SeqCst);
    return Err(sherr!(CleanExit(1), "input file not found",));
  }
  let Ok(input) = fs::read_to_string(path) else {
    eprintln!("shed: Failed to read input file: {}", path.display());
    QUIT_CODE.store(1, Ordering::SeqCst);
    return Err(sherr!(CleanExit(1), "failed to read input file",));
  };

  write_vars(|v| {
    v.cur_scope_mut()
      .bpush_arg(path.to_string_lossy().to_string())
  });
  for arg in args {
    write_vars(|v| v.cur_scope_mut().bpush_arg(arg))
  }

  exec_input(input, None, false, Some(path_raw.into()))
}

fn first_run_setup() -> ShResult<()> {
  let rc_path = generate_default_rc()?;

  if let Some(rc_path) = rc_path {
    write_meta(|m| {
      m.post_status_message(format!(
        "Generated default rc file at '{}'",
        rc_path.display()
      ))
    });
  }

  Ok(())
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
    with_term(|t| t.update_t_dims());
    readline.mark_dirty();
  }

  if JOB_DONE.swap(false, Ordering::SeqCst) {
    // update the prompt so any job count escape sequences update dynamically
    readline.prompt_mut().refresh();
  }

  if GOT_SIGUSR1.swap(false, Ordering::SeqCst) {
    log::info!("SIGUSR1 received: refreshing readline state");
    readline.mark_dirty();
    readline.prompt_mut().refresh();
  }

  readline.print_line(false)?;

  Ok(true)
}

fn get_poll_timeout(readline: &mut ShedLine) -> (PollTimeout, Option<String>) {
  let mut exec_if_timeout = None;

  let timeout = if !readline.pending_keymap.is_empty() {
    // wait for more keymap keys
    PollTimeout::from(1000u16)
  } else if let Some(timeout) = write_meta(|m| m.take_poll_timeout()) {
    // something gave us an explicit poll timeout to use.
    // usually this means there is a status message showing.
    // after the timeout, it will trigger a redraw that clears
    // the status message.
    timeout
  } else {
    let screensaver_cmd = read_shopts(|o| o.prompt.screensaver_cmd.clone())
      .trim()
      .to_string();
    let screensaver_idle_time = read_shopts(|o| o.prompt.screensaver_idle_time);
    if screensaver_idle_time == 0 || screensaver_cmd.is_empty() {
      // no screensaver stuff, set no timeout
      PollTimeout::NONE
    } else {
      exec_if_timeout = Some(screensaver_cmd);
      PollTimeout::try_from((screensaver_idle_time * 1000) as i32)
        .unwrap_or(PollTimeout::NONE)
    }
  };

  (timeout, exec_if_timeout)
}

fn shed_interactive(args: ShedArgs) -> ShResult<()> {
  let _raw_mode = with_term(|t| t.raw_mode_guard())?;
  sig_setup(args.login_shell);
  crate::state::INTERACTIVE.store(true, Ordering::SeqCst);

  write_meta(|m| {
    m.ensure_meta_table()?;
    m.create_socket()
  })?;

  if let Some(msg) = read_meta(|m| m.welcome_message(args.welcome)) {
    println!("\n{msg}");
  }

  if args.login_shell {
    source_login().ok();
  }

  if rc_file_path().is_none_or(|f| !f.is_file()) {
    // we didn't find any runtime files at all
    // let's run a first time setup
    if let Err(e) = first_run_setup() {
      e.print_error();
    }
  }

  if let Err(e) = source_rc() {
    e.print_error();
  }

  if let Ok(welcome) = env::var("SHELL_WELCOME") {
    eprintln!("{welcome}");
  }

  let mut readline = match ShedLine::new(Prompt::new()) {
    Ok(rl) => rl,
    Err(e) => {
      // try to fall back to no hist
      match ShedLine::new_no_hist(Prompt::new()) {
        Ok(rl) => {
          eprintln!("Failed to load history: {e}");
          rl
        }
        Err(e) => {
          // that failed too. we probably arent in a context where readline can work at all.
          eprintln!("Failed to initialize readline: {e}");
          QUIT_CODE.store(1, Ordering::SeqCst);
          return Err(sherr!(CleanExit(1), "readline initialization failed",));
        }
      }
    }
  };
  let mut vi_mode = read_shopts(|o| o.set.vi);
  let mut socket_mode = ShedSocket::mode();

  let mut poll_fds: SmallVec<[PollFd; 2]> = SmallVec::new();
  let Some(tty_fd) = with_term(|t| unsafe { t.tty() }) else {
    eprintln!("Failed to access terminal file descriptor");
    QUIT_CODE.store(1, Ordering::SeqCst);
    return Err(sherr!(CleanExit(1), "terminal access failed",));
  };
  let tty_poll = PollFd::new(borrow_fd(tty_fd), PollFlags::POLLIN);
  let socket_fd = write_meta(|m| m.get_socket().map(|s| s.as_raw_fd()));
  let socket_poll = socket_fd.map(|fd| PollFd::new(borrow_fd(fd), PollFlags::POLLIN));

  // Main poll loop
  loop {
    state::try_hash();
    error::clear_color();
    let _flush_guard = state::FlushGuard; // flushes terminal on drop

    poll_fds.clear();
    poll_fds.push(tty_poll);
    if let Some(fd) = socket_poll {
      poll_fds.push(fd);
    }

    if read_shopts(|o| o.set.vi) != vi_mode {
      // the editing mode option changed.
      // we have to make sure the edit mode reflects the option now
      readline.fix_editing_mode();

      vi_mode = !vi_mode; // and toggle this
    } else if read_meta(|m| m.num_subscribers()) == 0
      && readline.mode.report_mode() == ModeReport::Remote
    {
      // we are in remote mode with no consumers for our broadcasted input.
      // That effectively soft locks the shell, so let's fix that
      readline.fix_editing_mode();
    }

    if !handle_signals_interactive(&mut readline)? {
      return Ok(());
    }

    let (timeout, exec_if_timeout) = get_poll_timeout(&mut readline);
    with_term(|t| t.flush())?;

    match poll(&mut poll_fds, timeout) {
      Ok(0) => {
        // We timed out. Check if there's a screensaver command
        if let Some(cmd) = exec_if_timeout
          && readline.editor.is_empty()
        {
          // don't exec screensaver if we have a pending command
          let prepared = ReadlineEvent::Line(cmd.clone());
          let _guard = scopeguard::guard(read_shopts(|o| o.core.auto_hist), |opt| {
            // restores old auto_hist value
            write_shopts(|o| o.core.auto_hist = opt);
          });
          write_shopts(|o| o.core.auto_hist = false); // don't save screensaver command to history
          let pre_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnScreensaverExec));
          pre_cmds.exec();

          let res = handle_readline_event(&mut readline, Ok(prepared))?;

          let post_cmds = read_logic(|l| l.get_autocmds(AutoCmdKind::OnScreensaverReturn));
          post_cmds.exec();

          match res {
            true => return Ok(()),
            false => continue,
          }
        }
      }
      Err(Errno::EINTR) => {
        // Interrupted by signal, loop back to handle it
        continue;
      }
      Err(e) => {
        eprintln!("poll error: {e}");
        break;
      }
      Ok(_) => {}
    }

    // resolve pending keymap ambiguity
    if !readline.pending_keymap.is_empty()
      && poll_fds[0]
        .revents()
        .is_none_or(|r| !r.contains(PollFlags::POLLIN))
    {
      resolve_keymap(&mut readline)?;
      continue;
    }

    // Check if stdin has data
    if poll_fds[0]
      .revents()
      .is_some_and(|r| r.contains(PollFlags::POLLIN))
    {
      match with_term(|t| t.read()) {
        Ok(_) => { /* data read, will be processed below */ }
        Err(e) => match e.kind() {
          ShErrKind::LoopBreak(_) => break,
          ShErrKind::LoopContinue(_) => continue,
          _ => {
            e.print_error();
            break;
          }
        },
      }
    }

    // check socket fd
    if poll_fds
      .get(1)
      .and_then(|fd| fd.revents())
      .is_some_and(|r| r.contains(PollFlags::POLLIN))
    {
      let requests = write_meta(|m| m.read_socket())?;
      for (conn, req) in requests {
        let res = handle_socket_request(conn, req, &mut readline).transpose();
        if let Some(event) = res
          && handle_readline_event(&mut readline, event)?
        {
          return Ok(());
        }
      }
    }

    // Process the input that we read above
    let keys = with_term(|t| t.drain_keys())?;
    let event = readline.process_input(keys);

    match handle_readline_event(&mut readline, event)? {
      true => return Ok(()),
      false => { /* continue looping */ }
    }

    // check the socket mode
    let curr_socket_mode = ShedSocket::mode();

    if curr_socket_mode != socket_mode {
      // the mode changed, call chmod
      let path = ShedSocket::path();
      fchmodat(
        None,
        Path::new(&path),
        curr_socket_mode,
        FchmodatFlags::FollowSymlink,
      )
      .ok();
      socket_mode = curr_socket_mode;
    }
  }

  Ok(())
}

/// Handle a ReadlineEvent. Returns a boolean, `true` means "exit the shell", `false` means "keep looping"
fn handle_readline_event(
  readline: &mut ShedLine,
  event: ShResult<ReadlineEvent>,
) -> ShResult<bool> {
  match event {
    Ok(ReadlineEvent::Line(input)) => {
      let token = read_shopts(|s| s.core.auto_hist)
        .then(|| readline.history.push(input.clone()).ok().flatten())
        .flatten(); // token is used as a stable identifier for the command in the history

      let exec_start = Instant::now();
      let pre_exec = read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd));
      let post_exec = read_logic(|l| l.get_autocmds(AutoCmdKind::PostCmd));

      pre_exec.exec();

      let cmd_start = Instant::now();
      write_meta(|m| m.start_timer());

      write_term!("{OSC_EXEC_START}").ok();

      let res = {
        // _guard restores terminal state on drop
        let _guard = with_term(|t| t.prepare_for_exec())?;
        exec_input(input.clone(), None, false, Some("<stdin>".into()))
      };

      let osc_end = osc_exec_end(state::get_status());
      write_term!("{osc_end}").ok();

      if let Err(e) = res {
        match e.kind() {
          ShErrKind::Interrupt => {
            // We got Ctrl+C during command execution
            // Just fall through here
          }
          ShErrKind::CleanExit(code) => {
            QUIT_CODE.store(*code, Ordering::SeqCst);
            return Ok(true);
          }
          ShErrKind::ErrInterrupt => {
            // set -e exit path
            QUIT_CODE.store(0, Ordering::SeqCst);
            e.print_error();
            return Ok(true);
          }
          _ => e.print_error(),
        }
      }
      let command_run_time = cmd_start.elapsed();
      log::info!("Command executed in {:.2?}", command_run_time);
      let runtime = write_meta(|m| m.stop_timer());

      post_exec.exec();

      let was_func_def = write_meta(|m| m.take_last_was_func_def());
      let should_write = read_shopts(|o| o.core.auto_hist)
        && (!was_func_def || !read_shopts(|o| o.set.nolog))
        && !builtin::fixcmd::NO_HIST_SAVE.swap(false, Ordering::SeqCst)
        && !input.is_empty();

      if let Some(token) = token
        && !should_write
      {
        readline
          .history
          .delete("WHERE token = ?1", rusqlite::params![token.to_string()])?;
      }

      if read_shopts(|s| s.core.auto_hist)
        && should_write
        && let Some(token) = token
        && let Err(e) = readline
          .history
          .set_status(token, runtime, state::get_status())
      {
        e.print_error();
      }

      readline.history.refresh_hint_cache();

      let rl_cleanup_start = Instant::now();
      with_term(|t| t.fix_cursor_column())?;
      write_term!("\n\r")?;

      // Reset for next command with fresh prompt
      readline.reset(true)?;
      log::info!(
        "Readline reset and prompt redraw in {:.2?}",
        rl_cleanup_start.elapsed()
      );

      let real_end = exec_start.elapsed();
      log::info!("Total round trip time: {:.2?}", real_end);
      Ok(false)
    }
    Ok(ReadlineEvent::Eof) => {
      // Ctrl+D on empty line
      QUIT_CODE.store(0, Ordering::SeqCst);
      Ok(true)
    }
    Ok(ReadlineEvent::Pending) => {
      // No complete input yet, keep polling
      Ok(false)
    }
    Err(e) => match e.kind() {
      ShErrKind::CleanExit(code) => {
        QUIT_CODE.store(*code, Ordering::SeqCst);
        Ok(true)
      }
      _ => {
        e.print_error();
        Ok(false)
      }
    },
  }
}

fn resolve_keymap(readline: &mut ShedLine) -> ShResult<()> {
  let keymap_flags = readline.curr_keymap_flags();
  let matches = read_logic(|l| l.keymaps_filtered(keymap_flags, &readline.pending_keymap));
  // If there's an exact match, fire it; otherwise flush as normal keys
  let exact = matches
    .iter()
    .find(|km| km.compare(&readline.pending_keymap) == KeyMapMatch::IsExact);
  if let Some(km) = exact {
    let action = km.action_expanded();
    readline.pending_keymap.clear();
    for key in action {
      let event = readline.handle_key(key).transpose();
      if let Some(event) = event {
        handle_readline_event(readline, event)?;
      }
    }
  } else {
    let buffered = std::mem::take(&mut readline.pending_keymap);
    for key in buffered {
      let event = readline.handle_key(key).transpose();
      if let Some(event) = event {
        handle_readline_event(readline, event)?;
      }
    }
  }
  readline.print_line(false)?;
  Ok(())
}

fn handle_socket_request(
  conn: UnixStream,
  request: SocketRequest,
  readline: &mut ShedLine,
) -> ShResult<Option<ReadlineEvent>> {
  match request {
    SocketRequest::PostSystemMessage(msg) => {
      write_meta(|m| m.post_system_message(msg));
      write(&conn, b"ok\n").ok();
    }
    SocketRequest::PostStatusMessage(msg) => {
      write_meta(|m| m.post_status_message(msg));
      write(&conn, b"ok\n").ok();
    }
    SocketRequest::Subscribe => {
      write_meta(|m| m.push_subscriber(conn));
    }
    SocketRequest::RefreshPrompt => {
      kill(Pid::this(), Signal::SIGUSR1)?;
      write(&conn, b"ok\n").ok();
    }
    SocketRequest::LineGet(line_header) => {
      let LineData {
        buffer,
        cursor,
        anchor,
        hint,
        mode,
      } = readline.get_line_data();
      match line_header {
        LineHeader::Buffer => {
          write(&conn, buffer.as_bytes()).ok();
          write(&conn, b"\n").ok();
        }
        LineHeader::Cursor => {
          write(&conn, cursor.to_string().as_bytes()).ok();
          write(&conn, b"\n").ok();
        }
        LineHeader::Anchor => {
          if let Some(anchor) = anchor {
            write(&conn, anchor.to_string().as_bytes()).ok();
          }
          write(&conn, b"\n").ok();
        }
        LineHeader::Hint => {
          if let Some(hint) = hint {
            write(&conn, hint.as_bytes()).ok();
          }
          write(&conn, b"\n").ok();
        }
        LineHeader::Mode => {
          write(&conn, mode.to_string().as_bytes()).ok();
          write(&conn, b"\n").ok();
        }
      }
    }
    SocketRequest::LineSet(line_header, value) => {
      match line_header {
        LineHeader::Buffer => {
          readline.editor.edit(|this| {
            this.set_buffer(value.clone());
          });
          readline
            .history
            .update_pending_cmd((&readline.editor.joined(), readline.editor.cursor_to_flat()));
          let hint = readline.history.get_hint();
          readline.editor.set_hint(hint);
          readline.editor.move_cursor_to_end();
          readline.needs_redraw = true;
        }
        LineHeader::Cursor => readline.editor.with_hint(|this| {
          if let Some((row, col)) = value.split_once(':')
            && let Ok(row) = row.parse::<usize>()
            && let Ok(col) = col.parse::<usize>()
          {
            this.set_cursor(Pos::new(row, col));
          } else if let Ok(pos) = value.parse::<usize>() {
            this.set_cursor_from_flat(pos);
          }
        }),
        LineHeader::Hint => {
          readline
            .editor
            .set_hint(Some(Hint::Override(to_lines(value))));
        }
        LineHeader::Mode => {
          let Ok(mode) = value.parse::<ModeReport>() else {
            // invalid mode report, ignore
            return Ok(None);
          };
          let mut mode = mode.as_edit_mode();
          readline.swap_mode(&mut mode);
        }
        LineHeader::Anchor => {
          if let Some((row, col)) = value.split_once(':')
            && let Ok(row) = row.parse::<usize>()
            && let Ok(col) = col.parse::<usize>()
          {
            readline.editor.set_anchor(Pos::new(row, col));
          } else if let Ok(pos) = value.parse::<usize>() {
            readline.editor.set_anchor_from_flat(pos);
          }
        }
      }
    }
    SocketRequest::LineSendKeys(events) => {
      for key in events {
        if let Some(event) = readline.dispatch_key(key)? {
          return Ok(Some(event));
        }
      }
    }
    SocketRequest::Query(query_header) => match query_header {
      QueryHeader::Cwd => {
        let cwd = env::current_dir()?.to_string_lossy().to_string();
        write(&conn, cwd.as_bytes()).ok();
        write(&conn, b"\n").ok();
      }
      QueryHeader::GetVar(var) => {
        let var = read_vars(|v| v.get_var(&var));
        write(&conn, var.as_bytes()).ok();
        write(&conn, b"\n").ok();
      }
      QueryHeader::SetVar(var, val, flags) => {
        write_vars(|v| v.set_var(&var, VarKind::Str(val), flags)).ok();
        write(&conn, b"ok\n").ok();
      }
      QueryHeader::Status(headers) => {
        let mut responses = vec![];
        for header in headers {
          match header {
            StatusHeader::ExitCode => responses.push(state::get_status().to_string()),
            StatusHeader::CommandName => {
              if let Some(job) = read_meta(|m| m.last_job().cloned())
                && let Some(cmd) = job.name()
              {
                responses.push(cmd.to_string());
              } else {
                responses.push("".to_string());
              }
            }
            StatusHeader::Runtime => {
              let Some(dur) = write_meta(|m| m.get_time()) else {
                responses.push("".to_string());
                continue;
              };
              responses.push(format!("{}", dur.as_millis()));
            }
            StatusHeader::Pid => {
              let Some(job) = write_meta(|m| m.last_job().cloned()) else {
                responses.push("".to_string());
                continue;
              };
              responses.push(
                job
                  .get_pids()
                  .first()
                  .map(|p| p.to_string())
                  .unwrap_or_default(),
              );
            }
            StatusHeader::Pgid => {
              let Some(job) = write_meta(|m| m.last_job().cloned()) else {
                responses.push("".to_string());
                continue;
              };
              responses.push(job.pgid().to_string());
            }
          }
        }
        let output = responses.join(" ");
        write(&conn, output.as_bytes()).ok();
        write(&conn, b"\n").ok();
      }
    },
  }
  Ok(None)
}
