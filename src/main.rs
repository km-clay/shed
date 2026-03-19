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
pub mod testutil;

use std::os::fd::BorrowedFd;
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::read;

use crate::builtin::keymap::KeyMapMatch;
use crate::builtin::trap::TrapTarget;
use crate::libsh::error::{self, ShErr, ShErrKind, ShResult};
use crate::libsh::sys::TTY_FILENO;
use crate::libsh::utils::AutoCmdVecUtils;
use crate::parse::execute::{exec_dash_c, exec_input};
use crate::prelude::*;
use crate::procio::borrow_fd;
use crate::readline::term::{LineWriter, RawModeGuard, raw_mode};
use crate::readline::{Prompt, ReadlineEvent, ShedVi};
use crate::signal::{
  GOT_SIGUSR1, GOT_SIGWINCH, JOB_DONE, QUIT_CODE, check_signals, sig_setup, signals_pending,
};
use crate::state::{
  AutoCmdKind, read_logic, read_shopts, source_env, source_login, source_rc, write_jobs,
  write_meta, write_shopts,
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
}

/// We need to make sure that even if we panic, our child processes get sighup
fn setup_panic_handler() {
  let default_panic_hook = std::panic::take_hook();
  std::panic::set_hook(Box::new(move |info| {
    let _ = state::SHED.try_with(|shed| {
      if let Ok(mut jobs) = shed.jobs.try_borrow_mut() {
        jobs.hang_up();
      }
    });

    let data_dir = env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
      let home = env::var("HOME").unwrap();
      format!("{home}/.local/share")
    });
    let log_dir = Path::new(&data_dir).join("shed").join("log");
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
    unsafe { env::set_var("SHLVL", (lvl + 1).to_string()) };
  } else {
    unsafe { env::set_var("SHLVL", "1") };
  }

  if let Err(e) = source_env() {
    e.print_error();
  }

  if let Err(e) = if let Some(cmd) = args.command {
    exec_dash_c(cmd)
  } else if args.stdin || !isatty(STDIN_FILENO).unwrap_or(false) {
    read_commands(args.script_args)
  } else if !args.script_args.is_empty() {
    let path = args.script_args.remove(0);
    run_script(path, args.script_args)
  } else {
    let res = shed_interactive(args);
    write(borrow_fd(*TTY_FILENO), b"\x1b[?2004l").ok(); // disable bracketed paste mode on exit
    res
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
        return Err(ShErr::simple(
          ShErrKind::CleanExit(1),
          format!("error reading from stdin: {e}"),
        ));
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
    return Err(ShErr::simple(
      ShErrKind::CleanExit(1),
      "input file not found",
    ));
  }
  let Ok(input) = fs::read_to_string(path) else {
    eprintln!("shed: Failed to read input file: {}", path.display());
    QUIT_CODE.store(1, Ordering::SeqCst);
    return Err(ShErr::simple(
      ShErrKind::CleanExit(1),
      "failed to read input file",
    ));
  };

  write_vars(|v| {
    v.cur_scope_mut()
      .bpush_arg(path.to_string_lossy().to_string())
  });
  for arg in args {
    write_vars(|v| v.cur_scope_mut().bpush_arg(arg))
  }

  exec_input(input, None, false, Some(path_raw))
}

fn shed_interactive(args: ShedArgs) -> ShResult<()> {
  let _raw_mode = raw_mode(); // sets raw mode, restores termios on drop
  sig_setup(args.login_shell);

  if args.login_shell
    && let Err(e) = source_login()
  {
    e.print_error();
  }

  if let Err(e) = source_rc() {
    e.print_error();
  }

  // Create readline instance with initial prompt
  let mut readline = match ShedVi::new(Prompt::new(), *TTY_FILENO) {
    Ok(rl) => rl,
    Err(e) => {
      eprintln!("Failed to initialize readline: {e}");
      QUIT_CODE.store(1, Ordering::SeqCst);
      return Err(ShErr::simple(
        ShErrKind::CleanExit(1),
        "readline initialization failed",
      ));
    }
  };

  readline.writer.flush_write("\x1b[?2004h")?; // enable bracketed paste mode

  // Main poll loop
  loop {
    write_meta(|m| {
      m.try_rehash_commands();
      m.try_rehash_cwd_listing();
    });
    error::clear_color();

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
            return Ok(());
          }
          _ => e.print_error(),
        }
      }
    }

    if GOT_SIGWINCH.swap(false, Ordering::SeqCst) {
      log::info!("Window size change detected, updating readline dimensions");
      // Restore cursor to saved row before clearing, since the terminal
      // may have moved it during resize/rewrap
      readline.writer.update_t_cols();
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

    // Poll for
    // stdin input
    let mut fds = [PollFd::new(
      unsafe { BorrowedFd::borrow_raw(*TTY_FILENO) },
      PollFlags::POLLIN,
    )];

    let mut exec_if_timeout = None;

    let timeout = if readline.pending_keymap.is_empty() {
      let screensaver_cmd = read_shopts(|o| o.prompt.screensaver_cmd.clone())
        .trim()
        .to_string();
      let screensaver_idle_time = read_shopts(|o| o.prompt.screensaver_idle_time);
      if screensaver_idle_time > 0 && !screensaver_cmd.is_empty() {
        exec_if_timeout = Some(screensaver_cmd);
        PollTimeout::from((screensaver_idle_time * 1000) as u16)
      } else {
        PollTimeout::MAX
      }
    } else {
      PollTimeout::from(1000u16)
    };

    match poll(&mut fds, timeout) {
      Ok(0) => {
        // We timed out.
        if let Some(cmd) = exec_if_timeout {
          let prepared = ReadlineEvent::Line(cmd);
          let saved_hist_opt = read_shopts(|o| o.core.auto_hist);
          let _guard = scopeguard::guard(saved_hist_opt, |opt| {
            write_shopts(|o| o.core.auto_hist = opt);
          });
          write_shopts(|o| o.core.auto_hist = false); // don't save screensaver command to history

          match handle_readline_event(&mut readline, Ok(prepared))? {
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

    // Timeout — resolve pending keymap ambiguity
    if !readline.pending_keymap.is_empty()
      && fds[0]
        .revents()
        .is_none_or(|r| !r.contains(PollFlags::POLLIN))
    {
      log::debug!(
        "[keymap timeout] resolving pending={:?}",
        readline.pending_keymap
      );
      let keymap_flags = readline.curr_keymap_flags();
      let matches = read_logic(|l| l.keymaps_filtered(keymap_flags, &readline.pending_keymap));
      // If there's an exact match, fire it; otherwise flush as normal keys
      let exact = matches
        .iter()
        .find(|km| km.compare(&readline.pending_keymap) == KeyMapMatch::IsExact);
      if let Some(km) = exact {
        log::debug!(
          "[keymap timeout] firing exact match: {:?} -> {:?}",
          km.keys,
          km.action
        );
        let action = km.action_expanded();
        readline.pending_keymap.clear();
        for key in action {
          let event = readline.handle_key(key).transpose();
          if let Some(event) = event {
            handle_readline_event(&mut readline, event)?;
          }
        }
      } else {
        log::debug!(
          "[keymap timeout] no exact match, flushing {} keys as normal input",
          readline.pending_keymap.len()
        );
        let buffered = std::mem::take(&mut readline.pending_keymap);
        for key in buffered {
          let event = readline.handle_key(key).transpose();
          if let Some(event) = event {
            handle_readline_event(&mut readline, event)?;
          }
        }
      }
      readline.print_line(false)?;
      continue;
    }

    // Check if stdin has data
    if fds[0]
      .revents()
      .is_some_and(|r| r.contains(PollFlags::POLLIN))
    {
      let mut buffer = [0u8; 1024];
      match read(*TTY_FILENO, &mut buffer) {
        Ok(0) => {
          // EOF
          break;
        }
        Ok(n) => {
          readline.feed_bytes(&buffer[..n]);
        }
        Err(Errno::EINTR) => {
          // Interrupted, continue to handle signals
          continue;
        }
        Err(e) => {
          eprintln!("read error: {e}");
          break;
        }
      }
    }

    // Process any available input
    let event = readline.process_input();
    match handle_readline_event(&mut readline, event)? {
      true => return Ok(()),
      false => { /* continue looping */ }
    }
  }

  Ok(())
}

fn handle_readline_event(readline: &mut ShedVi, event: ShResult<ReadlineEvent>) -> ShResult<bool> {
  match event {
    Ok(ReadlineEvent::Line(input)) => {
      let pre_exec = read_logic(|l| l.get_autocmds(AutoCmdKind::PreCmd));
      let post_exec = read_logic(|l| l.get_autocmds(AutoCmdKind::PostCmd));

      pre_exec.exec_with(&input);

      // Time this command and temporarily restore cooked terminal mode while it runs.
      let start = Instant::now();
      write_meta(|m| m.start_timer());
      if let Err(e) = RawModeGuard::with_cooked_mode(|| {
        exec_input(input.clone(), None, true, Some("<stdin>".into()))
      }) {
        // CleanExit signals an intentional shell exit; any other error is printed.
        match e.kind() {
          ShErrKind::Interrupt => {
            // We got Ctrl+C during command execution
            // Just fall through here
          }
          ShErrKind::CleanExit(code) => {
            QUIT_CODE.store(*code, Ordering::SeqCst);
            return Ok(true);
          }
          _ => e.print_error(),
        }
      }
      let command_run_time = start.elapsed();
      log::info!("Command executed in {:.2?}", command_run_time);
      write_meta(|m| m.stop_timer());

      post_exec.exec_with(&input);

      if read_shopts(|s| s.core.auto_hist) && !input.is_empty() {
        readline.history.push(input.clone());
        readline.history.save()?;
      }

      readline.fix_column()?;
      readline.writer.flush_write("\n\r")?;

      // Reset for next command with fresh prompt
      readline.reset(true)?;

      let real_end = start.elapsed();
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
